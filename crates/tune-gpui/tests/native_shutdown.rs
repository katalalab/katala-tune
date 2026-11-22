//! Real native GPUI termination acceptance. Every collection byte is synthetic:
//! a PATH-only fake SSH consumes (never executes) the probe and waits on a parent gate.
#![cfg(target_os = "macos")]
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tune_core::db::Store;

struct Fixture {
    dir: PathBuf,
    app: Option<Child>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::write(self.dir.join("data/shutdown-check/release"), []);
        if let Some(app) = &mut self.app {
            if app.try_wait().ok().flatten().is_none() {
                let _ = app.kill();
            }
            let _ = app.wait();
        }
        // On a RED native exit, the fake SSH notices its parent disappearing.
        // Keep its fixture files until that exact known child has exited.
        let ready = self.dir.join("data/shutdown-check/child-ready");
        if let Ok(bytes) = std::fs::read(ready)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(pid) = value["pid"].as_u64()
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Command::new("/bin/kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success()) {
                if Instant::now() >= deadline {
                    eprintln!("known fixture child {pid} has not disappeared; retain {}", self.dir.display());
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn await_marker(app: &mut Child, path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        if let Some(status) = app.try_wait().unwrap() {
            let stderr = path.parent().unwrap().parent().unwrap().parent().unwrap().join("stderr");
            panic!("native app exited {status} before {}: {}", path.display(), std::fs::read_to_string(stderr).unwrap_or_default());
        }
        assert!(Instant::now() < deadline, "native fixture did not reach {}", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn snapshot_count(data: &Path) -> usize {
    Store::open_read_only(data).unwrap().last_snapshots("example-shutdown", 10).unwrap().len()
}
fn run_case(mode: &str, fail_journal: bool) {
    let mut fixture =
        Fixture { dir: std::env::temp_dir().join(format!("gpui-native-shutdown-{}-{}-{mode}", std::process::id(), tune_core::db::now_ms())), app: None };
    std::fs::create_dir(&fixture.dir).unwrap();
    let data = fixture.dir.join("data");
    let gate = data.join("shutdown-check");
    let bin = fixture.dir.join("bin");
    std::fs::create_dir_all(&gate).unwrap();
    std::fs::create_dir(&bin).unwrap();
    let config = fixture.dir.join("nodes.json");
    std::fs::write(
        &config,
        json!({"nodes":[{"id":"example-shutdown","alias":"example.invalid","os":"macos","benchmark":false,"network":false,"connectivity":false}]}).to_string(),
    )
    .unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(&ssh, r#"#!/usr/bin/python3
import json, os, pathlib, sys, time
root = pathlib.Path(os.environ['TUNE_SHUTDOWN_GATE'])
parent = os.getppid()
size = len(sys.stdin.buffer.read())
ready = root / 'child-ready.tmp'
ready.write_text(json.dumps({'pid':os.getpid(),'parent':parent,'stdin_bytes':size}))
ready.replace(root / 'child-ready')
end = time.monotonic() + 30
while not (root / 'release').exists():
    if time.monotonic() >= end:
        (root / 'child-timeout').touch()
        sys.exit(3)
    if os.getppid() != parent:
        (root / 'parent-gone').touch()
        sys.exit(4)
    time.sleep(0.02)
print(json.dumps({'probe':'mac','host':{'cpu':'Synthetic CPU','cores':4},'cpu_busy':2,'memory':{'total_gb':16,'available_pct':75},'disk':[],'processes':{'top_cpu':[]}}), flush=True)
(root / 'child-complete').touch()
"#).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let stdout = std::fs::File::create(fixture.dir.join("stdout")).unwrap();
    let stderr = std::fs::File::create(fixture.dir.join("stderr")).unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap()))).unwrap();
    fixture.app = Some(
        Command::new(env!("CARGO_BIN_EXE_katala-tune-gpui"))
            .args(["--selftest", "--shutdown-check", mode, "--config"])
            .arg(&config)
            .arg("--data")
            .arg(&data)
            .env("PATH", path)
            .env("TUNE_SHUTDOWN_GATE", &gate)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .unwrap(),
    );
    let app = fixture.app.as_mut().unwrap();
    await_marker(app, &gate.join("child-ready"));
    let child: serde_json::Value = serde_json::from_slice(&std::fs::read(gate.join("child-ready")).unwrap()).unwrap();
    assert_eq!(child["parent"], app.id());
    assert!(child["stdin_bytes"].as_u64().unwrap() > 0);
    await_marker(app, &gate.join("closing"));
    if fail_journal {
        Store::open(&data).unwrap().conn().execute_batch("DROP TABLE monitor_events").unwrap();
    }
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(450));
    let early_exit = app.try_wait().unwrap();
    let before = snapshot_count(&data);
    std::fs::write(gate.join("release"), []).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = app.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "native app did not finish after releasing accepted work");
        std::thread::sleep(Duration::from_millis(25));
    };
    let after = snapshot_count(&data);
    let complete = gate.join("child-complete").exists();
    let output = std::fs::read_to_string(fixture.dir.join("stdout")).unwrap();
    let error = std::fs::read_to_string(fixture.dir.join("stderr")).unwrap();
    eprintln!(
        "mode={mode} fail_journal={fail_journal} app_pid={} child_pid={} early_exit={early_exit:?} before={before} after={after} child_complete={complete} elapsed_ms={} status={status} stdout={output:?} stderr={error:?}",
        app.id(),
        child["pid"],
        start.elapsed().as_millis()
    );
    assert!(early_exit.is_none(), "native termination preceded accepted manual work at {mode}");
    assert_eq!(before, 0);
    assert_eq!(status.success(), !fail_journal);
    if fail_journal {
        assert_eq!(status.code(), Some(1));
        assert!(error.contains("journal_ack: Some(false)"), "missing native failure diagnostic: {error}");
    } else {
        assert!(output.contains("manual_complete: Some(true)"));
        assert!(output.contains("journal_ack: Some(true)"));
    }
    assert!(complete);
    assert_eq!(after, 1, "native termination must follow the real snapshot commit");
}
#[test]
#[ignore = "requires a native macOS GPU session; owned synthetic child only"]
fn last_window_close_drains_before_native_termination() {
    run_case("close", false);
}
#[test]
#[ignore = "requires a native macOS GPU session; owned synthetic child only"]
fn quit_action_drains_before_native_termination() {
    run_case("quit", false);
}
#[test]
#[ignore = "requires a native macOS GPU session; owned synthetic child only"]
fn native_quit_drains_before_native_termination() {
    run_case("native-quit", false);
}

#[test]
#[ignore = "requires a native macOS GPU session; owned synthetic child only"]
fn native_quit_reports_missing_journal_ack_as_failure() {
    run_case("native-quit", true);
}
