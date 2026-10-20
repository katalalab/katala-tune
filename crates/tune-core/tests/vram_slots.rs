use serde_json::{Value, json};
use tune_core::{
    db::Store,
    live::{Msg, Sample, parse_line},
    monitor::{rollup, series},
};

#[test]
fn bounded_vram_slots_survive_parse_rollup_sqlite_reopen_and_series() {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!("../../../test/fixtures/vram-slots.json")).unwrap();
    let dir = std::env::temp_dir().join(format!("tune-vram-slots-{}-{}", std::process::id(), tune_core::db::now_ms()));
    let mut rows = Vec::new();
    for f in &fixtures {
        let name = f["name"].as_str().unwrap();
        let Some(Msg::Sample(raw)) = parse_line(&json!({"type":"s", "cpu":0, "gpu":f["gpus"]}).to_string()) else { panic!("{name}: parse") };
        let (s, _, _) = Sample::from_raw(*raw, 1000);
        let gpus = serde_json::to_value(&s.gpu).unwrap();
        for (i, g) in gpus.as_array().unwrap().iter().enumerate() {
            assert_eq!(g["storage_slot"], f["slots"][i], "{name}: slot {i}");
            assert_eq!(g["adapter_index"], f["gpus"][i]["adapter_index"], "{name}: physical index retained");
            assert_eq!(g["uuid"], f["gpus"][i]["uuid"], "{name}: identity retained");
            if !f["reasons"][i].is_null() {
                assert_eq!(g["memory_reason"], f["reasons"][i], "{name}: reason");
                assert_eq!(g["memory_status"], "unavailable");
                for key in ["mem_used_mb", "mem_total_mb", "mem_free_mb", "mem_used_pct"] {
                    assert!(g[key].is_null(), "{name}: invalid identity must not display {key}");
                }
            }
        }
        let p = s.point();
        let projected: serde_json::Map<String, Value> =
            p.as_object().unwrap().iter().filter(|(k, _)| k.starts_with("gpu_")).map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(json!(projected), f["metrics"], "{name}: live projection");
        assert_eq!(p["cpu"], 0.0, "GPU identity failure must retain CPU zero");
        rows.extend(rollup(name, &[s], None, 0, 60000, "fixture"));
    }
    let mixed: Vec<_> = ["sparse-ordered", "duplicate-index", "sparse-reordered"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let f = fixtures.iter().find(|f| f["name"] == *name).unwrap();
            let Some(Msg::Sample(raw)) = parse_line(&json!({"type":"s", "gpu":f["gpus"]}).to_string()) else { panic!("mixed parse") };
            let (mut s, _, _) = Sample::from_raw(*raw, 1000 + i as i64 * 1000);
            if i == 2 {
                let g = s.gpu.as_mut().unwrap().iter_mut().find(|g| g.adapter_index == Some(2)).unwrap();
                g.mem_used_mb = Some(35.0);
                g.mem_free_mb = Some(64.0);
            }
            s
        })
        .collect();
    rows.extend(rollup("mixed-reordered", &mixed, None, 0, 60000, "fixture"));
    {
        let db = Store::open(&dir).unwrap();
        db.add_metrics(&rows).unwrap();
    }
    let db = Store::open(&dir).unwrap();
    for f in &fixtures {
        let name = f["name"].as_str().unwrap();
        let read = db.metrics(Some(name), 0, 60000).unwrap();
        assert_eq!(read.len(), 1);
        let v = series(&read, 0, 60000, 60);
        for (k, expected) in f["metrics"].as_object().unwrap() {
            assert_eq!(read[0].v[k], json!([expected, expected]), "{name}: avg/max {k}");
            assert_eq!(v[k], json!([expected]), "{name}: series {k}");
            assert_eq!(v[format!("{k}_max")], json!([expected]), "{name}: series max {k}");
        }
        let keys: Vec<_> = v.as_object().unwrap().keys().filter(|k| k.starts_with("gpu_")).collect();
        assert_eq!(keys.len(), f["metrics"].as_object().unwrap().len() * 2, "{name}: missing stays absent");
    }
    let read = db.metrics(Some("mixed-reordered"), 0, 60000).unwrap();
    assert_eq!(read[0].n, 3);
    assert_eq!(read[0].v["gpu_2_vram_used_mib"], json!([30.0, 35.0]), "missing duplicate sample must not dilute the mean");
    assert_eq!(read[0].v["gpu_15_vram_used_mib"], json!([75.0, 75.0]), "array reordering must not mix GPU slots");
    let v = series(&read, 0, 60000, 60);
    assert_eq!(v["gpu_2_vram_pct"], json!([30.0]));
    assert_eq!(v["gpu_2_vram_pct_max"], json!([35.0]));
    assert_eq!(v["gpu_15_vram_pct"], json!([75.0]));
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
