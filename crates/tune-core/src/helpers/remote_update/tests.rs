#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
use super::{PreparedNativeSampler, RemoteArchitecture};
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
use serde_json::json;
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
use std::path::Path;

/// Inert structural bytes and self-declared raw-v2 metadata. Never native success.
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
pub(crate) fn prepared(root: &Path, version: &str) -> PreparedNativeSampler {
    let source = root.join("synthetic-source");
    std::fs::create_dir_all(&source).unwrap();
    let bytes = if cfg!(windows) { super::super::binary::fixtures::pe() } else { super::super::binary::fixtures::macho() };
    let files: Vec<_> = [("katala-tune-gpui", "gui"), ("tune", "diagnostic-cli"), ("tune-agent", "native-sampler")]
        .into_iter()
        .map(|(name, role)| {
            let file = crate::helpers::exe(name);
            std::fs::write(source.join(&file), &bytes).unwrap();
            json!({"file":file,"role":role,"bytes":bytes.len(),"sha256":crate::db::sha256_hex(&bytes)})
        })
        .collect();
    let (platform, architecture, target) = if cfg!(windows) { ("win32", "x64", "x86_64-pc-windows-msvc") } else { ("darwin", "arm64", "aarch64-apple-darwin") };
    let manifest = json!({"schema":"katala_gpui_preview.v2","version":version,"revision":"a".repeat(40),"sourceTree":"b".repeat(40),"platform":platform,"architecture":architecture,"build":{"profile":"release","target":target},"ci":{"repository":"fixture/project","workflowRef":"fixture/project/.github/workflows/gpui-preview.yml@refs/heads/main","workflowSha":"c".repeat(40),"eventName":"workflow_dispatch","runId":"123","runAttempt":"1"},"files":files});
    std::fs::write(source.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    super::super::native_install::prepare_native_sampler(&source).unwrap()
}
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
pub(crate) fn architecture() -> Option<RemoteArchitecture> {
    Some(if cfg!(windows) { RemoteArchitecture::X64 } else { RemoteArchitecture::Arm64 })
}
#[test]
fn cpu_declaration_rejects_other_supported_cpus() {
    let mut pe = super::super::binary::fixtures::pe();
    assert!(super::super::binary::declared_target(&pe, "x86_64-pc-windows-msvc"));
    pe[132..134].copy_from_slice(&0xaa64u16.to_le_bytes());
    assert!(!super::super::binary::declared_target(&pe, "x86_64-pc-windows-msvc"));
    let mut mac = super::super::binary::fixtures::macho();
    assert!(super::super::binary::declared_target(&mac, "aarch64-apple-darwin"));
    mac[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
    assert!(!super::super::binary::declared_target(&mac, "aarch64-apple-darwin"));
    for wide in [false, true] {
        assert!(super::super::binary::declared_target(&super::super::binary::fixtures::fat(wide), "aarch64-apple-darwin"));
    }
    assert!(!super::super::binary::declared_target(&mac, "unknown"));
}

#[cfg(not(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64"))))]
#[test]
fn unsupported_host_refuses_native_source_before_source_io() {
    use crate::helpers::native_install::{Cause, Operation, prepare_native_sampler};
    let error = prepare_native_sampler(std::path::Path::new("synthetic-unopened-source")).unwrap_err();
    assert_eq!(error.operation(), Operation::Platform);
    assert_eq!(error.cause(), Cause::UnsupportedPlatform);
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[test]
fn watch_observation_retires_stale_pending_and_preserves_current_or_claimed_work() {
    use super::*;
    struct NeverTransport;
    impl RemoteUpdateTransport for NeverTransport {
        fn place<'a>(&'a self, _: &'a Node, _: RemoteUpdatePayload<'a>) -> RemoteUpdateFuture<'a> {
            panic!("watch observation must not start transport")
        }
    }
    let root = std::env::temp_dir().join(format!("remote-watch-observation-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let control = RemoteUpdateControl::new(Arc::new(NeverTransport));
    let (tx, mut rx) = watch::channel(RunRequest { mode: RunMode::Run, pull_seq: 0, start_seq: 1 });
    tx.send_modify(|request| request.start_seq = 2);
    let node = Node::from_value(&json!({"id":"synthetic-target","os":if cfg!(windows){"windows"}else{"macos"}}), "synthetic-host");
    let target = RemoteTargetSnapshot { node, architecture: architecture().unwrap() };
    let update = RemoteUpdateRequest::new(2, 2, Instant::now() + Duration::from_secs(120), target, prepared(&root, crate::helpers::VERSION)).unwrap();
    // This test isolates the IO-free watch/slot transition. Production checked
    // input admission and the actual worker are covered by controlled tests.
    control.admit_checked(&tx, CheckedRemoteUpdate(update)).unwrap();
    assert_eq!(control.observe_request(&mut rx).start_seq, 2);
    assert!(control.busy(), "an older observation must preserve the latest valid Pending");
    assert_eq!(control.outcome(), None);
    tx.send_modify(|request| request.start_seq = 3);
    tx.send_modify(|request| request.mode = RunMode::Paused);
    assert_eq!(control.observe_request(&mut rx).mode, RunMode::Paused);
    assert_eq!(control.outcome(), Some(RemoteUpdateOutcome::Refused { attempt_id: 2, reason: Refusal::Session }));
    assert!(!control.busy());
    control.state.lock().unwrap().slot = Slot::Claimed(4);
    tx.send_modify(|request| request.mode = RunMode::Exit);
    control.observe_request(&mut rx);
    assert!(matches!(control.state.lock().unwrap().slot, Slot::Claimed(4)), "stop observation cannot cancel Claimed work");
    std::fs::remove_dir_all(root).unwrap();
}
