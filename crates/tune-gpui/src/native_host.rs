//! Engine's OS host owns no Engine, GPUI entity, or main-thread sender.
use crate::os::{Effect, Observation, Service, State};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::runtime::Handle;
use tune_core::{
    db::Change,
    engine::{self, Host},
};

#[derive(Clone, Copy)]
pub enum Action {
    Refresh,
    Permission,
    Settings,
    Test,
    Login(bool),
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub observed: Observation,
    pub queried_at: Option<i64>,
    pub busy: bool,
    pub action: Option<(i64, Effect)>,
    pub notification: Option<(i64, Effect)>,
}
pub struct NativeHost {
    dirty: Arc<AtomicBool>,
    service: Arc<dyn Service>,
    rt: Handle,
    cache: Arc<Mutex<Snapshot>>,
}
impl NativeHost {
    pub fn new(dirty: Arc<AtomicBool>, service: Arc<dyn Service>, rt: Handle) -> Self {
        Self {
            dirty,
            service,
            rt,
            cache: Arc::new(Mutex::new(Snapshot {
                observed: Observation { source: "OS 未照会".into(), notification: State::Unknown, login: State::Unknown },
                queried_at: None,
                busy: false,
                action: None,
                notification: None,
            })),
        }
    }
    /// Only the cache is read, including by Engine::compute_checks.
    pub fn snapshot(&self) -> Snapshot {
        self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
    pub fn act(&self, action: Action) {
        {
            let mut cache = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if cache.busy {
                return;
            }
            cache.busy = true;
            self.dirty.store(true, Ordering::Release);
        }
        let (service, cache, dirty) = (self.service.clone(), self.cache.clone(), self.dirty.clone());
        self.rt.spawn(async move {
            let (send, receive) = tokio::sync::oneshot::channel();
            let work = service.clone();
            tokio::task::spawn_blocking(move || {
                if matches!(action, Action::Refresh) {
                    work.query(Box::new(move |value| {
                        let _ = send.send((None, Some(value)));
                    }));
                } else {
                    let callback = Box::new(move |effect| {
                        let _ = send.send((Some(effect), None));
                    });
                    match action {
                        Action::Permission => work.permission(callback),
                        Action::Settings => work.settings(callback),
                        Action::Test => work.notify(
                            "Katala Tune · 合成テスト通知".into(),
                            "明示操作による合成テストです。実機の異常や収集結果ではありません。".into(),
                            callback,
                        ),
                        Action::Login(enabled) => work.login(enabled, callback),
                        Action::Refresh => unreachable!(),
                    }
                }
            });
            // OS callbacks, never UI callbacks. A stalled OS query cannot leave
            // the controls permanently busy. Do not fabricate observed success.
            let response = tokio::time::timeout(std::time::Duration::from_secs(30), receive).await;
            let (effect, observation) =
                response.ok().and_then(Result::ok).unwrap_or_else(|| (Some(Effect::Failed("OS 応答を確認できません。再照会してください".into())), None));
            let mut value = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            value.busy = false;
            value.action = effect.map(|e| (tune_core::db::now_ms(), e));
            if let Some(observed) = observation {
                value.observed = observed;
                value.queried_at = Some(tune_core::db::now_ms());
            }
            // Mutations require a new explicit query. Cached prior observation
            // retains its timestamp and is never replaced by assumed success.
            dirty.store(true, Ordering::Release);
        });
    }
}
impl Host for NativeHost {
    fn notify(&self, important: &[Change]) {
        if important.is_empty() {
            return;
        }
        let (title, body) = engine::notification_text(important);
        let (service, cache, dirty) = (self.service.clone(), self.cache.clone(), self.dirty.clone());
        // Producer commits and quit drain never await OS delivery or GPUI.
        self.rt.spawn_blocking(move || {
            service.notify(
                title,
                body,
                Box::new(move |effect| {
                    cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).notification = Some((tune_core::db::now_ms(), effect));
                    dirty.store(true, Ordering::Release);
                }),
            )
        });
    }
    fn open_at_login(&self) -> bool {
        self.open_at_login_state() == Some(true)
    }
    fn open_at_login_state(&self) -> Option<bool> {
        self.snapshot().observed.login.observed()
    }
    fn probe_result(&self, _: &serde_json::Value) {
        self.dirty.store(true, Ordering::Release);
    }
    fn logs_synced(&self, _: &serde_json::Value) {
        self.dirty.store(true, Ordering::Release);
    }
    fn inventory_result(&self, _: &serde_json::Value) {
        self.dirty.store(true, Ordering::Release);
    }
    fn ai_synced(&self, _: &serde_json::Value) {
        self.dirty.store(true, Ordering::Release);
    }
    fn topology_updated(&self, _: &serde_json::Value) {
        self.dirty.store(true, Ordering::Release);
    }
    fn checks_updated(&self) {
        self.dirty.store(true, Ordering::Release);
    }
    fn state_changed(&self) {
        self.dirty.store(true, Ordering::Release);
    }
}
#[cfg(test)]
#[path = "native_host_tests.rs"]
mod tests;
