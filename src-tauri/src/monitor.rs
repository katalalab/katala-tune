//! 常時監視の殻。中身は tune-core の `monitor`（集計・ハブとの同期・ダッシュボード）。
//!
//! 繰り返し（tune_core::monitor::run）はアプリが開いているあいだ動き、画面が見ていなくても・ウィンドウを閉じて
//! メニューバー・通知領域に居るあいだも全機体の数字を集める。集計・写しのたびに `monitor` イベントを送る。

use std::sync::Arc;

use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, State};
use tune_core::db::now_ms;
use tune_core::engine::Engine;
use tune_core::live::Live;
use tune_core::monitor;

pub fn start(app: AppHandle, engine: Arc<Engine>, live: Arc<Live>) {
    tauri::async_runtime::spawn(monitor::run(
        engine,
        live,
        move |ev| {
            let _ = app.emit("monitor", ev);
        },
        None,
    ));
}

/// 常時監視でライブを保っているか（ウィンドウを閉じても・画面が止めても流し続ける）
pub fn collecting(engine: &Engine) -> bool {
    engine.with_db(|d| Ok(monitor::should_collect(d, now_ms()))).unwrap_or(false)
}

#[tauri::command]
pub async fn dashboard(e: State<'_, Arc<Engine>>, l: State<'_, Arc<Live>>, minutes: Option<i64>) -> Result<Value, String> {
    monitor::dashboard(&e, Some(&l), minutes.unwrap_or(60))
}

/// 常時監視とハブの設定を見る・変える（patch が無ければ見るだけ）
#[tauri::command]
pub async fn monitor_settings(e: State<'_, Arc<Engine>>, patch: Option<Value>) -> Result<Value, String> {
    let cfg = e.reload_config();
    let s = match patch {
        Some(p) => e.with_db(|d| Ok(monitor::set_settings(d, &cfg, &p)))??,
        None => e.with_db(|d| Ok(monitor::settings(d)))?,
    };
    Ok(json!({ "monitor": s.json(), "hub_pull": e.with_db(|d| d.get_meta("hubPull"))? }))
}

#[tauri::command]
pub async fn hub_pull(e: State<'_, Arc<Engine>>) -> Result<Value, String> {
    monitor::pull(&e).await
}
