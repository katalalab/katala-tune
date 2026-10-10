//! 自動更新（tauri-plugin-updater）。
//!
//! - 起動の1分後と6時間ごとに確かめる。知らせ（latest.json）は GitHub Releases に CI が置く（.github/workflows/release.yml、docs/release.md）
//! - 自動更新がオン（既定。メニューの「自動で更新する」・meta の update.auto）なら、新しい版を裏でダウンロードして署名を確かめ
//!   （tauri.conf.json の公開鍵・署名に入った版が知らせの版と同じこと）、ウィンドウを閉じていて分析・取り込み中でないときに入れて再起動する。
//!   入れる前にライブ・常時監視のサンプラーを止める。ウィンドウを開いているあいだは待ち、通知で知らせる
//! - オフなら通知だけ。メニューの「更新を確認…」から、確認ダイアログで「入れる」を選んだときだけ入れる
//! - 状態は meta の updateState（ダッシュボードと `tune update` で見る）

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt as _;
use tauri_plugin_updater::{Update, UpdaterExt as _};
use tune_core::db::now_ms;
use tune_core::engine::Engine;

/// 確認ダイアログの承認ボタン
pub const INSTALL: &str = "入れる";

const FIRST_CHECK: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// 入れられる時（ウィンドウを閉じた・分析が終わった）を見る間隔
const TICK: Duration = Duration::from_secs(60);

/// 確認・ダウンロード・入れる途中か（メニューを続けて押しても1つだけ）
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn is_busy() -> bool {
    BUSY.load(Ordering::SeqCst)
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        eprintln!("通知を出せなかった: {e}");
    }
}

fn engine(app: &AppHandle) -> Option<Arc<Engine>> {
    app.try_state::<Arc<Engine>>().map(|s| s.inner().clone())
}

/// 自動更新がオンか（既定はオン）
pub fn auto(app: &AppHandle) -> bool {
    engine(app).and_then(|e| e.with_db(|d| d.get_meta("update")).ok().flatten()).and_then(|v| v.get("auto").and_then(Value::as_bool)).unwrap_or(true)
}

pub fn set_auto(app: &AppHandle, on: bool) {
    if let Some(e) = engine(app) {
        let _ = e.with_db(|d| d.set_meta("update", &json!({ "auto": on })));
    }
}

/// 状態を残す（前の値に重ねる）
fn record(app: &AppHandle, patch: Value) {
    let Some(e) = engine(app) else { return };
    let mut s = e.with_db(|d| d.get_meta("updateState")).ok().flatten().and_then(|v| v.as_object().cloned()).unwrap_or_default();
    s.insert("current".into(), json!(app.package_info().version.to_string()));
    if let Value::Object(p) = patch {
        s.extend(p);
    }
    let _ = e.with_db(|d| d.set_meta("updateState", &Value::Object(s)));
}

/// 入れてよい時: ウィンドウが見えていない・分析も取り込みもしていない
fn idle(app: &AppHandle) -> bool {
    let visible = app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
    let busy = engine(app).is_some_and(|e| e.is_probing() || e.is_syncing());
    !visible && !busy
}

/// ダウンロード（署名を確かめる。合わなければ Err で、何も入れない）
async fn download(update: &Update) -> Result<Vec<u8>, String> {
    update.download(|_, _| {}, || {}).await.map_err(|e| format!("署名を確かめられない・ダウンロードの失敗: {e}"))
}

/// 入れて再起動する。サンプラーを先に止める（Windows はインストーラーを起こしてこのプロセスがすぐ終わるため）
fn install(app: &AppHandle, update: &Update, bytes: &[u8]) -> Result<(), String> {
    record(app, json!({ "installing": update.version, "installing_at": now_ms() }));
    crate::live::shutdown(app);
    update.install(bytes).map_err(|e| format!("入れられなかった: {e}"))?;
    // macOS は入れ替えた .app で起動し直す（Windows はインストーラーが入れ替えて起動し直す。ここには戻らない）
    app.restart()
}

