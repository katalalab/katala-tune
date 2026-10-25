//! Numeric retention parity and shared Store integration fixtures.
use tune_core::db::retention;

use rusqlite::{Connection, params};
use serde_json::{Value, json};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE metrics_minute(node_id TEXT,minute INTEGER,n INTEGER,v TEXT,src TEXT,PRIMARY KEY(node_id,minute)) WITHOUT ROWID; CREATE INDEX metrics_minute_by_time ON metrics_minute(minute);").unwrap();
    retention::initialize(&db).unwrap();
    db
}
fn put(db: &Connection, at: i64, value: Value) {
    db.execute("INSERT OR REPLACE INTO metrics_minute VALUES ('fixture',?,60,?,NULL)", params![at, value.to_string()]).unwrap();
}

#[test]
fn archive_is_atomic_replay_safe_and_weighted_by_valid_minutes() {
    let db = fixture();
    for (at, value) in [(10 * DAY, 0), (10 * DAY + 60_000, 2), (10 * DAY + 120_000, 4), (10 * DAY + HOUR, 6)] {
        put(&db, at, json!({"cpu":[value,value]}));
    }
    let result = retention::compact(&db, 16 * DAY, 30 * DAY, retention::Budget::default()).unwrap();
    assert_eq!(result["deleted"], 4);
    let hour = retention::query(&db, "fixture", 0, 700 * DAY, "hour").unwrap();
    assert_eq!(hour[0]["v"]["cpu"], json!({"mean":2,"max":4,"minutes":3}));
    retention::compact(&db, 16 * DAY, 30 * DAY, retention::Budget::default()).unwrap();
    assert_eq!(retention::query(&db, "fixture", 0, 700 * DAY, "hour").unwrap(), hour);
    put(&db, 10 * DAY, json!({"cpu":[999,999]}));
    retention::compact(&db, 16 * DAY, 30 * DAY, retention::Budget::default()).unwrap();
    assert_eq!(retention::status(&db).unwrap()["late_rows"], 1);
    retention::compact(&db, 100 * DAY, 130 * DAY, retention::Budget::default()).unwrap();
    let day = retention::query(&db, "fixture", 0, 700 * DAY, "day").unwrap();
    assert_eq!(day[0]["v"]["cpu"], json!({"mean":3,"max":6,"minutes":4}));
}

