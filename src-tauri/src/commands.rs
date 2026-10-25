//! window.tune の各関数（preload.js と同じ名前・引数・戻り値の形）。中身は tune-core の Engine を呼ぶだけ。
//! どれも async（画面のスレッドを止めない）。重い処理（分析・取り込み）の進み具合はイベントで知らせる。

use std::sync::Arc;

use serde_json::{Value, json};
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_clipboard_manager::ClipboardExt as _;
use tauri_plugin_opener::OpenerExt as _;
use tune_core::engine::Engine;
use tune_core::{handoff, js, nodes};

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
pub async fn ledger(e: E<'_>) -> R {
    e.ledger()
}

#[tauri::command]
pub async fn power_report(e: E<'_>) -> R {
    e.power_report()
}

#[tauri::command]
pub async fn network_check(e: E<'_>, ids: Option<Vec<String>>, active: Option<bool>) -> R {
    e.network_check(&ids.unwrap_or_default(), active.unwrap_or(false)).await
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

/// 所見を AI エージェントの CLI に渡す（Tauri 版だけ）。DB から所見を読み直してプロンプトを作り、
/// 全文を確認ダイアログで見せて承認されたときだけ、新しいターミナルで読み取り寄りの権限の CLI を開く（tune-core の handoff）
#[tauri::command]
pub async fn ai_handoff(
    app: AppHandle,
    window: tauri::WebviewWindow,
    e: E<'_>,
    review: State<'_, crate::handoff_review::Review>,
    node_id: String,
    finding_id: String,
    agent: String,
) -> R {
    let Some(agent) = handoff::Agent::parse(&agent) else {
        return Ok(json!({ "ok": false, "refused": "渡す先が分からない" }));
    };
    if !crate::confirm::SUPPORTED {
        return Ok(json!({ "ok": false, "refused": "この OS では確認ダイアログを出せないので開かない" }));
    }
    let Ok(_guard) = review.gate.try_lock() else {
        return Ok(json!({ "ok": false, "refused": "別の所見を確認中です" }));
    };
    let Some(generation) = review.generation().filter(|_| window.is_visible().is_ok()) else {
        return Ok(json!({ "ok": false, "refused": "確認する画面が閉じられたか、読み込み中です。もう一度押してください" }));
    };
    let p = match e.handoff_prepare(&node_id, &finding_id) {
        Ok(p) => p,
        Err(m) => return Ok(json!({ "ok": false, "refused": m })),
    };
    let title = format!("{} に渡す: {} / {}", agent.label(), p.node.id, p.title);
    let detail = format!("{} を新しいターミナルで {} で開きます。内容は {} に送られます。", agent.label(), agent.mode(), agent.vendor());
    let hash = tune_core::db::sha256_hex(p.prompt.as_bytes());
    let request_id = format!("{}-{hash}", tune_core::db::now_ms());
    if !review.show(&app, generation, request_id, title, detail.clone(), &p.prompt).await {
        return Ok(e.handoff_cancel(&p, agent));
    }
    let title = format!("{} に調査依頼を渡しますか", agent.label());
    let detail = format!("{detail}\n\n画面で確認した全文を渡します。設定・ファイルの変更は別途承認が必要です。\n\nプロンプト SHA-256: {hash}");
    if !crate::confirm::ask(app, crate::confirm::HANDOFF)(title, detail).await {
        return Ok(e.handoff_cancel(&p, agent));
    }
    Ok(e.handoff_launch(&p, agent))
}

/// 所見のプロンプトをクリップボードへ（Desktop 版や Web に貼る用）。プロンプトは Rust 側で作り、画面には渡さない
#[tauri::command]
pub async fn ai_handoff_copy(app: AppHandle, e: E<'_>, node_id: String, finding_id: String) -> R {
    let p = match e.handoff_prepare(&node_id, &finding_id) {
        Ok(p) => p,
        Err(m) => return Ok(json!({ "ok": false, "refused": m })),
    };
    app.clipboard().write_text(p.prompt).map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true }))
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
