//! メニューバー（Windows は通知領域）。main.js の updateTray / createTray と同じ項目と動き

use std::sync::Arc;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tune_core::engine::Engine;
use tune_core::js;

const ID: &str = "main";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    TrayIconBuilder::with_id(ID)
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(true)
        // macOS はクリックでメニュー、Windows は左クリックでウィンドウ・右クリックでメニュー
        .show_menu_on_left_click(!cfg!(windows))
        .on_menu_event(|app, ev| on_menu(app, ev.id().as_ref()))
        .on_tray_icon_event(|tray, ev| {
            if cfg!(windows) && matches!(ev, TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }) {
                crate::window::show(tray.app_handle());
            }
        })
        .build(app)?;
    update(app);
    Ok(())
}

fn engine(app: &AppHandle) -> Option<Arc<Engine>> {
    app.try_state::<Arc<Engine>>().map(|s| s.inner().clone())
}

fn on_menu(app: &AppHandle, id: &str) {
    let Some(e) = engine(app) else { return };
    match id {
        "status" => crate::window::show_view(app, "status"),
        "probe" => {
            tauri::async_runtime::spawn(async move {
                e.run_probe(None, false).await;
                let nodes = e.config().nodes;
                e.sync_logs(nodes).await;
            });
        }
        "logs" => {
            tauri::async_runtime::spawn(async move {
                let nodes = e.config().nodes;
                e.sync_logs(nodes).await;
            });
        }
        "auto" => {
            // いまの設定を反転して保存する（画面の「自動スキャン」と同じ値）
            let on = !js::truthy(e.schedule().get("enabled"));
            let mut s = e.with_db(|d| d.get_meta("schedule")).ok().flatten().and_then(|v| v.as_object().cloned()).unwrap_or_default();
            s.insert("enabled".into(), serde_json::Value::Bool(on));
            let _ = e.with_db(|d| d.set_meta("schedule", &serde_json::Value::Object(s)));
            e.compute_checks(false, true);
        }
        "update" => crate::update::from_menu(app),
        "quit" => app.exit(0),
        _ => {}
    }
}

/// 件数・分析中か・自動スキャンの設定を表示し直す
pub fn update(app: &AppHandle) {
    let (Some(e), Some(tray)) = (engine(app), app.tray_by_id(ID)) else { return };
    let c = e.status_counts();
    let n = |k: &str| c.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0);
    let (fail, warn, ok) = (n("fail"), n("warn"), n("ok"));
    let s = e.schedule();
    #[cfg(target_os = "macos")]
    let _ = tray.set_title(Some(if fail > 0 { format!(" {fail}") } else { String::new() }));
    let _ = tray.set_tooltip(Some(format!("Katala Tune — 異常 {fail}・注意 {warn}・正常 {ok}")));
    let busy = if e.is_probing() {
        "（分析中）"
    } else if e.is_syncing() {
        "（ログ取り込み中）"
    } else {
        ""
    };
    let menu = (|| -> tauri::Result<Menu<tauri::Wry>> {
        Menu::with_items(
            app,
            &[
                &MenuItem::with_id(app, "counts", format!("異常 {fail}　注意 {warn}　正常 {ok}{busy}"), false, None::<&str>)?,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "status", "状態を開く", true, None::<&str>)?,
                &MenuItem::with_id(app, "probe", "今すぐ分析", !e.is_probing(), None::<&str>)?,
                &MenuItem::with_id(app, "logs", "ログを取り込む", !e.is_syncing(), None::<&str>)?,
                &MenuItem::with_id(app, "update", "更新を確認…", !crate::update::is_busy(), None::<&str>)?,
                &PredefinedMenuItem::separator(app)?,
                &CheckMenuItem::with_id(
                    app,
                    "auto",
                    format!("自動スキャン（分析 {} 分・ログ {} 分ごと）", js::string(s.get("probe_minutes")), js::string(s.get("logs_minutes"))),
                    true,
                    js::truthy(s.get("enabled")),
                    None::<&str>,
                )?,
                &PredefinedMenuItem::separator(app)?,
                &MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?,
            ],
        )
    })();
    match menu {
        Ok(m) => {
            let _ = tray.set_menu(Some(m));
        }
        Err(err) => eprintln!("メニューを作れなかった: {err}"),
    }
}
