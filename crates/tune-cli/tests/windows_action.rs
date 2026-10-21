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

fn finite(child: &mut std::process::Child, duration: Duration) -> std::process::ExitStatus {
    let end = Instant::now() + duration;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            panic!("windows action receiver exceeded its finite exit window");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn sandbox() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "kt-action-cli-{}-{}-{}",
        std::process::id(),
        tune_core::db::now_ms(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

#[test]
fn receiver_rejects_invalid_helper_before_application_state() {
    let base = sandbox();
    let mut child = launch(&["windows-action-receiver-v2", "--helper-sha256", "0"], &base);
    let status = finite(&mut child, Duration::from_secs(3));
    assert!(!status.success());
    #[cfg(windows)]
    assert_eq!(status.code(), Some(9));
    #[cfg(not(windows))]
    assert_eq!(status.code(), Some(2));
    let output = child.wait_with_output().unwrap();
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!base.exists());
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    fn helper_hash() -> String {
        tune_core::db::sha256_hex(&std::fs::read(env!("CARGO_BIN_EXE_tune")).unwrap())
    }

    fn ready(reader: &mut BufReader<std::process::ChildStdout>) -> String {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let nonce = line.strip_prefix("KT2 READY ").and_then(|value| value.strip_suffix('\n')).unwrap();
        assert_eq!(nonce.len(), 64);
        assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
        nonce.to_owned()
    }

    fn request(nonce: &str, body: &[u8]) -> Vec<u8> {
        format!("KT2 REQUEST {nonce} {} {}\n", body.len(), tune_core::db::sha256_hex(body)).into_bytes().into_iter().chain(body.iter().copied()).collect()
    }

    #[test]
    fn malformed_nonce_after_ready_is_pre_admission_exit_nine() {
        let base = sandbox();
        let hash = helper_hash();
        let mut child = launch(&["windows-action-receiver-v2", "--helper-sha256", &hash], &base);
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let nonce = ready(&mut reader);
        child.stdin.as_mut().unwrap().write_all(&request(&format!("{nonce}0"), b"exit 3")).unwrap();
        child.stdin.take();
        let mut trailing = Vec::new();
        reader.read_to_end(&mut trailing).unwrap();
        assert_eq!(finite(&mut child, Duration::from_secs(3)).code(), Some(9));
        assert!(trailing.is_empty());
        assert!(!base.exists());
    }

    #[test]
    fn clean_nonzero_action_is_framed_and_receiver_exits_zero() {
        let base = sandbox();
        let hash = helper_hash();
        let mut child = launch(&["windows-action-receiver-v2", "--helper-sha256", &hash], &base);
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let nonce = ready(&mut reader);
        child.stdin.as_mut().unwrap().write_all(&request(&nonce, b"exit 3")).unwrap();
        child.stdin.take();
        let mut response = Vec::new();
        reader.read_to_end(&mut response).unwrap();
        assert_eq!(finite(&mut child, Duration::from_secs(10)).code(), Some(0));
        let header = format!("KT2 RESULT {nonce} {} 3 0 0\n", tune_core::db::sha256_hex(b"exit 3"));
        assert_eq!(response, header.as_bytes());
        assert!(!base.exists());
    }
}