/// 起動の1分後と6時間ごとに確かめる。自動更新がオンなら、入れられる時に入れて再起動する
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        #[cfg(debug_assertions)]
        if std::env::var_os("KATALA_TUNE_DEV_UPDATE").is_some() {
            // 開発ビルドの検証用: すぐに「更新を確認…」と同じ流れを動かす（答えは KATALA_TUNE_DEV_CONFIRM）
            match check_and_install(&app).await {
                Ok(m) => eprintln!("[update] {m}"),
                Err(e) => eprintln!("[update] 失敗: {e}"),
            }
            return;
        }
        // 開発ビルドの検証用: 確かめる間隔を秒で（KATALA_TUNE_DEV_UPDATE_EVERY）
        let every = if cfg!(debug_assertions) {
            std::env::var("KATALA_TUNE_DEV_UPDATE_EVERY").ok().and_then(|s| s.parse().ok()).map_or(EVERY, Duration::from_secs)
        } else {
            EVERY
        };
        tokio::time::sleep(if every < FIRST_CHECK { every } else { FIRST_CHECK }).await;
        let mut ready: Option<(Update, Vec<u8>)> = None;
        let mut told: Option<String> = None;
        let mut last_check: Option<Instant> = None;
        loop {
            if !is_busy() && last_check.is_none_or(|t| t.elapsed() >= every) {
                last_check = Some(Instant::now());
                let checked = async { app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string()) }.await;
                match checked {
                    Ok(Some(u)) => {
                        record(&app, json!({ "checked_at": now_ms(), "available": u.version, "error": null }));
                        let auto = auto(&app);
                        if auto && ready.as_ref().is_none_or(|(r, _)| r.version != u.version) {
                            match download(&u).await {
                                Ok(bytes) => {
                                    record(&app, json!({ "downloaded": u.version, "downloaded_at": now_ms() }));
                                    ready = Some((u, bytes));
                                }
                                Err(e) => record(&app, json!({ "error": e })),
                            }
                        } else if !auto && told.as_deref() != Some(u.version.as_str()) {
                            notify(&app, &format!("Katala Tune: 新しい版 {} があります", u.version), "メニューの「更新を確認…」から、確認のうえで入れられます");
                            told = Some(u.version.clone());
                        }
                    }
                    Ok(None) => {
                        record(&app, json!({ "checked_at": now_ms(), "available": null, "error": null }));
                        ready = None;
                    }
                    Err(e) => record(&app, json!({ "checked_at": now_ms(), "error": format!("更新を確かめられなかった: {e}") })),
                }
            }
            if ready.is_some() && !auto(&app) {
                ready = None;
            }
            if let Some((u, bytes)) = ready.take() {
                if idle(&app) && !BUSY.swap(true, Ordering::SeqCst) {
                    // 入れたら再起動してここには戻らない。失敗したら状態に残し、次の確認でやり直す
                    if let Err(e) = install(&app, &u, &bytes) {
                        record(&app, json!({ "error": e }));
                    }
                    BUSY.store(false, Ordering::SeqCst);
                } else {
                    if told.as_deref() != Some(u.version.as_str()) {
                        notify(
                            &app,
                            &format!("Katala Tune: {} を入れる準備ができました", u.version),
                            "ウィンドウを閉じると（分析の途中でなければ）入れて再起動します",
                        );
                        told = Some(u.version.clone());
                    }
                    ready = Some((u, bytes));
                }
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// メニューの「更新を確認…」。新しい版があれば確認し、承認したときだけ署名を確かめて入れ、再起動する。
/// 戻り値は結果の説明（最新・取り消し）。入れたときは再起動するので戻らない
pub async fn check_and_install(app: &AppHandle) -> Result<String, String> {
    if BUSY.swap(true, Ordering::SeqCst) {
        return Ok("更新を確かめている途中".into());
    }
    // メニューは作ったときの is_busy() を写しているので、状態を変えるたびに作り直す（確認中は「更新を確認…」を押せなくする）
    crate::tray::update(app);
    let r = run(app).await;
    BUSY.store(false, Ordering::SeqCst);
    crate::tray::update(app);
    r
}

async fn run(app: &AppHandle) -> Result<String, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| format!("更新を確かめられなかった: {e}"))? else {
        record(app, json!({ "checked_at": now_ms(), "available": null, "error": null }));
        return Ok(format!("最新の版（{}）", app.package_info().version));
    };
    let title = format!("新しい版があります（{} → {}）", update.current_version, update.version);
    const TAIL: &str = "入れる前に署名を確かめます。入れたあとアプリを再起動します（分析・取り込みの途中なら中断します）。";
    let detail = match update.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(notes) => format!("入れますか？\n\n{notes}\n\n{TAIL}"),
        None => format!("入れますか？\n\n{TAIL}"),
    };
    if !crate::confirm::ask(app.clone(), INSTALL)(title, detail).await {
        return Ok(format!("取り消した（{}）", update.version));
    }
    let bytes = download(&update).await.map_err(|e| format!("入れなかった（{e}）"))?;
    install(app, &update, &bytes)?;
    Ok(String::new())
}

/// メニューから: 結果を通知で知らせる（確認ダイアログは check_and_install の中で出る）
pub fn from_menu(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match check_and_install(&app).await {
            Ok(m) => notify(&app, "Katala Tune: 更新", &m),
            Err(e) => notify(&app, "Katala Tune: 更新できなかった", &e),
        }
    });
}
