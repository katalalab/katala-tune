//! Katala Tune のアプリ（Tauri 2）。分析エンジン tune-core を呼ぶ薄い殻。
//! 画面は renderer/ をそのまま使い、preload.js と同じ window.tune を初期化スクリプト（bridge/tune.js）で差し込む。
//!
//! - commands: window.tune の各関数（preload.js と同じ名前・引数・戻り値の形）
//! - commands_tools: 道具の棚卸し・Do-gu・AI エージェントのセッション
//! - window: ウィンドウ（vibrancy・Mica・透過・遷移の禁止・外部 URL は https だけ既定のブラウザで）
//! - tray: メニューバー（Windows は通知領域）
//! - confirm: 変更操作の確認ダイアログ
//! - live: ライブ表示（liveStart / liveStop と live イベント。中身は tune-core の live）
//! - 自動スキャン（1分ごとに期限を見る）・通知（異常化と回復だけ）・ログイン時の起動

mod accent;
mod commands;
mod commands_tools;
mod confirm;
mod live;
mod tray;
mod window;

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_notification::NotificationExt as _;
use tune_core::db::Change;
use tune_core::engine::{self, Engine, Host};
use tune_core::nodes;

/// 画面へのイベントと、常駐の表示・通知
struct TauriHost {
    app: AppHandle,
}

impl Host for TauriHost {
    fn probe_result(&self, r: &Value) {
        let _ = self.app.emit("probe-result", r);
    }
    fn logs_synced(&self, r: &Value) {
        let _ = self.app.emit("logs-synced", r);
    }
    fn checks_updated(&self) {
        let _ = self.app.emit("checks-updated", ());
    }
    // 悪くなって異常になったとき・異常から戻ったときだけ（engine が選んだもの）
    fn notify(&self, important: &[Change]) {
        let (title, body) = engine::notification_text(important);
        if cfg!(debug_assertions) {
            eprintln!("[notify] {title} / {}", body.replace('\n', " / "));
        }
        if let Err(e) = self.app.notification().builder().title(title).body(body).show() {
            eprintln!("通知を出せなかった: {e}");
        }
    }
    fn state_changed(&self) {
        tray::update(&self.app);
    }
    fn open_at_login(&self) -> bool {
        self.app.autolaunch().is_enabled().unwrap_or(false)
    }
    fn inventory_result(&self, r: &Value) {
        let _ = self.app.emit("inventory-result", r);
    }
    fn ai_synced(&self, r: &Value) {
        let _ = self.app.emit("ai-synced", r);
    }
}

/// process.platform と同じ名前（画面が見た目を変えるのに使う）
pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// 1分ごとに予定を見て、期限が来たものだけ動かす（main.js と同じ。起動の5秒後に1回目）
fn start_scheduler(e: Arc<Engine>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut iv = tokio::time::interval(Duration::from_secs(60));
        loop {
            iv.tick().await;
            let e = e.clone();
            tauri::async_runtime::spawn(async move { e.tick().await });
        }
    });
}

pub fn run() {
    // ログイン時の起動では、ウィンドウを開かずに常駐する
    let hidden = std::env::args().any(|a| a == "--hidden");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::config,
            commands::last,
            commands::probe,
            commands::history,
            commands::fleet,
            commands::action,
            commands::undo,
            commands::actions_log,
            commands::logs_sync,
            commands::logs_query,
            commands::logs_signatures,
            commands::logs_cursors,
            commands::copy,
            commands::open_data_dir,
            commands::open_config,
            commands::status,
            commands::set_schedule,
            commands::set_login,
            commands::dev_report,
            commands_tools::inventory,
            commands_tools::inventory_run,
            commands_tools::dogu_refresh,
            commands_tools::dogu_exclude,
            commands_tools::dogu_publish,
            commands_tools::ai_summary,
            commands_tools::ai_sessions,
            commands_tools::ai_sync,
            commands_tools::ai_trace,
            commands_tools::ai_verify,
            commands_tools::ai_provenance,
            live::live_start,
            live::live_stop,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let engine = Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(TauriHost { app: handle.clone() }))?;
            app.manage(engine.clone());
            app.manage(window::Pending::default());
            app.manage(live::create(&handle));
            if let Some(v) = std::env::var("KATALA_TUNE_DEV_VIEW").ok().filter(|_| cfg!(debug_assertions)) {
                window::navigate_after_load(&handle, &v);
            }
            window::create(&handle, !hidden)?;
            tray::create(&handle)?;
            engine.compute_checks(false, true);
            start_scheduler(engine);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Katala Tune を起動できない");
    app.run(|handle, ev| {
        match ev {
            // ウィンドウを閉じても、自動スキャンのためにメニューバー（Windows は通知領域）に残る。終了はメニューから
            RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
            // ライブ表示: ウィンドウを閉じたら止め、終了するときはサンプラーが終わるのを待つ
            RunEvent::WindowEvent { event: tauri::WindowEvent::Destroyed, .. } => live::stop_all(handle),
            RunEvent::Exit => live::shutdown(handle),
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => window::show(handle),
            _ => {}
        }
    });
}
