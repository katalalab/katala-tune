use serde_json::Value;
use std::{fs, path::Path, process::Command};

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
    if r["reason"] == "guest_capacity_only" {
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
}
