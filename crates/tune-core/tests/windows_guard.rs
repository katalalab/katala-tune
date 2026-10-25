#![cfg(windows)]
//! Dedicated single-spawner process: every native invocation and std/Tokio control holds SERIAL.
//! Real Windows children exercise the product backend. No fleet/SSH or shared process is used.
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tune_core::collect::{
    BoundedResult, OwnedWindowsCompletion, PinnedWindowsExecutable, ProcessStop, StandaloneWindowsEnvironment, run_owned_standalone_windows_bounded,
    run_standalone_windows_bounded,
};

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
        "root-first" => {
            let directory = std::path::PathBuf::from(std::env::var_os("KATALA_GUARD_QUIET_DIR").unwrap());
            let mut child = fixture_command("root-first-child");
            child.env("KATALA_GUARD_QUIET_DIR", &directory).env("KATALA_GUARD_ROOT_PID", std::process::id().to_string());
            for key in ["KATALA_GUARD_QUIET_DELAY_MS", "KATALA_GUARD_QUIET_EARLY_EOF"] {
                if let Some(value) = std::env::var_os(key) {
                    child.env(key, value);
                }
            }
            let _child = child.spawn().unwrap();
            let end = deadline(Duration::from_secs(5));
            while !directory.join("release-root").exists() {
                assert!(Instant::now() < end, "fixture root release deadline");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        "root-first-child" => {
            let directory = std::path::PathBuf::from(std::env::var_os("KATALA_GUARD_QUIET_DIR").unwrap());
            let pid = std::env::var("KATALA_GUARD_ROOT_PID").unwrap().parse().unwrap();
            // Our root remains alive until the test retains this child and releases
            // it. Retain that exact live root for observation only, never PID mutation.
            let root = retain_test_child(pid);
            let pending = directory.join("child-ready.tmp");
            let mut ready = std::fs::OpenOptions::new().create_new(true).write(true).open(&pending).unwrap();
            writeln!(ready, "{pid} {}", std::process::id()).unwrap();
            ready.flush().unwrap();
            drop(ready);
            std::fs::rename(pending, directory.join("child-ready")).unwrap();
            let early_eof = std::env::var("KATALA_GUARD_QUIET_EARLY_EOF").as_deref() == Ok("1");
            if early_eof {
                close_fixture_outputs();
            }
            let end = deadline(Duration::from_secs(5));
            while !signaled(&root) {
                assert!(Instant::now() < end, "fixture retained root exit deadline");
                std::thread::sleep(Duration::from_millis(5));
            }
            // This intentional post-root side effect characterizes the new lifetime
            // policy. It is not evidence that arbitrary descendants are read-only.
            let delay_ms = match std::env::var("KATALA_GUARD_QUIET_DELAY_MS").as_deref() {
                Ok("250") => 250,
                Ok("30000") => 30000,
                _ => panic!("unknown private fixture delay"),
            };
            std::thread::sleep(Duration::from_millis(delay_ms));
            let mut marker = std::fs::OpenOptions::new().create_new(true).write(true).open(directory.join("after-root")).unwrap();
            marker.write_all(b"post-root fixture write\n").unwrap();
            drop(marker);
            if !early_eof {
                std::io::stdout().write_all(b"natural-child-exit\n").unwrap();
                std::io::stdout().flush().unwrap();
            }
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
            if let Ok(value) = std::env::var("KATALA_GUARD_SET") {
                println!("set:{value}");
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

fn fixture_invocation() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "owned_fixture", "--nocapture"]);
    command
}

fn fixture_command(mode: &str) -> Command {
    let mut command = fixture_invocation();
    command.env_clear().envs(minimal_environment()).env(FIXTURE, mode);
    command
}

// Explicit non-secret OS locations only; do not collect the parent's complete environment.
fn minimal_environment() -> Vec<(OsString, OsString)> {
    ["SystemRoot", "WINDIR", "TEMP", "TMP", "COMSPEC"].into_iter().filter_map(|key| std::env::var_os(key).map(|value| (key.into(), value))).collect()
}

fn fixture_environment(mode: &str) -> Vec<(OsString, OsString)> {
    let mut entries = minimal_environment();
    entries.push((FIXTURE.into(), mode.into()));
    entries
}

fn deadline(timeout: Duration) -> Instant {
    Instant::now().checked_add(timeout).expect("finite test deadline")
}

async fn fixture(mode: &str, input: &[u8], timeout: Duration, limit: usize) -> BoundedResult {
    let end = deadline(timeout);
    let environment = fixture_environment(mode);
    run_standalone_windows_bounded(&fixture_invocation(), StandaloneWindowsEnvironment::new(&environment), input, end, limit, limit).await
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

struct QuiescenceDirectory(std::path::PathBuf);
impl QuiescenceDirectory {
    fn new() -> Self {
        let name =
            format!("katala-root-first-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let directory = std::env::temp_dir().join(name);
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
}
impl Drop for QuiescenceDirectory {
    fn drop(&mut self) {
        // Only this create-new fixture directory, after the owned invocation's cleanup.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[allow(unsafe_code)]
fn observer_exit_code(handle: &std::os::windows::io::OwnedHandle) -> u32 {
    use std::os::windows::io::AsRawHandle;
    let mut code = 0;
    // SAFETY: retained query-capable fixture observer; caller separately requires actual signal.
    assert_ne!(unsafe { windows_sys::Win32::System::Threading::GetExitCodeProcess(handle.as_raw_handle(), &mut code) }, 0);
    code
}

struct RootFirstInvocation {
    task: tokio::task::JoinHandle<BoundedResult>,
    root: std::os::windows::io::OwnedHandle,
    child: std::os::windows::io::OwnedHandle,
    end: Instant,
}

async fn start_root_first(directory: &QuiescenceDirectory, delay: &str, early_eof: bool) -> RootFirstInvocation {
    let mut environment = fixture_environment("root-first");
    environment.push(("KATALA_GUARD_QUIET_DIR".into(), directory.0.as_os_str().to_owned()));
    environment.push(("KATALA_GUARD_QUIET_DELAY_MS".into(), delay.into()));
    environment.push(("KATALA_GUARD_QUIET_EARLY_EOF".into(), if early_eof { "1" } else { "0" }.into()));
    let end = deadline(Duration::from_secs(5));
    let invocation = tokio::spawn(async move {
        run_standalone_windows_bounded(&fixture_invocation(), StandaloneWindowsEnvironment::new(&environment), b"", end, 65536, 65536).await
    });
    let (root, child) = loop {
        if let Ok(value) = std::fs::read_to_string(directory.0.join("child-ready"))
            && let Some(frame) = value.strip_suffix('\n')
        {
            let fields: Vec<_> = frame.split(' ').collect();
            if fields.len() == 2
                && let (Ok(root), Ok(child)) = (fields[0].parse::<u32>(), fields[1].parse::<u32>())
            {
                break (retain_test_child(root), retain_test_child(child));
            }
        }
        if Instant::now() >= end || invocation.is_finished() {
            invocation.abort();
            let _ = invocation.await;
            panic!("owned root-first fixture did not reach child-ready before its original deadline");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    RootFirstInvocation { task: invocation, root, child, end }
}

fn release_root(directory: &QuiescenceDirectory) {
    std::fs::OpenOptions::new().create_new(true).write(true).open(directory.0.join("release-root")).unwrap();
}

#[tokio::test]
async fn root_first_child_naturally_exits_and_writes_only_its_private_marker() {
    let _serial = SERIAL.lock().await;
    let directory = QuiescenceDirectory::new();
    let run = start_root_first(&directory, "250", false).await;
    // The actual child observer is retained before its root can exit. Its own
    // retained-root wait gates the subsequent marker write and natural exit.
    release_root(&directory);
    let result = run.task.await.unwrap();
    let receipt = result.process_receipt.as_ref().expect("actual guard receipt");
    let child_signaled = signaled(&run.child);
    let child_code = child_signaled.then(|| observer_exit_code(&run.child));
    eprintln!(
        "root_first_fixture stop={:?} root_code={:?} child_signaled={} child_code={:?} job_empty={} eof={} writer={} marker={}",
        receipt.stop,
        receipt.root_exit_code,
        child_signaled,
        child_code,
        receipt.job_empty,
        receipt.stdout_eof && receipt.stderr_eof,
        receipt.writer_finished,
        directory.0.join("after-root").exists()
    );
    assert_eq!(result.code, Some(0), "natural whole-invocation completion must be admitted");
    assert_eq!(receipt.stop, ProcessStop::Completed);
    assert!(receipt.cleanup_confirmed());
    assert_eq!(child_code, Some(0), "child must exit naturally, never by owned-Job termination");
    assert!(receipt.total_processes >= 2);
    assert!(result.stdout.ends_with(b"natural-child-exit\n"));
    assert!(std::fs::read(directory.0.join("after-root")).is_ok_and(|bytes| bytes == b"post-root fixture write\n"));
    assert!(signaled(&run.root) && Instant::now() < run.end);
}

#[tokio::test]
async fn root_first_live_child_is_rejected_even_after_output_eof() {
    let _serial = SERIAL.lock().await;
    let directory = QuiescenceDirectory::new();
    let run = start_root_first(&directory, "30000", true).await;
    release_root(&directory);
    let result = run.task.await.unwrap();
    let receipt = result.process_receipt.as_ref().unwrap();
    assert_eq!(receipt.stop, ProcessStop::LingeringDescendants);
    assert!(receipt.cleanup_confirmed());
    assert_eq!(result.code, None);
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
    assert!(signaled(&run.root) && signaled(&run.child));
    assert_eq!(observer_exit_code(&run.child), 124);
    assert!(!directory.0.join("after-root").exists());
    assert!(Instant::now() < run.end);
}

#[tokio::test]
async fn root_first_pending_child_cannot_extend_the_original_execution_deadline() {
    let _serial = SERIAL.lock().await;
    let directory = QuiescenceDirectory::new();
    let run = start_root_first(&directory, "30000", false).await;
    // Release near the original end rather than rebasing the request after setup.
    tokio::time::sleep_until(tokio::time::Instant::from_std(run.end - Duration::from_millis(100))).await;
    release_root(&directory);
    let result = run.task.await.unwrap();
    let receipt = result.process_receipt.as_ref().unwrap();
    assert_eq!(receipt.stop, ProcessStop::Timeout);
    assert!(result.timed_out && receipt.cleanup_confirmed());
    assert_eq!(receipt.root_exit_code, Some(0));
    assert!(signaled(&run.child));
    assert_eq!(observer_exit_code(&run.child), 124);
    assert!(result.code.is_none() && result.stdout.is_empty() && result.stderr.is_empty());
    assert!(!directory.0.join("after-root").exists());
}

#[tokio::test]
async fn cancelling_after_root_exit_still_terminates_the_owned_pending_child() {
    let _serial = SERIAL.lock().await;
    let directory = QuiescenceDirectory::new();
    let run = start_root_first(&directory, "30000", false).await;
    release_root(&directory);
    while !signaled(&run.root) && Instant::now() < run.end {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(signaled(&run.root) && !signaled(&run.child));
    run.task.abort();
    assert!(run.task.await.unwrap_err().is_cancelled());
    let end = deadline(Duration::from_secs(3));
    while !signaled(&run.child) && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(signaled(&run.child));
    assert!(!directory.0.join("after-root").exists());
}

#[tokio::test]
async fn missing_executable_is_never_success() {
    let _serial = SERIAL.lock().await;
    let result = run_standalone_windows_bounded(
        &Command::new("katala-guard-missing-executable"),
        StandaloneWindowsEnvironment::new(&[]),
        b"",
        deadline(Duration::from_secs(1)),
        1024,
        1024,
    )
    .await;
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
    command.args(args);
    let mut environment = fixture_environment("args");
    // Omission deletes ROUNDTRIP; a differently-cased complete-map key still sets SET.
    environment.push(("katala_guard_set".into(), "present".into()));
    let result =
        run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&environment), b"", deadline(Duration::from_secs(10)), 65536, 65536).await;
    assert_ack(&result, ProcessStop::Completed);
    assert_eq!(result.code, Some(0));
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(!output.contains("env:"));
    assert!(output.contains("set:present"));
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
        let command = fixture_invocation();
        let mut environment = fixture_environment("cancel");
        environment.push(("KATALA_GUARD_READY".into(), ready_path.into()));
        run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&environment), b"", deadline(Duration::from_secs(20)), 65536, 65536).await
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
    let result = run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&[]), b"", deadline(Duration::from_secs(2)), 65536, 65536).await;
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
        let result = run_standalone_windows_bounded(&Command::new(&exe), StandaloneWindowsEnvironment::new(&[]), b"", expired, 1024, 1024).await;
        assert!(result.timed_out);
        assert_eq!(result.code, None);
        let receipt = result.process_receipt.unwrap();
        assert_eq!(receipt.stop, ProcessStop::Timeout);
        assert!(receipt.root.is_none());
    }
    let command = Command::new(&exe);
    let future = run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&[]), b"", deadline(Duration::from_millis(50)), 1024, 1024);
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
    command.args(["--exact", name, "--nocapture"]);
    let environment = fixture_environment("stdio");
    let result = run_standalone_windows_bounded(
        &command,
        StandaloneWindowsEnvironment::new(&environment),
        b"fixture-input",
        deadline(Duration::from_secs(5)),
        65536,
        65536,
    )
    .await;
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
    command.env_clear().envs(minimal_environment());
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
    let mut drive_environment = minimal_environment();
    drive_environment.push(("=Q:".into(), r"Q:\fixture".into()));
    for environment in [minimal_environment(), std::env::vars_os().collect(), drive_environment] {
        let result =
            run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&environment), b"", deadline(Duration::from_secs(10)), 65536, 65536)
                .await;
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
}

