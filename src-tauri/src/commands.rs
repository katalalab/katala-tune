//! window.tune の各関数（preload.js と同じ名前・引数・戻り値の形）。中身は tune-core の Engine を呼ぶだけ。
//! どれも async（画面のスレッドを止めない）。重い処理（分析・取り込み）の進み具合はイベントで知らせる。

use std::sync::Arc;

use serde_json::{Value, json};
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_clipboard_manager::ClipboardExt as _;
use tauri_plugin_opener::OpenerExt as _;
use tune_core::engine::Engine;
use tune_core::{js, nodes};

type R = Result<Value, String>;
type E<'a> = State<'a, Arc<Engine>>;

#[tauri::command]
pub async fn config(app: AppHandle, e: E<'_>) -> R {
    let cfg = e.config();
    Ok(json!({
        "nodes": cfg.nodes.iter().map(nodes::Node::public).collect::<Vec<_>>(),
        "error": e.config_error(),
        "file": cfg.file,
        "dataDir": e.data_dir(),
        "platform": crate::platform(),
        "fleet": cfg.fleet.is_some(),
        "example": cfg.is_example(),
        "schedule": e.schedule(),
        // システムのアクセントカラー（macOS / Windows）。取れなければ既定の青
        "accent": crate::accent::get(&app),
    }))
}

#[tauri::command]
pub async fn last(e: E<'_>) -> R {
    e.last().map(Value::Array)
}

#[tauri::command]
pub async fn power_report(e: E<'_>) -> R {
    e.power_report()
}

#[tauri::command]
pub async fn power_settings(e: E<'_>, node_id: String, patch: Value) -> R {
    e.power_settings(&node_id, &patch)
}

/// 全機（ids が空）を分析したら、続けてログも取り込む（main.js と同じ）
#[tauri::command]
pub async fn probe(e: E<'_>, ids: Option<Vec<String>>) -> R {
    let e = e.inner().clone();
    let all = ids.as_ref().is_none_or(Vec::is_empty);
    let r = e.run_probe(ids, false).await;
    if all && r.get("busy").is_none() {
        tauri::async_runtime::spawn(async move {
            let nodes = e.config().nodes;
            e.sync_logs(nodes).await;
        });
    }
    Ok(r)
}

#[tauri::command]
pub async fn history(e: E<'_>, id: Option<Value>) -> R {
    let id = id.map(|v| js::string(Some(&v))).unwrap_or_default();
    e.with_db(|d| d.history(&id, 60)).map(Value::Array)
}

#[tauri::command]
pub async fn fleet(e: E<'_>) -> R {
    Ok(e.fleet().await)
}

/// 変更操作。台帳の読み直し → 計画と検証 → 確認ダイアログ → もう一度読み直し → 実行 → 実行記録
#[tauri::command]
pub async fn action(app: AppHandle, e: E<'_>, node_id: String, action: Value, label: Option<Value>) -> R {
    if !crate::confirm::SUPPORTED {
        return Ok(json!({ "ok": false, "refused": "この OS では確認ダイアログを出せないので実行しない" }));
    }
    let title = format!("{node_id}: {}", js::string(label.as_ref()));
    e.confirm_and_run(&node_id, &action, &title, None, crate::confirm::dialog(app)).await
}

#[tauri::command]
pub async fn undo(app: AppHandle, e: E<'_>, entry_id: String) -> R {
    if !crate::confirm::SUPPORTED {
        return Ok(json!({ "ok": false, "refused": "この OS では確認ダイアログを出せないので実行しない" }));
    }
    e.undo(&entry_id, crate::confirm::dialog(app)).await
}

#[tauri::command]
pub async fn actions_log(e: E<'_>) -> R {
    e.with_db(|d| d.actions(100)).map(Value::Array)
}

#[tauri::command]
pub async fn logs_sync(e: E<'_>, ids: Option<Vec<String>>) -> R {
    let e = e.inner().clone();
    let nodes = e.reload_config().nodes.into_iter().filter(|n| ids.as_ref().is_none_or(|i| i.is_empty() || i.contains(&n.id))).collect();
    Ok(e.sync_logs(nodes).await)
}

/// 全文検索。検索式の誤りは `{ error }` で返す（main.js と同じ）。件数の上限は Rust 側で守る
#[tauri::command]
pub async fn logs_query(e: E<'_>, filter: Option<Value>) -> R {
    let f = filter.unwrap_or_else(|| json!({}));
    Ok(match e.logs_query(&f) {
        Ok(rows) => json!({ "rows": rows }),
        Err(err) => json!({ "error": err }),
    })
}

#[tauri::command]
pub async fn logs_signatures(e: E<'_>, filter: Option<Value>) -> R {
    let f = filter.unwrap_or_else(|| json!({}));
    e.logs_signatures(&f).map(Value::Array)
}

#[tauri::command]
pub async fn logs_cursors(e: E<'_>) -> R {
    e.logs_cursors().map(Value::Array)
}

#[tauri::command]
pub async fn copy(app: AppHandle, text: String) -> R {
    app.clipboard().write_text(text).map_err(|e| e.to_string())?;
    Ok(json!(true))
}

/// shell.openPath と同じく、成功なら空文字・失敗ならその理由を返す
fn open_path(app: &AppHandle, path: &std::path::Path) -> Value {
    json!(app.opener().open_path(path.to_string_lossy(), None::<&str>).err().map(|e| e.to_string()).unwrap_or_default())
}

#[tauri::command]
pub async fn open_data_dir(app: AppHandle, e: E<'_>) -> R {
    Ok(open_path(&app, e.data_dir()))
}

#[tauri::command]
pub async fn open_config(app: AppHandle, e: E<'_>) -> R {
    let file = e.config().file.unwrap_or_else(|| e.config_path().to_path_buf());
    let _ = nodes::ensure_config(&file);
    Ok(open_path(&app, &file))
}

/// 画面からの問い合わせ（状態を計算し直すが、更新通知は送らない）
#[tauri::command]
pub async fn status(e: E<'_>) -> R {
    e.status()
}

#[tauri::command]
pub async fn set_schedule(e: E<'_>, patch: Option<Value>) -> R {
    e.set_schedule(&patch.unwrap_or_else(|| json!({})))
}

/// ログイン時の起動は、操作者が画面で切り替えたときだけ変える
#[tauri::command]
pub async fn set_login(app: AppHandle, e: E<'_>, on: bool) -> R {
    let al = app.autolaunch();
    let res = if on { al.enable() } else { al.disable() };
    if let Err(err) = res {
        eprintln!("ログイン時の起動を変えられなかった: {err}");
    }
    e.compute_checks(false, false);
    Ok(json!(al.is_enabled().unwrap_or(false)))
}

/// 開発時だけ: 画面のエラー・警告を標準エラーに出す（検証用。リリースでは何もしない）
#[tauri::command]
pub async fn dev_report(kind: String, message: String) -> R {
    if cfg!(debug_assertions) {
        eprintln!("[webview {kind}] {message}");
    }
    Ok(Value::Null)
}
