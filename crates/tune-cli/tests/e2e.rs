#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tune_core::db::{DB_FILE, Store, now_ms};

struct Fixture(PathBuf);

impl Fixture {
    fn new(nodes: Value) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("kt-cli-e2e-{}-{}-{}", std::process::id(), now_ms(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("nodes.json"), json!({"nodes": nodes}).to_string()).unwrap();
        Self(root)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tune"));
        cmd.arg("e2e").env("KATALA_TUNE_CONFIG", self.0.join("nodes.json")).env("KATALA_TUNE_DATA_DIR", self.0.join("production"));
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn report(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("stdout={} stderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn e2e_rejects_invalid_selection_before_creating_data() {
    let f = Fixture::new(json!([{"id":"dev","os":"macos","alias":"unused"}]));
    let data = f.0.join("verification");
    for args in [vec![], vec!["--db"], vec!["--db", data.to_str().unwrap(), "missing"], vec!["--db", data.to_str().unwrap(), "--unknown"]] {
        let out = f.run(&args);
        assert!(!out.status.success());
        assert!(!data.exists());
        assert!(!f.0.join("production").exists());
    }
    assert!(String::from_utf8_lossy(&f.run(&["--db", data.to_str().unwrap(), "missing"]).stderr).contains("unknown node id"));
}

#[test]
fn e2e_never_opens_an_existing_database() {
    let f = Fixture::new(json!([{"id":"dev","os":"macos","status":"paused"}]));
    let data = f.0.join("verification");
    let db = Store::open(&data).unwrap();
    db.set_meta("keep", &json!(42)).unwrap();
    db.conn().execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop(db);
    let before = std::fs::read(data.join(DB_FILE)).unwrap();
    let out = f.run(&["--db", data.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("new directory"));
    assert_eq!(std::fs::read(data.join(DB_FILE)).unwrap(), before);
}

#[test]
fn e2e_invalid_ledger_is_rejected_before_creating_data() {
    for nodes in [
        json!([]),
        json!([{"id":"dev","os":"macos"},{"id":"dev","os":"macos"}]),
        json!([{"os":"macos"}]),
        json!([{"id":123,"os":"macos"}]),
        json!([{"id":"dev","os":"linux"}]),
    ] {
        let f = Fixture::new(nodes);
        let data = f.0.join("verification");
        let out = f.run(&["--db", data.to_str().unwrap()]);
        assert!(!out.status.success());
        assert!(!data.exists());
    }
}

#[test]
fn e2e_exclusions_do_not_count_as_successful_targets() {
    let f = Fixture::new(json!([{"id":"paused","os":"macos","status":"paused","alias":"must-not-connect"}]));
    let data = f.0.join("verification");
    let out = f.run(&["--db", data.to_str().unwrap()]);
    assert!(!out.status.success());
    let v = report(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["counts"], json!({"targets":0,"passed":0,"failed":0,"excluded":1}));
    assert_eq!(v["nodes"][0]["status"], "excluded");
    assert!(!f.0.join("production").exists());
}

#[cfg(unix)]
fn fake_ssh(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        r#"#!/bin/sh
input=$(/bin/cat)
case "$input" in
  *MAX_ROWS*)
    /bin/cat "$KT_LOGS_FIXTURE"
    if [ "$KT_LOGS_BAD_EXIT" = 1 ]; then exit 255; fi
    ;;
  *)
    if [ "$KT_PROBE_FAIL" = 1 ]; then
      echo 'Connection refused' >&2
      exit 255
    fi
    echo '{"probe":"mac","cpu_busy":12,"memory":{"available_pct":70},"processes":[],"disk":{"free_gb":150}}'
    if [ "$KT_PROBE_BAD_EXIT" = 1 ]; then exit 255; fi
    ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[cfg(unix)]
#[test]
fn e2e_runs_collection_persistence_reload_and_logs_and_reports_partial_failures() {
    let f = Fixture::new(json!([
        {"id":"dev","os":"macos","alias":"fixture","network":false,"benchmark":false},
        {"id":"retired","os":"macos","alias":"must-not-connect","status":"retired"}
    ]));
    let config_before = std::fs::read(f.0.join("nodes.json")).unwrap();
    let bin = fake_ssh(&f.0);
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let fixture = f.0.join("logs.json");
    let at = now_ms();
    for (index, mode) in ["ok", "partial", "probe-failure", "logs-exit", "probe-exit", "no-cursor"].into_iter().enumerate() {
        let fail_logs = matches!(mode, "partial" | "logs-exit" | "no-cursor");
        let fail_probe = matches!(mode, "probe-failure" | "probe-exit");
        let mut payload = json!({"sources":{
            "mac_diag":{"cursor":"1","rows":[{"uid":"event","ts":at,"message":"fixture event","level":"warn"}]},
            "mac_kernel":if mode=="partial" {json!({"error":"fixture source unavailable"})} else {json!({"cursor":"1","rows":[]})}
        }});
        if mode == "no-cursor" {
            payload["sources"]["mac_kernel"].as_object_mut().unwrap().remove("cursor");
        }
        std::fs::write(&fixture, payload.to_string()).unwrap();
        let data = f.0.join(format!("verification-{index}"));
        let out = f
            .command()
            .args(["--db", data.to_str().unwrap()])
            .env("PATH", std::env::join_paths(&paths).unwrap())
            .env("KT_LOGS_FIXTURE", &fixture)
            .env("KT_PROBE_FAIL", if mode == "probe-failure" { "1" } else { "0" })
            .env("KT_LOGS_BAD_EXIT", if mode == "logs-exit" { "1" } else { "0" })
            .env("KT_PROBE_BAD_EXIT", if mode == "probe-exit" { "1" } else { "0" })
            .output()
            .unwrap();
        assert_eq!(out.status.success(), !fail_logs && !fail_probe, "{}", String::from_utf8_lossy(&out.stderr));
        let v = report(&out);
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["counts"], json!({"targets":1,"passed":usize::from(!fail_logs&&!fail_probe),"failed":usize::from(fail_logs||fail_probe),"excluded":1}));
        let dev = &v["nodes"][0];
        assert_eq!(dev["probe"]["ok"], !fail_probe);
        assert_eq!(dev["reload"]["ok"], !fail_probe);
        assert_eq!(dev["logs"]["ok"], !fail_logs);
        let db = Store::open_read_only(&data).unwrap();
        assert_eq!(db.last_snapshots("dev", 1).unwrap().len(), usize::from(!fail_probe));
        assert_eq!(db.query_logs(&json!({"node_id":"dev","since":0})).unwrap().len(), usize::from(mode != "logs-exit"));
        assert!(db.last_snapshots("retired", 1).unwrap().is_empty());
        assert!(!out.stdout.windows(b"fixture event".len()).any(|w| w == b"fixture event"));
        assert!(dev.get("data").is_none());
    }
    assert_eq!(std::fs::read(f.0.join("nodes.json")).unwrap(), config_before);
    assert!(!f.0.join("production").exists());
}
