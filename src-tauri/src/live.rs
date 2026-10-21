//! ライブ表示の殻。中身は tune-core の `live`（経路・解析・保持・管理）で、ここは
//! window.tune.liveStart / liveStop のコマンドと、画面へのイベント（`live`）と、終了・ウィンドウを閉じたときの停止だけ。
//!
//! 画面は見ているあいだ liveStart(ids) を呼び直して合図（heartbeat）を送る。合図が 2 分途切れると Rust 側で止める。

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tune_core::engine::Engine;
use tune_core::live::{Live, ProcessRoute, Stop};
use tune_core::nodes::Node;

/// 管理を作り、画面へまとめて送る tick を動かし始める（setup から 1 回）。同時に流せる数は台帳の台数まで（常時監視）
pub fn create(app: &AppHandle, nodes: usize) -> Arc<Live> {
    let h = app.clone();
    let live = Live::new(
        Arc::new(ProcessRoute),
        tune_core::monitor::live_opts(nodes),
        Box::new(move |ev: Value| {
            let _ = h.emit("live", ev);
        }),
    );
    tauri::async_runtime::spawn(live.clone().run_ticker());
    live
}

/// ウィンドウを人が見られる状態か（ある・表示中・最小化でない）
fn window_shown(exists: bool, visible: bool, minimized: bool) -> bool {
    exists && visible && !minimized
}

fn window_state_shown<E>(visible: Option<Result<bool, E>>, minimized: Option<Result<bool, E>>) -> bool {
    match (visible, minimized) {
        (Some(Ok(visible)), Some(Ok(minimized))) => window_shown(true, visible, minimized),
        _ => false,
    }
}

fn ui_hidden_stop(collecting: bool) -> Option<Stop> {
    (!collecting).then_some(Stop::Hidden)
}

/// 流し始める（流れていれば合図として扱う）。台帳を読み直し、台帳に無い id は理由つきで断る。
/// ウィンドウが隠れているあいだは流さず合図にもしない（--hidden の WebView でも document.visibilityState が visible のままで、
/// 常駐する全部の操作卓が全機体を流し続けていた。2026-10-12 に実機で確認）。
/// UI の分は Hidden で止め、復帰後の合図で再開できるようにする。常時監視は止めない
#[tauri::command]
pub async fn live_start(app: AppHandle, e: State<'_, Arc<Engine>>, l: State<'_, Arc<Live>>, ids: Vec<String>) -> Result<Value, String> {
    let w = app.get_webview_window(crate::window::LABEL);
    let shown = window_state_shown(w.as_ref().map(|w| w.is_visible()), w.as_ref().map(|w| w.is_minimized()));
    if !shown {
        if let Some(reason) = ui_hidden_stop(crate::monitor::collecting(&e)) {
            l.stop(None, reason);
        }
        return Ok(serde_json::json!({ "started": [], "running": [], "refused": [], "hidden": true }));
    }
    let cfg = e.reload_config();
    l.retain_active(&cfg.nodes);
    let (mut nodes, mut unknown): (Vec<Node>, Vec<String>) = (Vec::new(), Vec::new());
    for id in ids {
        match cfg.node(&id) {
            Some(n) => nodes.push(n.clone()),
            None => unknown.push(id),
        }
    }
    Ok(l.start(&nodes, &unknown))
}

/// 止める（ids が無ければ全部）。常時監視で流しているあいだは止めない（画面を移っても数字を集め続ける）
#[tauri::command]
pub async fn live_stop(e: State<'_, Arc<Engine>>, l: State<'_, Arc<Live>>, ids: Option<Vec<String>>) -> Result<Value, String> {
    if crate::monitor::collecting(&e) {
        return Ok(serde_json::json!({ "stopped": [], "monitor": true }));
    }
    Ok(l.stop(ids.as_deref(), Stop::User))
}

#[tauri::command]
pub async fn power_session(e: State<'_, Arc<Engine>>, l: State<'_, Arc<Live>>, node_id: String, save: bool) -> Result<Value, String> {
    let points: Vec<Value> = l.samples(&node_id).iter().map(tune_core::live::Sample::point).collect();
    e.power_observation(&node_id, &points, save)
}

/// ウィンドウを閉じた: 見ている画面が無いので全部止める（常時監視で流しているあいだは止めない）
pub fn stop_all(app: &AppHandle) {
    let collecting = app.try_state::<Arc<Engine>>().is_some_and(|e| crate::monitor::collecting(&e));
    if let Some(reason) = ui_hidden_stop(collecting)
        && let Some(l) = app.try_state::<Arc<Live>>()
    {
        l.stop(None, reason);
    }
}

/// アプリの終了: 全部止め、サンプラー（子プロセス）が終わるのを少しだけ待つ
pub fn shutdown(app: &AppHandle) {
    if let Some(l) = app.try_state::<Arc<Live>>() {
        let l = l.inner().clone();
        let engine = app.try_state::<Arc<Engine>>().map(|e| e.inner().clone());
        tauri::async_runtime::block_on(async move {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            l.shutdown(Duration::from_secs(3)).await;
            if let Some(e) = engine {
                let _ = e.finish_monitor_journal(deadline.saturating_duration_since(std::time::Instant::now())).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{ui_hidden_stop, window_shown, window_state_shown};
    use tune_core::live::Stop;

    #[test]
    fn hidden_or_minimized_window_does_not_stream() {
        assert!(window_shown(true, true, false));
        assert!(!window_shown(true, false, false), "--hidden で起動して隠れている");
        assert!(!window_shown(true, true, true), "最小化");
        assert!(!window_shown(false, true, false), "ウィンドウが無い");
    }

    #[test]
    fn unreadable_window_state_does_not_stream() {
        assert!(!window_state_shown(Some(Err(())), Some(Ok(false))));
        assert!(!window_state_shown(Some(Ok(true)), Some(Err(()))));
        assert!(!window_state_shown(None::<Result<bool, ()>>, None));
        assert!(window_state_shown(Some(Ok::<bool, ()>(true)), Some(Ok(false))));
    }

    #[test]
    fn hidden_ui_pauses_with_resume_reason_but_preserves_monitor() {
        assert!(matches!(ui_hidden_stop(false), Some(Stop::Hidden)));
        assert!(ui_hidden_stop(true).is_none());
    }
}
