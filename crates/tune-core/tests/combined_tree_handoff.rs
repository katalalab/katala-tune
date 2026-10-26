//! Parent combined-tree fixture: apply only with the #93 unique finding IDs.
//! Uses real rules/enrich/prepare with synthetic SQLite data and NoHost.
//! Never calls handoff_launch, a provider CLI, a clipboard, or a native dialog.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tune_core::db::now_ms;
use tune_core::engine::{Engine, NoHost};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn second_same_name_tree_uses_its_own_prompt_after_producer_ids_are_unique() {
    let path = std::env::temp_dir().join(format!("kt-combined-tree-handoff-{}-{}", std::process::id(), now_ms()));
    std::fs::create_dir(&path).unwrap();
    let dir = Directory(path);
    let config = dir.0.join("nodes.json");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-a","alias":"fixture-a","os":"windows"}],"protect":[]}"#).unwrap();
    // Declared after Directory so the DB closes before its directory is removed.
    let engine = Engine::open(config, dir.0.join("data"), Arc::new(NoHost)).unwrap();
    let snapshot = json!({
        "probe": "windows",
        "memory": {"total_gb": 1},
        "processes": {"apps": [], "apps_cpu": [], "top_cpu": [], "groups": [], "trees": [
            {"root": "fixture-worker.exe", "pid": 101, "owner": "user", "count": 3, "children": 2, "mem_mb": 256},
            {"root": "fixture-worker.exe", "pid": 202, "owner": "user", "count": 4, "children": 3, "mem_mb": 300}
        ]}
    });
    engine.with_db(|db| db.add_snapshot("fixture-a", now_ms(), None, None, &json!([]), &snapshot, &json!({}))).unwrap();
    let rows = engine.last().unwrap();
    let result = rows.iter().find(|row| row["node_id"] == "fixture-a").unwrap();
    let findings = result["findings"].as_array().unwrap();
    let by_pid = |pid: &str| {
        findings.iter().find(|finding| finding["detail"].as_str().is_some_and(|detail| detail.contains("fixture-worker.exe") && detail.contains(pid))).unwrap()
    };
    let first = by_pid("PID 101");
    let second = by_pid("PID 202");
    let first_id = first["id"].as_str().filter(|id| !id.is_empty()).unwrap();
    let second_id = second["id"].as_str().filter(|id| !id.is_empty()).unwrap();
    assert_ne!(first_id, second_id, "different roots must be separately selectable");
    let prepared = engine.handoff_prepare("fixture-a", second_id).unwrap();
    assert!(prepared.prompt.contains("PID 202"), "the selected second root must reach the prompt");
    assert!(!prepared.prompt.contains("PID 101"), "the first root must not replace the selected root");
    assert!(!dir.0.join("data/handoff").exists());
    let audit: Vec<Value> = engine.with_db(|db| db.actions(10)).unwrap();
    assert!(audit.is_empty());
}
