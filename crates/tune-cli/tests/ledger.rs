use std::process::Command;
use tune_core::db::{DB_FILE, Store};

#[test]
fn ledger_never_initializes_or_migrates_user_files() {
    let root = std::env::temp_dir().join(format!("kt-cli-ledger-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&root).unwrap();
    let cfg = root.join("nodes.json");
    let data = root.join("data");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_tune")).args(["ledger", "--json"]).env("KATALA_TUNE_CONFIG", &cfg).env("KATALA_TUNE_DATA_DIR", &data).output().unwrap()
    };
    // Missing ledger must fail without installing the example or creating a DB.
    assert!(!run().status.success());
    assert!(!cfg.exists());
    assert!(!data.exists());
    std::fs::write(&cfg, r#"{"nodes":[{"id":"dev","os":"windows"}]}"#).unwrap();
    let output = run();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!data.exists());
    let row: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(row["counts"]["unobserved"], 1);
    // Existing databases must be queried without migrations or legacy import.
    let db = Store::open(&data).unwrap();
    db.conn().execute("DELETE FROM meta WHERE k = 'migr_win_ts_v1'", []).unwrap();
    db.conn().execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop(db);
    let before = std::fs::read(data.join(DB_FILE)).unwrap();
    assert!(run().status.success());
    assert_eq!(std::fs::read(data.join(DB_FILE)).unwrap(), before);
    let db = Store::open(&data).unwrap();
    db.add_snapshot("dev", 10, None, None, &serde_json::json!([]), &serde_json::json!({}), &serde_json::json!({})).unwrap();
    db.set_meta("probeFailure:dev", &serde_json::json!({"at":20,"error":"ssh: timeout"})).unwrap();
    drop(db);
    let row: serde_json::Value = serde_json::from_slice(&run().stdout).unwrap();
    assert_eq!(row["rows"][0]["status"], "unobserved");
    assert!(row["rows"][0]["reason"].as_str().unwrap().contains("ssh: timeout"));
    assert_eq!(std::fs::read_to_string(cfg).unwrap(), r#"{"nodes":[{"id":"dev","os":"windows"}]}"#);
    std::fs::remove_dir_all(root).unwrap();
}
