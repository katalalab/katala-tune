#![cfg(windows)]
//! Dedicated single-spawner process: every native invocation and std/Tokio control holds SERIAL.
//! Real Windows children exercise the product backend. No fleet/SSH or shared process is used.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tune_core::collect::{BoundedResult, ProcessStop, run_standalone_windows_bounded};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const FIXTURE: &str = "KATALA_GUARD_TEST_MODE";

#[test]
#[allow(clippy::zombie_processes)] // Deliberate lingering child: the owned Job must terminate it.
fn owned_fixture() {
    let Ok(mode) = std::env::var(FIXTURE) else {
        return;
    };
    match mode.as_str() {
        "happy" => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            std::io::stdout().write_all(&input).unwrap();
            std::io::stderr().write_all(b"stderr-ok").unwrap();
        }
        "sleep" => {
            println!("{}", std::process::id());
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        "stdout" | "stderr" => {
            let block = [b'x'; 8192];
            for _ in 0..512 {
                let result = if mode == "stdout" { std::io::stdout().write_all(&block) } else { std::io::stderr().write_all(&block) };
                if result.is_err() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_secs(30));
        }
        "linger" => {
            let child = fixture_command("sleep").spawn().unwrap();
            println!("{}", child.id());
            std::io::stdout().flush().unwrap();
            // It is intentionally not waited: only the product-owned Job may remove this descendant.
        }
        "cancel" => {
            let child = fixture_command("sleep").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
            let path = std::env::var_os("KATALA_GUARD_READY").unwrap();
            let mut ready = std::fs::OpenOptions::new().create_new(true).write(true).open(path).unwrap();
            writeln!(ready, "{} {}", std::process::id(), child.id()).unwrap();
            ready.sync_all().unwrap();
            drop(ready);
            std::thread::sleep(Duration::from_secs(30));
        }
        "close-exit" => close_fixture_outputs(),
        "early-eof" => {
            close_fixture_outputs();
            std::thread::sleep(Duration::from_secs(30));
        }
        "args" => {
            for (name, value) in std::env::vars() {
                if name == "KATALA_GUARD_ROUNDTRIP" {
                    println!("env:{value}");
                }
            }
            let args = std::env::args().skip_while(|arg| arg != "--").skip(1).collect::<Vec<_>>();
            for arg in args {
                println!("arg:{}:{arg}", arg.len());
            }
        }
        _ => panic!("unknown fixture mode"),
    }
    // Avoid test harness stdout/stderr trailers and release all inherited handles immediately.
    std::process::exit(0);
}

fn fixture_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "owned_fixture", "--nocapture"]).env(FIXTURE, mode);
    command
}

fn deadline(timeout: Duration) -> Instant {
    Instant::now().checked_add(timeout).expect("finite test deadline")
}

async fn fixture(mode: &str, input: &[u8], timeout: Duration, limit: usize) -> BoundedResult {
    let end = deadline(timeout);
    run_standalone_windows_bounded(&fixture_command(mode), input, end, limit, limit).await
}

fn assert_ack(result: &BoundedResult, stop: ProcessStop) {
    let receipt = result.process_receipt.as_ref().expect("Windows receipt");
    assert_eq!(receipt.stop, stop, "{receipt:?}");
    assert!(receipt.cleanup_confirmed(), "{receipt:?}");
    assert_eq!(receipt.owner.as_ref().unwrap().pid, std::process::id());
    assert!(receipt.root_exit_code.is_some());
    let root = receipt.root.as_ref().unwrap();
    assert!(root.pid > 0);
    let filetime: u64 = root.created_filetime.parse().unwrap();
    assert!(filetime > (1u64 << 53), "full FILETIME remains decimal text");
    assert_eq!(receipt.owner_session.len(), 32);
    assert!(receipt.total_processes >= receipt.observed.len() as u32);
}