const CANARY: &str = "KATALA_GUARD_ENV_CANARY";
const SYNTHETIC_CANARY: &str = "synthetic-canary-not-a-credential";

fn environment_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "environment_fixture", "--nocapture", "--", mode]);
    command
}

#[test]
fn environment_fixture() {
    let arguments: Vec<_> = std::env::args().collect();
    if arguments.iter().any(|value| value == "--guard-env-child") {
        assert!(std::env::vars_os().next().is_none(), "child must receive exactly the empty map");
        println!("empty-environment-child-ran");
        std::process::exit(0);
    }
    if !arguments.iter().any(|value| value == "--guard-env-parent") {
        return;
    }
    assert!(std::env::var(CANARY).as_deref() == Ok(SYNTHETIC_CANARY), "synthetic parent canary must be present");
    let mut command = environment_command("--guard-env-child");
    command.env_clear();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let result =
        runtime.block_on(run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&[]), b"", deadline(Duration::from_secs(5)), 65536, 65536));
    assert_ack(&result, ProcessStop::Completed);
    assert_eq!(result.code, Some(0));
    assert!(String::from_utf8_lossy(&result.stdout).contains("empty-environment-child-ran"), "actual child execution required");
    assert!(!result.stdout.windows(SYNTHETIC_CANARY.len()).any(|part| part == SYNTHETIC_CANARY.as_bytes()));
    assert!(!result.stderr.windows(SYNTHETIC_CANARY.len()).any(|part| part == SYNTHETIC_CANARY.as_bytes()));
    println!("empty-environment-parent-confirmed");
    std::process::exit(0);
}

