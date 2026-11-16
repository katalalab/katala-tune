//! Synthetic Engine admission tests; children are this unit-test executable only.
use super::{Engine, NoHost};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kt-data-claim-{}-{}", std::process::id(), crate::db::now_ms()));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("owner.json"), br#"{"nodes":[]}"#).unwrap();
        Self(path)
    }
    fn open(&self) -> Result<Arc<Engine>, String> {
        Engine::open(self.0.join("owner.json"), self.0.join("data"), Arc::new(NoHost))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child(root: &Path, data: &Path, mode: &str) -> OwnedChild {
    OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "engine::ownership_tests::owned_engine_child", "--ignored", "--nocapture"])
            .env("TUNE_ENGINE_FIXTURE", root)
            .env("TUNE_ENGINE_DATA", data)
            .env("TUNE_ENGINE_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}
fn finished(child: &mut OwnedChild) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "owned child failed: {status}");
            return;
        }
        assert!(Instant::now() < deadline, "owned child exceeded deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn contender(root: &Path, data: &Path, busy: bool) {
    finished(&mut child(root, data, if busy { "busy" } else { "open" }));
}

#[test]
#[ignore = "owned Engine child: only the synthetic parent supplies paths"]
fn owned_engine_child() {
    let root = PathBuf::from(std::env::var_os("TUNE_ENGINE_FIXTURE").unwrap());
    let data = PathBuf::from(std::env::var_os("TUNE_ENGINE_DATA").unwrap());
    let mode = std::env::var("TUNE_ENGINE_MODE").unwrap();
    if mode == "owner" {
        let engine = Engine::open(root.join("owner.json"), data, Arc::new(NoHost)).unwrap();
        let db = engine.db.clone();
        drop(engine);
        fs::write(root.join("ready"), b"Store remains alive").unwrap();
        let mut command = String::new();
        std::io::stdin().read_line(&mut command).unwrap();
        assert_eq!(command.trim(), "release");
        drop(db);
    } else {
        let config = root.join("contender.json");
        let result = Engine::open(config.clone(), data, Arc::new(NoHost));
        if mode == "busy" {
            let error = result.err().expect("same-data Engine contender must be refused before config or DB writes");
            assert!(error.starts_with("Engine data ownership busy:"), "{error}");
            assert!(!config.exists(), "refused contender created config");
        } else {
            assert!(result.is_ok());
        }
    }
}

#[test]
fn independent_engine_contender_is_refused_while_db_arc_survives() {
    let fixture = Fixture::new();
    let engine = fixture.open().unwrap();
    let db = engine.db.clone();
    drop(engine);
    contender(&fixture.0, &fixture.0.join("data"), true);
    drop(db);
    contender(&fixture.0, &fixture.0.join("data"), false);
}

fn data_bytes(data: &Path) -> std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> {
    fs::read_dir(data)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name();
            // Windows byte-range locks can forbid ordinary reads of the held
            // claim and SQLite's shm lock bytes. Preserve their directory entries;
            // compare all database/WAL/journal bytes, and test stale lock bytes
            // separately after its owner closes.
            let bytes = if name == ".katala-tune-engine.lock" || name == "katala-tune.db-shm" { Vec::new() } else { fs::read(entry.path()).unwrap() };
            (name, bytes)
        })
        .collect()
}

#[test]
fn same_data_is_busy_without_config_database_or_journal_changes_but_other_data_coexists() {
    let fixture = Fixture::new();
    let engine = fixture.open().unwrap();
    engine.with_db(|db| db.set_meta("ownership_fixture", &serde_json::json!({"enabled": true}))).unwrap();
    let before = data_bytes(engine.data_dir());
    contender(&fixture.0, engine.data_dir(), true);
    assert_eq!(data_bytes(engine.data_dir()), before);
    assert!(!fixture.0.join("contender.json").exists());
    contender(&fixture.0, &fixture.0.join("other-data"), false);
    assert_eq!(data_bytes(engine.data_dir()), before);
}

#[test]
fn stale_file_missing_directories_and_constructor_failure_release_claim() {
    let fixture = Fixture::new();
    let data = fixture.0.join("data");
    fs::create_dir(&data).unwrap();
    let lock = data.join(".katala-tune-engine.lock");
    fs::write(&lock, b"residual lock is not ownership").unwrap();
    fs::write(data.join(crate::db::DB_FILE), b"invalid SQLite fixture").unwrap();
    let error = fixture.open().err().unwrap();
    assert_eq!(error, "Engine database could not be opened or initialized");
    assert_eq!(fs::read(data.join(crate::db::DB_FILE)).unwrap(), b"invalid SQLite fixture");
    fs::remove_file(data.join(crate::db::DB_FILE)).unwrap();
    let engine = fixture.open().unwrap();
    assert_eq!(engine.data_dir(), data.canonicalize().unwrap());
    drop(engine);
    contender(&fixture.0, &data, false);
    assert_eq!(fs::read(lock).unwrap(), b"residual lock is not ownership");
    let nested = fixture.0.join("missing/parent/data");
    let engine = Engine::open(fixture.0.join("owner.json"), nested.clone(), Arc::new(NoHost)).unwrap();
    assert_eq!(engine.data_dir(), nested.canonicalize().unwrap());
}