#[tokio::test]
async fn happy_real_stdin_eof_and_exit_ack() {
    let _serial = SERIAL.lock().await;
    let input: Vec<u8> = (0..128 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
    let result = fixture("happy", &input, Duration::from_secs(10), 256 * 1024).await;
    assert_eq!(result.code, Some(0));
    assert!(result.stdout.ends_with(&input));
    assert_eq!(result.stderr, b"stderr-ok");
    assert_ack(&result, ProcessStop::Completed);
}

#[tokio::test]
async fn timeout_reaps_owned_process_and_pipes() {
    let _serial = SERIAL.lock().await;
    let start = Instant::now();
    let result = fixture("sleep", b"", Duration::from_secs(2), 65536).await;
    assert!(result.timed_out);
    assert_eq!(result.code, None);
    assert!(result.stdout.is_empty());
    assert_ack(&result, ProcessStop::Timeout);
    assert!(start.elapsed() < Duration::from_secs(8));
}

#[tokio::test]
async fn stdout_and_stderr_limits_reap_and_withhold_payload() {
    let _serial = SERIAL.lock().await;
    for mode in ["stdout", "stderr"] {
        let result = fixture(mode, b"", Duration::from_secs(10), 1024).await;
        assert!(result.output_limited);
        assert_eq!(result.code, None);
        assert!(result.stdout.is_empty() && result.stderr.is_empty());
        assert_ack(&result, ProcessStop::OutputLimit);
    }
}

#[tokio::test]
async fn unread_stdin_cannot_keep_writer_alive_after_timeout() {
    let _serial = SERIAL.lock().await;
    let result = fixture("sleep", &vec![b'x'; 4 * 1024 * 1024], Duration::from_secs(2), 65536).await;
    assert!(result.timed_out);
    assert_ack(&result, ProcessStop::Timeout);
}

#[tokio::test]
async fn root_exit_with_lingering_descendant_is_a_failure_with_ack() {
    let _serial = SERIAL.lock().await;
    let result = fixture("linger", b"", Duration::from_secs(10), 65536).await;
    assert_eq!(result.code, None);
    assert!(result.stdout.is_empty());
    assert_ack(&result, ProcessStop::LingeringDescendants);
    assert!(result.process_receipt.as_ref().unwrap().total_processes >= 2);
}

#[tokio::test]
async fn missing_executable_is_never_success() {
    let _serial = SERIAL.lock().await;
    let result = run_standalone_windows_bounded(&Command::new("katala-guard-missing-executable"), b"", deadline(Duration::from_secs(1)), 1024, 1024).await;
    assert!(result.spawn_failed);
    assert_eq!(result.code, None);
    assert!(!result.process_receipt.unwrap().cleanup_confirmed());
}

#[tokio::test]
async fn own_job_cleanup_preserves_unrelated_child() {
    let _serial = SERIAL.lock().await;
    let mut unrelated = fixture_command("sleep").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let result = fixture("sleep", b"", Duration::from_secs(2), 65536).await;
    let alive = unrelated.try_wait().unwrap().is_none();
    // This handle is owned by the test, not selected by a PID or by the product.
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    assert!(alive);
    assert_ack(&result, ProcessStop::Timeout);
}

#[tokio::test]
async fn argv_and_case_insensitive_environment_roundtrip() {
    let _serial = SERIAL.lock().await;
    let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    let expected = ["", "space value", "quote\"value", r"trailing\", r#"slash\\\"quote"#, "日本語"];
    let mut args = vec!["--exact".into(), "owned_fixture".into(), "--nocapture".into(), "--".into()];
    args.extend(expected.iter().map(|arg| (*arg).to_owned()));
    let mut command = Command::new(exe);
    command.args(args).env(FIXTURE, "args").env("KATALA_GUARD_ROUNDTRIP", "removed").env_remove("katala_guard_roundtrip");
    let result = run_standalone_windows_bounded(&command, b"", deadline(Duration::from_secs(10)), 65536, 65536).await;
    assert_ack(&result, ProcessStop::Completed);
    assert_eq!(result.code, Some(0));
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(!output.contains("env:"));
    assert_eq!(output.matches("running 1 test").count(), 1, "only the selected fixture must run");
    for arg in expected {
        assert!(output.contains(&format!("arg:{}:{arg}", arg.len())), "argv roundtrip missing");
    }
}

#[allow(unsafe_code)]
fn retain_test_child(pid: u32) -> std::os::windows::io::OwnedHandle {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    // SAFETY: open an observer handle only. PID came from our create-new readiness file; no PID termination occurs.
    let handle = unsafe { windows_sys::Win32::System::Threading::OpenProcess(0x0010_1000, 0, pid) };
    assert!(!handle.is_null());
    // SAFETY: successful OpenProcess transferred one owned handle.
    unsafe { OwnedHandle::from_raw_handle(handle) }
}

#[allow(unsafe_code)]
fn signaled(handle: &std::os::windows::io::OwnedHandle) -> bool {
    use std::os::windows::io::AsRawHandle;
    // SAFETY: retained observer handle, zero timeout, never a PID kill or unbounded wait.
    unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(handle.as_raw_handle(), 0) == 0 }
}

#[tokio::test]
async fn cancelling_future_closes_only_its_owned_job() {
    let _serial = SERIAL.lock().await;
    let name = format!("katala-guard-ready-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    let path = std::env::temp_dir().join(name);
    let ready_path = path.to_string_lossy().into_owned();
    let task = tokio::spawn(async move {
        let mut command = fixture_command("cancel");
        command.env("KATALA_GUARD_READY", &ready_path);
        run_standalone_windows_bounded(&command, b"", deadline(Duration::from_secs(20)), 65536, 65536).await
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let pids = loop {
        if let Ok(value) = std::fs::read_to_string(&path) {
            let pids: Result<Vec<u32>, _> = value.split_whitespace().map(str::parse).collect();
            if let Ok(pids) = pids
                && pids.len() == 2
            {
                break pids;
            }
        }
        if Instant::now() >= deadline {
            task.abort();
            let _ = task.await;
            panic!("fixture readiness deadline");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let observers: Vec<_> = pids.into_iter().map(retain_test_child).collect();
    task.abort();
    match task.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("fixture finished before cancellation"),
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !observers.iter().all(signaled) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let exited = observers.iter().all(signaled);
    let _ = std::fs::remove_file(path);
    assert!(exited, "cancellation must terminate the retained owned process");
}

#[allow(unsafe_code)]
fn close_fixture_outputs() {
    use std::os::windows::io::AsRawHandle;
    std::io::stdout().flush().unwrap();
    // SAFETY: this fixture closes only its own inherited endpoints and performs no later output before exit.
    unsafe {
        windows_sys::Win32::Foundation::CloseHandle(std::io::stdout().as_raw_handle());
        windows_sys::Win32::Foundation::CloseHandle(std::io::stderr().as_raw_handle());
    }
}

#[tokio::test]
async fn pipe_eof_does_not_ack_a_live_root() {
    let _serial = SERIAL.lock().await;
    let start = Instant::now();
    let result = fixture("early-eof", b"", Duration::from_secs(2), 65536).await;
    assert!(result.timed_out);
    assert!(start.elapsed() >= Duration::from_secs(2));
    assert_ack(&result, ProcessStop::Timeout);
}

#[tokio::test]
async fn invalid_arguments_fail_before_execution() {
    let _serial = SERIAL.lock().await;
    let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    let mut command = Command::new(exe);
    command.arg("invalid\0argument");
    let result = run_standalone_windows_bounded(&command, b"", deadline(Duration::from_secs(2)), 65536, 65536).await;
    assert_eq!(result.code, None);
    assert!(result.stdout.is_empty());
    let receipt = result.process_receipt.unwrap();
    assert_eq!(receipt.stop, ProcessStop::SetupFailed);
    assert!(receipt.root.is_none());
    assert!(!receipt.cleanup_confirmed());
}

#[tokio::test]
async fn expired_budget_cannot_start_a_reader() {
    let _serial = SERIAL.lock().await;
    let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    let now = Instant::now();
    for expired in [now, now.checked_sub(Duration::from_secs(1)).unwrap()] {
        let result = run_standalone_windows_bounded(&Command::new(&exe), b"", expired, 1024, 1024).await;
        assert!(result.timed_out);
        assert_eq!(result.code, None);
        let receipt = result.process_receipt.unwrap();
        assert_eq!(receipt.stop, ProcessStop::Timeout);
        assert!(receipt.root.is_none());
    }
    let command = Command::new(&exe);
    let future = run_standalone_windows_bounded(&command, b"", deadline(Duration::from_millis(50)), 1024, 1024);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let result = future.await;
    assert!(result.timed_out);
    assert_eq!(result.code, None);
    assert!(result.process_receipt.unwrap().root.is_none(), "delayed polling must not rebase the entry deadline");
}

#[test]
fn stdio_fixture() {
    if std::env::var(FIXTURE).as_deref() != Ok("stdio") {
        return;
    }
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    std::io::stdout().write_all(&input).unwrap();
    std::io::stderr().write_all(b"fixture-stderr").unwrap();
    std::process::exit(0);
}

#[tokio::test]
async fn output_pipe_observes_eof_after_its_only_client_closes() {
    let _serial = SERIAL.lock().await;
    let result = fixture("close-exit", b"", Duration::from_secs(3), 65536).await;
    assert_eq!(result.code, Some(0), "guard metadata: receipt={:?}", result.process_receipt);
    assert!(result.stderr.is_empty());
    assert_ack(&result, ProcessStop::Completed);
}

#[tokio::test]
async fn product_guard_runs_a_native_rust_stdio_fixture() {
    let _serial = SERIAL.lock().await;
    let name = "stdio_fixture";
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", name, "--nocapture"]).env(FIXTURE, "stdio");
    let result = run_standalone_windows_bounded(&command, b"fixture-input", deadline(Duration::from_secs(5)), 65536, 65536).await;
    assert_eq!(
        result.code,
        Some(0),
        "guard metadata: timeout={} output_limit={} spawn_failed={} receipt={:?}",
        result.timed_out,
        result.output_limited,
        result.spawn_failed,
        result.process_receipt
    );
    assert_ack(&result, ProcessStop::Completed);
    assert_eq!(String::from_utf8_lossy(&result.stdout).matches("running 1 test").count(), 1);
    assert!(result.stdout.ends_with(b"fixture-input"));
    assert_eq!(result.stderr, b"fixture-stderr");
}

async fn stdio_control(program: &std::path::Path) -> (Option<i32>, bool) {
    let mut command = tokio::process::Command::new(program);
    command
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "[Console]::Out.Write('guard-marker')"])
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let Ok(child) = command.spawn() else {
        return (None, false);
    };
    match tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await {
        Ok(Ok(output)) => (output.status.code(), false),
        Ok(Err(_)) => (None, false),
        Err(_) => (None, true),
    }
}

#[tokio::test]
#[allow(unsafe_code)]
async fn product_guard_preserves_simple_windows_powershell_startup() {
    let _serial = SERIAL.lock().await;
    let mut buffer = [0u16; 32768];
    // SAFETY: writable bounded buffer for the OS Windows-directory query; no environment-derived path is logged.
    let count = unsafe { windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    assert!(count > 0 && (count as usize) < buffer.len());
    use std::os::windows::ffi::OsStringExt;
    let normal = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..count as usize]))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let canonical = normal.canonicalize().unwrap();
    let normal_control = stdio_control(&normal).await;
    let canonical_control = stdio_control(&canonical).await;
    eprintln!("PowerShell startup control metadata: normal={normal_control:?} canonical={canonical_control:?}");
    assert_eq!(normal_control, (Some(0), false));
    assert_eq!(canonical_control, (Some(0), false));
    let mut command = std::process::Command::new(normal);
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "[Console]::Out.Write('guard-marker')"]);
    let result = run_standalone_windows_bounded(&command, b"", deadline(Duration::from_secs(10)), 65536, 65536).await;
    assert_eq!(
        result.code,
        Some(0),
        "guard metadata: timeout={} output_limit={} spawn_failed={} receipt={:?}",
        result.timed_out,
        result.output_limited,
        result.spawn_failed,
        result.process_receipt
    );
    assert_ack(&result, ProcessStop::Completed);
    assert_eq!(result.stdout, b"guard-marker");
}