#[tokio::test]
async fn complete_empty_environment_does_not_reintroduce_parent_values() {
    let _serial = SERIAL.lock().await;
    // A separate parent gets only a public synthetic value. No global set_var or secret read is needed.
    let mut command = tokio::process::Command::from(environment_command("--guard-env-parent"));
    command.env_clear().env(CANARY, SYNTHETIC_CANARY).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let child = command.spawn().unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await.unwrap().unwrap();
    assert!(output.status.success(), "isolated environment parent failed: code={:?}", output.status.code());
    assert!(String::from_utf8_lossy(&output.stdout).contains("empty-environment-parent-confirmed"), "actual parent verification required");
    assert!(!output.stdout.windows(SYNTHETIC_CANARY.len()).any(|part| part == SYNTHETIC_CANARY.as_bytes()));
    assert!(!output.stderr.windows(SYNTHETIC_CANARY.len()).any(|part| part == SYNTHETIC_CANARY.as_bytes()));
}

#[tokio::test]
async fn complete_environment_rejects_ambiguous_and_invalid_inputs() {
    let _serial = SERIAL.lock().await;
    for entries in [
        vec![("Name".into(), "first".into()), ("name".into(), "second".into())],
        vec![("NAME".into(), "first".into()), ("NAME".into(), "second".into())],
        vec![("".into(), "value".into())],
        vec![("NAME=OTHER".into(), "value".into())],
        vec![("=NOTDRIVE".into(), "value".into())],
        vec![("=1:".into(), "value".into())],
        vec![("=CC:".into(), "value".into())],
        vec![("=C:=".into(), "value".into())],
        vec![("NAME\0".into(), "value".into())],
        vec![("NAME".into(), "value\0".into())],
    ] {
        let result = run_standalone_windows_bounded(
            &environment_command("--guard-env-child"),
            StandaloneWindowsEnvironment::new(&entries),
            b"",
            deadline(Duration::from_secs(2)),
            65536,
            65536,
        )
        .await;
        assert_eq!(result.code, None);
        let receipt = result.process_receipt.unwrap();
        assert_eq!(receipt.stop, ProcessStop::SetupFailed);
        assert!(receipt.root.is_none(), "invalid complete map must fail before spawn");
    }
    for remove in [false, true] {
        let mut command = environment_command("--guard-env-child");
        if remove {
            command.env_remove("NAME");
        } else {
            command.env("NAME", "value");
        }
        let result =
            run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&[]), b"", deadline(Duration::from_secs(2)), 65536, 65536).await;
        assert_eq!(result.code, None);
        let receipt = result.process_receipt.unwrap();
        assert_eq!(receipt.stop, ProcessStop::SetupFailed);
        assert!(receipt.root.is_none(), "Command edits must not be silently ignored");
    }
}

