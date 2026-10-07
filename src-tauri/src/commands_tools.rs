//! window.tune の道具の棚卸し・Do-gu・AI エージェントのセッション（commands.rs と同じ作り）。
//! inventory・inventoryRun・doguRefresh・doguExclude・doguPublish は preload.js（Electron 版）と同じ名前・同じ戻り値の形。
//! aiSummary・aiSessions・aiSync・aiTrace・aiVerify・aiProvenance は Tauri 版だけ（Electron 版の preload は「未対応」を返すか、関数が無い）。

use std::sync::Arc;

use serde_json::{Value, json};
use tauri::{AppHandle, State};
use tune_core::dogu;
use tune_core::engine::Engine;

type R = Result<Value, String>;
type E<'a> = State<'a, Arc<Engine>>;

/// opts = { all: true } で、依存として入ったものも含める（既定は自分で入れたものだけ。Electron 版と同じ）
fn all(opts: &Option<Value>) -> bool {
    opts.as_ref().and_then(|o| o.get("all")).and_then(Value::as_bool).unwrap_or(false)
}

#[tauri::command]
pub async fn inventory(e: E<'_>, opts: Option<Value>) -> R {
    e.inventory_view(all(&opts))
}

/// 棚卸しを実行してから一覧を返す（Electron 版と同じ）。実行中だったときは busy を付ける
#[tauri::command]
pub async fn inventory_run(e: E<'_>, ids: Option<Vec<String>>) -> R {
    let e = e.inner().clone();
    let r = e.run_inventory(ids).await;
    let mut v = e.inventory_view(false)?;
    if r.get("busy").is_some() {
        v["busy"] = json!(true);
    }
    Ok(v)
}

/// Do-gu の共通マスターを取りにいく（画面のボタンを押したときだけ。送るものは無い）
#[tauri::command]
pub async fn dogu_refresh(e: E<'_>) -> R {
    e.dogu_refresh(&dogu::Curl, false).await
}

#[tauri::command]
pub async fn dogu_exclude(e: E<'_>, slugs: Option<Value>) -> R {
    e.dogu_exclude(&slugs.unwrap_or(Value::Null), false)
}

/// デッキへの登録（外への送信）。下書きの検証 → 確認ダイアログ → 下書きの再検証 → 送信 → 実行記録。リトライしない
#[tauri::command]
pub async fn dogu_publish(app: AppHandle, e: E<'_>, slugs: Option<Value>) -> R {
    if !crate::confirm::SUPPORTED {
        return Ok(json!({ "ok": false, "refused": "この OS では確認ダイアログを出せないので送らない" }));
    }
    e.dogu_publish(&slugs.unwrap_or(Value::Null), &dogu::Curl, dogu::api_key_here(), crate::confirm::dialog(app)).await
}

/// AI エージェントのセッションの集計（画面には集計結果だけ返す）
#[tauri::command]
pub async fn ai_summary(e: E<'_>, filter: Option<Value>) -> R {
    e.ai_summary(&filter.unwrap_or_else(|| json!({})))
}

/// セッションの一覧（ページング。1回 200 件まで）
#[tauri::command]
pub async fn ai_sessions(e: E<'_>, filter: Option<Value>) -> R {
    e.ai_sessions(&filter.unwrap_or_else(|| json!({})))
}

/// 各機体のセッションを取り込む（続きから差分だけ。1回 25 秒で区切る）
#[tauri::command]
pub async fn ai_sync(e: E<'_>, ids: Option<Vec<String>>) -> R {
    let e = e.inner().clone();
    Ok(e.ai_sync(ids).await)
}

/// 数字の出どころ。filter = { node_id, day }（その日の内訳）か { node_id, file }（1 ファイルの区間と検算）
#[tauri::command]
pub async fn ai_trace(e: E<'_>, filter: Option<Value>) -> R {
    e.ai_trace(&filter.unwrap_or_else(|| json!({})))
}

/// 1 ファイルの区間を元ファイルと照合する（機体で区間を読み直して指紋だけを受け取る。読み取り専用）
#[tauri::command]
pub async fn ai_verify(e: E<'_>, filter: Option<Value>) -> R {
    let e = e.inner().clone();
    e.ai_verify(&filter.unwrap_or_else(|| json!({}))).await
}

/// 出どころの台帳の様子（連鎖の検算・区間の状態・調査スクリプトと単価表の版）
#[tauri::command]
pub async fn ai_provenance(e: E<'_>) -> R {
    e.ai_provenance()
}
