//! メインのウィンドウ。main.js の createWindow / showWindow と同じ挙動:
//! macOS はサイドバーの vibrancy（タイトルバーは中身と一体）、Windows 11 は Mica。透過背景。
//! 画面の遷移は禁止し、新しいウィンドウを開く要求は https だけ既定のブラウザで開く（will-navigate・setWindowOpenHandler と同じ）。

use std::sync::Mutex;

use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt as _;

pub const LABEL: &str = "main";
const BRIDGE: &str = include_str!("../bridge/tune.js");

/// 読み込みが終わったら画面に送る移動先（ウィンドウを作り直したときに、状態の画面を開くため）
#[derive(Default)]
pub struct Pending(Mutex<Option<String>>);

pub fn navigate_after_load(app: &AppHandle, view: &str) {
    if let Some(p) = app.try_state::<Pending>() {
        *p.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(view.to_string());
    }
}

/// アプリ自身の画面の URL か（それ以外への遷移は止める）
fn is_app_url(url: &Url) -> bool {
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => url.host_str() == Some("tauri.localhost") || (cfg!(debug_assertions) && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))),
        "about" => url.as_str() == "about:blank",
        _ => false,
    }
}

fn bridge() -> String {
    BRIDGE
        .replace("__KT_PLATFORM__", &serde_json::Value::from(crate::platform()).to_string())
        .replace("__KT_DEBUG__", if cfg!(debug_assertions) { "true" } else { "false" })
}

/// ウィンドウを作る。show_when_loaded = false のときは作るだけで見せない（ログイン時の起動）
pub fn create(app: &AppHandle, show_when_loaded: bool) -> tauri::Result<WebviewWindow> {
    let opener = app.clone();
    let b = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Katala Tune")
        .inner_size(1440.0, 920.0)
        .min_inner_size(1040.0, 640.0)
        .visible(false)
        .initialization_script(bridge())
        .on_navigation(is_app_url)
        .on_new_window(move |url, _features| {
            if url.scheme() == "https"
                && let Err(e) = opener.opener().open_url(url.as_str(), None::<&str>)
            {
                eprintln!("外部で開けなかった: {e}");
            }
            NewWindowResponse::Deny
        })
        .on_page_load(move |w, p| {
            if p.event() != PageLoadEvent::Finished {
                return;
            }
            if show_when_loaded {
                let _ = w.show();
                let _ = w.set_focus();
            }
            let pending = w.app_handle().try_state::<Pending>().and_then(|p| p.0.lock().ok().and_then(|mut g| g.take()));
            if let Some(view) = pending {
                // 画面がイベントの受け取りを登録し終えるのを少し待ってから送る
                let app = w.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                    let _ = app.emit("navigate", view);
                });
            }
            // 開発時だけ: KATALA_TUNE_DEV_EVAL の JS（@<ファイル> ならその中身）を画面で動かす
            // （検証用。開発ビルドは開発者ツールも開けるので、できることは増えない）
            #[cfg(debug_assertions)]
            if let Some(js) = std::env::var("KATALA_TUNE_DEV_EVAL").ok().and_then(|v| match v.strip_prefix('@') {
                Some(f) => std::fs::read_to_string(f).ok(),
                None => Some(v),
            }) {
                let w = w.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    let _ = w.eval(js);
                });
            }
        });
    #[cfg(target_os = "macos")]
    let b = {
        use tauri::utils::config::WindowEffectsConfig;
        use tauri::window::{Effect, EffectState};
        b.title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(18.0, 18.0))
            .transparent(true)
            .effects(WindowEffectsConfig { effects: vec![Effect::Sidebar], state: Some(EffectState::FollowsWindowActiveState), ..Default::default() })
    };
    #[cfg(windows)]
    let b = {
        use tauri::utils::config::WindowEffectsConfig;
        use tauri::window::Effect;
        b.transparent(true).effects(WindowEffectsConfig { effects: vec![Effect::Mica], ..Default::default() })
    };
    b.build()
}

/// ウィンドウを前に出す（閉じていれば作り直す）
pub fn show(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    } else if let Err(e) = create(app, true) {
        eprintln!("ウィンドウを開けなかった: {e}");
    }
}

/// ウィンドウを出して、画面を移動する（通知・メニューの「状態を開く」）
pub fn show_view(app: &AppHandle, view: &str) {
    if app.get_webview_window(LABEL).is_some() {
        show(app);
        let _ = app.emit("navigate", view);
    } else {
        navigate_after_load(app, view);
        show(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_app_urls_are_allowed() {
        assert!(is_app_url(&Url::parse("tauri://localhost/index.html").unwrap()));
        assert!(is_app_url(&Url::parse("http://tauri.localhost/index.html?platform=win32").unwrap()));
        assert!(!is_app_url(&Url::parse("https://example.com/").unwrap()));
        assert!(!is_app_url(&Url::parse("file:///etc/passwd").unwrap()));
        assert!(!is_app_url(&Url::parse("tauri://evil.example/").unwrap()));
    }

    /// preload.js（Electron 版）にある window.tune の名前が、橋渡しにも全部ある
    #[test]
    fn bridge_has_every_preload_api() {
        let b = bridge();
        assert!(!b.contains("__KT_PLATFORM__") && !b.contains("__KT_DEBUG__"));
        let preload = include_str!("../../preload.js");
        let names: Vec<&str> = preload
            .lines()
            .filter_map(|l| l.trim().split_once(": (").map(|(n, _)| n))
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric()))
            .collect();
        assert!(names.len() >= 22, "preload.js を読めていない: {names:?}");
        for name in names {
            assert!(b.contains(&format!("    {name}: ")), "window.tune.{name} が橋渡しに無い");
        }
    }
}