#[test]
fn normal_and_abnormal_owned_process_exit_release_kernel_lock_without_unlink() {
    let fixture = Fixture::new();
    let data = fixture.0.join("data");
    for abnormal in [false, true] {
        let ready = fixture.0.join("ready");
        let mut owner = child(&fixture.0, &data, "owner");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.exists() {
            assert!(owner.0.try_wait().unwrap().is_none(), "owner exited before ready");
            assert!(Instant::now() < deadline, "owner readiness deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        contender(&fixture.0, &data, true);
        if abnormal {
            owner.0.kill().unwrap();
            owner.0.wait().unwrap();
        } else {
            owner.0.stdin.as_mut().unwrap().write_all(b"release\n").unwrap();
            finished(&mut owner);
        }
        assert!(data.join(".katala-tune-engine.lock").is_file());
        contender(&fixture.0, &data, false);
        fs::remove_file(fixture.0.join("contender.json")).unwrap();
        fs::remove_file(ready).unwrap();
    }
}

#[test]
fn unsafe_nonregular_and_hardlinked_files_are_refused_before_config_creation() {
    let fixture = Fixture::new();
    let data = fixture.0.join("data");
    fs::create_dir(&data).unwrap();
    let config = fixture.0.join("must-not-create.json");
    for name in [".katala-tune-engine.lock", crate::db::DB_FILE, "katala-tune.db-wal", "katala-tune.db-shm", "katala-tune.db-journal"] {
        let path = data.join(name);
        // A prior successful claim can leave its persistent lock file.
        if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        fs::create_dir(&path).unwrap();
        let error = Engine::open(config.clone(), data.clone(), Arc::new(NoHost)).err().unwrap();
        assert!(error.starts_with("Engine data ownership unsafe path:"), "{error}");
        assert!(!config.exists());
        fs::remove_dir(&path).unwrap();
        let original = fixture.0.join("unrelated-file");
        fs::write(&original, b"preserve unrelated bytes").unwrap();
        fs::hard_link(&original, &path).unwrap();
        let error = Engine::open(config.clone(), data.clone(), Arc::new(NoHost)).err().unwrap();
        assert!(error.starts_with("Engine data ownership unsafe path:"), "{error}");
        assert!(!config.exists());
        assert_eq!(fs::read(&original).unwrap(), b"preserve unrelated bytes");
        fs::remove_file(path).unwrap();
        fs::remove_file(original).unwrap();
    }
    let blocker = fixture.0.join("file-parent");
    fs::write(&blocker, b"not a directory").unwrap();
    let error = Engine::open(config.clone(), blocker.join("data"), Arc::new(NoHost)).err().unwrap();
    assert!(error.starts_with("Engine data ownership I/O error:"));
    assert!(!error.contains(&fixture.0.to_string_lossy().to_string()));
    assert!(!config.exists());
    drop(fixture.open().unwrap());
}

#[cfg(unix)]
#[test]
fn directory_aliases_resolve_before_dotdot_and_file_symlinks_are_refused() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let engine = fixture.open().unwrap();
    let data = engine.data_dir();
    fs::create_dir(data.join("child")).unwrap();
    symlink(data.join("child"), fixture.0.join("alias")).unwrap();
    let alias = fixture.0.join("alias/..");
    contender(&fixture.0, &alias, true);
    drop(engine);
    let through_alias = Engine::open(fixture.0.join("owner.json"), alias, Arc::new(NoHost)).unwrap();
    assert_eq!(through_alias.data_dir(), fixture.0.join("data").canonicalize().unwrap());
    drop(through_alias);
    let config = fixture.0.join("must-not-create.json");
    for name in [".katala-tune-engine.lock", crate::db::DB_FILE, "katala-tune.db-wal", "katala-tune.db-shm", "katala-tune.db-journal"] {
        let path = fixture.0.join("data").join(name);
        if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        let absent_target = fixture.0.join("must-not-follow");
        symlink(&absent_target, &path).unwrap();
        let error = Engine::open(config.clone(), fixture.0.join("data"), Arc::new(NoHost)).err().unwrap();
        assert!(error.starts_with("Engine data ownership unsafe path:"));
        assert!(!absent_target.exists());
        assert!(!config.exists());
        fs::remove_file(path).unwrap();
    }
}

#[cfg(windows)]
#[test]
fn windows_existing_directory_case_alias_contends() {
    let fixture = Fixture::new();
    let _engine = fixture.open().unwrap();
    contender(&fixture.0, &fixture.0.join("DATA"), true);
}
