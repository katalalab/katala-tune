//! 全文は高さを制限した画面で読み、最後の承認は既存のネイティブダイアログで行う。
//! 画面の返答は承認ではない。Rust 側で保持した Prepared だけを、その後の確認・起動に使う。
use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

#[derive(Default)]
pub struct Review {
    pub gate: tokio::sync::Mutex<()>,
    lifecycle: Mutex<Lifecycle>,
}

#[derive(Default)]
struct Lifecycle {
    generation: u64,
    ready: bool,
    pending: Option<(String, oneshot::Sender<bool>)>,
}

impl Review {
    /// 確認画面を失ったときは待機を終える。ネイティブ承認へは進まない。
    pub fn cancel(&self) {
        if let Ok(mut state) = self.lifecycle.lock() {
            state.generation = state.generation.wrapping_add(1);
            state.ready = false;
            state.pending.take();
        }
    }

    pub fn ready(&self) {
        if let Ok(mut state) = self.lifecycle.lock() {
            state.ready = true;
        }
    }

    pub fn generation(&self) -> Option<u64> {
        self.lifecycle.lock().ok().and_then(|state| state.ready.then_some(state.generation))
    }

    fn register(&self, generation: u64, id: String, tx: oneshot::Sender<bool>) -> bool {
        let Ok(mut state) = self.lifecycle.lock() else { return false };
        if !state.ready || state.generation != generation {
            return false;
        }
        state.pending = Some((id, tx));
        true
    }

    fn answer(&self, id: &str, proceed: bool) -> bool {
        let Ok(mut state) = self.lifecycle.lock() else { return false };
        if state.pending.as_ref().is_none_or(|(key, _)| key != id) {
            return false;
        }
        state.pending.take().is_some_and(|(_, tx)| tx.send(proceed).is_ok())
    }

    pub async fn show(&self, app: &AppHandle, generation: u64, id: String, title: String, detail: String, prompt: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        if !self.register(generation, id.clone(), tx) {
            return false;
        }
        let sent = app.emit("handoff-review", json!({"id": id, "title": title, "detail": detail, "prompt": prompt})).is_ok();
        let proceed = sent && tokio::time::timeout(std::time::Duration::from_secs(600), rx).await.is_ok_and(|r| r == Ok(true));
        if let Ok(mut state) = self.lifecycle.lock() {
            state.pending.take();
        }
        proceed
    }
}

#[tauri::command]
pub fn ai_handoff_review(review: State<'_, Review>, id: String, proceed: bool) -> bool {
    review.answer(&id, proceed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_are_matched_and_consumed_once_and_cancel_is_not_approval() {
        let review = Review::default();
        review.ready();
        let (tx, rx) = oneshot::channel();
        assert!(review.register(review.generation().unwrap(), "request-a".into(), tx));
        assert!(!review.answer("old-request", true));
        assert!(review.answer("request-a", false));
        assert!(!rx.await.unwrap());
        assert!(!review.answer("request-a", true));
    }

    #[tokio::test]
    async fn losing_the_review_window_wakes_the_waiter_and_allows_a_new_request() {
        let review = Review::default();
        review.ready();
        let (tx, rx) = oneshot::channel();
        assert!(review.register(review.generation().unwrap(), "closed-window".into(), tx));
        review.cancel();
        assert!(rx.await.is_err());
        assert!(!review.answer("closed-window", true));
        review.ready();
        let (tx, rx) = oneshot::channel();
        assert!(review.register(review.generation().unwrap(), "new-window".into(), tx));
        assert!(review.answer("new-window", true));
        assert!(rx.await.unwrap());
    }

    #[test]
    fn a_window_lost_during_preparation_cannot_register_a_late_preview() {
        let review = Review::default();
        assert!(review.generation().is_none());
        review.ready();
        let before_preparation = review.generation().unwrap();
        review.cancel();
        let (tx, _) = oneshot::channel();
        assert!(!review.register(before_preparation, "gone".into(), tx));
        review.ready();
        let (tx, _) = oneshot::channel();
        assert!(!review.register(before_preparation, "old-page".into(), tx));
    }
}
