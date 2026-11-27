use serde_json::Value;
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn wrapper(dir: &Path, report: &str, expected: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tune"));
    c.current_dir(dir).args([
        "storage-run",
        "--job-id",
        "sample",
        "--target",
        ".",
        "--report",
        report,
        "--expected-peak-bytes",
        expected,
        "--other-reserved-bytes",
        "0",
        "--os-reserve-bytes",
        "0",
        "--category",
        "artifact=out",
    ]);
    c
}

#[test]
fn refuses_overlap_or_existing_report_before_spawning() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("out/nested")).unwrap();
    let report = root.path().join("report.json");
    let exe = env!("CARGO_BIN_EXE_tune");
    assert_eq!(
        wrapper(root.path(), "report.json", "1").args(["--category", "log=out/nested", "--", exe, "--version"]).output().unwrap().status.code(),
        Some(2)
    );
    assert!(!report.exists());
    fs::write(&report, b"prior result").unwrap();
    assert_eq!(wrapper(root.path(), "report.json", "1").args(["--", exe, "--version"]).output().unwrap().status.code(), Some(2));
    assert_eq!(fs::read(&report).unwrap(), b"prior result");
}

#[test]
fn admission_and_failed_commands_leave_distinct_private_evidence() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("out")).unwrap();
    let execute = |report, expected, program, args: &[&str]| {
        let status = wrapper(root.path(), report, expected).arg("--").arg(program).args(args).output().unwrap().status;
        let text = fs::read_to_string(root.path().join(report)).unwrap();
        assert!(!text.contains("sensitive-argument"));
        let r: Value = serde_json::from_str(&text).unwrap();
        (status, r)
    };
    let exe = env!("CARGO_BIN_EXE_tune");
    let (_, r) = execute("hold.json", "9007199254740991", "missing-program", &["sensitive-argument"]);
    assert_eq!(r["status"], "blocked");
    assert!(r["peak_bytes"].is_null());
    if environment_held(&r) {
        return;
    }
    assert_eq!(r["reason"], "insufficient_space");
    let (status, r) = execute("failed.json", "1", "nonexistent-storage-run-test-program", &["sensitive-argument"]);
    assert!(!status.success());
    assert_eq!(r["reason"], "spawn_error");
    let (status, r) = execute("child-error.json", "1", exe, &["invalid-command", "sensitive-argument"]);
    assert!(!status.success());
    assert_eq!(r["reason"], "command_failed");
    assert_eq!(r["exit_code"], 2);
    let (status, r) = execute("ok.json", "1", exe, &["--version"]);
    assert!(status.success());
    assert_eq!(r["status"], "completed");
    assert!(r["samples"].as_array().unwrap().len() >= 2);
    assert_eq!(r["categories"][0]["logical_delta_bytes"], 0);
    assert!(r["residual_bytes"].is_null());
    assert_eq!(r["net_bytes"], r["unattributed_net_bytes"]);
    assert!(!root.path().join("ok.json.incomplete").exists());
}

fn environment_held(r: &Value) -> bool {
    r["status"] == "blocked" && matches!(r["reason"].as_str(), Some("guest_capacity_only" | "environment_unknown"))
}

fn held_in(dir: &Path) -> bool {
    fs::read(dir.join("sample.json")).ok().and_then(|v| serde_json::from_slice(&v).ok()).is_some_and(|r| environment_held(&r))
}

// The integration-test executable doubles as a native child on both Windows and Unix.
fn fixture(dir: &Path, action: &str) -> Command {
    let mut cmd = wrapper(dir, "sample.json", "0");
    cmd.arg("--").arg(std::env::current_exe().unwrap()).args(["--ignored", "--exact", "storage_fixture", "--nocapture"]).env("STORAGE_FIXTURE_ACTION", action);
    cmd
}

#[test]
#[ignore = "only invoked by the storage-run tests as their native child process"]
fn storage_fixture() {
    let Ok(action) = std::env::var("STORAGE_FIXTURE_ACTION") else { return };
    match action.as_str() {
        "replace-result" => {
            fs::write("sample.new", b"result from child").unwrap();
            fs::rename("sample.new", "sample.json").unwrap();
        }
        "replace-claim" => {
            if let Err(e) = fs::rename("sample.json.incomplete", "moved-claim") {
                // Some Windows filesystems prevent renaming a directory with open handles.
                if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied {
                    fs::write("rename-refused", b"").unwrap();
                    return;
                }
                panic!("could not move claim: {e}");
            }
            fs::write("sample.json.incomplete", b"replacement").unwrap();
        }
        "wait" => {
            println!("ready-for-interrupt");
            io::stdout().flush().unwrap();
            io::stdin().read_to_end(&mut Vec::new()).unwrap();
        }
        _ => panic!("invalid child fixture"),
    }
}

fn staged(dir: &Path, prefix: &str) -> Value {
    let path: PathBuf = fs::read_dir(dir.join("sample.json.incomplete"))
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .unwrap()
        .path();
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn preserves_a_report_created_by_the_child() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("out")).unwrap();
    let status = fixture(root.path(), "replace-result").output().unwrap().status;
    if held_in(root.path()) {
        return;
    }
    assert_eq!(fs::read(root.path().join("sample.json")).unwrap(), b"result from child");
    assert!(!status.success());
    assert_eq!(staged(root.path(), "result-")["status"], "completed");
}

#[test]
fn preserves_a_replaced_claim_and_does_not_publish_under_it() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("out")).unwrap();
    let status = fixture(root.path(), "replace-claim").output().unwrap().status;
    if held_in(root.path()) {
        return;
    }
    if root.path().join("rename-refused").exists() {
        assert!(status.success());
        assert!(!root.path().join("sample.json.incomplete").exists());
        return;
    }
    assert!(!status.success());
    assert_eq!(fs::read(root.path().join("sample.json.incomplete")).unwrap(), b"replacement");
    assert!(!root.path().join("sample.json").exists());
}

#[test]
fn interrupted_run_keeps_a_readable_receipt_and_excludes_a_second_wrapper() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("out")).unwrap();
    let mut child = fixture(root.path(), "wait").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdout = io::BufReader::new(child.stdout.take().unwrap());
    let ready = stdout.by_ref().lines().map(Result::unwrap).any(|s| s.contains("ready-for-interrupt"));
    if !ready && held_in(root.path()) {
        assert!(!child.wait().unwrap().success());
        return;
    }
    assert!(ready);
    let other = fixture(root.path(), "replace-result").output().unwrap();
    assert_eq!(other.status.code(), Some(2));
    child.kill().unwrap();
    // Closing the pipe allows the orphaned fixture child to exit, including on Windows.
    drop(child.stdin.take());
    child.wait().unwrap();
    io::copy(&mut stdout, &mut io::sink()).unwrap();
    assert!(!root.path().join("sample.json").exists());
    let receipt = staged(root.path(), "started-");
    assert_eq!(receipt["status"], "failed");
    assert_eq!(receipt["reason"], "incomplete");
}
