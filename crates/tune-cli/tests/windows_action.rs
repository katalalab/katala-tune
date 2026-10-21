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

    fn receiver(base: &std::path::Path) -> (std::process::Child, BufReader<std::process::ChildStdout>, String) {
        let hash = helper_hash();
        let mut child = launch(&["windows-action-receiver-v2", "--helper-sha256", &hash, "--budget-ms", "9000"], base);
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let nonce = ready(&mut reader);
        (child, reader, nonce)
    }

    fn complete(
        mut child: std::process::Child,
        mut reader: BufReader<std::process::ChildStdout>,
        body: &[u8],
        nonce: &str,
    ) -> (std::process::ExitStatus, Vec<u8>) {
        child.stdin.as_mut().unwrap().write_all(&request(nonce, body)).unwrap();
        child.stdin.take();
        let mut response = Vec::new();
        reader.read_to_end(&mut response).unwrap();
        (finite(&mut child, Duration::from_secs(10)), response)
    }

    fn result(nonce: &str, body: &[u8], code: i32, stdout: usize, stderr: usize) -> Vec<u8> {
        format!("KT2 RESULT {nonce} {} {code} {stdout} {stderr}\n", tune_core::db::sha256_hex(body)).into_bytes()
    }

    fn terminal_code(response: &[u8]) -> i32 {
        let end = response.iter().position(|byte| *byte == b'\n').unwrap();
        let fields: Vec<_> = std::str::from_utf8(&response[..end]).unwrap().split(' ').collect();
        assert_eq!(fields.len(), 7);
        fields[4].parse().unwrap()
    }

    #[test]
    fn malformed_nonce_after_ready_is_pre_admission_exit_nine() {
        let base = sandbox();
        let (mut child, mut reader, nonce) = receiver(&base);
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
        let (child, reader, nonce) = receiver(&base);
        let (status, response) = complete(child, reader, b"exit 3", &nonce);
        assert_eq!(status.code(), Some(0));
        assert_eq!(response, result(&nonce, b"exit 3", 3, 0, 0));
        assert!(!base.exists());
    }

    #[test]
    fn normal_and_bootstrap_exit_codes_are_terminal_not_receiver_codes() {
        for (body, action_code) in
            [(b"exit 0".as_slice(), 0), (b"exit 7".as_slice(), 7), (b"exit 9".as_slice(), 9), (b"throw 'fixture'".as_slice(), 7), (b"[".as_slice(), 9)]
        {
            let base = sandbox();
            let (child, reader, nonce) = receiver(&base);
            let (status, response) = complete(child, reader, body, &nonce);
            assert_eq!(status.code(), Some(0), "receiver must finish a complete terminal frame");
            assert_eq!(response, result(&nonce, body, action_code, 0, 0));
            assert!(!base.exists());
        }
    }

    #[test]
    fn write_error_is_an_action_exception_not_a_successful_null_exit_code() {
        let body = b"Write-Error 'fixture'";
        let base = sandbox();
        let (child, reader, nonce) = receiver(&base);
        let (status, response) = complete(child, reader, body, &nonce);
        assert_eq!(status.code(), Some(0));
        assert_eq!(terminal_code(&response), 7);
        assert!(response.starts_with(&format!("KT2 RESULT {nonce} {} 7 ", tune_core::db::sha256_hex(body)).into_bytes()));
        assert!(!base.exists());
    }

    #[test]
    fn output_limits_are_per_stream_during_drain() {
        let exact = b"$o=[Console]::OpenStandardOutput();$e=[Console]::OpenStandardError();$b=New-Object byte[] 65536;$o.Write($b,0,$b.Length);$e.Write($b,0,$b.Length);exit 0";
        let base = sandbox();
        let (child, reader, nonce) = receiver(&base);
        let (status, response) = complete(child, reader, exact, &nonce);
        assert_eq!(status.code(), Some(0));
        let header = result(&nonce, exact, 0, 65_536, 65_536);
        assert_eq!(&response[..header.len()], header);
        assert_eq!(response.len(), header.len() + 131_072);
        assert!(response[header.len()..].iter().all(|byte| *byte == 0));
        assert!(!base.exists());

        for oversized in [
            b"$o=[Console]::OpenStandardOutput();$b=New-Object byte[] 65537;$o.Write($b,0,$b.Length);exit 0".as_slice(),
            b"$e=[Console]::OpenStandardError();$b=New-Object byte[] 65537;$e.Write($b,0,$b.Length);exit 0".as_slice(),
        ] {
            let base = sandbox();
            let (child, reader, nonce) = receiver(&base);
            let (status, response) = complete(child, reader, oversized, &nonce);
            assert_eq!(status.code(), Some(7));
            assert!(response.is_empty());
            assert!(!base.exists());
        }
    }

    #[test]
    fn held_input_eof_and_response_reader_cannot_outlive_the_original_budget() {
        let base = sandbox();
        let (mut child, reader, nonce) = receiver(&base);
        child.stdin.as_mut().unwrap().write_all(&request(&nonce, b"exit 0")).unwrap();
        assert_eq!(finite(&mut child, Duration::from_secs(6)).code(), Some(9));
        drop(reader);
        assert!(!base.exists());

        let body = b"$o=[Console]::OpenStandardOutput();$e=[Console]::OpenStandardError();$b=New-Object byte[] 65536;$o.Write($b,0,$b.Length);$e.Write($b,0,$b.Length);exit 0";
        let base = sandbox();
        let (mut child, reader, nonce) = receiver(&base);
        child.stdin.as_mut().unwrap().write_all(&request(&nonce, body)).unwrap();
        child.stdin.take();
        assert_eq!(finite(&mut child, Duration::from_secs(11)).code(), Some(7));
        drop(reader);
        assert!(!base.exists());
    }

    #[test]
    fn disconnect_and_owned_grandchild_cannot_return_a_clean_terminal_result() {
        let base = sandbox();
        let (mut child, reader, nonce) = receiver(&base);
        child.stdin.as_mut().unwrap().write_all(&request(&nonce, b"Start-Sleep -Milliseconds 250;exit 0")).unwrap();
        child.stdin.take();
        drop(reader);
        assert_eq!(finite(&mut child, Duration::from_secs(10)).code(), Some(7));
        assert!(!base.exists());

        let body = b"$p=New-Object System.Diagnostics.Process;$p.StartInfo.FileName=$env:ComSpec;$p.StartInfo.Arguments='/c ping -n 30 127.0.0.1 >NUL';$p.StartInfo.UseShellExecute=$false;[void]$p.Start();exit 0";
        let base = sandbox();
        let (child, reader, nonce) = receiver(&base);
        let (status, response) = complete(child, reader, body, &nonce);
        assert_eq!(status.code(), Some(7));
        assert!(response.is_empty());
        assert!(!base.exists());
    }
}
