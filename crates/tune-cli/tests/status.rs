use std::process::Command;

use serde_json::{Value, json};
use tune_core::db::{Check, Store, now_ms};

fn run_status(config: &std::path::Path, data: &std::path::Path, recompute: bool) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tune"));
    command.arg("status").env("KATALA_TUNE_CONFIG", config).env("KATALA_TUNE_DATA_DIR", data);
    if recompute {
        command.arg("--recompute");
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn status_filters_removed_scopes_with_and_without_recompute() {
    let tmp = std::env::temp_dir().join(format!("katala-tune-cli-status-test-{}-{}", std::process::id(), now_ms()));
    let data = tmp.join("data");
    std::fs::create_dir_all(&tmp).unwrap();
    let config = tmp.join("nodes.json");
    std::fs::write(&config, r#"{"nodes":[{"id":"current","alias":"unused","os":"windows"}]}"#).unwrap();
    let db = Store::open(&data).unwrap();
    let now = now_ms();
    db.save_checks("removed", &[Check { id: "probe".into(), name: "probe".into(), status: "fail".into(), detail: None }], now).unwrap();
    db.save_checks("current", &[Check { id: "probe".into(), name: "probe".into(), status: "warn".into(), detail: None }], now).unwrap();
    db.save_checks("_app", &[Check { id: "db".into(), name: "db".into(), status: "ok".into(), detail: None }], now).unwrap();
    drop(db);

    let normal = run_status(&config, &data, false);
    assert_eq!(normal["counts"], json!({ "ok": 1, "warn": 1, "fail": 0, "unknown": 0 }));
    assert!(normal["checks"].as_array().unwrap().iter().all(|row| row["scope"] != "removed"));
    assert_eq!(normal["checks"].as_array().unwrap().iter().find(|row| row["scope"] == "current" && row["id"] == "probe").unwrap()["status"], "warn");

    let recomputed = run_status(&config, &data, true);
    assert!(recomputed["checks"].as_array().unwrap().iter().all(|row| row["scope"] != "removed"));
    assert_eq!(recomputed["checks"].as_array().unwrap().iter().find(|row| row["scope"] == "current" && row["id"] == "probe").unwrap()["status"], "unknown");
    assert!(recomputed["events"].as_array().unwrap().iter().any(|row| row["scope"] == "removed"));
    assert!(Store::open(&data).unwrap().checks().unwrap().iter().any(|row| row["scope"] == "removed"));

    let _ = std::fs::remove_dir_all(tmp);
}
