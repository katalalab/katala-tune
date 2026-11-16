//! Real process/kernel tests. This integration parent never acquires the static claim.
//! The Tauri helper is its actual setup boundary, not a native Tauri E2E test.
#[path = "../../../src-tauri/src/collection_admission.rs"]
mod tauri_admission;
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Arc,
    time::{Duration, Instant},
};
use tune_core::{engine::NoHost, gui_collection::ensure_gui_collection_owner};

struct OwnedChild(Child);
impl OwnedChild {
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "owned harness child timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn wait(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "owned child barrier timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn child(root: &Path, role: &str) -> OwnedChild {
    OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "collection_process_lifetime_and_retry", "--nocapture"])
            .env("TUNE_COLLECTION_TEST_ROOT", root)
            .env("TUNE_COLLECTION_TEST_ROLE", role)
            .env("HOME", root.join(role))
            .env("APPDATA", root.join(role))
            .env("TMPDIR", root.join(role))
            .spawn()
            .unwrap(),
    )
}
fn primary(root: &Path) -> Result<Arc<tune_core::engine::Engine>, String> {
    tauri_admission::open_primary_engine(root.join("tauri-nodes.json"), root.join("tauri-data"), Arc::new(NoHost))
}
#[cfg(unix)]
fn lock_identity() -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    #[allow(unsafe_code)]
    let uid = unsafe { libc::geteuid() };
    let meta = std::fs::symlink_metadata(format!("/tmp/katala-tune-collector-v1-{uid}/collection.lock")).unwrap();
    assert!(meta.is_file());
    assert_eq!(meta.nlink(), 1);
    (meta.dev(), meta.ino())
}
#[test]
fn collection_process_lifetime_and_retry() {
    if let Some(root) = std::env::var_os("TUNE_COLLECTION_TEST_ROOT") {
        let root = PathBuf::from(root);
        match std::env::var("TUNE_COLLECTION_TEST_ROLE").unwrap().as_str() {
            "owner" => {
                // Concurrency tests the real first-acquisition mutex and success caching.
                let threads: Vec<_> = (0..8).map(|_| std::thread::spawn(ensure_gui_collection_owner)).collect();
                for t in threads {
                    t.join().unwrap().expect("unexpected external owner; never take over");
                }
                std::fs::write(root.join("owner-ready"), "ready").unwrap();
                wait(&root.join("owner-exit"));
            }
            "retry" => {
                assert!(primary(&root).is_err());
                assert!(!root.join("tauri-nodes.json").exists());
                assert!(!root.join("tauri-data").exists(), "actual Tauri setup boundary must refuse before Engine writes");
                std::fs::write(root.join("retry-refused"), "refused").unwrap();
                wait(&root.join("retry-now"));
                let engine = primary(&root).unwrap();
                ensure_gui_collection_owner().unwrap();
                drop(engine); // Data lifetime is independent and cannot release the GUI claim.
                std::fs::write(root.join("retry-owned"), "owned").unwrap();
                wait(&root.join("retry-exit"));
            }
            "probe" => {
                ensure_gui_collection_owner().expect("failure must not trigger automatic resume elsewhere");
            }
            "busy" => {
                assert!(ensure_gui_collection_owner().is_err(), "claim must survive Engine drop");
            }
            _ => panic!("unknown owned child role"),
        }
        return;
    }
    let root = std::env::temp_dir().join(format!("gui-collection-process-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    for abnormal in [false, true] {
        let round = root.join(if abnormal { "abnormal" } else { "normal" });
        std::fs::create_dir(&round).unwrap();
        let mut owner = child(&round, "owner");
        wait(&round.join("owner-ready"));
        #[cfg(unix)]
        let inode = lock_identity();
        let mut retry = child(&round, "retry");
        wait(&round.join("retry-refused"));
        if abnormal {
            owner.0.kill().unwrap();
            assert!(!owner.0.wait().unwrap().success());
        } else {
            std::fs::write(round.join("owner-exit"), "exit").unwrap();
            owner.finish();
        }
        // The waiting refused process has not automatically resumed. A third
        // explicit participant can acquire before we permit the retry.
        child(&round, "probe").finish();
        std::fs::write(round.join("retry-now"), "retry").unwrap();
        wait(&round.join("retry-owned"));
        child(&round, "busy").finish();
        std::fs::write(round.join("retry-exit"), "exit").unwrap();
        retry.finish();
        child(&round, "probe").finish();
        #[cfg(unix)]
        assert_eq!(lock_identity(), inode, "normal/abnormal recovery must not unlink or replace the fixed claim file");
    }
    std::fs::remove_dir_all(root).unwrap();
}