fn fixture_pin(end: Instant) -> (PinnedWindowsExecutable, String) {
    let executable = std::env::current_exe().unwrap();
    let bytes = std::fs::read(&executable).expect("read this test executable for its expected digest");
    let digest = Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let pin = PinnedWindowsExecutable::open(&executable, &digest, end).expect("pin this test executable within the original deadline");
    (pin, digest)
}

async fn owned_fixture_run(mode: &str, input: &[u8], timeout: Duration, limit: usize) -> (Result<OwnedWindowsCompletion, BoundedResult>, String) {
    let end = deadline(timeout);
    let (pin, digest) = fixture_pin(end);
    let mut command = Command::new(pin.executable());
    command.args(["--exact", "owned_fixture", "--nocapture"]);
    let environment = fixture_environment(mode);
    let result = run_owned_standalone_windows_bounded(pin, &command, StandaloneWindowsEnvironment::new(&environment), input, end, limit, limit).await;
    (result, digest)
}

fn owned_completion_failure_diagnostic(failure: &BoundedResult) -> String {
    let receipt = failure.process_receipt.as_ref();
    let stop = match receipt.map(|value| value.stop) {
        None => "missing",
        Some(ProcessStop::SetupFailed) => "setup_failed",
        Some(ProcessStop::SpawnFailed) => "spawn_failed",
        Some(ProcessStop::Completed) => "completed",
        Some(ProcessStop::Timeout) => "timeout",
        Some(ProcessStop::OutputLimit) => "output_limit",
        Some(ProcessStop::LingeringDescendants) => "lingering_descendants",
        Some(ProcessStop::IoFailure) => "io_failure",
        Some(ProcessStop::CleanupUnconfirmed) => "cleanup_unconfirmed",
    };
    let root_exit = match receipt.and_then(|value| value.root_exit_code) {
        None => "missing",
        Some(0) => "zero",
        Some(_) => "nonzero",
    };
    format!(
        "owned_completion_failure timed_out={} output_limited={} spawn_failed={} receipt_some={} stop={} root_exit={} root_exited={} job_empty={} observed_exited={} stdout_eof={} stderr_eof={} writer_finished={} cleanup_confirmed={}",
        failure.timed_out,
        failure.output_limited,
        failure.spawn_failed,
        receipt.is_some(),
        stop,
        root_exit,
        receipt.is_some_and(|value| value.root_exited),
        receipt.is_some_and(|value| value.job_empty),
        receipt.is_some_and(|value| value.observed_exited),
        receipt.is_some_and(|value| value.stdout_eof),
        receipt.is_some_and(|value| value.stderr_eof),
        receipt.is_some_and(|value| value.writer_finished),
        receipt.is_some_and(|value| value.cleanup_confirmed()),
    )
}

