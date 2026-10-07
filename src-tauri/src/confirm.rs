//! 変更操作の確認ダイアログ（main.js の dialog.showMessageBox と同じ強さ）。
//! 既定のボタン（Return / Enter）は「やめる」。「実行する」を押したときだけ実行する。
//! それ以外の終わり方（やめる・Esc・閉じる・表示の失敗・想定外の戻り値）はすべて「実行しない」。
//! 確認ダイアログを出せない OS（Linux など）では変更操作をしない。

use tauri::AppHandle;
use tune_core::engine::Confirm;

/// この OS で確認ダイアログを出せるか
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));

#[cfg(any(target_os = "macos", windows))]
mod native {
    use tauri::{AppHandle, Manager};
    use tune_core::engine::Confirm;

    pub const RUN: &str = "実行する";
    pub const CANCEL: &str = "やめる";

    /// 押されたボタンが「実行する」だったときだけ true
    pub fn approved(r: &rfd::MessageDialogResult) -> bool {
        matches!(r, rfd::MessageDialogResult::Custom(s) if s == RUN)
    }

    pub fn dialog(app: AppHandle) -> Confirm {
        Box::new(move |title: String, detail: String| {
            Box::pin(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let parent = app.get_webview_window(crate::window::LABEL);
                let shown = app.run_on_main_thread(move || {
                    // 1番目のボタンが既定（Return）。安全側の「やめる」を1番目に置く
                    let mut d = rfd::AsyncMessageDialog::new()
                        .set_level(rfd::MessageLevel::Warning)
                        .set_title(&title)
                        .set_description(&detail)
                        .set_buttons(rfd::MessageButtons::OkCancelCustom(CANCEL.into(), RUN.into()));
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
                shown.is_ok() && rx.await.is_ok_and(|r| approved(&r))
            })
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rfd::MessageDialogResult as R;

        #[test]
        fn only_the_run_button_approves() {
            assert!(approved(&R::Custom(RUN.into())));
            for r in [R::Custom(CANCEL.into()), R::Cancel, R::Ok, R::Yes, R::No, R::Custom(String::new())] {
                assert!(!approved(&r), "{r:?} で実行してはいけない");
            }
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
pub fn dialog(app: AppHandle) -> Confirm {
    native::dialog(app)
}

/// 確認ダイアログを出せない OS では常に「実行しない」
#[cfg(not(any(target_os = "macos", windows)))]
pub fn dialog(_app: AppHandle) -> Confirm {
    Box::new(|_, _| Box::pin(async { false }))
}
