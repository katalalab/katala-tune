//! 収集の経過だけを残す。自由文・stdout/stderr・引数・環境変数を受け取らない。
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::db::{Store, now_ms};

pub const QUEUE_CAP: usize = 256;
pub const BATCH_MAX: usize = 64;
pub const EVENT_BYTES: usize = 1024;
pub const KEEP_DAYS: i64 = 14;
pub const KEEP_ROWS: i64 = 50_000;
pub const RETENTION_MAINTENANCE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipeEnd {
    Eof,
    ReadError,
    Cancelled,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputEnd {
    Written,
    WriteError,
    Cancelled,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    OpenFailed,
    FirstSampleTimeout,
    StallTimeout,
    ExitNonzero,
    StatusWithoutCode,
    WaitUnknown,
    DbBusy,
    DbFull,
    DbIo,
    Unknown,
}
impl Category {
    pub fn database(e: &rusqlite::Error) -> Self {
        match e.sqlite_error_code() {
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => Self::DbBusy,
            Some(rusqlite::ErrorCode::DiskFull) => Self::DbFull,
            Some(rusqlite::ErrorCode::SystemIoFailure) => Self::DbIo,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    MaxAge,
    Count,
    Other,
    User,
    Idle,
    Hidden,
    Disconnected,
    Limit,
    AppExit,
    Restart,
    Excluded,
}
impl Reason {
    pub fn end(s: &str) -> Self {
        match s {
            "max_age" => Self::MaxAge,
            "count" => Self::Count,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatePhase {
    Connecting,
    Running,
    Retrying,
    Stopped,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    StreamBackoff,
    PlannedRecycle,
    MonitorDisconnected,
    Local,
    Hub,
    Disabled,
    SettingsUnknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CollectorStarted,
    CollectorStopping,
    CollectorStopped,
    Policy,
    Open,
    Hello,
    FirstSample,
    End,
    Retry,
    Stopped,
    Checkpoint,
    MinuteCommit,
    JournalGap,
}
impl Kind {
    pub fn summary(self) -> &'static str {
        match self {
            Self::CollectorStarted => "収集記録を開始",
            Self::CollectorStopping => "収集記録の終了待ち",
            Self::CollectorStopped => "収集記録を終了",
            Self::Policy => "収集方法が変わりました",
            Self::Open => "接続を開始",
            Self::Hello => "サンプラーの応答",
            Self::FirstSample => "最初の測定を受信",
            Self::End => "サンプラーの終了結果",
            Self::Retry => "次の接続を待機",
            Self::Stopped => "収集を停止",
            Self::Checkpoint => "収集の経過",
            Self::MinuteCommit => "分集計の保存結果",
            Self::JournalGap => "収集記録に欠落があります",
        }
    }
}

/// Option は未確認。PID は役割を分け、producer は現protocolでは未確認のまま。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Data {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<StatePhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<Policy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collector_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_pid: Option<u32>,
    pub producer_pid: Option<u32>,
    pub actual_source_sha256: Option<[u8; 32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_rust: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_version: Option<[u32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_local: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sample_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_observed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<PipeEnd>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<PipeEnd>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin: Option<InputEnd>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minute: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_failures: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub v: u8,
    pub collector_id: String,
    pub app_version: String,
    pub event_seq: u64,
    pub node_id: Option<String>,
    pub generation: u64,
    pub attempt: u32,
    pub ts: i64,
    pub kind: Kind,
    pub data: Data,
}
impl Event {
    pub fn json(&self) -> Value {
        let mut v = serde_json::to_value(self).expect("typed finite event");
        v["summary"] = Value::from(self.kind.summary());
        v
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    pub node_id: Option<String>,
    pub before_id: Option<i64>,
    pub since: Option<i64>,
    pub limit: Option<u32>,
    pub kind: Option<Kind>,
    pub collector_id: Option<String>,
    pub generation: Option<u64>,
    pub attempt: Option<u32>,
}

struct State {
    collector_id: String,
    seq: AtomicU64,
    dropped: AtomicU64,
    overflow: AtomicU64,
    rejected: AtomicU64,
    db_failures: AtomicU64,
    committed: AtomicU64,
    pruned: AtomicU64,
    unconfirmed_acks: AtomicU64,
    last_commit: AtomicI64,
    running: AtomicBool,
    last_error: Mutex<Option<Category>>,
}
impl State {
    fn row(&self, node: Option<&str>, generation: u64, attempt: u32, kind: Kind, data: Data) -> Option<Event> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        if node.is_some_and(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control)) {
            return None;
        }
        let e = Event {
            v: 1,
            collector_id: self.collector_id.clone(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            event_seq: seq,
            node_id: node.map(str::to_string),
            generation,
            attempt,
            ts: now_ms(),
            kind,
            data,
        };
        (serde_json::to_vec(&e).ok()?.len() <= EVENT_BYTES).then_some(e)
    }
}

enum Command {
    Event(Box<Event>),
    Flush(oneshot::Sender<bool>),
    Shutdown(oneshot::Sender<bool>),
}
#[derive(Clone)]
pub struct Journal {
    tx: mpsc::Sender<Command>,
    state: Arc<State>,
}
impl Journal {
    /// 既存Tokio runtime内で1回だけ起動。writerはStoreを借りるだけでEngine/送信者を所有しない。
    pub fn start(db: Arc<Mutex<Store>>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let state = Arc::new(State {
            collector_id: format!("{:x}-{:x}-{:x}", std::process::id(), nonce, NEXT.fetch_add(1, Ordering::Relaxed)),
            seq: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            overflow: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            db_failures: AtomicU64::new(0),
            committed: AtomicU64::new(0),
            pruned: AtomicU64::new(0),
            unconfirmed_acks: AtomicU64::new(0),
            last_commit: AtomicI64::new(0),
            running: AtomicBool::new(true),
            last_error: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel(QUEUE_CAP);
        tokio::spawn(writer(db, rx, state.clone()));
        let j = Self { tx, state };
        j.record(None, 0, 0, Kind::CollectorStarted, Data { collector_pid: Some(std::process::id()), ..Data::default() });
        j
    }
    pub fn record(&self, node: Option<&str>, generation: u64, attempt: u32, kind: Kind, data: Data) {
        match self.state.row(node, generation, attempt, kind, data) {
            Some(e) => match self.tx.try_send(Command::Event(Box::new(e))) {
                Ok(()) => return,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.state.overflow.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            },
            None => {
                self.state.rejected.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.state.dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn health(&self) -> Value {
        let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let dropped = n(&self.state.dropped);
        let failed = n(&self.state.db_failures);
        let acks = n(&self.state.unconfirmed_acks);
        let running = self.state.running.load(Ordering::Relaxed);
        json!({ "collector_id":self.state.collector_id, "counter_scope":"current_collector", "running": running, "status": if dropped + failed + acks > 0 { "degraded" } else if !running { "stopped" } else if n(&self.state.committed) == 0 { "unknown" } else { "available" },
            "summary": if dropped + failed + acks > 0 { "収集記録に欠落または保存失敗があります" } else if !running { "収集記録の保存を終了しました" } else if n(&self.state.committed) == 0 { "収集記録の保存は未確認です" } else { "収集記録を保存しています" },
            "last_error_category": *self.state.last_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            "overflow_total": n(&self.state.overflow), "rejected_total": n(&self.state.rejected), "dropped_total": dropped, "db_failures": failed, "unconfirmed_acks": acks, "committed_total": n(&self.state.committed), "pruned_total": n(&self.state.pruned),
            "queue_depth": QUEUE_CAP - self.tx.capacity(), "queue_limit": QUEUE_CAP, "last_commit_at": match self.state.last_commit.load(Ordering::Relaxed) {0 => None, t => Some(t)},
            "keep_days_max": KEEP_DAYS, "keep_rows_max": KEEP_ROWS, "event_bytes_max": EVENT_BYTES,
            "retention_maintenance_seconds": RETENTION_MAINTENANCE.as_secs() })
    }
    pub async fn flush(&self, budget: Duration) -> bool {
        self.control(budget, false).await
    }
    pub async fn shutdown(&self, budget: Duration) -> bool {
        self.record(None, 0, 0, Kind::CollectorStopping, Data::default());
        self.control(budget, true).await
    }
    async fn control(&self, budget: Duration, stop: bool) -> bool {
        let (tx, rx) = oneshot::channel();
        let command = if stop { Command::Shutdown(tx) } else { Command::Flush(tx) };
        let ok = tokio::time::timeout(budget, async {
            self.tx.send(command).await.map_err(|_| ())?;
            rx.await.map_err(|_| ())
        })
        .await
        .is_ok_and(|x| x == Ok(true));
        if !ok {
            self.state.unconfirmed_acks.fetch_add(1, Ordering::Relaxed);
        }
        ok
    }
}

async fn commit(db: &Arc<Mutex<Store>>, batch: &[Event], state: &State) -> bool {
    let db = db.clone();
    let rows = batch.to_vec();
    // 一度に1仕事のみ。mutex/SQLite競合は即Busy。timeout/dropで孤立blocking仕事を作らない。
    let result = tokio::task::spawn_blocking(move || {
        let store = db.try_lock().map_err(|_| Category::DbBusy)?;
        store.insert_monitor_events(&rows, now_ms()).map_err(|e| Category::database(&e))
    })
    .await;
    match result {
        Ok(Ok(pruned)) => {
            state.pruned.fetch_add(pruned as u64, Ordering::Relaxed);
            if !batch.is_empty() {
                state.committed.fetch_add(batch.len() as u64, Ordering::Relaxed);
                state.last_commit.store(now_ms(), Ordering::Relaxed);
            }
            true
        }
        other => {
            let category = match other {
                Ok(Err(c)) => c,
                _ => Category::Unknown,
            };
            *state.last_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(category);
            state.db_failures.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

async fn retention_tick(db: &Arc<Mutex<Store>>, state: &State, next: &mut Instant, now: Instant) -> Option<bool> {
    if now < *next {
        return None;
    }
    *next = now + RETENTION_MAINTENANCE;
    Some(commit(db, &[], state).await)
}

async fn commit_tail(db: &Arc<Mutex<Store>>, batch: &mut Vec<Event>, state: &State) -> bool {
    let ok = commit(db, batch, state).await;
    if !ok {
        // 保存を確認できないaccepted行を、receiverのDropで無言に失わない。
        state.dropped.fetch_add(batch.len() as u64, Ordering::Relaxed);
    }
    batch.clear();
    ok
}

async fn finish_tail(db: &Arc<Mutex<Store>>, rx: &mut mpsc::Receiver<Command>, batch: &mut Vec<Event>, state: &State, ack: oneshot::Sender<bool>) {
    rx.close();
    let mut acks = vec![ack];
    let mut ok = if batch.len() == BATCH_MAX { commit_tail(db, batch, state).await } else { true };
    // close前のpermitも容量256に含まれる。EmptyではなくNoneまで待ち、未確定のtailへACKしない。
    while let Some(command) = rx.recv().await {
        match command {
            Command::Event(e) if ok => batch.push(*e),
            Command::Event(_) => {
                // 最初のtail失敗でDB書き込みを打ち切り、残りのaccepted行も欠落計上する。
                state.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Command::Flush(a) | Command::Shutdown(a) => acks.push(a),
        }
        if batch.len() == BATCH_MAX && ok {
            ok = commit_tail(db, batch, state).await;
        }
    }
    if ok {
        if let Some(e) = state.row(None, 0, 0, Kind::CollectorStopped, Data::default()) {
            batch.push(e);
        }
        ok = commit_tail(db, batch, state).await;
    }
    for a in acks {
        let _ = a.send(ok);
    }
}

async fn writer(db: Arc<Mutex<Store>>, mut rx: mpsc::Receiver<Command>, state: Arc<State>) {
    let mut batch = Vec::with_capacity(BATCH_MAX);
    let mut iv = tokio::time::interval(Duration::from_secs(1));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut noted = (0, 0);
    let mut next_maintenance = Instant::now() + RETENTION_MAINTENANCE;
    loop {
        let cmd = tokio::select! {
            _ = iv.tick() => None,
            c = rx.recv() => match c { Some(c) => Some(c), None => break },
        };
        let tick = cmd.is_none();
        let mut ack = None;
        match cmd {
            Some(Command::Event(e)) if batch.len() < BATCH_MAX => batch.push(*e),
            Some(Command::Event(_)) => {
                // 失敗batchは次tickまで保持するが、満杯でもcontrolへのFIFOを塞がない。
                // DB backpressureによる欠落。channel自体のoverflowとは区別する。
                state.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            Some(Command::Flush(a)) => ack = Some(a),
            Some(Command::Shutdown(a)) => {
                finish_tail(&db, &mut rx, &mut batch, &state, a).await;
                state.running.store(false, Ordering::Relaxed);
                return;
            }
            None => {}
        }
        if batch.len() >= BATCH_MAX || ack.is_some() || tick {
            let counters = (state.dropped.load(Ordering::Relaxed), state.db_failures.load(Ordering::Relaxed));
            // 次の成功batchに欠落を結ぶ。上限64はgapを含めて守る。
            if counters != noted
                && batch.len() < BATCH_MAX
                && let Some(e) = state.row(None, 0, 0, Kind::JournalGap, Data { dropped: Some(counters.0), db_failures: Some(counters.1), ..Data::default() })
            {
                batch.push(e);
            }
            if batch.is_empty() && ack.is_none() {
                let _ = retention_tick(&db, &state, &mut next_maintenance, Instant::now()).await;
                continue;
            }
            let ok = commit(&db, &batch, &state).await;
            if ok {
                if let Some(e) = batch.iter().rev().find(|e| e.kind == Kind::JournalGap) {
                    noted = (e.data.dropped.unwrap_or(0), e.data.db_failures.unwrap_or(0));
                }
                batch.clear();
                next_maintenance = Instant::now() + RETENTION_MAINTENANCE;
            }
            if let Some(a) = ack {
                let _ = a.send(ok);
            }
        }
    }
    if !batch.is_empty() {
        let _ = commit(&db, &batch, &state).await;
    }
    state.running.store(false, Ordering::Relaxed);
}

pub fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

pub fn reported_version(s: Option<&str>) -> Option<[u32; 3]> {
    let mut xs = s?.split('.');
    let mut out = [0; 3];
    for n in &mut out {
        let x = xs.next()?;
        if x.is_empty() || x.len() > 5 || !x.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        *n = x.parse().ok()?;
    }
    xs.next().is_none().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> Arc<Mutex<Store>> {
        Arc::new(Mutex::new(Store::open_in_memory().unwrap()))
    }

    #[tokio::test]
    async fn retention_empty_batch_prunes_without_claiming_an_event_commit() {
        let db = store();
        let j = Journal::start(db.clone());
        assert!(j.flush(Duration::from_secs(2)).await);
        assert!(j.shutdown(Duration::from_secs(2)).await);
        let before = j.health();
        let expired = now_ms() - KEEP_DAYS * 86_400_000 - 1;
        db.lock().unwrap().conn().execute("UPDATE monitor_events SET ts=?1,payload=json_set(payload,'$.ts',?1)", [expired]).unwrap();
        let ok = commit(&db, &[], &j.state).await;
        let remaining: i64 = db.lock().unwrap().conn().query_row("SELECT count(*) FROM monitor_events", [], |r| r.get(0)).unwrap();
        let after = j.health();
        assert!(ok);
        assert_eq!(remaining, 0);
        assert_eq!(after["committed_total"], before["committed_total"]);
        assert_eq!(after["last_commit_at"], before["last_commit_at"]);
        assert!(after["pruned_total"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn retention_tick_has_a_minute_cadence_including_after_failure() {
        let db = store();
        let j = Journal::start(db.clone());
        assert!(j.shutdown(Duration::from_secs(2)).await);
        let before = j.health();
        let expired = now_ms() - KEEP_DAYS * 86_400_000 - 1;
        db.lock().unwrap().conn().execute("UPDATE monitor_events SET ts=?1,payload=json_set(payload,'$.ts',?1)", [expired]).unwrap();
        db.lock()
            .unwrap()
            .conn()
            .execute_batch("CREATE TEMP TRIGGER block_prune BEFORE DELETE ON monitor_events BEGIN SELECT RAISE(ABORT,'fixture'); END")
            .unwrap();
        let due = Instant::now() + RETENTION_MAINTENANCE;
        let mut next = due;
        assert_eq!(retention_tick(&db, &j.state, &mut next, due - Duration::from_millis(1)).await, None);
        assert_eq!(j.health()["db_failures"], 0, "not-due ticks perform no DB work");
        assert_eq!(retention_tick(&db, &j.state, &mut next, due).await, Some(false));
        let changes = db.lock().unwrap().conn().total_changes();
        assert_eq!(retention_tick(&db, &j.state, &mut next, due + Duration::from_secs(59)).await, None);
        assert_eq!(db.lock().unwrap().conn().total_changes(), changes);
        db.lock().unwrap().conn().execute_batch("DROP TRIGGER block_prune").unwrap();
        assert_eq!(retention_tick(&db, &j.state, &mut next, due + RETENTION_MAINTENANCE).await, Some(true));
        assert_eq!(db.lock().unwrap().conn().query_row("SELECT count(*) FROM monitor_events", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(j.health()["committed_total"], before["committed_total"]);
        assert_eq!(j.health()["last_commit_at"], before["last_commit_at"]);
        assert_eq!(j.health()["retention_maintenance_seconds"], 60);
    }

    #[tokio::test]
    async fn shutdown_drains_accepted_tail_and_replies_to_queued_controls() {
        let db = store();
        let j = Journal::start(db.clone());
        let (stop, stopped) = oneshot::channel();
        assert!(j.tx.try_send(Command::Shutdown(stop)).is_ok());
        for _ in 0..128 {
            j.record(Some("tail"), 1, 1, Kind::Checkpoint, Data::default());
        }
        let (flush, flushed) = oneshot::channel();
        assert!(j.tx.try_send(Command::Flush(flush)).is_ok());
        let stop_ack = tokio::time::timeout(Duration::from_secs(4), stopped).await;
        let flush_ack = tokio::time::timeout(Duration::from_secs(4), flushed).await;
        let rows: i64 = db.lock().unwrap().conn().query_row("SELECT count(*) FROM monitor_events WHERE node_id='tail'", [], |r| r.get(0)).unwrap();
        assert!(matches!(stop_ack, Ok(Ok(true))));
        assert!(matches!(flush_ack, Ok(Ok(true))));
        assert_eq!(rows, 128);
        assert_eq!(j.health()["dropped_total"], 0);
        j.record(Some("closed"), 1, 1, Kind::Checkpoint, Data::default());
        assert_eq!(j.health()["dropped_total"], 1, "post-seal rejection must remain visible");
    }

    #[tokio::test]
    async fn shutdown_does_not_ack_before_an_issued_permit_is_drained() {
        let db = store();
        let j = Journal::start(db.clone());
        let permit = j.tx.clone().reserve_owned().await.unwrap();
        let (stop, mut stopped) = oneshot::channel();
        assert!(j.tx.try_send(Command::Shutdown(stop)).is_ok());
        let closed = tokio::time::timeout(Duration::from_secs(4), async {
            while !j.tx.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let early = stopped.try_recv();
        let waited = matches!(early, Err(oneshot::error::TryRecvError::Empty));
        let e = j.state.row(Some("reserved-tail"), 1, 1, Kind::Checkpoint, Data::default()).unwrap();
        // An issued permit remains valid after Receiver::close. Always release it before assertions.
        drop(permit.send(Command::Event(Box::new(e))));
        let ack = match early {
            Ok(ok) => Some(ok),
            Err(oneshot::error::TryRecvError::Empty) => tokio::time::timeout(Duration::from_secs(4), stopped).await.ok().and_then(Result::ok),
            Err(_) => None,
        };
        let rows: i64 = db.lock().unwrap().conn().query_row("SELECT count(*) FROM monitor_events WHERE node_id='reserved-tail'", [], |r| r.get(0)).unwrap();
        assert!(closed.is_ok() && waited, "a still-issued permit cannot be called a clean tail");
        assert_eq!(ack, Some(true));
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn shutdown_failed_tail_accounts_for_every_row_and_fails_queued_controls() {
        let db = store();
        db.lock()
            .unwrap()
            .conn()
            .execute_batch("CREATE TEMP TRIGGER reject_events BEFORE INSERT ON monitor_events BEGIN SELECT RAISE(ABORT,'fixture'); END")
            .unwrap();
        let j = Journal::start(db.clone());
        let (stop, stopped) = oneshot::channel();
        assert!(j.tx.try_send(Command::Shutdown(stop)).is_ok());
        for _ in 0..128 {
            j.record(Some("tail"), 1, 1, Kind::Checkpoint, Data::default());
        }
        let (flush, flushed) = oneshot::channel();
        assert!(j.tx.try_send(Command::Flush(flush)).is_ok());
        let stop_ack = tokio::time::timeout(Duration::from_secs(4), stopped).await;
        let flush_ack = tokio::time::timeout(Duration::from_secs(4), flushed).await;
        let rows: i64 = db.lock().unwrap().conn().query_row("SELECT count(*) FROM monitor_events", [], |r| r.get(0)).unwrap();
        assert!(matches!(stop_ack, Ok(Ok(false))));
        assert!(matches!(flush_ack, Ok(Ok(false))));
        assert_eq!(rows, 0);
        assert_eq!(j.health()["dropped_total"].as_u64(), Some(j.state.seq.load(Ordering::Relaxed)));
        assert!(j.health()["db_failures"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn shutdown_external_write_lock_ends_writer_without_retrying_failed_tail() {
        let dir = std::env::temp_dir().join(format!("monitor-journal-tail-lock-{}-{}", std::process::id(), now_ms()));
        let db = Arc::new(Mutex::new(Store::open(&dir).unwrap()));
        let blocker = Store::open(&dir).unwrap();
        let j = Journal::start(db.clone());
        assert!(j.flush(Duration::from_secs(2)).await);
        let before = j.health();
        blocker.conn().execute_batch("BEGIN IMMEDIATE").unwrap();
        let (stop, stopped) = oneshot::channel();
        assert!(j.tx.try_send(Command::Shutdown(stop)).is_ok());
        for _ in 0..128 {
            j.record(Some("tail"), 1, 1, Kind::Checkpoint, Data::default());
        }
        let (flush, flushed) = oneshot::channel();
        assert!(j.tx.try_send(Command::Flush(flush)).is_ok());
        let began = Instant::now();
        let stop_ack = tokio::time::timeout(Duration::from_secs(2), stopped).await;
        let flush_ack = tokio::time::timeout(Duration::from_secs(2), flushed).await;
        let after = j.health();
        let finished = !j.state.running.load(Ordering::Relaxed);
        // Keep the external lock through writer completion and verify no blocking job owns Store.
        let store_available = db.try_lock().is_ok();
        let elapsed = began.elapsed();
        blocker.conn().execute_batch("ROLLBACK").unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(4), async {
            while j.state.running.load(Ordering::Relaxed) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let stopped_rows: i64 = db
            .lock()
            .unwrap()
            .conn()
            .query_row("SELECT count(*) FROM monitor_events WHERE json_extract(payload,'$.kind')='collector_stopped'", [], |r| r.get(0))
            .unwrap();
        drop(blocker);
        drop(j);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(matches!(stop_ack, Ok(Ok(false))), "shutdown must fail before its deadline");
        assert!(matches!(flush_ack, Ok(Ok(false))), "queued flush must receive the same failure");
        assert!(finished && store_available, "writer and blocking job must finish while the lock is held");
        assert!(elapsed < Duration::from_secs(2));
        assert_eq!(after["db_failures"].as_u64().unwrap() - before["db_failures"].as_u64().unwrap(), 1, "shutdown does not retry a failed tail");
        assert_eq!(after["dropped_total"], 128, "every accepted tail row is accounted for");
        assert_eq!(stopped_rows, 0);
    }

    #[tokio::test]
    async fn shutdown_reaches_a_failed_full_batch_and_accounts_for_queued_rows() {
        let dir = std::env::temp_dir().join(format!("monitor-journal-full-lock-{}-{}", std::process::id(), now_ms()));
        let db = Arc::new(Mutex::new(Store::open(&dir).unwrap()));
        let blocker = Store::open(&dir).unwrap();
        let j = Journal::start(db.clone());
        assert!(j.flush(Duration::from_secs(2)).await);
        blocker.conn().execute_batch("BEGIN IMMEDIATE").unwrap();
        for _ in 0..BATCH_MAX {
            j.record(Some("full"), 1, 1, Kind::Checkpoint, Data::default());
        }
        // Observe a failed full batch before enqueuing controls; a shutdown-first fixture misses this path.
        let failed_full_batch = tokio::time::timeout(Duration::from_secs(2), async {
            while j.state.db_failures.load(Ordering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        for _ in 0..128 {
            j.record(Some("queued"), 1, 1, Kind::Checkpoint, Data::default());
        }
        let (stop, stopped) = oneshot::channel();
        assert!(j.tx.try_send(Command::Shutdown(stop)).is_ok());
        let (flush, flushed) = oneshot::channel();
        assert!(j.tx.try_send(Command::Flush(flush)).is_ok());
        let began = Instant::now();
        let stop_ack = tokio::time::timeout(Duration::from_secs(2), stopped).await;
        let flush_ack = tokio::time::timeout(Duration::from_secs(2), flushed).await;
        let after = j.health();
        let finished = !j.state.running.load(Ordering::Relaxed);
        let store_available = db.try_lock().is_ok();
        let elapsed = began.elapsed();
        // The external writer stays locked until all completion observations above.
        blocker.conn().execute_batch("ROLLBACK").unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(4), async {
            while j.state.running.load(Ordering::Relaxed) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let stopped_rows: i64 = db
            .lock()
            .unwrap()
            .conn()
            .query_row("SELECT count(*) FROM monitor_events WHERE json_extract(payload,'$.kind')='collector_stopped'", [], |r| r.get(0))
            .unwrap();
        drop(blocker);
        drop(j);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(failed_full_batch, "fixture must observe a failed full batch before shutdown");
        assert!(matches!(stop_ack, Ok(Ok(false))) && matches!(flush_ack, Ok(Ok(false))));
        assert!(finished && store_available, "full failed batch must not keep controls or a blocking job alive");
        assert!(elapsed < Duration::from_secs(2));
        assert_eq!(after["dropped_total"], BATCH_MAX + 128, "all accepted rows remain accounted for");
        assert_eq!(after["overflow_total"], 0);
        assert_eq!(stopped_rows, 0);
    }

    #[tokio::test]
    async fn commits_typed_metadata_without_error_text_and_reopens_query() {
        let db = store();
        let j = Journal::start(db.clone());
        let secret = "dummy-private-canary/token/path";
        j.record(
            Some("fixture"),
            1,
            2,
            Kind::End,
            Data { reason: Some(Reason::end(secret)), exit_code: Some(3), stdout: Some(PipeEnd::Unknown), ..Data::default() },
        );
        assert!(j.flush(Duration::from_secs(2)).await);
        let out = db.lock().unwrap().monitor_events(&Filter::default()).unwrap();
        let body = out.to_string();
        assert!(!body.contains(secret));
        assert!(body.contains("unknown"));
        let e = &out["events"][0];
        assert_eq!(e["data"]["producer_pid"], Value::Null);
        assert_eq!(e["attempt"], 2);
        assert_eq!(e["generation"], 1);
        assert_eq!(e["data"]["reason"], "other");
        assert!(j.shutdown(Duration::from_secs(2)).await);
    }
    #[tokio::test]
    async fn overflow_is_bounded_and_never_healthy_then_persists_gap() {
        let db = store();
        let j = Journal::start(db.clone());
        // current-thread runtime: producer fills queue before writer can run.
        for _ in 0..QUEUE_CAP + 50 {
            j.record(Some("fixture"), 1, 1, Kind::Checkpoint, Data::default());
        }
        assert!(j.health()["dropped_total"].as_u64().unwrap() > 0);
        assert!(j.health()["overflow_total"].as_u64().unwrap() > 0);
        assert!(j.flush(Duration::from_secs(4)).await);
        // Empty tick/flush also persists gap after a full64 batch.
        assert!(j.flush(Duration::from_secs(2)).await);
        assert_eq!(j.health()["status"], "degraded");
        let out = db.lock().unwrap().monitor_events(&Filter { limit: Some(500), ..Filter::default() }).unwrap();
        assert!(out["events"].as_array().unwrap().iter().any(|e| e["kind"] == "journal_gap"));
        assert!(j.shutdown(Duration::from_secs(2)).await);
    }
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // 意図した競合fixture。製品producer側はDB mutexを待たない。
    async fn busy_does_not_block_producer_or_ack_then_recovers() {
        let db = store();
        let j = Journal::start(db.clone());
        let held = db.lock().unwrap();
        assert!(!j.flush(Duration::from_secs(2)).await);
        for _ in 0..BATCH_MAX + 1 {
            j.record(Some("fixture"), 1, 1, Kind::Open, Data::default());
        }
        assert!(!j.flush(Duration::from_secs(2)).await, "full failed batch must still receive controls");
        assert!(j.health()["db_failures"].as_u64().unwrap() > 0);
        assert!(j.health()["dropped_total"].as_u64().unwrap() > 0);
        assert_eq!(j.health()["overflow_total"], 0, "DB backpressure is distinct from channel overflow");
        drop(held);
        assert!(j.flush(Duration::from_secs(2)).await);
        assert!(j.flush(Duration::from_secs(2)).await, "a recovered full batch leaves room for the gap record");
        assert_eq!(j.health()["status"], "degraded");
        let dropped = j.health()["dropped_total"].as_u64();
        let out = db.lock().unwrap().monitor_events(&Filter { limit: Some(500), ..Default::default() }).unwrap();
        assert!(out["events"].as_array().unwrap().iter().any(|e| e["kind"] == "journal_gap" && e["data"]["dropped"].as_u64() == dropped));
        assert!(j.shutdown(Duration::from_secs(2)).await);
    }
    #[tokio::test]
    async fn oversized_identifier_is_loss_and_controls_never_enter_db() {
        let db = store();
        let j = Journal::start(db.clone());
        j.record(Some(&"x".repeat(5000)), 1, 1, Kind::Open, Data::default());
        j.record(Some("bad\nnode"), 1, 1, Kind::Open, Data::default());
        assert_eq!(j.health()["dropped_total"], 2);
        assert_eq!(j.health()["rejected_total"], 2);
        assert!(j.flush(Duration::from_secs(2)).await);
        // 合法なhex識別子にも"bad"は含まれ得る。nonceへの偶然の一致を決定的に再現する。
        db.lock().unwrap().conn().execute("UPDATE monitor_events SET collector_id='bad-1-0',payload=json_set(payload,'$.collector_id','bad-1-0')", []).unwrap();
        let out = db.lock().unwrap().monitor_events(&Filter::default()).unwrap();
        let events = out["events"].as_array().unwrap();
        assert!(events.iter().any(|e| e["kind"] == "collector_started"));
        assert!(events.iter().all(|e| e["collector_id"] == "bad-1-0"));
        assert!(events.iter().all(|e| e["node_id"].is_null()), "neither oversized nor control-bearing node IDs may be stored");
        assert!(j.shutdown(Duration::from_secs(2)).await);
    }
    #[test]
    fn reported_agent_version_is_numeric_metadata_only() {
        assert_eq!(reported_version(Some("0.6.4")), Some([0, 6, 4]));
        for s in ["token/path", "0.6.4-private-canary", "1.2", "1.2.3.4", "1.2.-3", ""] {
            assert_eq!(reported_version(Some(s)), None);
        }
    }
}