#[tokio::test]
async fn owned_completion_consumes_bound_actual_output() {
    let _serial = SERIAL.lock().await;
    let input: Vec<u8> = (0..128 * 1024).map(|i| b'a' + (i % 26) as u8).collect();
    let (result, digest) = owned_fixture_run("happy", &input, Duration::from_secs(15), 256 * 1024).await;
    let completion = match result {
        Ok(completion) => completion,
        Err(failure) => panic!("{}", owned_completion_failure_diagnostic(&failure)),
    };
    assert_ack(completion.diagnostic(), ProcessStop::Completed);
    assert_eq!(completion.diagnostic().code, Some(0));
    let output = completion.into_verified_output(&input, &digest).ok().expect("consume matching request and artifact once");
    assert!(output.ends_with(&input));
    assert_eq!(String::from_utf8_lossy(&output).matches("running 1 test").count(), 1);
}

#[tokio::test]
async fn owned_completion_rejects_different_request_or_artifact() {
    let _serial = SERIAL.lock().await;
    let wrong_digest = "0".repeat(64);
    for wrong_input in [true, false] {
        let (result, digest) = owned_fixture_run("happy", b"bound-input", Duration::from_secs(15), 65536).await;
        let completion = result.ok().expect("actual completion before binding rejection");
        assert_ack(completion.diagnostic(), ProcessStop::Completed);
        let rejected =
            completion.into_verified_output(if wrong_input { b"changed-input" } else { b"bound-input" }, if wrong_input { &digest } else { &wrong_digest });
        let failure = rejected.expect_err("changed request or artifact cannot consume completion");
        assert_eq!(failure.code, None);
        assert!(failure.stdout.is_empty() && failure.stderr.is_empty());
    }
}

