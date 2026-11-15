//! Native session permission, independent from persisted collection settings.
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};
use tune_core::{
    engine::Engine,
    live::Live,
    monitor::{self, RunMode, RunPhase, RunRequest, RunStatus},
};

pub struct MonitorHost {
    engine: Arc<Engine>,
    live: Arc<Live>,
    rt: Handle,
    dirty: Arc<AtomicBool>,
    state: Mutex<State>,
    request: watch::Sender<RunRequest>,
    status: watch::Sender<RunStatus>,
    started: AtomicBool,
}
struct State {
    worker: Option<JoinHandle<()>>,
    ticker: Option<JoinHandle<()>>,
    exiting: bool,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub started: bool,
    pub mode: RunMode,
    pub status: RunStatus,
    pub streams: usize,
    pub latest_sample: Option<i64>,
}
impl MonitorHost {
    pub fn new(engine: Arc<Engine>, live: Arc<Live>, rt: Handle, dirty: Arc<AtomicBool>) -> Self {
        let (request, _) = watch::channel(RunRequest { mode: RunMode::Paused, pull_seq: 0 });
        let (status, _) = watch::channel(RunStatus { phase: RunPhase::Paused, policy: None, local_collecting: None, hub_configured: false });
        Self {
            engine,
            live,
            rt,
            dirty,
            request,
            status,
            started: AtomicBool::new(false),
            state: Mutex::new(State { worker: None, ticker: None, exiting: false }),
        }
    }
    fn fail(&self) {
        self.request.send_modify(|r| r.mode = RunMode::Paused);
        self.live.release_owned(tune_core::live::Owner::Monitor, &self.live.active(), tune_core::live::Stop::User);
        self.status.send_replace(RunStatus { phase: RunPhase::Failed, policy: None, local_collecting: None, hub_configured: false });
    }
    fn inputs(&self, state: &State) -> Result<(tune_core::nodes::Config, monitor::Settings), String> {
        if state.exiting {
            return Err("アプリを終了しています".into());
        }
        monitor::controlled_inputs(&self.engine).inspect_err(|_| self.fail())
    }
    fn ticker(&self, state: &mut State) {
        if state.ticker.is_none() {
            state.ticker = Some(self.rt.spawn(self.live.clone().run_ticker()));
        }
    }
    fn worker(&self, state: &mut State) {
        self.ticker(state);
        if state.worker.is_none() {
            self.started.store(true, Ordering::Release);
            let dirty = self.dirty.clone();
            state.worker = Some(self.rt.spawn(monitor::run_controlled(
                self.engine.clone(),
                self.live.clone(),
                move |_| {
                    dirty.store(true, Ordering::Release);
                },
                self.request.subscribe(),
                self.status.clone(),
                None,
            )));
        }
    }
    pub fn start_monitor(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_, settings) = self.inputs(&state)?;
        if !settings.enabled && settings.hub.is_none() {
            self.request.send_modify(|r| r.mode = RunMode::Paused);
            return Err("監視設定が無効でハブもありません。設定を保存してから明示的に開始してください".into());
        }
        self.status.send_replace(RunStatus { phase: RunPhase::Starting, policy: None, local_collecting: None, hub_configured: settings.hub.is_some() });
        self.request.send_modify(|r| r.mode = RunMode::Run);
        self.worker(&mut state);
        Ok(())
    }
    pub fn pause_monitor(&self) {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.exiting {
            return;
        }
        if state.worker.is_some() {
            self.status.send_modify(|s| s.phase = RunPhase::Stopping);
        }
        self.request.send_modify(|r| r.mode = RunMode::Paused);
    }
    pub fn request_pull(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_, settings) = self.inputs(&state)?;
        if settings.hub.is_none() {
            return Err("取得するハブが設定されていません".into());
        }
        if self.request.borrow().mode == RunMode::Paused {
            self.status.send_modify(|s| s.phase = RunPhase::Starting);
        }
        self.request.send_modify(|r| r.pull_seq = r.pull_seq.wrapping_add(1));
        self.worker(&mut state);
        Ok(())
    }
    pub fn save_monitor_settings(&self, patch: Value) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.exiting {
            return Err("アプリを終了しています".into());
        }
        let cfg = self.engine.reload_config();
        if let Some(error) = self.engine.config_error() {
            self.fail();
            return Err(error);
        }
        let result = self.engine.with_db(|d| {
            let mut next = monitor::read_settings(d)?.json();
            for key in ["enabled", "hub"] {
                if let Some(value) = patch.get(key) {
                    next[key] = value.clone();
                }
            }
            Ok(monitor::set_settings(d, &cfg, &next))
        });
        match result {
            Ok(Ok(_)) => {
                self.request.send_modify(|_| {});
                self.dirty.store(true, Ordering::Release);
                Ok(())
            }
            Ok(Err(error)) | Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }
    /// Memory-only observation: no DB read, writer, pruning or notification.
    pub fn snapshot(&self) -> Snapshot {
        let active = self.live.active();
        let latest_sample = active.iter().take(200).filter_map(|id| self.live.brief(id)?.pointer("/point/t")?.as_i64()).max();
        Snapshot {
            started: self.started.load(Ordering::Acquire),
            mode: self.request.borrow().mode,
            status: self.status.borrow().clone(),
            streams: active.len(),
            latest_sample,
        }
    }
    /// Manual Live and the monitor share the same lazy ticker.
    pub fn ensure_ticker(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.exiting {
            self.ticker(&mut state);
        }
    }
    /// Called after Application::run returns, while Tokio is still alive. A pull
    /// finishes at its existing transport boundary; this has no overall deadline.
    pub async fn shutdown(&self) -> bool {
        let (worker, ticker) = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.exiting = true;
            self.request.send_modify(|r| r.mode = RunMode::Exit);
            (state.worker.take(), state.ticker.take())
        };
        let joined = match worker {
            Some(worker) => worker.await.is_ok(),
            None => true,
        };
        self.live.shutdown(Duration::from_secs(3)).await;
        if let Some(ticker) = ticker {
            ticker.abort();
            let _ = ticker.await;
        }
        let acknowledged = self.engine.finish_monitor_journal(Duration::from_secs(3)).await;
        joined && acknowledged
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tune_core::live::{Opts, Route, Source};
    struct Refused;
    impl Route for Refused {
        fn open(&self, _: &tune_core::nodes::Node, _: &Opts) -> Result<Source, String> {
            Err("synthetic unavailable".into())
        }
    }
    #[tokio::test]
    async fn refused_start_never_leaves_run_permission_and_save_does_not_start() {
        let dir = std::env::temp_dir().join(format!("monitor-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[{"id":"fixture","os":"macos"}]}"#).unwrap();
        let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
        engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":false}))).unwrap();
        let live = Live::new(Arc::new(Refused), Opts { max_retries: 0, ..Opts::default() }, Box::new(|_| {}));
        let host = MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false)));
        for _ in 0..3 {
            assert!(!host.snapshot().started);
        }
        assert!(host.start_monitor().is_err(), "disabled/no hub must reject without granting Run");
        assert!(host.request_pull().is_err(), "no hub cannot start a pull worker");
        host.save_monitor_settings(json!({"enabled":true})).unwrap();
        assert!(!host.snapshot().started);
        assert_eq!(host.snapshot().mode, RunMode::Paused);
        assert_eq!(engine.monitor_journal_health()["running"], false);
        host.start_monitor().unwrap();
        let worker_id = host.state.lock().unwrap().worker.as_ref().unwrap().id();
        let ticker_id = host.state.lock().unwrap().ticker.as_ref().unwrap().id();
        host.start_monitor().unwrap();
        host.pause_monitor();
        host.start_monitor().unwrap();
        assert_eq!(host.state.lock().unwrap().worker.as_ref().unwrap().id(), worker_id);
        assert_eq!(host.state.lock().unwrap().ticker.as_ref().unwrap().id(), ticker_id);
        std::fs::write(dir.join("nodes.json"), "invalid").unwrap();
        assert!(host.save_monitor_settings(json!({"enabled":false})).is_err());
        assert_eq!(host.snapshot().status.phase, RunPhase::Failed);
        assert_eq!(host.snapshot().status.local_collecting, None);
        assert_eq!(host.snapshot().mode, RunMode::Paused);
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[{"id":"fixture","os":"macos"}]}"#).unwrap();
        engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":false,"hub":"removed"}))).unwrap();
        host.save_monitor_settings(json!({"hub":null})).expect("saving must permit repair of a missing hub");
        assert_eq!(host.snapshot().mode, RunMode::Paused);
        host.shutdown().await;
        assert_eq!(engine.monitor_journal_health()["running"], false);
        assert!(host.start_monitor().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    struct Notifications(Arc<AtomicBool>);
    impl tune_core::engine::Host for Notifications {
        fn notify(&self, _: &[tune_core::db::Change]) {
            self.0.store(true, Ordering::Release);
        }
        fn checks_updated(&self) {
            self.0.store(true, Ordering::Release);
        }
        fn state_changed(&self) {
            self.0.store(true, Ordering::Release);
        }
    }
    #[tokio::test]
    async fn snapshot_does_not_wait_for_settings_database_and_reads_start_nothing() {
        let dir = std::env::temp_dir().join(format!("monitor-observe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
        let dirty = Arc::new(AtomicBool::new(false));
        let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(Notifications(dirty.clone()))).unwrap();
        let live = Live::new(Arc::new(Refused), Opts::default(), Box::new(|_| {}));
        dirty.store(false, Ordering::Release);
        let host = Arc::new(MonitorHost::new(engine.clone(), live, Handle::current(), dirty.clone()));
        let records = engine.monitor_events(&tune_core::monitor_journal::Filter { limit: Some(50), ..Default::default() }).unwrap();
        assert!(records["events"].as_array().unwrap().is_empty());
        assert_eq!(records["health"]["running"], false);
        assert!(!dirty.load(Ordering::Acquire));
        assert!(!host.snapshot().started);
        assert!(host.state.lock().unwrap().ticker.is_none());
        let db = engine.db.lock().unwrap();
        let h = host.clone();
        let start = std::thread::spawn(move || h.start_monitor());
        // Observe the real operation reaching its DB boundary, without a fake host.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while host.state.try_lock().is_ok() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let (send, recv) = std::sync::mpsc::channel();
        let h = host.clone();
        let observe = std::thread::spawn(move || send.send(h.snapshot()).unwrap());
        let result = recv.recv_timeout(Duration::from_millis(100));
        drop(db);
        assert!(start.join().unwrap().is_err());
        observe.join().unwrap();
        host.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
        assert!(result.is_ok(), "status observation must not wait on settings IO");
    }
    #[tokio::test]
    async fn shutdown_reports_missing_journal_ack() {
        let dir = std::env::temp_dir().join(format!("monitor-shutdown-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
        let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
        engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true}))).unwrap();
        let live = Live::new(Arc::new(Refused), Opts::default(), Box::new(|_| {}));
        let host = MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false)));
        host.start_monitor().unwrap();
        let mut status = host.status.subscribe();
        tokio::time::timeout(Duration::from_secs(2), async {
            while status.borrow_and_update().phase != RunPhase::Running {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        engine.with_db(|d| d.conn().execute_batch("DROP TABLE monitor_events")).unwrap();
        assert!(!host.shutdown().await, "final shutdown must not report an unconfirmed journal commit as success");
        assert!(engine.monitor_journal_health()["unconfirmed_acks"].as_u64().unwrap() > 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
