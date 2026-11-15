//! Manual desktop smoke with populated synthetic saved state. This exercises the
//! real executable/reader/routes; it does not simulate clicks or an OS IME.
#![cfg(any(target_os = "macos", target_os = "windows"))]
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use tune_core::{
    db::Store,
    engine::{Engine, NoHost},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn saved_state(db: &Store) -> Value {
    let snapshots = ["example-active", "example-failed", "example-paused", "example-unobserved"].map(|id| {
        let rows: Vec<_> =
            db.last_snapshots(id, 20).unwrap().into_iter().map(|s| json!({"node_id":s.node_id,"at":s.at,"wall_s":s.wall_s,"data":s.data})).collect();
        json!({"node_id":id,"rows":rows,"history":db.history(id,20).unwrap(),"failure":db.get_meta(&format!("probeFailure:{id}")).unwrap()})
    });
    json!({
        "snapshots": snapshots,
        "logs": db.query_logs(&json!({})).unwrap(),
        "cursors": db.cursors().unwrap(),
        "checks": db.checks().unwrap(),
        "events": db.check_events(150).unwrap(),
        "actions": db.actions(100).unwrap(),
        "lastProbeAt": db.get_meta("lastProbeAt").unwrap(),
        "lastLogsAt": db.get_meta("lastLogsAt").unwrap()
    })
}

#[test]
#[ignore = "requires a macOS/Windows desktop GPU session; no remote collection"]
fn populated_saved_routes_do_not_collect_or_recompute() {
    let f = Fixture(std::env::temp_dir().join(format!("tune-gpui-saved-{}-{}", std::process::id(), tune_core::db::now_ms())));
    std::fs::create_dir_all(&f.0).unwrap();
    let config = f.0.join("nodes.json");
    let data = f.0.join("data");
    std::fs::write(
        &config,
        json!({"nodes":[
            {"id":"example-active","os":"windows","alias":"unused","network":false},
            {"id":"example-failed","os":"macos","alias":"unused","network":false},
            {"id":"example-paused","os":"macos","alias":"unused","status":"paused"},
            {"id":"example-unobserved","os":"windows","alias":"unused"}
        ]})
        .to_string(),
    )
    .unwrap();
    let engine = Engine::open(config.clone(), data.clone(), Arc::new(NoHost)).unwrap();
    let now = tune_core::db::now_ms();
    engine
        .with_db(|d| {
            d.add_snapshot(
                "example-active",
                now,
                Some(0.1),
                Some(90),
                &json!([]),
                &json!({
                    "probe":"windows", "host":{"cpu":"Example CPU","cores":8,"uptime_h":12},
                    "cpu_busy":12.5,"cpu_busy_availability":"partial","cpu_busy_samples":1,
                    "memory":{"total_gb":32,"available_pct":50},
                    "disk":[{"mount":"C:","free_pct":50,"free_gb":100,"total_gb":200}],
                    "gpus":[{"name":"Example GPU","power_w":40,"clocks_graphics_mhz":1000}],
                    "processes":{"top_cpu":[{"pid":42,"name":"Example process","cpu":2,"mem_mb":128}]},
                    "jobs":[{"name":"Example job","kind":"task","state":"ready","last_result":0}]
                }),
                &json!({}),
            )?;
            d.set_meta("probeFailure:example-failed", &json!({"at":now,"error":"synthetic unavailable"}))?;
            d.insert_logs(
                "example-active",
                "win_system",
                &tune_core::logs::normalize("win_system", &[json!({"uid":"synthetic-log","ts":now,"level":"error","message":"合成テストログ"})]),
                now,
            )?;
            d.add_action(&json!({"id":"synthetic-incomplete","node_id":"example-active","at":now,"type":"synthetic","label":"Synthetic incomplete action"}))?;
            Ok(())
        })
        .unwrap();
    let before = engine.with_db(|d| Ok(saved_state(d))).unwrap();
    drop(engine);
    let mut child = Command::new(env!("CARGO_BIN_EXE_katala-tune-gpui"))
        .args(["--selftest", "--logs", "--open", "logs", "--config"])
        .arg(&config)
        .arg("--data")
        .arg(&data)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(35);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("saved-route smoke timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "selftest logs rows=1 sigs=1\nselftest ok nodes=4 views=15");
    let db = Store::open_read_only(&data).unwrap();
    assert_eq!(saved_state(&db), before, "view loading changed saved observations or health state");
}
