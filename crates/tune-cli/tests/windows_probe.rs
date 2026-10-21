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
