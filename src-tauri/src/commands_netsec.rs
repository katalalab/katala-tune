//! window.tune の「セキュリティ」（ネットワークとセキュリティ。docs/observability.md の 6）。
//! Tauri 版だけ（Electron 版の preload には無いので、画面は有無で判定する）。中身は tune-core の netsec（読み取りだけ）。

use std::sync::Arc;

use serde_json::{Value, json};
use tauri::State;
use tune_core::engine::Engine;

/// 機体ごとの待ち受け・防御・常駐の増減・ログイン・初めての接続先
#[tauri::command]
pub async fn netsec(e: State<'_, Arc<Engine>>, filter: Option<Value>) -> Result<Value, String> {
    e.netsec_view(&filter.unwrap_or_else(|| json!({})))
}
