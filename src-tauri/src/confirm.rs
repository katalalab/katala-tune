//! 確認ダイアログ（変更操作・更新を入れるとき。main.js の dialog.showMessageBox と同じ強さ）。
//! 既定のボタン（Return / Enter）は「やめる」。承認のボタン（「実行する」「入れる」）を押したときだけ進む。
//! それ以外の終わり方（やめる・Esc・閉じる・表示の失敗・想定外の戻り値）はすべて「進まない」。
//! 確認ダイアログを出せない OS（Linux など）では進まない。
//!
//! 開発ビルドだけ: KATALA_TUNE_DEV_CONFIRM を付けると、ダイアログを出さずに答える（approve なら承認、それ以外は「やめる」）。
//! 検証で操作者の画面に本物のダイアログを出さないため。リリースビルドには入らない。

use tauri::AppHandle;
use tune_core::engine::Confirm;

/// この OS で確認ダイアログを出せるか
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));

/// 変更操作の承認ボタン
pub const RUN: &str = "実行する";

/// リリースビルドでは常に本物のダイアログ
#[cfg(not(debug_assertions))]
fn dev_answer(_title: &str, _detail: &str) -> Option<bool> {
    None
}

/// 開発ビルドで KATALA_TUNE_DEV_CONFIRM があれば、ダイアログを出さずにその答えを返す
#[cfg(debug_assertions)]
fn dev_answer(title: &str, detail: &str) -> Option<bool> {
    let v = std::env::var("KATALA_TUNE_DEV_CONFIRM").ok()?;
    let ok = v == "approve";
    eprintln!("[confirm:dev] {} / {} → {}", title, detail.replace('\n', " / "), if ok { "承認" } else { "やめる" });
    Some(ok)
}

#[cfg(any(target_os = "macos", windows))]
mod native {
    use tauri::{AppHandle, Manager};
    use tune_core::engine::Confirm;

    pub const CANCEL: &str = "やめる";

    /// 押されたボタンが承認のボタン（ok）だったときだけ true
    pub fn approved(r: &rfd::MessageDialogResult, ok: &str) -> bool {
        matches!(r, rfd::MessageDialogResult::Custom(s) if s == ok)
    }

    pub fn ask(app: AppHandle, ok: &'static str) -> Confirm {
        Box::new(move |title: String, detail: String| {
            Box::pin(async move {
                if let Some(a) = super::dev_answer(&title, &detail) {
                    return a;
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                let parent = app.get_webview_window(crate::window::LABEL);
                let shown = app.run_on_main_thread(move || {
                    // 1番目のボタンが既定（Return）。安全側の「やめる」を1番目に置く
                    let mut d = rfd::AsyncMessageDialog::new()
                        .set_level(rfd::MessageLevel::Warning)
                        .set_title(&title)
                        .set_description(&detail)
                        .set_buttons(rfd::MessageButtons::OkCancelCustom(CANCEL.into(), ok.into()));
                    if let Some(w) = &parent {
                        d = d.set_parent(w);
                    }
                    let fut = d.show();
                    std::thread::spawn(move || {
                        let r = tauri::async_runtime::block_on(fut);
                        if cfg!(debug_assertions) {
                            eprintln!("[confirm] {r:?}");
                        }
                        let _ = tx.send(r);
                    });
                });
                shown.is_ok() && rx.await.is_ok_and(|r| approved(&r, ok))
            })
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rfd::MessageDialogResult as R;

        #[test]
        fn only_the_approve_button_approves() {
            for ok in [super::super::RUN, crate::update::INSTALL] {
                assert!(approved(&R::Custom(ok.into()), ok));
                for r in [R::Custom(CANCEL.into()), R::Cancel, R::Ok, R::Yes, R::No, R::Custom(String::new())] {
                    assert!(!approved(&r, ok), "{r:?} で進んではいけない");
                }
            }
        }
    }
}

/// 承認のボタンの名前を指定して確認する
#[cfg(any(target_os = "macos", windows))]
pub fn ask(app: AppHandle, ok: &'static str) -> Confirm {
    native::ask(app, ok)
}

/// 確認ダイアログを出せない OS では常に「進まない」（開発ビルドの KATALA_TUNE_DEV_CONFIRM だけは答える）
#[cfg(not(any(target_os = "macos", windows)))]
pub fn ask(_app: AppHandle, _ok: &'static str) -> Confirm {
    Box::new(|title: String, detail: String| Box::pin(async move { dev_answer(&title, &detail).unwrap_or(false) }))
}

/// 変更操作の確認
pub fn dialog(app: AppHandle) -> Confirm {
    ask(app, RUN)
}

#[cfg(test)]
mod tests {
    #[test]
    fn dev_answer_is_off_without_env() {
        // 環境変数が無ければ答えない（本物のダイアログに進む）
        if std::env::var_os("KATALA_TUNE_DEV_CONFIRM").is_none() {
            assert_eq!(super::dev_answer("t", "d"), None);
        }
    }
}
