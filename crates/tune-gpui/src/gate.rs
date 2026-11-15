//! 変更操作の承認の関門。tune-core の `Confirm`（見出しと本文を見せ、操作者が「実行する」を選んだときだけ true）を、
//! GPUI の画面の確認欄につなぐ。GPUI に依存しないので、画面なしでテストできる。
//!
//! 進むのは `answer(true)` が呼ばれたときだけ。やめる・背景を押す・別の確認に置き換わる・時間切れ・
//! アプリの終了（送り手が捨てられる）はすべて「進まない」。

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;
use tune_core::engine::Confirm;

/// 確認を放っておいたら進まないことにする時間
pub const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub title: String,
    pub detail: String,
}

struct Slot {
    id: u64,
    pending: Pending,
    tx: oneshot::Sender<bool>,
}

#[derive(Default)]
struct State {
    next_id: u64,
    slot: Option<Slot>,
}

#[derive(Clone)]
pub struct Gate {
    state: Arc<Mutex<State>>,
    /// 確認が出た・消えたことを画面に知らせる（描き直しの合図）
    on_change: Arc<dyn Fn() + Send + Sync>,
    timeout: Duration,
}

impl Gate {
    pub fn new(on_change: impl Fn() + Send + Sync + 'static) -> Self {
        Self { state: Arc::default(), on_change: Arc::new(on_change), timeout: TIMEOUT }
    }

    #[cfg(test)]
    fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 画面に出す確認（なければ None）
    pub fn pending(&self) -> Option<Pending> {
        self.lock().slot.as_ref().map(|s| s.pending.clone())
    }

    /// 操作者の答え。確認が出ていなければ何もしない
    pub fn answer(&self, ok: bool) {
        let slot = self.lock().slot.take();
        if let Some(s) = slot {
            let _ = s.tx.send(ok);
            (self.on_change)();
        }
    }

