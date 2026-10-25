use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn launch(arguments: &[&str], base: &std::path::Path) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_tune"))
        .args(arguments)
        .env("KATALA_TUNE_CONFIG", base.join("config/nodes.json"))
        .env("KATALA_TUNE_DATA_DIR", base.join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn windows_probe_rejects_nonclosed_arguments_before_application_state() {
    let base = std::env::temp_dir().join(format!("kt-probe-cli-{}-{}", std::process::id(), tune_core::db::now_ms()));
    let mut child =
        launch(&["windows-probe-v1", "--helper-sha256", "0", "--benchmark", "2", "--network", "0", "--observe-wsl", "0", "--budget-ms", "120000"], &base);
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            let output = child.wait_with_output().unwrap();
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
            assert!(!base.exists());
            return;
        }
        assert!(Instant::now() < end, "windows probe parser exceeded finite exit window");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
#[test]
fn native_embedded_probe_collects_a_real_snapshot_without_application_state() {
    use std::io::Read;
    let base = std::env::temp_dir().join(format!("kt-probe-real-{}-{}", std::process::id(), tune_core::db::now_ms()));
    let hash = tune_core::db::sha256_hex(&std::fs::read(env!("CARGO_BIN_EXE_tune")).unwrap());
    let mut child =
        launch(&["windows-probe-v1", "--helper-sha256", &hash, "--benchmark", "0", "--network", "0", "--observe-wsl", "0", "--budget-ms", "120000"], &base);
    drop(child.stdin.take());
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    for (name, pipe) in [("out", Box::new(stdout) as Box<dyn Read + Send>), ("err", Box::new(stderr) as Box<dyn Read + Send>)] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let limit = if name == "out" { 1_048_576 } else { 65_536 };
            let mut bytes = Vec::new();
            let read = pipe.take(limit + 1).read_to_end(&mut bytes);
            let _ = tx.send((name, read.is_ok() && bytes.len() <= limit as usize, bytes));
        });
    }
    drop(tx);
    let end = Instant::now() + Duration::from_secs(125);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            panic!("real Windows probe exceeded its finite exit window");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut out = Vec::new();
    for _ in 0..2 {
        let (name, accepted, bytes) = rx.recv_timeout(Duration::from_secs(2)).expect("both owned pipes must reach EOF");
        assert!(accepted, "probe pipe exceeded its cap or failed");
        if name == "out" {
            out = bytes;
        }
    }
    assert!(status.success(), "real embedded probe must finish successfully: {status:?}");
    let text = String::from_utf8(out).unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(text.lines().last().expect("one snapshot line").trim_start_matches('\u{feff}')).unwrap();
    assert_eq!(snapshot["probe"], "windows");
    assert_eq!(snapshot["probe_version"], 2);
    assert!(!base.exists(), "the fixed probe must not initialize private application state");
}
