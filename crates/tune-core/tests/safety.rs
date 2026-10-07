// 変更操作の安全: 確認ダイアログを開いているあいだに台帳の接続先が変わったら、承認した操作でも実行しない
use std::sync::Arc;

use serde_json::json;
use tune_core::engine::{Confirm, Engine, NoHost};

fn ledger(alias: &str) -> String {
    json!({ "protect": [], "nodes": [{ "id": "box", "alias": alias, "os": "macos" }] }).to_string()
}

#[tokio::test]
async fn refuses_when_route_changes_during_confirmation() {
    // 依存を増やさないため tempfile は使わない
    let dir = std::env::temp_dir().join(format!(
        "kt-safety-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("nodes.json");
    std::fs::write(&cfg, ledger("box-a")).unwrap();
    let engine = Engine::open(cfg.clone(), dir.join("data"), Arc::new(NoHost)).unwrap();
    let action = json!({ "type": "launchd-kickstart", "params": { "label": "com.example.job" } });
    let cfg2 = cfg.clone();
    // 操作者が承認する直前に、別の手で台帳の alias が書き換わった
    let confirm: Confirm = Box::new(move |_t, _b| {
        Box::pin(async move {
            std::fs::write(&cfg2, ledger("box-b")).unwrap();
            true
        })
    });
    let r = engine.confirm_and_run("box", &action, "テスト", None, confirm).await.unwrap();
    assert_eq!(r["ok"], json!(false));
    assert!(r["refused"].as_str().unwrap_or("").contains("接続先"), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}