    /// Engine に渡す確認。呼ばれたら画面に確認を出し、答えを待つ。
    /// すでに別の確認が出ていたら、前の確認は「進まない」で終わる（送り手が捨てられる）
    pub fn confirm(&self) -> Confirm {
        let this = self.clone();
        Box::new(move |title: String, detail: String| {
            Box::pin(async move {
                let (tx, rx) = oneshot::channel();
                let id = {
                    let mut st = this.lock();
                    st.next_id += 1;
                    let id = st.next_id;
                    st.slot = Some(Slot { id, pending: Pending { title, detail }, tx });
                    id
                };
                (this.on_change)();
                let answer = tokio::time::timeout(this.timeout, rx).await;
                {
                    // 時間切れ・置き換わりのとき、自分の確認が残っていれば片付ける
                    let mut st = this.lock();
                    if st.slot.as_ref().is_some_and(|s| s.id == id) {
                        st.slot = None;
                    }
                }
                (this.on_change)();
                matches!(answer, Ok(Ok(true)))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn gate() -> (Gate, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        (Gate::new(move || _ = c.fetch_add(1, Ordering::SeqCst)), n)
    }

    async fn wait_pending(g: &Gate) -> Pending {
        for _ in 0..200 {
            if let Some(p) = g.pending() {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("確認が出なかった");
    }

    #[tokio::test]
    async fn only_an_explicit_approval_goes_ahead() {
        let (g, changes) = gate();
        let run = tokio::spawn((g.confirm())("見出し".into(), "本文".into()));
        assert_eq!(wait_pending(&g).await, Pending { title: "見出し".into(), detail: "本文".into() });
        g.answer(true);
        assert!(run.await.unwrap());
        assert!(g.pending().is_none());
        assert!(changes.load(Ordering::SeqCst) >= 2, "出たときと消えたときに画面へ知らせる");
    }

    #[tokio::test]
    async fn cancel_does_not_go_ahead() {
        let (g, _) = gate();
        let run = tokio::spawn((g.confirm())("t".into(), "d".into()));
        wait_pending(&g).await;
        g.answer(false);
        assert!(!run.await.unwrap());
        assert!(g.pending().is_none());
    }

    #[tokio::test]
    async fn timeout_does_not_go_ahead_and_clears_the_prompt() {
        let (g, _) = gate();
        let g = g.with_timeout(Duration::from_millis(30));
        let ok = (g.confirm())("t".into(), "d".into()).await;
        assert!(!ok);
        assert!(g.pending().is_none());
    }

    #[tokio::test]
    async fn a_newer_prompt_cancels_the_older_one() {
        let (g, _) = gate();
        let first = tokio::spawn((g.confirm())("一つ目".into(), "d".into()));
        assert_eq!(wait_pending(&g).await.title, "一つ目");
        let second = tokio::spawn((g.confirm())("二つ目".into(), "d".into()));
        for _ in 0..200 {
            if g.pending().is_some_and(|p| p.title == "二つ目") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!first.await.unwrap(), "置き換えられた確認は進まない");
        g.answer(true);
        assert!(second.await.unwrap());
    }

    #[tokio::test]
    async fn answering_with_nothing_pending_is_harmless() {
        let (g, changes) = gate();
        g.answer(true);
        assert_eq!(changes.load(Ordering::SeqCst), 0);
        assert!(g.pending().is_none());
    }

    // ---- Engine との結合: 関門を通した確認で、取り消し・拒否のときに何も実行されない ----

    use serde_json::json;
    use tune_core::engine::{Engine, NoHost};

    fn engine(nodes: serde_json::Value) -> (Arc<Engine>, std::path::PathBuf) {
        // 同じ時刻に始まる別のテストと同じ場所にならないよう、連番を付ける
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("kt-gate-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("nodes.json");
        std::fs::write(&cfg, json!({ "protect": [], "nodes": nodes }).to_string()).unwrap();
        (Engine::open(cfg, dir.join("data"), Arc::new(NoHost)).unwrap(), dir)
    }

    fn kickstart() -> serde_json::Value {
        json!({ "type": "launchd-kickstart", "params": { "label": "com.example.job" } })
    }

    #[tokio::test]
    async fn engine_does_not_run_when_the_operator_cancels() {
        let (e, dir) = engine(json!([{ "id": "box", "alias": "box-a", "os": "macos" }]));
        let (g, _) = gate();
        let (e2, g2) = (e.clone(), g.clone());
        let run = tokio::spawn(async move { e2.confirm_and_run("box", &kickstart(), "テスト", None, g2.confirm()).await });
        let p = wait_pending(&g).await;
        assert!(p.detail.contains("box") || !p.detail.is_empty(), "承認の前に、何をするかが見える");
        g.answer(false);
        let r = run.await.unwrap().unwrap();
        assert_eq!(r["cancelled"], json!(true), "{r}");
        assert!(e.with_db(|d| d.actions(10)).unwrap().iter().all(|a| a["ok"] != json!(true)), "実行記録に成功が残らない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn engine_refuses_shared_nodes_without_asking() {
        let (e, dir) = engine(json!([{ "id": "box", "alias": "box-a", "os": "macos", "shared": true }]));
        let (g, _) = gate();
        let r = e.confirm_and_run("box", &kickstart(), "テスト", None, g.confirm()).await.unwrap();
        assert!(r["refused"].as_str().is_some_and(|s| s.contains("共用機")), "{r}");
        assert!(g.pending().is_none(), "拒否するものは確認も出さない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn engine_does_not_run_when_the_prompt_times_out() {
        let (e, dir) = engine(json!([{ "id": "box", "alias": "box-a", "os": "macos" }]));
        let (g, _) = gate();
        let g = g.with_timeout(Duration::from_millis(40));
        let r = e.confirm_and_run("box", &kickstart(), "テスト", None, g.confirm()).await.unwrap();
        assert_eq!(r["cancelled"], json!(true), "{r}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
