//! 自動更新（tauri-plugin-updater）。
//!
//! - 起動の1分後と6時間ごとに確かめる。知らせ（latest.json）は GitHub Releases に CI が置く（.github/workflows/release.yml、docs/release.md）
//! - 自動更新がオン（既定。メニューの「自動で更新する」・meta の update.auto）なら、新しい版を裏でダウンロードして署名を確かめ
//!   （tauri.conf.json の公開鍵・署名に入った版が知らせの版と同じこと）、ウィンドウを閉じていて分析・取り込み中でないときに入れて再起動する。
//!   入れる前にライブ・常時監視のサンプラーを止める。ウィンドウを開いているあいだは待ち、通知で知らせる
//! - 自動で入れるのは公開から2時間たってから（latest.json の pub_date）。そのあいだに問題の版を prerelease・下書きに戻せば、
//!   どの機体にも入らない（main へのマージから全機体に届くまでの止める余地。docs/release.md）
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
/// 公開から自動で入れるまで（止める余地）
const SOAK: Duration = Duration::from_secs(2 * 3600);

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

/// 入れて再起動する。サンプラーを先に止める（Windows はインストーラーを起こしてこのプロセスがすぐ終わるため）。
/// 非同期の中から呼ぶので、止まるのを await で待つ（live::shutdown の block_on は非同期の中では使えない）
async fn install(app: &AppHandle, update: &Update, bytes: &[u8]) -> Result<(), String> {
    record(app, json!({ "installing": update.version, "installing_at": now_ms() }));
    if let Some(l) = app.try_state::<Arc<tune_core::live::Live>>() {
        l.inner().clone().shutdown(Duration::from_secs(3)).await;
    }
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
        // 開発ビルドの検証用: 確かめる間隔・公開から入れるまでを秒で（KATALA_TUNE_DEV_UPDATE_EVERY・KATALA_TUNE_DEV_UPDATE_SOAK）
        let dev = |k: &str, d: Duration| {
            if cfg!(debug_assertions) { std::env::var(k).ok().and_then(|s| s.parse().ok()).map_or(d, Duration::from_secs) } else { d }
        };
        let (every, soak) = (dev("KATALA_TUNE_DEV_UPDATE_EVERY", EVERY), dev("KATALA_TUNE_DEV_UPDATE_SOAK", SOAK));
        tokio::time::sleep(if every < FIRST_CHECK { every } else { FIRST_CHECK }).await;
        // 用意した版・その中身・自動で入れてよい時刻（公開日 + soak。公開日が無ければ用意した時から）
        let mut ready: Option<(Update, Vec<u8>, i64)> = None;
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
                        if auto && ready.as_ref().is_none_or(|(r, _, _)| r.version != u.version) {
                            match download(&u).await {
                                Ok(bytes) => {
                                    let published = u.date.map_or(now_ms(), |d| d.unix_timestamp() * 1000);
                                    let after = published.saturating_add(soak.as_millis() as i64);
                                    record(&app, json!({ "downloaded": u.version, "downloaded_at": now_ms(), "install_after": after }));
                                    ready = Some((u, bytes, after));
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
            if let Some((u, bytes, after)) = ready.take() {
                if now_ms() >= after && idle(&app) && !BUSY.swap(true, Ordering::SeqCst) {
                    // 入れたら再起動してここには戻らない。失敗したら状態に残し、次の確認でやり直す
                    if let Err(e) = install(&app, &u, &bytes).await {
                        record(&app, json!({ "error": e }));
                    }
                    BUSY.store(false, Ordering::SeqCst);
                } else {
                    if now_ms() >= after && told.as_deref() != Some(u.version.as_str()) {
                        notify(
                            &app,
                            &format!("Katala Tune: {} を入れる準備ができました", u.version),
                            "ウィンドウを閉じると（分析の途中でなければ）入れて再起動します",
                        );
                        told = Some(u.version.clone());
                    }
                    ready = Some((u, bytes, after));
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
    install(app, &update, &bytes).await?;
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
