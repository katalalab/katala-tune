//! Collection must not use the last-good ledger after a failed reread.
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tune_core::engine::{Engine, NoHost};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn rejects_broken_ledger(kind: &str) {
    let f = Fixture(std::env::temp_dir().join(format!("tune-manual-{kind}-{}-{}", std::process::id(), tune_core::db::now_ms())));
    std::fs::create_dir_all(&f.0).unwrap();
    let config = f.0.join("nodes.json");
    // Even the buggy implementation cannot contact a real machine with this fixture.
    std::fs::write(&config, json!({"nodes":[{"id":"paused-example","os":"macos","status":"paused","ai_sessions":false}]}).to_string()).unwrap();
    let engine = Engine::open(config.clone(), f.0.join("data"), Arc::new(NoHost)).unwrap();
    std::fs::write(config, "{broken synthetic ledger").unwrap();
    let (out, marker, running) = match kind {
        "probe" => (engine.run_probe(None, false).await, "lastProbeAt", engine.is_probing()),
        "inventory" => (engine.run_inventory(None).await, "lastInventoryAt", engine.is_inventorying()),
        "ai" => (engine.ai_sync(None).await, "lastAiAt", engine.is_ai_syncing()),
        _ => unreachable!(),
    };
    assert!(out["error"].is_string(), "invalid ledger must refuse {kind}: {out}");
    assert_eq!(out.get("results"), None);
    assert!(!running, "refusal must release the busy state");
    assert_eq!(engine.with_db(|d| d.get_meta(marker)).unwrap(), None::<Value>);
}

#[tokio::test]
async fn probe_refuses_last_good_config_after_reread_failure() {
    rejects_broken_ledger("probe").await;
}
#[tokio::test]
async fn inventory_refuses_last_good_config_after_reread_failure() {
    rejects_broken_ledger("inventory").await;
}
#[tokio::test]
async fn ai_refuses_last_good_config_after_reread_failure() {
    rejects_broken_ledger("ai").await;
}
