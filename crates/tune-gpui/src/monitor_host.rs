//! App-owned collection workers and native session permission.
use crate::collection::{self, Job, Kind, Receipt};
use serde_json::Value;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};
use tune_core::{
    engine::Engine,
    gui_collection::ensure_gui_collection_owner,
    helpers::native_install::{self, InstallError, InstallOutcome, LocalInstallReceipt, NativeTargetSnapshot, Operation, PreparedNativeSampler},
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
    schedule_request: watch::Sender<bool>,
    schedule_status: watch::Sender<ScheduleStatus>,
    drain: tokio::sync::Mutex<Option<ShutdownReport>>,
}
struct State {
    manual: Option<JoinHandle<bool>>,
    worker: Option<JoinHandle<()>>,
    ticker: Option<JoinHandle<()>>,
    scheduler: Option<JoinHandle<()>>,
    scheduler_in_flight: bool,
    exiting: bool,
    ever_collection_admitted: bool,
    install_job: Option<JoinHandle<()>>,
    install_result: Option<Arc<NativeInstallResult>>,
    install_confirmed: Option<bool>,
    install_phase: InstallPhase,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum InstallPhase {
    Idle,
    Running,
    RecoveryRequired,
}
#[derive(Debug)]
pub(crate) enum NativeInstallResult {
    Completed(Result<LocalInstallReceipt, InstallError>),
    WorkerFailed,
}
impl NativeInstallResult {
    fn confirmed(&self) -> bool {
        match self {
            Self::Completed(Ok(_)) => true,
            Self::Completed(Err(error)) => error.outcome() != InstallOutcome::RecoveryRequired && error.operation() != Operation::Close,
            Self::WorkerFailed => false,
        }
    }
}
pub(crate) struct NativeInstallSnapshot {
    pub(crate) ever_collection_admitted: bool,
    pub(crate) busy: bool,
    pub(crate) recovery_required: bool,
    pub(crate) result: Option<Arc<NativeInstallResult>>,
    pub(crate) unavailable: Option<String>,
}

/// These are local completion/commit acknowledgements, never proof that a
/// remote producer no longer exists. None means that worker was never started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownReport {
    pub manual_complete: Option<bool>,
    pub monitor_complete: Option<bool>,
    pub scheduler_complete: Option<bool>,
    pub journal_ack: Option<bool>,
    pub install_complete: Option<bool>,
    pub install_recovery_required: bool,
}
impl ShutdownReport {
    pub fn confirmed(self) -> bool {
        self.install_complete != Some(false)
            && !self.install_recovery_required
            && self.scheduler_complete != Some(false)
            && self.manual_complete != Some(false)
            && self.monitor_complete != Some(false)
            && self.journal_ack == Some(true)
    }
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub started: bool,
    pub mode: RunMode,
    pub status: RunStatus,
    pub streams: usize,
    pub latest_sample: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchedulePhase {
    Idle,
    Waiting,
    Running,
    ManualBusy,
    Stopping,
    Failed,
    Exited,
}
#[derive(Clone, Debug)]
pub struct ScheduleStatus {
    pub phase: SchedulePhase,
    pub cycles: u64,
    pub last_completed: Option<i64>,
    pub error: Option<String>,
}
impl MonitorHost {
    pub fn new(engine: Arc<Engine>, live: Arc<Live>, rt: Handle, dirty: Arc<AtomicBool>) -> Self {
        let (request, _) = watch::channel(RunRequest { mode: RunMode::Paused, pull_seq: 0, start_seq: 0 });
        let (status, _) = watch::channel(RunStatus { phase: RunPhase::Paused, policy: None, local_collecting: None, hub_configured: false });
        let (schedule_request, _) = watch::channel(false);
        let (schedule_status, _) = watch::channel(ScheduleStatus { phase: SchedulePhase::Idle, cycles: 0, last_completed: None, error: None });
        Self {
            schedule_request,
            schedule_status,
            engine,
            live,
            rt,
            dirty,
            request,
            status,
            started: AtomicBool::new(false),
            state: Mutex::new(State {
                manual: None,
                worker: None,
                ticker: None,
                scheduler: None,
                scheduler_in_flight: false,
                exiting: false,
                ever_collection_admitted: false,
                install_job: None,
                install_result: None,
                install_confirmed: None,
                install_phase: InstallPhase::Idle,
            }),
            drain: tokio::sync::Mutex::new(None),
        }
    }
    fn install_busy(state: &State) -> bool {
        state.install_phase == InstallPhase::Running || state.install_job.as_ref().is_some_and(|job| !job.is_finished())
    }
    fn collection_allowed(state: &State) -> Result<(), String> {
        if state.exiting {
            return Err("アプリを終了しています".into());
        }
        if state.install_phase == InstallPhase::RecoveryRequired {
            return Err("収集ヘルパーの状態を確認できません。導入結果の回復情報を確認してください".into());
        }
        if Self::install_busy(state) {
            return Err("収集ヘルパーを導入しています。完了をお待ちください".into());
        }
        Ok(())
    }
    fn install_allowed(state: &State) -> Result<(), String> {
        Self::collection_allowed(state)?;
        if state.ever_collection_admitted {
            return Err("収集・診断を開始済みです。アプリを終了し、収集・診断を始めずに開き直してください".into());
        }
        Ok(())
    }
    pub(crate) fn native_install_snapshot(&self) -> NativeInstallSnapshot {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        NativeInstallSnapshot {
            ever_collection_admitted: state.ever_collection_admitted,
            busy: Self::install_busy(&state),
            recovery_required: state.install_phase == InstallPhase::RecoveryRequired,
            result: state.install_result.clone(),
            unavailable: Self::install_allowed(&state).err(),
        }
    }
    pub(crate) fn admit_native_install(
        self: &Arc<Self>,
        prepared: PreparedNativeSampler,
        expected: NativeTargetSnapshot,
        gate: &crate::gate::Gate,
    ) -> Result<(), String> {
        self.admit_install_work(gate, move || native_install::install_native_sampler(prepared, expected))
    }
    #[cfg(test)]
    pub(crate) fn start_fixture_install(
        self: &Arc<Self>,
        gate: &crate::gate::Gate,
        work: impl FnOnce() -> Result<LocalInstallReceipt, InstallError> + Send + 'static,
    ) -> Result<(), String> {
        self.admit_install_work(gate, work)
    }
    fn record_install(state: &mut State, result: NativeInstallResult) {
        let confirmed = result.confirmed();
        state.install_confirmed = Some(state.install_confirmed.unwrap_or(true) && confirmed);
        if !confirmed || state.install_phase == InstallPhase::RecoveryRequired {
            state.install_phase = InstallPhase::RecoveryRequired;
        } else {
            state.install_phase = InstallPhase::Idle;
        }
        state.install_result = Some(Arc::new(result));
    }
    fn admit_install_work(
        self: &Arc<Self>,
        gate: &crate::gate::Gate,
        work: impl FnOnce() -> Result<LocalInstallReceipt, InstallError> + Send + 'static,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::install_allowed(&state)?;
        ensure_gui_collection_owner().map_err(|error| error.to_string())?;
        let _admission = gate.admit()?;
        state.install_phase = InstallPhase::Running;
        let host = self.clone();
        state.install_job = Some(self.rt.spawn(async move {
            // Registration and Gate admission finish before any transaction IO.
            drop(host.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            let result = match tokio::task::spawn_blocking(work).await {
                Ok(result) => NativeInstallResult::Completed(result),
                Err(_) => NativeInstallResult::WorkerFailed,
            };
            Self::record_install(&mut host.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner), result);
            host.dirty.store(true, Ordering::Release);
        }));
        Ok(())
    }
    /// Host -> Gate -> UI Live -> core Live. The callback must be memory-only.
    pub(crate) fn admit_live_request(&self, gate: &crate::gate::Gate, commit: impl FnOnce() -> Option<(u64, String)>) -> Result<Option<(u64, String)>, String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::collection_allowed(&state)?;
        ensure_gui_collection_owner().map_err(|error| error.to_string())?;
        let _admission = gate.admit()?;
        let request = commit();
        if request.is_some() {
            state.ever_collection_admitted = true;
            self.ticker(&mut state);
        }
        Ok(request)
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
                self.request.clone(),
                self.status.clone(),
                None,
            )));
        }
    }
    pub fn start_monitor_from_ui(&self, gate: &crate::gate::Gate) -> Result<(), String> {
        self.start_monitor_admitted(Some(gate))
    }
    pub fn request_pull_from_ui(&self, gate: &crate::gate::Gate) -> Result<(), String> {
        self.request_pull_admitted(Some(gate))
    }
    pub fn start_schedule_from_ui(self: &Arc<Self>, gate: &crate::gate::Gate) -> Result<(), String> {
        self.start_schedule_admitted(Some(gate), |engine| async move { engine.checked_tick().await })
    }
    // Preserve app-owned callers; per-window UI uses the gated entry point.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_monitor(&self) -> Result<(), String> {
        self.start_monitor_admitted(None)
    }
    fn start_monitor_admitted(&self, gate: Option<&crate::gate::Gate>) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::collection_allowed(&state)?;
        let (_, settings) = self.inputs(&state)?;
        if !settings.enabled && settings.hub.is_none() {
            self.request.send_modify(|r| r.mode = RunMode::Paused);
            return Err("監視設定が無効でハブもありません。設定を保存してから明示的に開始してください".into());
        }
        // Host state -> blocking validation -> Gate -> memory-only permission.
        // close() never takes Host state, and no Gate lock spans the DB wait.
        ensure_gui_collection_owner().map_err(|e| e.to_string())?;
        let _admission = gate.map(crate::gate::Gate::admit).transpose()?;
        self.request.send_modify(|r| {
            r.start_seq = r.start_seq.wrapping_add(1);
            r.mode = RunMode::Run;
            self.status.send_replace(RunStatus { phase: RunPhase::Starting, policy: None, local_collecting: None, hub_configured: settings.hub.is_some() });
        });
        self.worker(&mut state);
        state.ever_collection_admitted = true;
        Ok(())
    }
    pub fn pause_monitor(&self) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.exiting {
            return Err("アプリを終了しています".into());
        }
        if state.worker.is_some() {
            self.status.send_modify(|s| s.phase = RunPhase::Stopping);
        }
        self.request.send_modify(|r| r.mode = RunMode::Paused);
        Ok(())
    }
    // Preserve app-owned callers; per-window UI uses the gated entry point.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn request_pull(&self) -> Result<(), String> {
        self.request_pull_admitted(None)
    }
    fn request_pull_admitted(&self, gate: Option<&crate::gate::Gate>) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::collection_allowed(&state)?;
        let (_, settings) = self.inputs(&state)?;
        if settings.hub.is_none() {
            return Err("取得するハブが設定されていません".into());
        }
        ensure_gui_collection_owner().map_err(|e| e.to_string())?;
        let _admission = gate.map(crate::gate::Gate::admit).transpose()?;
        if self.request.borrow().mode == RunMode::Paused {
            self.status.send_modify(|s| s.phase = RunPhase::Starting);
        }
        self.request.send_modify(|r| r.pull_seq = r.pull_seq.wrapping_add(1));
        self.worker(&mut state);
        state.ever_collection_admitted = true;
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
    fn schedule_failure(&self, error: String) {
        self.schedule_request.send_replace(false);
        self.schedule_status.send_modify(|s| {
            s.phase = SchedulePhase::Failed;
            s.error = Some(error);
        });
        self.dirty.store(true, Ordering::Release);
    }
    pub fn save_schedule_settings(&self, patch: Value) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.exiting {
            return Err("アプリを終了しています".into());
        }
        let result = self.engine.checked_schedule().and_then(|_| self.engine.set_schedule(&patch));
        match result {
            Ok(settings) => {
                if settings["enabled"] == false {
                    self.schedule_request.send_replace(false);
                }
                self.dirty.store(true, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                self.schedule_failure(error.clone());
                Err(error)
            }
        }
    }
    pub fn schedule_snapshot(&self) -> (bool, ScheduleStatus) {
        (*self.schedule_request.borrow(), self.schedule_status.borrow().clone())
    }
    // Preserve app-owned callers; per-window UI uses the gated entry point.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_schedule(self: &Arc<Self>) -> Result<(), String> {
        self.start_schedule_admitted(None, |engine| async move { engine.checked_tick().await })
    }
    #[cfg(test)]
    fn start_schedule_with<F, Fut>(self: &Arc<Self>, tick: F) -> Result<(), String>
    where
        F: Fn(Arc<Engine>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send,
    {
        self.start_schedule_admitted(None, tick)
    }
    fn start_schedule_admitted<F, Fut>(self: &Arc<Self>, gate: Option<&crate::gate::Gate>, tick: F) -> Result<(), String>
    where
        F: Fn(Arc<Engine>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send,
    {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::collection_allowed(&state)?;
        let settings = self.engine.checked_schedule().inspect_err(|e| self.schedule_failure(e.clone()))?;
        if settings["enabled"] != true {
            self.schedule_failure("自動スキャン設定は無効です。保存後に開始してください".into());
            return Err("自動スキャン設定は無効です。保存後に開始してください".into());
        }
        ensure_gui_collection_owner().map_err(|e| e.to_string())?;
        let _admission = gate.map(crate::gate::Gate::admit).transpose()?;
        if !*self.schedule_request.borrow() {
            self.schedule_status.send_modify(|s| {
                s.phase = if state.scheduler_in_flight { SchedulePhase::Running } else { SchedulePhase::Waiting };
                s.error = None;
            });
            self.schedule_request.send_replace(true);
        }
        if state.scheduler.is_none() {
            let weak = Arc::downgrade(self);
            let mut request = self.schedule_request.subscribe();
            state.scheduler = Some(self.rt.spawn(async move {
                loop {
                    let Some(host) = weak.upgrade() else { return };
                    if host.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).exiting {
                        return;
                    }
                    drop(host);
                    while !*request.borrow_and_update() {
                        if request.changed().await.is_err() {
                            return;
                        }
                        let Some(host) = weak.upgrade() else { return };
                        if host.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).exiting {
                            return;
                        }
                    }
                    let mut cadence = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(5), Duration::from_secs(60));
                    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            biased;
                            result = request.changed() => {
                                if result.is_err() { return; }
                                break;
                            }
                            _ = cadence.tick() => {}
                        }
                        let Some(host) = weak.upgrade() else { return };
                        // Admission and manual state share the host lock. A claimed
                        // cycle is owned work; Stop drains it instead of cancelling.
                        let admitted = {
                            let mut state = host.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            if state.exiting {
                                return;
                            }
                            if !*host.schedule_request.borrow() {
                                false
                            } else if state.manual.as_ref().is_some_and(|t| !t.is_finished()) {
                                host.schedule_status.send_modify(|s| s.phase = SchedulePhase::ManualBusy);
                                false
                            } else {
                                state.scheduler_in_flight = true;
                                host.schedule_status.send_modify(|s| s.phase = SchedulePhase::Running);
                                true
                            }
                        };
                        if admitted {
                            // Checked current ledger/settings on every cycle, even
                            // with injected synthetic work. No last-good admission.
                            let checked = host.engine.checked_schedule();
                            let result = match checked {
                                Ok(settings) if settings["enabled"] == true => tick(host.engine.clone()).await,
                                Ok(_) => Err("自動スキャン設定が無効になりました".into()),
                                Err(error) => Err(error),
                            };
                            // Serialize the actual completion boundary with Start/Stop.
                            let mut state = host.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            state.scheduler_in_flight = false;
                            match result {
                                Ok(()) => host.schedule_status.send_modify(|s| {
                                    s.cycles += 1;
                                    s.last_completed = Some(tune_core::db::now_ms());
                                    s.phase = if *host.schedule_request.borrow() { SchedulePhase::Waiting } else { SchedulePhase::Idle };
                                }),
                                Err(error) => host.schedule_failure(error),
                            }
                        }
                        host.dirty.store(true, Ordering::Release);
                        if host.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).exiting {
                            host.schedule_status.send_modify(|s| s.phase = SchedulePhase::Exited);
                            return;
                        }
                        if !*host.schedule_request.borrow() {
                            break;
                        }
                    }
                    let Some(host) = weak.upgrade() else { return };
                    let state = host.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.exiting {
                        return;
                    }
                    host.schedule_status.send_modify(|s| {
                        if s.phase != SchedulePhase::Failed {
                            s.phase = if *host.schedule_request.borrow() { SchedulePhase::Waiting } else { SchedulePhase::Idle };
                        }
                    });
                }
            }));
        }
        state.ever_collection_admitted = true;
        Ok(())
    }
    pub fn stop_schedule(&self) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.exiting {
            return Err("アプリを終了しています".into());
        }
        self.schedule_request.send_replace(false);
        self.schedule_status.send_modify(|s| s.phase = if state.scheduler_in_flight { SchedulePhase::Stopping } else { SchedulePhase::Idle });
        Ok(())
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
    #[cfg(test)]
    pub fn ensure_ticker(&self) -> Result<(), String> {
        self.ensure_ticker_admitted(None)
    }
    #[cfg(test)]
    pub fn ensure_ticker_from_ui(&self, gate: &crate::gate::Gate) -> Result<(), String> {
        self.ensure_ticker_admitted(Some(gate))
    }
    #[cfg(test)]
    fn ensure_ticker_admitted(&self, gate: Option<&crate::gate::Gate>) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::collection_allowed(&state)?;
        ensure_gui_collection_owner().map_err(|e| e.to_string())?;
        let _admission = gate.map(crate::gate::Gate::admit).transpose()?;
        self.ticker(&mut state);
        state.ever_collection_admitted = true;
        Ok(())
    }
    /// The explicit shutdown diagnostic also owns a journal producer. It uses
    /// the same admission and shutdown ACK path; ordinary preview never calls it.
    pub fn start_diagnostic_journal_from_ui(&self, gate: &crate::gate::Gate) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::collection_allowed(&state)?;
        ensure_gui_collection_owner().map_err(|e| e.to_string())?;
        let _admission = gate.admit()?;
        let _runtime = self.rt.enter();
        self.engine.start_monitor_journal();
        state.ever_collection_admitted = true;
        Ok(())
    }
    pub fn start_manual_from_ui(&self, job: &mut Job, kind: Kind, gate: &crate::gate::Gate) -> Result<(), String> {
        let engine = self.engine.clone();
        let progress = job.progress.clone();
        let handle = self.rt.clone();
        self.start_manual_admitted(job, kind, Some(gate), move || handle.block_on(collection::run(engine, kind, None, progress)))
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_manual(&self, job: &mut Job, kind: Kind) -> Result<(), String> {
        let engine = self.engine.clone();
        let progress = job.progress.clone();
        let handle = self.rt.clone();
        self.start_manual_with(job, kind, move || handle.block_on(collection::run(engine, kind, None, progress)))
    }
    #[cfg(test)]
    pub(crate) fn start_fixture_manual(&self, job: &mut Job, work: impl FnOnce() -> Receipt + Send + 'static) -> Result<(), String> {
        self.start_manual_with(job, Kind::Probe, work)
    }
    fn start_manual_with(&self, job: &mut Job, kind: Kind, work: impl FnOnce() -> Receipt + Send + 'static) -> Result<(), String> {
        self.start_manual_admitted(job, kind, None, work)
    }
    fn start_manual_admitted(
        &self,
        job: &mut Job,
        kind: Kind,
        gate: Option<&crate::gate::Gate>,
        work: impl FnOnce() -> Receipt + Send + 'static,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::collection_allowed(&state)?;
        if job.running.is_some() || state.manual.as_ref().is_some_and(|task| !task.is_finished()) {
            return Err("別の収集が実行中です".into());
        }
        ensure_gui_collection_owner().map_err(|e| e.to_string())?;
        let _admission = gate.map(crate::gate::Gate::admit).transpose()?;
        let sender = job.begin(kind).ok_or("別の収集が実行中です")?;
        state.ever_collection_admitted = true;
        state.manual = Some(self.rt.spawn(async move {
            // Started blocking work is not abortable. Keep both task scopes
            // alive through result delivery, even if the UI receiver is gone.
            let result = tokio::task::spawn_blocking(work).await;
            let completed = result.is_ok();
            let receipt = result.unwrap_or_else(|_| Receipt::error(kind, "収集タスクが停止しました"));
            let _ = sender.send(receipt);
            completed
        }));
        Ok(())
    }
    /// Close admission synchronously, including queued UI submissions. The
    /// preview invokes this on last-window close and on every app Quit path.
    /// Window release alone continues to release only UI Live ownership.
    pub fn request_exit(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.exiting = true;
        self.schedule_request.send_replace(false);
        self.request.send_modify(|r| r.mode = RunMode::Exit);
        if let Some(ticker) = &state.ticker {
            ticker.abort();
        }
    }
    /// Called outside Application::run, while Tokio is still alive. In-flight
    /// producers finish at their existing transport/DB boundary with no overall
    /// deadline. Only the subsequent Live/journal phase shares a three-second
    /// budget. GPUI's 100ms on_app_quit allowance is not a drain guarantee.
    pub async fn shutdown(&self) -> ShutdownReport {
        self.request_exit();
        let mut drained = self.drain.lock().await;
        if let Some(report) = *drained {
            return report;
        }
        let (manual, worker, ticker, scheduler, install) = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (state.manual.take(), state.worker.take(), state.ticker.take(), state.scheduler.take(), state.install_job.take())
        };
        if let Some(ticker) = ticker {
            let _ = ticker.await;
        }
        let manual_complete = match manual {
            Some(task) => Some(task.await.unwrap_or(false)),
            None => None,
        };
        let monitor_complete = match worker {
            Some(task) => Some(task.await.is_ok()),
            None => None,
        };
        let scheduler_complete = match scheduler {
            Some(task) => Some(task.await.is_ok()),
            None => None,
        };
        if let Some(install) = install
            && install.await.is_err()
        {
            Self::record_install(&mut self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner), NativeInstallResult::WorkerFailed);
            self.dirty.store(true, Ordering::Release);
        }
        let (install_complete, install_recovery_required) = {
            let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (state.install_confirmed, state.install_phase == InstallPhase::RecoveryRequired)
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        self.live.shutdown(deadline.saturating_duration_since(tokio::time::Instant::now())).await;
        let journal_ack = Some(self.engine.finish_monitor_journal(deadline.saturating_duration_since(tokio::time::Instant::now())).await);
        let report = ShutdownReport { manual_complete, monitor_complete, scheduler_complete, journal_ack, install_complete, install_recovery_required };
        *drained = Some(report);
        report
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
        host.pause_monitor().unwrap();
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
        drop((host, engine));
        std::fs::remove_dir_all(dir).unwrap();
    }
    async fn assert_malformed_monitor_refused(raw: &str) {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("monitor-malformed-{}-{id}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[{"id":"fixture","os":"macos"}]}"#).unwrap();
        let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
        engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true}))).unwrap();
        engine.with_db(|d| d.conn().execute("UPDATE meta SET v=? WHERE k='monitor'", [raw])).unwrap();
        let live = Live::new(Arc::new(Refused), Opts::default(), Box::new(|_| {}));
        let host = MonitorHost::new(engine.clone(), live.clone(), Handle::current(), Arc::new(AtomicBool::new(false)));
        let refused = host.start_monitor().is_err();
        let snapshot = host.snapshot();
        let inactive = live.active().is_empty();
        let no_journal = engine.monitor_journal_health()["running"] == false;
        host.save_monitor_settings(json!({"enabled":true,"hub":null})).expect("explicit valid repair remains possible");
        let repaired_mode = host.snapshot().mode;
        host.shutdown().await;
        drop((host, engine));
        std::fs::remove_dir_all(dir).unwrap();
        assert!(refused && !snapshot.started && inactive && no_journal, "malformed settings must refuse admission: {raw}");
        assert_eq!(snapshot.status.phase, RunPhase::Failed, "malformed is unknown, not disabled: {raw}");
        assert_eq!(snapshot.status.local_collecting, None);
        assert_eq!(repaired_mode, RunMode::Paused);
    }
    #[tokio::test]
    async fn malformed_monitor_json_rejects_start_without_worker() {
        assert_malformed_monitor_refused("{broken").await;
    }
    #[tokio::test]
    async fn malformed_monitor_hub_rejects_start_without_worker() {
        assert_malformed_monitor_refused(r#"{"enabled":true,"hub":123}"#).await;
    }
    #[tokio::test]
    async fn malformed_monitor_types_reject_start_without_worker() {
        for raw in ["null", "[]", "true", r#"{"enabled":"invalid"}"#, r#"{"enabled":null}"#, r#"{"hub":false}"#] {
            assert_malformed_monitor_refused(raw).await;
        }
    }
    async fn monitor_phase(status: &mut watch::Receiver<RunStatus>, wanted: RunPhase) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while status.borrow_and_update().phase != wanted {
                status.changed().await.unwrap();
            }
        })
        .await
        .expect("worker must acknowledge phase");
    }
    #[tokio::test(start_paused = true)]
    async fn worker_failure_revokes_host_permission_and_settings_save_does_not_restart() {
        let dir = std::env::temp_dir().join(format!("monitor-host-repair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
        let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
        engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true}))).unwrap();
        let live = Live::new(Arc::new(Refused), Opts::default(), Box::new(|_| {}));
        let host = MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false)));
        let mut status = host.status.subscribe();
        host.start_monitor().unwrap();
        monitor_phase(&mut status, RunPhase::Running).await;
        let worker_id = host.state.lock().unwrap().worker.as_ref().unwrap().id();
        let ticker_id = host.state.lock().unwrap().ticker.as_ref().unwrap().id();
        engine.with_db(|d| d.conn().execute("UPDATE meta SET v=? WHERE k='monitor'", ["{broken"])).unwrap();
        tokio::time::advance(Duration::from_secs(15)).await;
        monitor_phase(&mut status, RunPhase::Failed).await;
        assert_eq!(host.snapshot().mode, RunMode::Paused);
        host.save_monitor_settings(json!({"enabled":true,"hub":null})).unwrap();
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(15)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(host.snapshot().mode, RunMode::Paused);
        assert_eq!(host.snapshot().status.phase, RunPhase::Paused);
        host.start_monitor().unwrap();
        monitor_phase(&mut status, RunPhase::Running).await;
        assert_eq!(host.snapshot().mode, RunMode::Run);
        assert_eq!(host.state.lock().unwrap().worker.as_ref().unwrap().id(), worker_id);
        assert_eq!(host.state.lock().unwrap().ticker.as_ref().unwrap().id(), ticker_id);
        assert!(host.shutdown().await.confirmed());
        drop((host, engine));
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
        drop((host, engine));
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
        assert_eq!(host.shutdown().await.journal_ack, Some(false), "final shutdown must not report an unconfirmed journal commit as success");
        assert!(engine.monitor_journal_health()["unconfirmed_acks"].as_u64().unwrap() > 0);
        drop((host, engine));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod shutdown_tests;

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod schedule_tests;

#[cfg(test)]
#[path = "admission_tests.rs"]
mod admission_tests;

#[cfg(test)]
#[path = "persisted_state_tests.rs"]
mod persisted_state_tests;