#[test]
fn failed_archive_preserves_raw_data_and_watermark() {
    let db = fixture();
    put(&db, 10 * DAY, json!({"cpu":[3,3]}));
    db.execute_batch("CREATE TRIGGER fail_archive BEFORE INSERT ON metrics_history BEGIN SELECT RAISE(ABORT,'fixture archive failure'); END;").unwrap();
    assert!(retention::compact(&db, 16 * DAY, 30 * DAY, retention::Budget::default()).is_err());
    assert_eq!(db.query_row("SELECT count(*) FROM metrics_minute", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(retention::status(&db).unwrap()["sealed_before"], 0);
    assert_eq!(retention::status(&db).unwrap()["rows"], 0);
}

#[test]
fn invalid_large_sample_count_is_omitted_without_blocking_valid_rows() {
    let db = fixture();
    put(&db, 10 * DAY, json!({"cpu":[4,4]}));
    db.execute("INSERT INTO metrics_minute VALUES ('fixture',?,9007199254740992,'{\"cpu\":[9,9]}',NULL)", [10 * DAY + 60_000]).unwrap();
    let result = retention::compact(&db, 16 * DAY, 30 * DAY, retention::Budget::default()).unwrap();
    assert_eq!(result["omitted_rows"], 1);
    assert_eq!(retention::query(&db, "fixture", 0, 700 * DAY, "hour").unwrap()[0]["v"]["cpu"]["mean"], 4);
}

#[test]
fn hour_cutoff_becomes_a_partial_daily_bucket_before_deletion() {
    let db = fixture();
    put(&db, 110 * DAY + 11 * HOUR, json!({"cpu":[2,2]}));
    put(&db, 110 * DAY + 12 * HOUR, json!({"cpu":[4,4]}));
    retention::compact(&db, 180 * DAY, 200 * DAY + 12 * HOUR + 34 * 60_000, retention::Budget::default()).unwrap();
    assert_eq!(retention::query(&db, "fixture", 0, 700 * DAY, "day").unwrap()[0]["v"]["cpu"]["mean"], 2);
    assert_eq!(retention::query(&db, "fixture", 0, 700 * DAY, "hour").unwrap()[0]["v"]["cpu"]["mean"], 4);
}

#[test]
fn javascript_and_rust_share_numeric_bytes_capacity_and_replay_contract() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let input: Value = serde_json::from_str(include_str!("fixtures/metrics-retention.json")).unwrap();
    let driver = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js/metrics_retention.js");
    let mut child = Command::new("node").arg(driver).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let want: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut results = Vec::new();
    for scenario in input.as_array().unwrap() {
        let db = fixture();
        let mut got = Vec::new();
        for op in scenario.as_array().unwrap() {
            match op["type"].as_str().unwrap() {
                "put" => {
                    let n = op["n"].as_i64().unwrap_or_else(|| op["n"].as_str().unwrap().parse().unwrap());
                    let payload = if let Some(bytes) = op["v_blob"].as_array() {
                        rusqlite::types::Value::Blob(bytes.iter().map(|b| b.as_u64().unwrap() as u8).collect())
                    } else {
                        rusqlite::types::Value::Text(op["v"].to_string())
                    };
                    db.execute(
                        "INSERT OR REPLACE INTO metrics_minute VALUES (?,?,?,?,NULL)",
                        params![op["node"].as_str().unwrap(), op["at"].as_i64().unwrap(), n, payload],
                    )
                    .unwrap();
                }
                "compact" => {
                    let budget =
                        retention::Budget { max_rows: op["maxRows"].as_i64().unwrap_or(50_000), max_bytes: op["maxBytes"].as_i64().unwrap_or(16_777_216) };
                    got.push(retention::compact(&db, op["before"].as_i64().unwrap(), op["now"].as_i64().unwrap(), budget).unwrap());
                }
                "query" => got.push(retention::query(&db, op["node"].as_str().unwrap(), 0, 700 * DAY, op["resolution"].as_str().unwrap()).unwrap()),
                _ => panic!("unknown fixture operation"),
            }
        }
        results.push(Value::Array(got));
    }
    assert_eq!(Value::Array(results), want);
}

#[test]
fn malformed_blob_payload_is_omitted_without_blocking_valid_text() {
    let db = fixture();
    put(&db, 10 * DAY, json!({"cpu":[4,4]}));
    db.execute("INSERT INTO metrics_minute VALUES ('fixture',?,60,?,NULL)", params![10 * DAY + 60_000, br#"{"cpu":[9,9]}"#.to_vec()]).unwrap();
    let result = retention::compact(&db, 16 * DAY, 30 * DAY, retention::Budget::default()).unwrap();
    assert_eq!(result["omitted_rows"], 1);
    assert_eq!(retention::query(&db, "fixture", 0, 700 * DAY, "hour").unwrap()[0]["v"]["cpu"]["mean"], 4);
}

#[test]
fn store_registration_and_prune_preserve_the_additive_action_target() {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("tune-retention-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    {
        let db = Connection::open(dir.join("katala-tune.db")).unwrap();
        // Existing issue19 additive schema, before retention tables exist.
        db.execute_batch("CREATE TABLE tune_actions(id TEXT PRIMARY KEY,at INTEGER NOT NULL,node_id TEXT NOT NULL,type TEXT NOT NULL,params TEXT,label TEXT,ok INTEGER,output TEXT,undo TEXT,undo_of TEXT,target TEXT); INSERT INTO tune_actions(id,at,node_id,type,target) VALUES ('fixture-audit',0,'fixture','fixture','{\"alias\":\"example\",\"os\":\"windows\",\"local\":false}');").unwrap();
    }
    {
        let store = tune_core::db::Store::open(&dir).unwrap();
        let db = store.conn();
        let now = tune_core::db::now_ms();
        db.execute("INSERT INTO metrics_minute(node_id,minute,n,v) VALUES ('fixture',?,60,'{\"cpu\":[4,4]}')", [((now - 15 * DAY) / HOUR) * HOUR]).unwrap();
        let before = now - 14 * DAY;
        assert_eq!(store.prune_metrics(before).unwrap(), 1);
        assert_eq!(store.metrics_retention().unwrap()["rows"], 1);
        assert_eq!(store.metrics_history("fixture", 0, now, "hour").unwrap()[0]["v"]["cpu"]["mean"], 4);
        let target: String = db.query_row("SELECT target FROM tune_actions WHERE id='fixture-audit'", [], |r| r.get(0)).unwrap();
        assert_eq!(target, r#"{"alias":"example","os":"windows","local":false}"#);
    }
    {
        let store = tune_core::db::Store::open(&dir).unwrap();
        assert_eq!(store.metrics_retention().unwrap()["rows"], 1);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn maintenance_metadata_has_the_same_meaning_in_both_database_readers() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("tune-retention-meta-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let failure = json!({"at":10,"ok":false,"category":"db_busy","next_attempt_at":300010,"last_success_at":null});
    let driver = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js/metrics_retention.js");
    let mut child = Command::new("node").arg(driver).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(json!({"store_dir":dir,"maintenance":failure}).to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let expected: Value = serde_json::from_slice(&output.stdout).unwrap();
    {
        let store = tune_core::db::Store::open(&dir).unwrap();
        assert_eq!(store.metrics_retention().unwrap(), expected);
        assert_eq!(expected["maintenance"], failure);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn clock_rollback_does_not_delete_new_recent_raw_observations() {
    let db = fixture();
    put(&db, 10 * DAY, json!({"cpu":[4,4]}));
    retention::compact(&db, 26 * DAY, 40 * DAY, retention::Budget::default()).unwrap();
    put(&db, 20 * DAY, json!({"cpu":[9,9]}));
    retention::compact(&db, 6 * DAY, 20 * DAY, retention::Budget::default()).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM metrics_minute", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(retention::status(&db).unwrap()["late_rows"], 0);
    assert_eq!(retention::status(&db).unwrap()["sealed_before"], 26 * DAY);
    retention::compact(&db, 26 * DAY, 40 * DAY, retention::Budget::default()).unwrap();
    assert_eq!(retention::status(&db).unwrap()["late_rows"], 1);
    assert_eq!(retention::query(&db, "fixture", 0, 700 * DAY, "hour").unwrap()[0]["v"]["cpu"]["mean"], 4);
}