#[tokio::test]
async fn owned_timeout_and_overflow_issue_no_completion() {
    let _serial = SERIAL.lock().await;
    for (mode, timeout, limit, stop) in [
        ("sleep", Duration::from_secs(3), 65536, ProcessStop::Timeout),
        ("stdout", Duration::from_secs(15), 1024, ProcessStop::OutputLimit),
        ("stderr", Duration::from_secs(15), 1024, ProcessStop::OutputLimit),
    ] {
        let (result, _) = owned_fixture_run(mode, b"", timeout, limit).await;
        let failure = result.err().expect("failed native run cannot issue owned completion");
        assert_ack(&failure, stop);
        assert_eq!(failure.code, None);
        assert!(failure.stdout.is_empty() && failure.stderr.is_empty());
    }
}

#[tokio::test]
async fn owned_pin_budget_may_shorten_but_cannot_extend() {
    let _serial = SERIAL.lock().await;
    for extend in [true, false] {
        let pin_end = deadline(Duration::from_secs(15));
        let (pin, digest) = fixture_pin(pin_end);
        let mut command = Command::new(pin.executable());
        command.args(["--exact", "owned_fixture", "--nocapture"]);
        let environment = fixture_environment("happy");
        let run_end = if extend { pin_end.checked_add(Duration::from_secs(1)).unwrap() } else { pin_end.checked_sub(Duration::from_secs(1)).unwrap() };
        let result =
            run_owned_standalone_windows_bounded(pin, &command, StandaloneWindowsEnvironment::new(&environment), b"original-budget", run_end, 65536, 65536)
                .await;
        if extend {
            let failure = result.err().expect("extended pin budget must fail before spawn");
            assert_eq!(failure.code, None);
            assert!(failure.stdout.is_empty() && failure.stderr.is_empty());
            let receipt = failure.process_receipt.expect("rejected setup diagnostic");
            assert_eq!(receipt.stop, ProcessStop::SetupFailed);
            assert!(receipt.root.is_none() && receipt.observed.is_empty());
            assert!(!receipt.cleanup_confirmed());
        } else {
            let completion = result.ok().expect("shortening the original pin budget is allowed");
            assert_ack(completion.diagnostic(), ProcessStop::Completed);
            let output = completion.into_verified_output(b"original-budget", &digest).ok().expect("shortened completion remains bound");
            assert!(output.ends_with(b"original-budget"));
        }
    }
}
