//! ライブ表示の殻。中身は tune-core の `live`（経路・解析・保持・管理）で、ここは
//! window.tune.liveStart / liveStop のコマンドと、画面へのイベント（`live`）と、終了・ウィンドウを閉じたときの停止だけ。
//!
//! 画面は見ているあいだ liveStart(ids) を呼び直して合図（heartbeat）を送る。合図が 2 分途切れると Rust 側で止める。

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tune_core::engine::Engine;
use tune_core::live::{Live, Opts, ProcessRoute, Stop};
use tune_core::nodes::Node;

/// 管理を作り、画面へまとめて送る tick を動かし始める（setup から 1 回）
pub fn create(app: &AppHandle) -> Arc<Live> {
    let h = app.clone();
    let live = Live::new(
        Arc::new(ProcessRoute),
        Opts::default(),
        Box::new(move |ev: Value| {
            let _ = h.emit("live", ev);
        }),
    );
    tauri::async_runtime::spawn(live.clone().run_ticker());
    live
}

/// 流し始める（流れていれば合図として扱う）。台帳を読み直し、台帳に無い id は理由つきで断る
#[tauri::command]
pub async fn live_start(e: State<'_, Arc<Engine>>, l: State<'_, Arc<Live>>, ids: Vec<String>) -> Result<Value, String> {
    let cfg = e.reload_config();
    let (mut nodes, mut unknown): (Vec<Node>, Vec<String>) = (Vec::new(), Vec::new());
    for id in ids {
        match cfg.node(&id) {
            Some(n) => nodes.push(n.clone()),
            None => unknown.push(id),
        }
    }
    Ok(l.start(&nodes, &unknown))
}

/// 止める（ids が無ければ全部）
#[tauri::command]
pub async fn live_stop(l: State<'_, Arc<Live>>, ids: Option<Vec<String>>) -> Result<Value, String> {
    Ok(l.stop(ids.as_deref(), Stop::User))
}

/// ウィンドウを閉じた: 見ている画面が無いので全部止める
pub fn stop_all(app: &AppHandle) {
    if let Some(l) = app.try_state::<Arc<Live>>() {
        l.stop(None, Stop::Hidden);
    }
}

/// アプリの終了: 全部止め、サンプラー（子プロセス）が終わるのを少しだけ待つ
pub fn shutdown(app: &AppHandle) {
    if let Some(l) = app.try_state::<Arc<Live>>() {
        let l = l.inner().clone();
        tauri::async_runtime::block_on(async move { l.shutdown(Duration::from_secs(3)).await });
    }
}
