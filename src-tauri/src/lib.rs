//! Katala Tune のアプリ（Tauri 2）。分析エンジン tune-core を呼ぶ薄い殻。
//! 画面は renderer/ をそのまま使い、preload.js と同じ window.tune を初期化スクリプト（bridge/tune.js）で差し込む。
//!
//! - commands: window.tune の各関数（preload.js と同じ名前・引数・戻り値の形）
//! - window: ウィンドウ（vibrancy・Mica・透過・遷移の禁止・外部 URL は https だけ既定のブラウザで）

mod accent;
mod commands;
mod window;

use std::sync::Arc;

use serde_json::Value;
#[cfg(target_os = "macos")]
use tauri::RunEvent;
use tauri::{AppHandle, Emitter, Manager};
use tune_core::engine::{Engine, Host};
use tune_core::nodes;

/// 画面へのイベント
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
}

/// process.platform と同じ名前（画面が見た目を変えるのに使う）
pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::config,
            commands::last,
            commands::history,
            commands::actions_log,
            commands::logs_query,
            commands::logs_signatures,
            commands::logs_cursors,
            commands::copy,
            commands::open_data_dir,
            commands::open_config,
            commands::status,
            commands::set_schedule,
            commands::dev_report,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let engine = Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(TauriHost { app: handle.clone() }))?;
            app.manage(engine.clone());
            app.manage(window::Pending::default());
            if let Some(v) = std::env::var("KATALA_TUNE_DEV_VIEW").ok().filter(|_| cfg!(debug_assertions)) {
                window::navigate_after_load(&handle, &v);
            }
            window::create(&handle, true)?;
            engine.compute_checks(false, true);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Katala Tune を起動できない");
    app.run(|_app, _ev| {
        // macOS: Dock のアイコンを押したらウィンドウを出す（閉じていれば作り直す）
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = _ev {
            window::show(_app);
        }
    });
}
