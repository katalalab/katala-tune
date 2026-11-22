use super::*;
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};
use tune_core::{
    engine::{Engine, NoHost},
    live::{Live, Opts, Route, Source},
};

struct Never;
impl Route for Never {
    fn open(&self, _: &tune_core::nodes::Node, _: &Opts) -> Result<Source, String> {
        panic!("source confirmation never collects")
    }
}
pub(crate) fn source(root: &Path) -> Vec<u8> {
    let bytes = if cfg!(windows) { pe() } else { macho() };
    let files: Vec<Value> = [("katala-tune-gpui", "gui"), ("tune", "diagnostic-cli"), ("tune-agent", "native-sampler")]
        .into_iter()
        .map(|(name, role)| {
            let file = tune_core::helpers::exe(name);
            fs::write(root.join(&file), &bytes).unwrap();
            json!({"file":file,"role":role,"bytes":bytes.len(),"sha256":tune_core::db::sha256_hex(&bytes)})
        })
        .collect();
    let (platform, architecture, target) = if cfg!(windows) { ("win32", "x64", "x86_64-pc-windows-msvc") } else { ("darwin", "arm64", "aarch64-apple-darwin") };
    let manifest = json!({"schema":"katala_gpui_preview.v2","version":"0.7.0-rc.1","revision":"a".repeat(40),"sourceTree":"b".repeat(40),"platform":platform,"architecture":architecture,"build":{"profile":"release","target":target},"ci":{"repository":"fixture/project","workflowRef":"fixture/project/.github/workflows/gpui-preview.yml@refs/heads/main","workflowSha":"c".repeat(40),"eventName":"workflow_dispatch","runId":"123","runAttempt":"1"},"files":files});
    fs::write(root.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    bytes
}
async fn pending(gate: &Gate) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while gate.pending().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn real_source_confirmation_revalidation_and_host_receipt() {
    // One process lifetime binding, never reset. All scenarios use this owned
    // namespace; no test-only target override can bypass the production API.
    let root = std::env::temp_dir().join(format!("sampler-ui-flow-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    fs::create_dir(root.join("source")).unwrap();
    fs::create_dir(root.join("data")).unwrap();
    fs::write(root.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    tune_core::helpers::bind_process_data_dir(&root.join("data")).unwrap();
    let target = root.join("data/bin").join(tune_core::helpers::exe("tune-agent"));
    for scenario in [
        "picker-cancel",
        "empty",
        "multiple",
        "gate-cancel",
        "gate-timeout",
        "gate-replaced",
        "gate-close",
        "manifest-changed",
        "sampler-changed",
        "target-replaced",
        "live-pending",
        "install",
        "same-hash",
        "journal-failed",
    ] {
        let bytes = source(&root.join("source"));
        if target.parent().unwrap().exists() {
            fs::remove_dir_all(target.parent().unwrap()).unwrap();
        }
        if matches!(scenario, "target-replaced" | "same-hash") {
            fs::create_dir(target.parent().unwrap()).unwrap();
            fs::write(&target, &bytes).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let before = fs::read(&target).ok();
        let engine = Engine::open(root.join("nodes.json"), root.join("data"), Arc::new(NoHost)).unwrap();
        let live = Live::new(Arc::new(Never), Opts::default(), Box::new(|_| {}));
        let dirty = Arc::new(AtomicBool::new(false));
        let host = Arc::new(MonitorHost::new(engine.clone(), live.clone(), tokio::runtime::Handle::current(), dirty.clone()));
        let gate = Gate::new(|| {}).with_timeout(if scenario == "gate-timeout" { Duration::from_millis(50) } else { Duration::from_secs(5) });
        let state = Arc::new(Mutex::new(UiState::default()));
        assert!(state.lock().unwrap().choose());
        assert!(!state.lock().unwrap().choose(), "a duplicate click cannot launch another picker");
        let control = super::super::LiveControl::new(live, true);
        if scenario == "live-pending" {
            assert!(control.request(&host, &gate, "fixture").is_some());
        }
        let paths = match scenario {
            "picker-cancel" => None,
            "empty" => Some(vec![]),
            "multiple" => Some(vec![root.join("source"), root.join("source")]),
            _ => Some(vec![root.join("source")]),
        };
        let run = tokio::spawn(run_selection(paths, host.clone(), gate.clone(), state.clone(), dirty));
        let mut replacement = None;
        if !matches!(scenario, "picker-cancel" | "empty" | "multiple") {
            pending(&gate).await;
            assert!(!target.exists() || before.is_some(), "preparation cannot create bin or target");
            assert_eq!(host.native_install_snapshot().ever_collection_admitted, scenario == "live-pending");
            match scenario {
                "gate-cancel" => gate.answer(false),
                "gate-close" => gate.close(),
                "gate-timeout" => {}
                "gate-replaced" => {
                    replacement = Some(tokio::spawn((gate.confirm())("other".into(), "other".into())));
                    tokio::task::yield_now().await;
                }
                "manifest-changed" => {
                    let path = root.join("source/manifest.json");
                    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    manifest["ci"]["runAttempt"] = json!("2");
                    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
                    gate.answer(true);
                }
                "sampler-changed" => {
                    let mut changed = bytes.clone();
                    changed.push(0);
                    let name = tune_core::helpers::exe("tune-agent");
                    fs::write(root.join("source").join(&name), &changed).unwrap();
                    let path = root.join("source/manifest.json");
                    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    let entry = manifest["files"].as_array_mut().unwrap().iter_mut().find(|entry| entry["role"] == "native-sampler").unwrap();
                    entry["bytes"] = json!(changed.len());
                    entry["sha256"] = json!(tune_core::db::sha256_hex(&changed));
                    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
                    gate.answer(true);
                }
                "target-replaced" => {
                    fs::rename(&target, target.with_extension("original")).unwrap();
                    fs::write(&target, &bytes).unwrap();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
                    }
                    gate.answer(true);
                }
                _ => gate.answer(true),
            }
        }
        let outcome = run.await.unwrap();
        if let Some(replacement) = replacement {
            gate.answer(false);
            assert!(!replacement.await.unwrap());
        }
        let accepted = matches!(scenario, "install" | "same-hash" | "journal-failed");
        assert_eq!(
            outcome.is_err(),
            matches!(scenario, "empty" | "multiple" | "manifest-changed" | "sampler-changed" | "target-replaced" | "live-pending"),
            "{scenario}: {outcome:?}"
        );
        assert!(state.lock().unwrap().phase == Phase::Idle);
        gate.close(); // A lost old window cannot cancel accepted placement.
        drop(state);
        if scenario == "journal-failed" {
            engine.start_monitor_journal();
            engine.with_db(|db| db.conn().execute_batch("DROP TABLE monitor_events")).unwrap();
        }
        let report = host.shutdown().await;
        assert_eq!(report.install_complete, accepted.then_some(true), "{scenario}");
        assert_eq!(report.confirmed(), scenario != "journal-failed", "{scenario}");
        let snapshot = host.native_install_snapshot();
        assert_eq!(snapshot.ever_collection_admitted, scenario == "live-pending");
        if accepted {
            let Some(NativeInstallResult::Completed(Ok(receipt))) = snapshot.result.as_deref() else {
                panic!("{scenario}: expected typed receipt, got {:?}", snapshot.result)
            };
            assert_eq!(receipt.changed(), scenario != "same-hash");
            assert_eq!(receipt.target(), target);
            assert_eq!(fs::read(&target).unwrap(), bytes);
            assert!(receipt_text(snapshot.result.as_deref().unwrap()).contains(&target.display().to_string()));
        } else {
            assert!(snapshot.result.is_none(), "{scenario}");
            assert_eq!(fs::read(&target).ok(), before, "{scenario}");
        }
        assert_eq!(host.shutdown().await, report);
        drop((snapshot, host, engine));
    }
    fs::remove_dir_all(root).unwrap();
}

// Synthetic structure fixtures; never executed.
fn pe() -> Vec<u8> {
    let mut b = vec![0; 1024];
    b[..2].copy_from_slice(b"MZ");
    b[60..64].copy_from_slice(&128u32.to_le_bytes());
    b[128..132].copy_from_slice(b"PE\0\0");
    b[132..134].copy_from_slice(&0x8664u16.to_le_bytes());
    b[134..136].copy_from_slice(&1u16.to_le_bytes());
    b[148..150].copy_from_slice(&240u16.to_le_bytes());
    b[150..152].copy_from_slice(&0x22u16.to_le_bytes());
    b[152..154].copy_from_slice(&0x20bu16.to_le_bytes());
    b[168..172].copy_from_slice(&4096u32.to_le_bytes()); // entry RVA
    b[184..188].copy_from_slice(&4096u32.to_le_bytes());
    b[188..192].copy_from_slice(&512u32.to_le_bytes());
    b[208..212].copy_from_slice(&8192u32.to_le_bytes());
    b[212..216].copy_from_slice(&512u32.to_le_bytes()); // headers
    b[220..222].copy_from_slice(&3u16.to_le_bytes()); // console
    b[260..264].copy_from_slice(&16u32.to_le_bytes()); // directories
    b[392..397].copy_from_slice(b".text");
    b[400..404].copy_from_slice(&512u32.to_le_bytes());
    b[404..408].copy_from_slice(&4096u32.to_le_bytes());
    b[408..412].copy_from_slice(&512u32.to_le_bytes());
    b[412..416].copy_from_slice(&512u32.to_le_bytes());
    b[428..432].copy_from_slice(&0x60000020u32.to_le_bytes());
    b[512] = 0xc3;
    b
}

fn macho() -> Vec<u8> {
    let mut b = vec![0; 132];
    b[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    b[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
    b[12..16].copy_from_slice(&2u32.to_le_bytes()); // MH_EXECUTE
    b[16..20].copy_from_slice(&2u32.to_le_bytes());
    b[20..24].copy_from_slice(&96u32.to_le_bytes());
    b[32..36].copy_from_slice(&0x19u32.to_le_bytes()); // LC_SEGMENT_64
    b[36..40].copy_from_slice(&72u32.to_le_bytes());
    b[40..46].copy_from_slice(b"__TEXT");
    b[56..64].copy_from_slice(&0x100000000u64.to_le_bytes());
    b[64..72].copy_from_slice(&4096u64.to_le_bytes());
    b[80..88].copy_from_slice(&132u64.to_le_bytes()); // file size
    b[88..92].copy_from_slice(&7u32.to_le_bytes());
    b[92..96].copy_from_slice(&5u32.to_le_bytes()); // read / execute
    b[104..108].copy_from_slice(&0x80000028u32.to_le_bytes()); // LC_MAIN
    b[108..112].copy_from_slice(&24u32.to_le_bytes());
    b[112..120].copy_from_slice(&128u64.to_le_bytes());
    b[128..132].copy_from_slice(&0xd65f03c0u32.to_le_bytes()); // ret
    b
}
