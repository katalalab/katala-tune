//! Owned synthetic DBs and a finite, owned Node fixture child only; no host probes.
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tune_core::{db::Store, hourly_baseline::analyze_hours};
const H: i64 = 3_600_000;
const D: i64 = 24 * H;
const M: i64 = 60_000;
fn input() -> Value {
    serde_json::from_str(include_str!("fixtures/hourly-baseline.json")).unwrap()
}
fn num(v: &Value, k: &str, default: i64) -> i64 {
    v[k].as_i64().unwrap_or(default)
}
fn points(s: &Value, end: i64) -> Value {
    let days = num(s, "days", 28);
    let mut xs = Vec::new();
    let mut qi = 0;
    for i in 0..days * 24 {
        let at = end - days * D + i * H;
        let weekday = (at / D + 4) % 7;
        let hour = at / H % 24;
        let mut mean = s.get("base").cloned().unwrap_or(json!(20));
        if s["pattern"] == "weekly" {
            mean = json!(
                (if weekday == 0 || weekday == 6 {
                    10
                } else if (8..16).contains(&hour) {
                    80
                } else {
                    20
                }) + hour % 4
            );
        }
        if s["quantiles"] == true && at < end - D && weekday == 1 && (8..12).contains(&hour) {
            mean = json!(qi);
            qi += 1;
        }
        if at >= end - D && s["recent"].is_array() {
            mean = s["recent"][((at - end + D) / H) as usize].clone();
        }
        let idx = (at - end + D) / H;
        if at >= end - D && s["drop_recent"].as_array().is_some_and(|a| a.contains(&json!(idx))) {
            continue;
        }
        xs.push(
            json!({"at":at,"mean":mean,"minutes":if at>=end-D {num(s,"recent_minutes",60)}else{60},"resolution":s["resolution"].as_str().unwrap_or("hour")}),
        );
    }
    if s["duplicate"] == true {
        xs.push(xs.last().unwrap().clone());
    }
    if s["conflict"] == true {
        let mut p = xs.last().unwrap().clone();
        p["mean"] = json!(99);
        xs.push(p);
    }
    if s["future"] == true {
        for i in 0..2 {
            xs.push(json!({"at":end+i*H,"mean":100,"minutes":60,"resolution":"hour"}));
        }
    }
    if let Some(n) = s["repeat"].as_u64() {
        xs = vec![xs[0].clone(); n as usize];
    }
    if s["reverse"] == true {
        xs.reverse();
    }
    Value::Array(xs)
}
fn populate(store: &Store, s: &Value) {
    let db = store.conn();
    db.execute_batch("BEGIN").unwrap();
    for o in s["operations"].as_array().unwrap() {
        match o["type"].as_str().unwrap() {
            "state" => {
                db.execute("UPDATE metrics_retention_state SET sealed_before=?,last_now=?", params![o["seal"].as_f64().unwrap(), o["last"].as_f64().unwrap()])
                    .unwrap();
            }
            "missing_state" => {
                db.execute("DELETE FROM metrics_retention_state", []).unwrap();
            }
            "hour" => {
                db.execute(
                    "INSERT INTO metrics_history VALUES (?,?,?,?)",
                    params![
                        o["node"].as_str().unwrap_or("fixture"),
                        o["resolution"].as_str().unwrap_or("hour"),
                        o["at"].as_f64().unwrap(),
                        json!({"mem":o["value"]}).to_string()
                    ],
                )
                .unwrap();
            }
            "archive_payload" => {
                db.execute("INSERT INTO metrics_history VALUES ('fixture','hour',?,?)", params![num(o, "at", 0), o["text"].as_str().unwrap()]).unwrap();
            }
            "payload" => {
                for i in 0..num(o, "count", 0) {
                    db.execute(
                        "INSERT INTO metrics_minute(node_id,minute,n,v) VALUES ('fixture',?,60,?)",
                        params![num(o, "at", 0) + i * M, o["text"].as_str().unwrap()],
                    )
                    .unwrap();
                }
            }
            "grid" => {
                for h in 0..num(o, "hours", 0) {
                    let at = num(o, "at", 0) + h * H;
                    let v = o["value"].as_f64().unwrap();
                    if o["archived"] == true {
                        db.execute("INSERT INTO metrics_history VALUES ('fixture','hour',?,?)", params![at, json!({"mem":[v*30.0,30,v]}).to_string()]).unwrap();
                    } else {
                        for m in 0..30 {
                            db.execute(
                                "INSERT INTO metrics_minute(node_id,minute,n,v) VALUES ('fixture',?,60,?)",
                                params![at + m * M, json!({"mem":[v,v]}).to_string()],
                            )
                            .unwrap();
                        }
                    }
                }
            }
            "minutes" => {
                let mut put = db.prepare("INSERT OR REPLACE INTO metrics_minute(node_id,minute,n,v) VALUES (?,?,?,?)").unwrap();
                for i in 0..num(o, "count", 0) {
                    let text = json!({"mem":o["value"]}).to_string();
                    let payload = if o["blob"] == true { rusqlite::types::Value::Blob(text.into_bytes()) } else { rusqlite::types::Value::Text(text) };
                    let n = o["n"].as_str().map(|s| s.parse().unwrap()).unwrap_or_else(|| num(o, "n", 60));
                    put.execute(params![o["node"].as_str().unwrap_or("fixture"), o["at"].as_f64().unwrap() + (i * num(o, "step", M)) as f64, n, payload])
                        .unwrap();
                }
            }
            _ => panic!("unknown fixture operation"),
        }
    }
    db.execute_batch("COMMIT; PRAGMA query_only=ON").unwrap();
}
fn expected(r: &Value, s: &Value) {
    let e = &s["expect"];
    let name = s["name"].as_str().unwrap();
    assert_eq!(r["state"], e["state"], "{name}");
    if let Some(reason) = e["reason"].as_str() {
        assert!(r["reasons"].as_array().unwrap().contains(&json!(reason)), "{name}");
    }
    for k in ["history_hours", "matched_hours", "recent_hours", "conflicting_hours", "duplicate_points", "ignored_points", "accepted_hours"] {
        if let Some(want) = e.get(k) {
            assert_eq!(&r["coverage"][k], want, "{name}:{k}");
        }
    }
    for k in ["raw_rows", "history_rows", "omitted_raw_rows"] {
        if let Some(want) = e.get(k) {
            assert_eq!(&r["source"][k], want, "{name}:{k}");
        }
    }
    for (k, path) in [("slope", "slope_pp_per_hour"), ("change", "change_pp"), ("trend_assessed", "assessed")] {
        if let Some(want) = e.get(k) {
            assert_eq!(&r["evidence"]["trend"][path], want, "{name}:{k}");
        }
    }
    if let Some(want) = e.get("elevated_hours") {
        assert_eq!(&r["evidence"]["elevated_hours"], want, "{name}");
    }
    if let Some(p) = e["profile"].as_array() {
        let found = r["reference"]["profiles"].as_array().unwrap().iter().find(|x| x["weekday"] == p[0] && x["slot"] == p[1]).unwrap();
        for (i, k) in ["p10", "median", "p90"].iter().enumerate() {
            assert_eq!(found[k], p[i + 2], "{name}:{k}");
        }
    }
    if r["state"] == "insufficient" {
        assert_eq!(r["observations"], json!([]), "{name}");
    }
}
static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        Self::create_at(&std::env::temp_dir(), n, &SCRATCH_SEQUENCE).expect("create owned baseline scratch")
    }
    fn create_at(parent: &Path, stamp: u128, sequence: &AtomicU64) -> std::io::Result<Self> {
        for _ in 0..32 {
            let serial = sequence.fetch_add(1, Ordering::Relaxed);
            let p = parent.join(format!("baseline-fixture-{}-{stamp}-{serial}", std::process::id()));
            match std::fs::create_dir(&p) {
                Ok(()) => return Ok(Self(p)), // Ownership begins only after exclusive creation.
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "baseline scratch candidates exhausted"))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("retaining owned baseline scratch after failure: {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
fn node_reports(input: &Value) -> Value {
    let dir = Scratch::new();
    let file = dir.0.join("input.json");
    std::fs::write(&file, input.to_string()).unwrap();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/js/hourly_baseline.js");
    let args = [script.into_os_string(), file.into_os_string()];
    let limits = owned_node::Limits { execution: Duration::from_secs(15), cleanup: Duration::from_secs(2), stdout: 4_194_304, stderr: 65_536 };
    let out = owned_node::capture_node(&args, limits)
        .unwrap_or_else(|failure| panic!("Node fixture failed: {failure:?}; cleanup resolved={}", failure.cleanup_resolved()));
    std::fs::write(dir.0.join("stdout.json"), &out.stdout).unwrap();
    std::fs::write(dir.0.join("stderr.txt"), &out.stderr).unwrap();
    serde_json::from_slice(&out.stdout).expect("complete fixture JSON required; owned scratch retained on failure")
}
#[test]
fn pure_shared_fixtures_match_literal_contract_and_javascript() {
    let x = input();
    let end = x["end"].as_i64().unwrap();
    let want = node_reports(&x);
    for (i, s) in x["pure"].as_array().unwrap().iter().enumerate() {
        let got = analyze_hours(&points(s, end), s["metric"].as_str().unwrap_or("mem"), end + H / 2).unwrap();
        expected(&got, s);
        assert_eq!(got, want["pure"][i], "{}", s["name"]);
    }
}
#[test]
fn saved_shared_fixtures_match_literal_contract_and_javascript_without_writes() {
    let x = input();
    let end = x["end"].as_i64().unwrap();
    let want = node_reports(&x);
    for (i, s) in x["saved"].as_array().unwrap().iter().enumerate() {
        let store = Store::open_in_memory().unwrap();
        populate(&store, s);
        let before = store.conn().total_changes();
        let got = store.metrics_baseline("fixture", "mem", end + H / 2).unwrap();
        expected(&got, s);
        assert_eq!(got, want["saved"][i], "{}", s["name"]);
        assert_eq!(before, store.conn().total_changes());
    }
}

#[test]
fn nested_snapshot_keeps_old_data_across_a_concurrent_wal_compaction() {
    let dir = Scratch::new();
    let store = Store::open(&dir.0).unwrap();
    let db = store.conn();
    let x = input();
    let end = x["end"].as_i64().unwrap();
    for m in 0..30 {
        db.execute("INSERT INTO metrics_minute(node_id,minute,n,v) VALUES ('fixture',?,60,'{\"mem\":[20,20]}')", [end - 15 * D + m * M]).unwrap();
    }
    db.execute_batch("BEGIN").unwrap();
    assert_eq!(db.query_row("SELECT sealed_before FROM metrics_retention_state", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    let writer = rusqlite::Connection::open(dir.0.join("katala-tune.db")).unwrap();
    tune_core::db::retention::compact(&writer, end - 14 * D, end + H / 2, tune_core::db::retention::Budget::default()).unwrap();
    let old = store.metrics_baseline("fixture", "mem", end + H / 2).unwrap();
    assert_eq!(old["source"]["sealed_before"], 0);
    assert_eq!(old["source"]["raw_rows"], 30);
    assert_eq!(old["source"]["history_rows"], 0);
    assert_eq!(old["coverage"]["accepted_hours"], 1);
    db.execute_batch("ROLLBACK").unwrap();
    let next = store.metrics_baseline("fixture", "mem", end + H / 2).unwrap();
    assert_eq!(next["source"]["sealed_before"], end - 14 * D);
    assert_eq!(next["source"]["raw_rows"], 0);
    assert_eq!(next["source"]["history_rows"], 1);
    assert_eq!(next["coverage"]["accepted_hours"], 1);
}
#[test]
fn old_read_only_reopen_and_api_errors_never_initialize_or_commit_a_database() {
    let dir = Scratch::new();
    let file = dir.0.join("katala-tune.db");
    rusqlite::Connection::open(&file).unwrap().execute_batch("CREATE TABLE old_marker(x)").unwrap();
    let bytes = std::fs::read(&file).unwrap();
    {
        let store = Store::open_read_only(&dir.0).unwrap();
        assert_eq!(store.metrics_baseline("fixture", "mem", 0).unwrap()["reasons"], json!(["storage_unavailable"]));
    }
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let store = Store::open_in_memory().unwrap();
    let db = store.conn();
    assert!(store.metrics_baseline("", "mem", 0).is_err());
    assert!(store.metrics_baseline("fixture", "unknown", 0).is_err());
    assert!(store.metrics_baseline("fixture", "mem", -1).is_err());
    assert!(analyze_hours(&json!({}), "mem", 0).is_err());
    assert!(analyze_hours(&json!([]), "mem", 8_640_000_000_000_001).is_err());
    db.execute_batch("BEGIN; INSERT INTO metrics_minute(node_id,minute,n,v) VALUES ('fixture',0,60,'{}')").unwrap();
    store.metrics_baseline("fixture", "mem", H).unwrap();
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM metrics_minute", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    db.execute_batch("DROP TABLE metrics_minute; CREATE TABLE metrics_minute(unrelated TEXT)").unwrap();
    assert!(store.metrics_baseline("fixture", "mem", H).is_err());
    db.execute_batch("BEGIN; ROLLBACK").unwrap();
}
#[test]
fn busy_read_errors_propagate_without_a_fabricated_baseline_and_can_retry() {
    let dir = Scratch::new();
    {
        let store = Store::open(&dir.0).unwrap();
        store.conn().execute_batch("PRAGMA journal_mode=DELETE").unwrap();
    }
    let store = Store::open_read_only(&dir.0).unwrap();
    store.conn().busy_timeout(Duration::ZERO).unwrap();
    let writer = rusqlite::Connection::open(dir.0.join("katala-tune.db")).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let err = store.metrics_baseline("fixture", "mem", H).unwrap_err();
    assert_eq!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(store.metrics_baseline("fixture", "mem", H).unwrap()["source"]["state"], "ready");
}

#[path = "support/owned_node.rs"]
mod owned_node;

#[test]
fn scratch_retries_a_controlled_collision_without_touching_an_existing_marker() {
    let parent = Scratch::new();
    let first = Scratch::create_at(&parent.0, 7, &AtomicU64::new(0)).unwrap();
    std::fs::write(first.0.join("marker"), b"existing owner").unwrap();
    let sequence = AtomicU64::new(0);
    let result = Scratch::create_at(&parent.0, 7, &sequence);
    assert!(result.is_ok(), "AlreadyExists must select another candidate");
    let second = result.unwrap();
    assert_ne!(first.0, second.0);
    drop(second);
    assert_eq!(std::fs::read(first.0.join("marker")).unwrap(), b"existing owner");
    assert_eq!(sequence.load(Ordering::Relaxed), 2);
}

#[test]
fn scratch_fixed_clock_parallel_constructors_have_distinct_owned_directories() {
    let parent = Scratch::new();
    let sequence = AtomicU64::new(0);
    let guards = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16).map(|_| scope.spawn(|| Scratch::create_at(&parent.0, 7, &sequence).unwrap())).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
    });
    let paths: std::collections::BTreeSet<_> = guards.iter().map(|g| g.0.clone()).collect();
    assert_eq!(paths.len(), 16);
    assert!(paths.iter().all(|p| p.is_dir()));
    assert_eq!(sequence.load(Ordering::Relaxed), 16);
    drop(guards);
    assert!(paths.iter().all(|p| !p.exists()));
}

#[test]
fn scratch_collision_exhaustion_is_finite_and_preserves_every_existing_owner() {
    let parent = Scratch::new();
    let seed = AtomicU64::new(0);
    let guards: Vec<_> = (0..32).map(|_| Scratch::create_at(&parent.0, 7, &seed).unwrap()).collect();
    for g in &guards {
        std::fs::write(g.0.join("marker"), b"owned").unwrap();
    }
    let sequence = AtomicU64::new(0);
    let err = match Scratch::create_at(&parent.0, 7, &sequence) {
        Ok(_) => panic!("exhausted collisions must fail"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(sequence.load(Ordering::Relaxed), 32);
    assert!(guards.iter().all(|g| std::fs::read(g.0.join("marker")).unwrap() == b"owned"));
}

#[test]
fn scratch_noncollision_error_does_not_retry_or_change_the_existing_file() {
    let parent = Scratch::new();
    let file = parent.0.join("file");
    std::fs::write(&file, b"unchanged").unwrap();
    let sequence = AtomicU64::new(0);
    let err = match Scratch::create_at(&file, 7, &sequence) {
        Ok(_) => panic!("file parent cannot own a directory"),
        Err(err) => err,
    };
    assert_ne!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(sequence.load(Ordering::Relaxed), 1);
    assert_eq!(std::fs::read(&file).unwrap(), b"unchanged");
}
