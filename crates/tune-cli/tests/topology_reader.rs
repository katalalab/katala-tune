#[cfg(windows)]
use std::io::Write;
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn spawn(args: &[&str], base: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_tune"))
        .args(args)
        .env("KATALA_TUNE_CONFIG", base.join("config/nodes.json"))
        .env("KATALA_TUNE_DATA_DIR", base.join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}
fn wait_finite(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("reader exceeded its finite exit window");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn base() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "kt-reader-cli-{}-{}-{}",
        std::process::id(),
        tune_core::db::now_ms(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}
#[test]
fn dedicated_entry_rejects_silently_before_any_config_or_database_access() {
    let base = base();
    for args in [vec!["topology-reader"], vec!["topology-reader", "--budget-ms", "invalid"], vec!["topology-reader", "--budget-ms", "3600001"]] {
        let mut child = spawn(&args, &base);
        assert!(!wait_finite(&mut child, Duration::from_secs(3)).success());
        let output = child.wait_with_output().unwrap();
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    assert!(!base.exists());
}
#[cfg(windows)]
#[test]
fn unclosed_stdin_and_partial_frames_exit_without_eof_or_private_state() {
    let base = base();
    for input in [b"".as_slice(), b"KTRDR001".as_slice(), b"KTRDR001\x20\0\0\0\0\0\0\0{".as_slice()] {
        let mut child = spawn(&["topology-reader", "--budget-ms", "200"], &base);
        // A deliberately short negative-case deadline may expire before the
        // parent is scheduled to write. Only that closed-pipe outcome is valid.
        if let Err(error) = child.stdin.as_mut().unwrap().write_all(input) {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        } // Remains open while observing actual process exit.
        assert!(!wait_finite(&mut child, Duration::from_secs(3)).success());
        let output = child.wait_with_output().unwrap();
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    assert!(!base.exists());
}

#[cfg(windows)]
mod windows {
    use super::*;
    use serde_json::{Value, json};
    use std::io::Read;
    use tune_core::topology::{
        TopologyLimits,
        adapters::{
            remote::{WINDOWS_PROBE, identity_hash},
            windows_reader::{self as reader, ReaderRequest},
        },
    };

    #[allow(unsafe_code)]
    fn retained_identity(child: &Child) -> tune_core::collect::ProcessIdentity {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::{RemoteDesktop::ProcessIdToSessionId, Threading::GetProcessTimes},
        };
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let mut session = 0;
        // SAFETY: the test retains this freshly spawned child handle; writable
        // FILETIME/session outputs live for the synchronous read-only calls.
        unsafe {
            assert_ne!(GetProcessTimes(child.as_raw_handle(), &mut created, &mut exited, &mut kernel, &mut user), 0);
            assert_ne!(ProcessIdToSessionId(child.id(), &mut session), 0);
        }
        tune_core::collect::ProcessIdentity {
            pid: child.id(),
            created_filetime: (((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64).to_string(),
            os_session: session,
        }
    }
    fn git(program: &str, root: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new(program).current_dir(root).args(args).env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "NUL").output().unwrap();
        assert!(out.status.success(), "fixture git failed");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    fn fixture(large: bool) -> (std::path::PathBuf, ReaderRequest, Vec<u8>) {
        let root = base();
        std::fs::create_dir_all(&root).unwrap();
        let content = serde_json::to_vec(&json!({"name":"fixture-package","description":if large {"x".repeat(256*1024)} else {"fixture".into()}})).unwrap();
        std::fs::write(root.join("package.json"), &content).unwrap();
        let program =
            std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|p| p.join("git.exe")).find(|p| p.is_file()).unwrap().canonicalize().unwrap();
        let program = program.to_string_lossy().trim_start_matches("\\\\?\\").replace('\\', "/");
        git(&program, &root, &["init", "-q"]);
        git(&program, &root, &["add", "package.json"]);
        git(&program, &root, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.org", "commit", "-qm", "fixture"]);
        let head = git(&program, &root, &["rev-parse", "HEAD"]);
        let request:ReaderRequest=serde_json::from_value(json!({
            "schema":reader::REQUEST_SCHEMA,"nonce":"0123456789abcdef0123456789abcdef","scope_id":"fixture:environment","observed_at_ms":1000,
            "expected_identity_hash":identity_hash(&tune_core::nodes::local_host()),"expected_runtime":"native","expected_distro":null,
            "roots":[root.to_string_lossy().replace('\\',"/")],"configs":[],"cursor":null,
            "source_allowlist":[{"root_index":0,"relative_path":".","expected_head":head,"manifests":[{"relative_path":"package.json","bytes":content.len(),"sha256":tune_core::db::sha256_hex(&content)}]}],
            "git_program":program,"limits":TopologyLimits::default(),"budget_ms":30000,"probe_sha256":tune_core::db::sha256_hex(WINDOWS_PROBE.as_bytes())
        })).unwrap();
        let frame = reader::encode_request(&request).unwrap();
        (root, request, frame)
    }
    #[test]
    fn embedded_probe_returns_real_guard_receipt_with_stdin_still_open() {
        let (root, request, frame) = fixture(false);
        let state = base();
        let mut child = spawn(&["topology-reader", "--budget-ms", "30000"], &state);
        let identity = retained_identity(&child);
        child.stdin.as_mut().unwrap().write_all(&frame).unwrap();
        child.stdin.as_mut().unwrap().write_all(b"deliberately ignored after the one request frame").unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut v = Vec::new();
            stdout.read_to_end(&mut v).unwrap();
            v
        });
        assert!(wait_finite(&mut child, Duration::from_secs(35)).success());
        let bytes = reader.join().unwrap();
        let header_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let header: Value = serde_json::from_slice(&bytes[16..16 + header_len]).unwrap();
        let reported: tune_core::collect::ProcessIdentity = serde_json::from_value(header["helper_identity"].clone()).unwrap();
        assert_eq!(reported, identity, "retained handle must match PID, full FILETIME and OS session");
        let capture = reader::decode_response(&bytes, &request, &frame[16..]).unwrap();
        let capture: Value = serde_json::from_slice(capture).unwrap();
        assert_eq!(capture["repositories"][0]["manifests"][0]["relative_path"], "package.json");
        assert!(capture["failures"].as_array().unwrap().is_empty());
        assert!(!state.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unread_large_response_reaches_hard_exit_after_real_probe_cleanup() {
        let (root, _request, frame) = fixture(true);
        let state = base();
        let mut child = spawn(&["topology-reader", "--budget-ms", "30000"], &state);
        child.stdin.as_mut().unwrap().write_all(&frame).unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let consumer = std::thread::spawn(move || {
            let mut prefix = [0u8; 16];
            stdout.read_exact(&mut prefix).unwrap();
            let n = u32::from_le_bytes(prefix[8..12].try_into().unwrap()) as usize;
            assert!(n <= reader::MAX_RESPONSE_HEADER_BYTES);
            let payload = u32::from_le_bytes(prefix[12..16].try_into().unwrap());
            assert!(payload > 64 * 1024);
            let mut header = vec![0; n];
            stdout.read_exact(&mut header).unwrap();
            let header: Value = serde_json::from_slice(&header).unwrap();
            assert_eq!(header["process_receipt"]["job_empty"], true);
            assert_eq!(header["process_receipt"]["root_exit_code"], 0);
            tx.send(stdout).unwrap(); // Hold the pipe open without consuming any capture bytes.
        });
        let unread = match rx.recv_timeout(Duration::from_secs(32)) {
            Ok(pipe) => pipe,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("probe did not reach response stage");
            }
        };
        assert!(!wait_finite(&mut child, Duration::from_secs(35)).success());
        drop(unread);
        consumer.join().unwrap();
        assert!(!state.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
