use super::*;
use crate::db::sha256_hex;
use serde_json::{Value, json};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    source: PathBuf,
    bin: PathBuf,
    bytes: Vec<u8>,
}
impl Fixture {
    fn new(old: bool) -> Self {
        let root = std::env::temp_dir().join(format!("native-install-fixture-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let source = root.join("source");
        let bin = root.join("data/bin");
        fs::create_dir(&source).unwrap();
        fs::create_dir(root.join("data")).unwrap();
        fs::create_dir(&bin).unwrap();
        let bytes = if cfg!(windows) { crate::helpers::binary::fixtures::pe() } else { crate::helpers::binary::fixtures::macho() };
        let entries: Vec<Value> = [("katala-tune-gpui", "gui"), ("tune", "diagnostic-cli"), ("tune-agent", "native-sampler")]
            .iter()
            .map(|(name, role)| {
                let file = crate::helpers::exe(name);
                fs::write(source.join(&file), &bytes).unwrap();
                json!({"file":file,"role":role,"bytes":bytes.len(),"sha256":sha256_hex(&bytes)})
            })
            .collect();
        let (platform, architecture, target) =
            if cfg!(windows) { ("win32", "x64", "x86_64-pc-windows-msvc") } else { ("darwin", "arm64", "aarch64-apple-darwin") };
        let manifest = json!({"schema":"katala_gpui_preview.v2","version":"0.7.0-rc.1","revision":"a".repeat(40),"sourceTree":"b".repeat(40),"platform":platform,"architecture":architecture,"build":{"profile":"release","target":target},"ci":{"repository":"fixture/project","workflowRef":"fixture/project/.github/workflows/gpui-preview.yml@refs/heads/main","workflowSha":"c".repeat(40),"eventName":"workflow_dispatch","runId":"123","runAttempt":"1"},"files":entries});
        fs::write(source.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        fs::write(bin.join(crate::helpers::exe("tune")), b"action helper sentinel").unwrap();
        fs::write(bin.join("tune-agent.old-unowned"), b"unowned sentinel").unwrap();
        fs::write(root.join("data/settings.json"), b"settings sentinel").unwrap();
        if old {
            fs::write(bin.join(crate::helpers::exe("tune-agent")), b"old sampler").unwrap();
            executable(&bin.join(crate::helpers::exe("tune-agent")));
        }
        Self { root, source, bin, bytes }
    }
    fn target(&self) -> PathBuf {
        self.bin.join(crate::helpers::exe("tune-agent"))
    }
    fn sentinels(&self) {
        assert_eq!(fs::read(self.bin.join(crate::helpers::exe("tune"))).unwrap(), b"action helper sentinel");
        assert_eq!(fs::read(self.bin.join("tune-agent.old-unowned")).unwrap(), b"unowned sentinel");
        assert_eq!(fs::read(self.root.join("data/settings.json")).unwrap(), b"settings sentinel");
        for name in ["tune", "katala-tune-gpui"] {
            assert_eq!(fs::read(self.source.join(crate::helpers::exe(name))).unwrap(), self.bytes);
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    let _ = path;
}
#[test]
fn install_exact_bytes_and_preserve_unowned_files() {
    let f = Fixture::new(true);
    let prepared = prepare_native_sampler(&f.source).unwrap();
    let snapshot = inspect_at(&f.bin).unwrap();
    let result = install_at(prepared, snapshot).unwrap();
    assert!(result.changed());
    assert_eq!(fs::read(f.target()).unwrap(), f.bytes);
    f.sentinels();
}

type Hook = Box<dyn FnOnce()>;
thread_local! {
    static FAILURES: std::cell::RefCell<Vec<Operation>> = const { std::cell::RefCell::new(Vec::new()) };
    static HOOK: std::cell::RefCell<Option<(Operation, Hook)>> = const { std::cell::RefCell::new(None) };
    static COLLISIONS: std::cell::RefCell<Option<CollisionFiles>> = const { std::cell::RefCell::new(None) };
}
struct CollisionFiles {
    bin: PathBuf,
    stages: usize,
    backups: usize,
    paths: Vec<PathBuf>,
}
fn collide_names(bin: &Path, stages: usize, backups: usize) {
    COLLISIONS.with(|c| *c.borrow_mut() = Some(CollisionFiles { bin: bin.to_owned(), stages, backups, paths: Vec::new() }));
}
pub(super) fn name_chosen(role: &str, name: &str) {
    // The global counter may advance in other tests. Collide with the actual
    // allocated name in this thread's owned fixture; never replace the allocator
    // or fake the exclusive-create / no-replace filesystem result.
    COLLISIONS.with(|c| {
        let mut c = c.borrow_mut();
        let Some(c) = c.as_mut() else {
            return;
        };
        let remaining = match role {
            "stage" => &mut c.stages,
            "backup" => &mut c.backups,
            _ => panic!("unexpected install name role"),
        };
        if *remaining == 0 {
            return;
        }
        *remaining -= 1;
        let path = c.bin.join(name);
        fs::File::create_new(&path).unwrap().write_all(b"collision sentinel").unwrap();
        c.paths.push(path);
    });
}
fn take_collision_files() -> Vec<PathBuf> {
    let c = COLLISIONS.with(|c| c.borrow_mut().take().unwrap());
    assert_eq!((c.stages, c.backups), (0, 0), "every requested real collision was exercised");
    c.paths
}
pub(super) fn fault(operation: Operation) -> Result<(), InstallError> {
    let hook = HOOK.with(|h| {
        let mut h = h.borrow_mut();
        if h.as_ref().is_some_and(|v| v.0 == operation) { h.take().map(|v| v.1) } else { None }
    });
    if let Some(h) = hook {
        h();
    }
    let fail = FAILURES.with(|f| {
        let mut f = f.borrow_mut();
        if f.first() == Some(&operation) {
            f.remove(0);
            true
        } else {
            false
        }
    });
    if fail { Err(rejected(operation, Cause::Io)) } else { Ok(()) }
}
fn fail(operations: &[Operation]) {
    FAILURES.with(|f| *f.borrow_mut() = operations.to_vec());
}
fn hook(operation: Operation, f: impl FnOnce() + 'static) {
    HOOK.with(|h| *h.borrow_mut() = Some((operation, Box::new(f))));
}
fn leftovers(f: &Fixture) -> Vec<PathBuf> {
    fs::read_dir(&f.bin).unwrap().map(|v| v.unwrap().path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(".tune-agent-")).collect()
}
fn change_manifest(f: &Fixture, mutate: impl FnOnce(&mut Value)) {
    let path = f.source.join("manifest.json");
    let mut value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    mutate(&mut value);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[test]
fn missing_target_install_and_same_hash_noop() {
    let f = Fixture::new(false);
    assert!(inspect_at(&f.bin).unwrap().is_missing());
    let r = install_at(prepare_native_sampler(&f.source).unwrap(), inspect_at(&f.bin).unwrap()).unwrap();
    assert!(r.changed());
    assert_eq!(fs::read(f.target()).unwrap(), f.bytes);
    let before = inspect_at(&f.bin).unwrap();
    let r = install_at(prepare_native_sampler(&f.source).unwrap(), inspect_at(&f.bin).unwrap()).unwrap();
    assert!(!r.changed());
    assert_eq!(before.observed, inspect_at(&f.bin).unwrap().observed);
    assert!(leftovers(&f).is_empty());
    f.sentinels();
}
#[test]
fn missing_bin_created_only_by_install() {
    let f = Fixture::new(false);
    fs::remove_dir_all(&f.bin).unwrap();
    let snapshot = inspect_at(&f.bin).unwrap();
    assert!(!f.bin.exists());
    install_at(prepare_native_sampler(&f.source).unwrap(), snapshot).unwrap();
    assert_eq!(fs::read(f.target()).unwrap(), f.bytes);
}
#[test]
fn prepared_bytes_survive_source_path_replacement() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let source = f.source.join(crate::helpers::exe("tune-agent"));
    fs::rename(&source, f.source.join("retired")).unwrap();
    fs::write(source, b"future bytes must never be copied").unwrap();
    install_at(p, inspect_at(&f.bin).unwrap()).unwrap();
    assert_eq!(fs::read(f.target()).unwrap(), f.bytes);
    f.sentinels();
}
#[test]
fn strict_manifest_rejects_schema_role_tuple_and_ci_relationships() {
    let changes: Vec<(&str, Value)> = vec![
        ("/schema", json!("katala_gpui_preview.v1")),
        ("/version", json!("01.2.3")),
        ("/revision", json!("f".repeat(39))),
        ("/sourceTree", json!("A".repeat(40))),
        ("/platform", json!("linux")),
        ("/architecture", json!("other")),
        ("/build/profile", json!("debug")),
        ("/build/target", json!("other-target")),
        ("/ci/repository", json!("different/project")),
        ("/ci/eventName", json!("pull_request")),
        ("/ci/runId", json!(123)),
        ("/ci/runAttempt", json!("0")),
        ("/ci/workflowSha", json!("c".repeat(40) + "\n")),
        ("/files/0/role", json!("native-sampler")),
        ("/files/0/file", json!("../outside")),
        ("/files/0/bytes", json!(0)),
        ("/files/0/bytes", json!(artifact::MAX_BINARY + 1)),
        ("/files/0/sha256", json!("0".repeat(64))),
    ];
    for (pointer, replacement) in changes {
        let f = Fixture::new(true);
        change_manifest(&f, |m| *m.pointer_mut(pointer).unwrap() = replacement);
        assert!(prepare_native_sampler(&f.source).is_err(), "{pointer}");
        assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
        assert!(leftovers(&f).is_empty());
    }
}
#[test]
fn duplicate_unknown_extra_missing_and_invalid_raw_manifest_rejected() {
    for variant in 0..10 {
        let f = Fixture::new(true);
        let path = f.source.join("manifest.json");
        let raw = fs::read_to_string(&path).unwrap();
        let bytes = match variant {
            0 => raw.replacen('{', "{\"schema\":\"katala_gpui_preview.v2\",", 1).into_bytes(),
            1 => raw.replacen('{', "{\"approved\":true,", 1).into_bytes(),
            2 => {
                change_manifest(&f, |m| {
                    let extra = m["files"][0].clone();
                    m["files"].as_array_mut().unwrap().push(extra);
                });
                fs::read(&path).unwrap()
            }
            3 => {
                change_manifest(&f, |m| {
                    m["ci"].as_object_mut().unwrap().remove("runId");
                });
                fs::read(&path).unwrap()
            }
            4 => vec![b' '; artifact::MAX_MANIFEST as usize + 1],
            5 => vec![],
            6 => vec![0xff],
            7 => b"{\"status\":\"integrity-checked\"}".to_vec(),
            8 => raw.replace("\"profile\":", "\"profile\":\"release\",\"profile\":").into_bytes(),
            _ => raw.replace("\"profile\":", "\"extra\":true,\"profile\":").into_bytes(),
        };
        fs::write(path, bytes).unwrap();
        assert!(prepare_native_sampler(&f.source).is_err(), "variant {variant}");
    }
}
#[test]
fn each_role_is_actually_read_and_missing_size_hash_structure_fail_closed() {
    for name in ["katala-tune-gpui", "tune", "tune-agent"] {
        for variant in 0..3 {
            let f = Fixture::new(true);
            let path = f.source.join(crate::helpers::exe(name));
            match variant {
                0 => fs::remove_file(path).unwrap(),
                1 => fs::write(path, b"different").unwrap(),
                _ => {
                    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
                    file.set_len(artifact::MAX_BINARY + 1).unwrap();
                }
            }
            assert!(prepare_native_sampler(&f.source).is_err());
        }
    }
    let f = Fixture::new(true);
    let corrupt = vec![0; f.bytes.len()];
    fs::write(f.source.join(crate::helpers::exe("tune-agent")), &corrupt).unwrap();
    change_manifest(&f, |m| m["files"][2]["sha256"] = json!(sha256_hex(&corrupt)));
    assert_eq!(prepare_native_sampler(&f.source).unwrap_err().cause(), Cause::InvalidArtifact);
}
#[test]
fn source_read_close_and_identity_failure_never_produces_prepared_value() {
    for op in [Operation::Read, Operation::Close, Operation::ReadIdentity] {
        let f = Fixture::new(true);
        fail(&[op]);
        assert_eq!(prepare_native_sampler(&f.source).unwrap_err().operation(), op);
        assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
    }
    #[cfg(target_os = "macos")]
    {
        let f = Fixture::new(true);
        let source = f.source.join("manifest.json");
        hook(Operation::ReadIdentity, move || {
            let b = fs::read(&source).unwrap();
            fs::rename(&source, source.with_extension("retired")).unwrap();
            fs::write(source, b).unwrap();
        });
        assert!(prepare_native_sampler(&f.source).is_err());
    }
}
#[test]
fn source_hardlinks_and_destination_hardlinks_are_rejected() {
    let f = Fixture::new(true);
    fs::hard_link(f.source.join(crate::helpers::exe("tune-agent")), f.source.join("alias")).unwrap();
    assert!(prepare_native_sampler(&f.source).is_err());
    let f = Fixture::new(true);
    fs::hard_link(f.target(), f.bin.join("alias")).unwrap();
    assert_eq!(inspect_at(&f.bin).unwrap_err().cause(), Cause::UnsafeFile);
    let f = Fixture::new(true);
    fs::remove_file(f.target()).unwrap();
    let action = f.bin.join(crate::helpers::exe("tune"));
    fs::hard_link(&action, f.target()).unwrap();
    assert_eq!(inspect_at(&f.bin).unwrap_err().cause(), Cause::UnsafeFile);
    assert_eq!(fs::read(action).unwrap(), b"action helper sentinel");
}
#[test]
fn fixed_new_hardlink_and_unowned_unique_collision_survive_install() {
    let f = Fixture::new(true);
    let action = f.bin.join(crate::helpers::exe("tune"));
    // A hardlinked action helper is unsafe; test the fixed .new sentinel collision
    // with an independent outside sentinel to avoid weakening action alias checks.
    let outside = f.root.join("outside");
    fs::write(&outside, b"outside sentinel").unwrap();
    fs::hard_link(&outside, f.bin.join(format!("{}.new", crate::helpers::exe("tune-agent")))).unwrap();
    collide_names(&f.bin, 1, 1);
    install_at(prepare_native_sampler(&f.source).unwrap(), inspect_at(&f.bin).unwrap()).unwrap();
    let collisions = take_collision_files();
    assert_eq!(collisions.len(), 2);
    assert_eq!(leftovers(&f).len(), 2);
    for path in collisions {
        assert_eq!(fs::read(path).unwrap(), b"collision sentinel");
    }
    assert_eq!(fs::read(outside).unwrap(), b"outside sentinel");
    assert_eq!(fs::read(action).unwrap(), b"action helper sentinel");
}
#[test]
fn fixed_new_hardlinked_action_is_never_written() {
    let f = Fixture::new(true);
    let action = f.bin.join(crate::helpers::exe("tune"));
    fs::hard_link(&action, f.bin.join(format!("{}.new", crate::helpers::exe("tune-agent")))).unwrap();
    let result = install_at(prepare_native_sampler(&f.source).unwrap(), inspect_at(&f.bin).unwrap());
    assert!(result.is_err());
    assert_eq!(fs::read(action).unwrap(), b"action helper sentinel");
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
}
#[test]
fn target_change_after_snapshot_and_during_stage_never_overwritten() {
    for during_stage in [false, true] {
        let f = Fixture::new(true);
        let p = prepare_native_sampler(&f.source).unwrap();
        let s = inspect_at(&f.bin).unwrap();
        let target = f.target();
        if during_stage {
            hook(Operation::StageVerify, move || fs::write(target, b"external replacement").unwrap());
        } else {
            fs::write(target, b"external replacement").unwrap();
        }
        let error = install_at(p, s).unwrap_err();
        assert_eq!(error.cause(), Cause::TargetChanged);
        assert_eq!(fs::read(f.target()).unwrap(), b"external replacement");
        assert!(leftovers(&f).is_empty());
    }
}
#[test]
fn stage_io_failures_preserve_old_bytes_and_remove_only_owned_stage() {
    for op in [
        Operation::CreateStage,
        Operation::Write,
        Operation::Flush,
        Operation::Permission,
        Operation::Sync,
        Operation::Close,
        Operation::StageVerify,
        Operation::Backup,
    ] {
        let f = Fixture::new(true);
        let p = prepare_native_sampler(&f.source).unwrap();
        let s = inspect_at(&f.bin).unwrap();
        // Close must fail at the stage close rather than the earlier target reads.
        if op == Operation::Close {
            hook(Operation::Sync, move || fail(&[Operation::Close]));
        } else {
            fail(&[op]);
        }
        let error = install_at(p, s).unwrap_err();
        assert_eq!(error.outcome(), InstallOutcome::Rejected, "{op:?}");
        assert_eq!(fs::read(f.target()).unwrap(), b"old sampler", "{op:?}");
        assert!(leftovers(&f).is_empty(), "{op:?}");
        f.sentinels();
    }
}
#[test]
fn placement_and_final_verification_failures_restore_old_or_missing() {
    for old in [false, true] {
        for op in [Operation::Place, Operation::FinalVerify] {
            let f = Fixture::new(old);
            let p = prepare_native_sampler(&f.source).unwrap();
            let s = inspect_at(&f.bin).unwrap();
            fail(&[op]);
            let error = install_at(p, s).unwrap_err();
            assert_eq!(error.outcome(), InstallOutcome::RolledBack, "{old}/{op:?}");
            if old {
                assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
            } else {
                assert!(!f.target().exists());
            }
            assert!(leftovers(&f).is_empty());
            f.sentinels();
        }
    }
}
#[test]
fn rollback_failure_retains_actual_old_backup_and_stage() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    fail(&[Operation::Place, Operation::Rollback]);
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.outcome(), InstallOutcome::RecoveryRequired);
    let remaining = leftovers(&f);
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().any(|p| fs::read(p).unwrap() == b"old sampler"));
    assert!(remaining.iter().any(|p| fs::read(p).unwrap() == f.bytes));
    for path in remaining {
        assert!(error.recovery_paths().contains(&path));
    }
}
#[test]
fn unknown_target_during_rollback_is_not_overwritten() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    let target = f.target();
    hook(Operation::FinalVerify, move || {
        fs::rename(&target, target.with_extension("unowned-moved")).unwrap();
        fs::write(target, b"unknown replacement").unwrap();
    });
    fail(&[Operation::FinalVerify]);
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.outcome(), InstallOutcome::RecoveryRequired);
    assert_eq!(fs::read(f.target()).unwrap(), b"unknown replacement");
    assert!(leftovers(&f).iter().any(|p| fs::read(p).unwrap() == b"old sampler"));
}
#[test]
fn cleanup_failure_reports_installed_with_backup_retained() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    fail(&[Operation::Cleanup]);
    let result = install_at(p, s).unwrap();
    assert!(result.changed());
    assert_eq!(fs::read(f.target()).unwrap(), f.bytes);
    assert_eq!(fs::read(result.retained_backup().unwrap()).unwrap(), b"old sampler");
    f.sentinels();
}
#[test]
fn source_target_and_source_action_identity_collisions_refused() {
    for name in ["tune-agent", "tune"] {
        let f = Fixture::new(true);
        let p = prepare_native_sampler(&f.source).unwrap();
        let dest = f.bin.join(crate::helpers::exe(name));
        fs::remove_file(&dest).unwrap();
        fs::rename(f.source.join(crate::helpers::exe("tune-agent")), &dest).unwrap();
        executable(&dest);
        let error = install_at(p, inspect_at(&f.bin).unwrap()).unwrap_err();
        assert_eq!(error.cause(), Cause::Alias);
        assert_eq!(fs::read(dest).unwrap(), f.bytes);
    }
}
#[cfg(target_os = "macos")]
#[test]
fn same_hash_unsafe_mode_and_symlinks_are_never_repaired() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new(true);
    fs::write(f.target(), &f.bytes).unwrap();
    fs::set_permissions(f.target(), fs::Permissions::from_mode(0o644)).unwrap();
    let error = install_at(prepare_native_sampler(&f.source).unwrap(), inspect_at(&f.bin).unwrap()).unwrap_err();
    assert_eq!(error.cause(), Cause::UnsafeFile);
    assert_eq!(fs::metadata(f.target()).unwrap().permissions().mode() & 0o777, 0o644);
    fs::remove_file(f.target()).unwrap();
    symlink(f.source.join(crate::helpers::exe("tune-agent")), f.target()).unwrap();
    assert!(inspect_at(&f.bin).is_err());
    let f = Fixture::new(true);
    let source = f.source.join(crate::helpers::exe("tune-agent"));
    fs::remove_file(&source).unwrap();
    symlink(f.target(), source).unwrap();
    assert!(prepare_native_sampler(&f.source).is_err());
}
#[cfg(target_os = "macos")]
#[test]
fn read_denied_is_not_missing_and_bin_symlink_is_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new(true);
    fs::set_permissions(f.target(), fs::Permissions::from_mode(0o0)).unwrap();
    let error = inspect_at(&f.bin).unwrap_err();
    assert_eq!(error.cause(), Cause::PermissionDenied);
    fs::set_permissions(f.target(), fs::Permissions::from_mode(0o755)).unwrap();
    let old = f.root.join("old-bin");
    fs::rename(&f.bin, &old).unwrap();
    symlink(old, &f.bin).unwrap();
    assert!(inspect_at(&f.bin).is_err());
}
#[test]
fn stage_cleanup_failure_exposes_only_its_owned_recovery_path() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    fail(&[Operation::Write, Operation::Cleanup]);
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.outcome(), InstallOutcome::RecoveryRequired);
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
    let remaining = leftovers(&f);
    assert_eq!(remaining.len(), 1);
    assert_eq!(error.recovery_paths(), remaining);
}
#[test]
fn bounded_stage_collisions_preserve_every_existing_entry() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    collide_names(&f.bin, 8, 0);
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.cause(), Cause::Collision);
    let collisions = take_collision_files();
    assert_eq!(collisions.len(), 8);
    assert_eq!(leftovers(&f).len(), 8);
    for path in collisions {
        assert_eq!(fs::read(path).unwrap(), b"collision sentinel");
    }
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
}
#[test]
fn final_hash_corruption_rolls_back_actual_files() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    let target = f.target();
    hook(Operation::FinalVerify, move || fs::write(target, b"damaged stage bytes").unwrap());
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.outcome(), InstallOutcome::RolledBack);
    assert_eq!(error.cause(), Cause::Integrity);
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
    assert!(leftovers(&f).is_empty());
}
#[test]
fn final_read_close_failure_rolls_back_instead_of_claiming_success() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    hook(Operation::FinalVerify, || fail(&[Operation::Close]));
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.outcome(), InstallOutcome::RolledBack);
    assert_eq!(error.operation(), Operation::Close);
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
    assert!(leftovers(&f).is_empty());
}
#[test]
fn source_display_and_raw_manifest_hash_are_exact() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    assert_eq!(p.source(), f.source);
    assert_eq!(p.manifest_sha256(), sha256_hex(&fs::read(f.source.join("manifest.json")).unwrap()));
    assert_eq!(p.sampler_sha256(), sha256_hex(&f.bytes));
    assert_eq!(p.sampler_bytes(), f.bytes.len() as u64);
    assert_eq!(p.run_id(), "123");
    assert_eq!(p.run_attempt(), "1");
    assert!(prepare_native_sampler(Path::new("relative")).is_err());
}
#[cfg(target_os = "macos")]
#[test]
fn source_read_denied_and_same_hash_hardlink_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new(true);
    let path = f.source.join(crate::helpers::exe("tune"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
    assert_eq!(prepare_native_sampler(&f.source).unwrap_err().cause(), Cause::PermissionDenied);
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(f.target(), &f.bytes).unwrap();
    fs::hard_link(f.target(), f.bin.join("target-alias")).unwrap();
    assert_eq!(inspect_at(&f.bin).unwrap_err().cause(), Cause::UnsafeFile);
}
#[cfg(windows)]
#[test]
fn windows_active_handle_refuses_replacement_without_changing_old_file() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    let active = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(f.target()).unwrap();
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.outcome(), InstallOutcome::Rejected);
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
    assert!(leftovers(&f).is_empty());
    drop(active);
}
#[cfg(windows)]
#[test]
fn windows_case_alias_is_observed_and_no_replace_is_native() {
    let f = Fixture::new(true);
    let original = inspect_at(&f.bin).unwrap();
    fs::rename(f.target(), f.bin.join("TUNE-AGENT.EXE")).unwrap();
    let observed = inspect_at(&f.bin).unwrap();
    assert_eq!(original.observed.unwrap().state.identity, observed.observed.unwrap().state.identity);
    let dir = Directory::open(&f.bin).unwrap();
    let old = os::named(dir.file(), OsStr::new("TUNE-AGENT.EXE")).unwrap();
    let error = os::rename(dir.file(), OsStr::new("TUNE-AGENT.EXE"), OsStr::new("tune.exe"), &old.identity).err().unwrap();
    assert!(!error.moved);
    assert_eq!(error.error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
    f.sentinels();
    // A negative-only check could pass on an invalid native request. Exercise
    // actual moves as well, including the shortest valid destination component.
    for (from, to) in [("TUNE-AGENT.EXE", "x"), ("x", "TUNE-AGENT.EXE")] {
        if let Err(failure) = os::rename(dir.file(), OsStr::new(from), OsStr::new(to), &old.identity) {
            panic!("native rename {from} -> {to}: {} (moved={})", failure.error, failure.moved);
        }
        assert_eq!(os::named(dir.file(), OsStr::new(to)).unwrap().identity, old.identity);
        assert_eq!(fs::read(f.bin.join(to)).unwrap(), b"old sampler");
        assert!(!f.bin.join(from).exists());
        f.sentinels();
    }
    dir.close().unwrap();
}
#[cfg(windows)]
#[test]
fn windows_reparse_leaf_and_directory_are_refused() {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let f = Fixture::new(true);
    let source = f.source.join(crate::helpers::exe("tune-agent"));
    fs::remove_file(&source).unwrap();
    if let Err(e) = symlink_file(f.target(), &source) {
        if e.raw_os_error() == Some(1314) {
            eprintln!("CAPABILITY SKIP: Windows symbolic-link privilege unavailable; reparse fixture not exercised");
            return;
        }
        panic!("symlink fixture failed: {e}");
    }
    assert!(prepare_native_sampler(&f.source).is_err());
    let old = f.root.join("old-bin");
    fs::rename(&f.bin, &old).unwrap();
    symlink_dir(old, &f.bin).unwrap();
    assert!(inspect_at(&f.bin).is_err());
}
#[test]
fn public_bound_api_roundtrip_in_subprocess() {
    // The real OnceLock is process lifetime. Only this isolated test executable
    // binds a synthetic directory; no reset or alternate public target is added.
    const CHILD: &str = "KATALA_NATIVE_INSTALL_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "helpers::native_install::tests::public_bound_api_roundtrip_in_subprocess", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let f = Fixture::new(true);
    assert_eq!(inspect_bound_target().unwrap_err().cause(), Cause::UnboundTarget);
    crate::helpers::bind_process_data_dir(f.bin.parent().unwrap()).unwrap();
    let snapshot = inspect_bound_target().unwrap();
    assert_eq!(snapshot.target(), f.target());
    let result = install_native_sampler(prepare_native_sampler(&f.source).unwrap(), snapshot).unwrap();
    assert!(result.changed());
    assert_eq!(fs::read(f.target()).unwrap(), f.bytes);
    f.sentinels();
}
#[test]
fn directory_close_failure_after_install_is_explicit_recovery() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    hook(Operation::Cleanup, || fail(&[Operation::Close]));
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.outcome(), InstallOutcome::RecoveryRequired);
    assert_eq!(error.operation(), Operation::Close);
    assert_eq!(error.recovery_paths(), [f.target()]);
    assert_eq!(fs::read(f.target()).unwrap(), f.bytes);
    assert!(leftovers(&f).is_empty());
}
#[cfg(windows)]
#[test]
fn windows_checked_close_after_native_rename_restores_old_bytes() {
    for op in [Operation::Backup, Operation::Place] {
        let f = Fixture::new(true);
        let p = prepare_native_sampler(&f.source).unwrap();
        let s = inspect_at(&f.bin).unwrap();
        hook(op, || fail(&[Operation::Close]));
        let error = install_at(p, s).unwrap_err();
        assert_eq!(error.outcome(), InstallOutcome::RolledBack, "{op:?}");
        assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
        assert!(leftovers(&f).is_empty());
    }
}
#[test]
fn stage_reopen_rejects_equal_bytes_at_another_identity() {
    let f = Fixture::new(true);
    let p = prepare_native_sampler(&f.source).unwrap();
    let s = inspect_at(&f.bin).unwrap();
    let bin = f.bin.clone();
    let replacement = f.bytes.clone();
    hook(Operation::StageVerify, move || {
        let path = fs::read_dir(&bin)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.file_name().unwrap().to_string_lossy().starts_with(".tune-agent-stage-"))
            .unwrap();
        fs::rename(&path, bin.join("moved-stage-sentinel")).unwrap();
        fs::write(&path, replacement).unwrap();
        executable(&path);
    });
    let error = install_at(p, s).unwrap_err();
    assert_eq!(error.outcome(), InstallOutcome::RecoveryRequired);
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
    assert_eq!(leftovers(&f).len(), 1);
    assert_eq!(fs::read(&leftovers(&f)[0]).unwrap(), f.bytes);
    assert_eq!(fs::read(f.bin.join("moved-stage-sentinel")).unwrap(), f.bytes);
}
#[cfg(windows)]
#[test]
fn windows_directory_opens_check_type_and_creation_remains_exclusive() {
    let f = Fixture::new(true);
    let dir = Directory::open(&f.bin).unwrap();
    let name = OsStr::new("fresh-directory");
    os::mkdir(dir.file(), name).unwrap();
    let before = os::named(dir.file(), name).unwrap();
    let held = os::open(dir.file(), name, filesystem::Kind::Directory).unwrap();
    let opened = os::state(&held).unwrap();
    assert!(opened.directory);
    assert_eq!(opened.identity, before.identity);
    checked_close(held).unwrap();
    assert!(os::mkdir(dir.file(), name).is_err());
    assert_eq!(os::named(dir.file(), name).unwrap().identity, before.identity);
    let sampler = crate::helpers::exe("tune-agent");
    assert!(os::mkdir(dir.file(), OsStr::new(&sampler)).is_err());
    assert!(os::open(dir.file(), OsStr::new(&sampler), filesystem::Kind::Directory).is_err());
    assert_eq!(fs::read(f.target()).unwrap(), b"old sampler");
    // The compatible existing-directory open must still deny parent deletion.
    let moved = f.root.join("moved-bin");
    assert!(fs::rename(&f.bin, &moved).is_err());
    dir.close().unwrap();
    fs::rename(&f.bin, &moved).unwrap();
    fs::rename(&moved, &f.bin).unwrap();
    f.sentinels();
}
