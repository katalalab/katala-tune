//! 自動更新（tauri-plugin-updater）。勝手には入れない:
//! - 起動の1分後と24時間ごとに確かめ、新しい版があれば通知するだけ
//! - 入れるのはメニューの「更新を確認…」から。確認ダイアログ（既定は「やめる」）で「入れる」を選んだときだけ、
//!   署名（tauri.conf.json の公開鍵・署名に入った版が知らせの版と同じこと）を確かめてから入れ、再起動する
//!
//! 更新の知らせ（latest.json）は GitHub Releases に CI が置く（.github/workflows/release.yml、docs/release.md）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt as _;
use tauri_plugin_updater::UpdaterExt as _;

/// 確認ダイアログの承認ボタン
pub const INSTALL: &str = "入れる";

const FIRST_CHECK: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(24 * 3600);

/// 確認・ダウンロード中か（メニューを続けて押しても1つだけ）
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn is_busy() -> bool {
    BUSY.load(Ordering::SeqCst)
}

/// 新しい版があればその版（無ければ None）
async fn available(app: &AppHandle) -> Result<Option<String>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    Ok(updater.check().await.map_err(|e| e.to_string())?.map(|u| u.version))
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        eprintln!("通知を出せなかった: {e}");
    }
}

/// 起動の1分後と24時間ごとに確かめ、新しい版があれば通知だけする（同じ版は1回だけ）
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
        tokio::time::sleep(FIRST_CHECK).await;
        let mut told: Option<String> = None;
        loop {
            match available(&app).await {
                Ok(Some(v)) if told.as_deref() != Some(v.as_str()) => {
                    notify(&app, &format!("Katala Tune: 新しい版 {v} があります"), "メニューの「更新を確認…」から、確認のうえで入れられます");
                    told = Some(v);
                }
                Ok(_) => {}
                Err(e) => eprintln!("更新を確かめられなかった: {e}"),
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// メニューの「更新を確認…」。新しい版があれば確認し、承認したときだけ署名を確かめて入れ、再起動する。
/// 戻り値は結果の説明（最新・取り消し）。入れたときは再起動するので戻らない
pub async fn check_and_install(app: &AppHandle) -> Result<String, String> {
    if BUSY.swap(true, Ordering::SeqCst) {
        return Ok("更新を確かめている途中".into());
    }
    let r = run(app).await;
    BUSY.store(false, Ordering::SeqCst);
    r
}

async fn run(app: &AppHandle) -> Result<String, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| format!("更新を確かめられなかった: {e}"))? else {
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
    // download が署名（公開鍵・署名に入った版）を確かめ、合わなければ入れずに Err を返す
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| format!("入れなかった（署名を確かめられない・ダウンロードの失敗など）: {e}"))?;
    // macOS は入れ替えた版で起動し直す（Windows はインストーラーがアプリを終了して入れ替え、起動し直す）
    app.restart()
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
