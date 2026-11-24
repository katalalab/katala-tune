use super::*;

pub(super) async fn read_lines(out: Option<tokio::process::ChildStdout>, tx: mpsc::Sender<String>, boundary: Arc<Mutex<ReadBoundary>>) -> PipeEnd {
    let Some(out) = out else {
        if let Ok(mut b) = boundary.lock() {
            b.data.pre_stdout = Some(PipeEnd::Unknown);
        }
        return PipeEnd::Unknown;
    };
    read_lines_from(out, tx, boundary).await
}

pub(super) async fn read_lines_from<R: AsyncRead + Unpin>(out: R, tx: mpsc::Sender<String>, boundary: Arc<Mutex<ReadBoundary>>) -> PipeEnd {
    let mut r = BufReader::new(ObservedRead { inner: out, boundary: boundary.clone() });
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = match (&mut r).take(MAX_LINE as u64).read_until(b'\n', &mut buf).await {
            Ok(n) => n,
            Err(_) => return PipeEnd::ReadError,
        };
        if n == 0 {
            return PipeEnd::Eof;
        }
        if buf.last() != Some(&b'\n') && n >= MAX_LINE {
            if let Ok(mut b) = boundary.lock() {
                b.data.overflow |= b.data.discarded == u32::MAX;
                b.data.discarded = b.data.discarded.saturating_add(1);
            }
            loop {
                let mut skip = Vec::new();
                match (&mut r).take(MAX_LINE as u64).read_until(b'\n', &mut skip).await {
                    Ok(0) => return PipeEnd::Eof,
                    Err(_) => return PipeEnd::ReadError,
                    Ok(_) if skip.last() == Some(&b'\n') => break,
                    Ok(_) => {}
                }
            }
            continue;
        }
        let frame = collect::decode(&buf);
        let Ok(permit) = tx.reserve().await else {
            if let Ok(mut b) = boundary.lock() {
                b.data.pre_stdout = Some(PipeEnd::Cancelled);
            }
            return PipeEnd::Cancelled;
        };
        // Commit metadata before publishing: the consumer may immediately snapshot and close.
        match boundary.lock() {
            Ok(mut b) => {
                b.data.overflow |= b.data.forwarded == u32::MAX;
                b.data.forwarded = b.data.forwarded.saturating_add(1);
                if buf.last() == Some(&b'\n') {
                    b.data.last_line_ms = b.at_ms();
                }
                // No await: snapshot and publication share the same linearization boundary.
                permit.send(frame);
            }
            Err(_) => {
                permit.send(frame);
            } // Keep delivery; poisoned diagnostics stay unknown.
        }
    }
}

pub(super) async fn read_tail(err: Option<tokio::process::ChildStderr>, keep: usize) -> (String, PipeEnd) {
    let Some(mut err) = err else { return (String::new(), PipeEnd::Unknown) };
    let mut tail = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        match err.read(&mut chunk).await {
            Ok(0) => break PipeEnd::Eof,
            Err(_) => break PipeEnd::ReadError,
            Ok(n) => {
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > keep {
                    tail.drain(..tail.len() - keep);
                }
            }
        }
    };
    (collect::decode(&tail), end)
}

/// 今の経路: この機体はローカル実行、ほかは SSH（`launch`）
pub struct ProcessRoute;

impl Route for ProcessRoute {
    fn open(&self, node: &Node, opts: &Opts) -> Result<Source, String> {
        spawn(launch(node, opts)?)
    }
}

// ---------------------------------------------------------------------------
// 管理
// ---------------------------------------------------------------------------

/// 止まった理由
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// 画面で止めた
    User,
    /// 画面からの合図が途切れた（自動停止）
    Idle,
    /// ウィンドウを閉じた
    Hidden,
    /// つなぎ直しの上限まで失敗した（接続切れ）
    Disconnected(String),
    /// 同時に流せる台数の上限
    Limit,
    /// アプリの終了
    AppExit,
    /// サンプラーを入れ替えたので、つなぎ直す（常時監視が次の合図で流し直す）
    Restart,
    /// 台帳から削除されたか、運用対象外になった
    Excluded,
}

impl Stop {
    pub fn code(&self) -> &'static str {
        match self {
            Stop::User => "user",
            Stop::Idle => "idle",
            Stop::Hidden => "hidden",
            Stop::Disconnected(_) => "disconnected",
            Stop::Limit => "limit",
            Stop::AppExit => "app_exit",
            Stop::Restart => "restart",
            Stop::Excluded => "excluded",
        }
    }

    fn detail(&self, o: &Opts) -> String {
        match self {
            Stop::User => "画面で止めた".into(),
            Stop::Idle => format!("画面からの合図が {} 秒ないので自動で止めた", o.idle_stop.as_secs()),
            Stop::Hidden => "ウィンドウを閉じたので止めた".into(),
            Stop::Disconnected(e) => format!("{} 回つなぎ直しても続かないので止めた: {e}", o.max_retries),
            Stop::Limit => format!("同時に流せるのは {} 台まで", o.max_streams),
            Stop::AppExit => "アプリを終了した".into(),
            Stop::Restart => "サンプラーを入れ替えたので、つなぎ直す".into(),
            Stop::Excluded => "台帳で運用対象外になったので収集を止めた".into(),
        }
    }
}

/// Shared native-host stream consumers. Legacy callers keep their existing API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Ui,
    Monitor,
}

impl Owner {
    fn bit(self) -> u8 {
        match self {
            Self::Ui => 1,
            Self::Monitor => 2,
        }
    }
}

/// 機体ごとの流れの段階
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Running,
    Retrying,
    Stopped,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Connecting => "connecting",
            Phase::Running => "running",
            Phase::Retrying => "retrying",
            Phase::Stopped => "stopped",
        }
    }
}

/// 機体ごとの保持（止めても残す。もう一度流すと続きから描ける）
pub(super) struct NodeLive {
    ring: Ring<Sample>,
    procs: Option<Value>,
    info: Option<Hello>,
    session_epoch: Option<String>,
    phase: Phase,
    reason: Option<&'static str>,
    detail: Option<String>,
    attempt: u32,
    /// サンプラー自身の負荷（1 コア換算の %）と RSS。前回の (受け取った時刻, CPU 秒)
    load: Option<f64>,
    rss_mb: Option<f64>,
    me_prev: Option<(i64, f64)>,
    // 画面へまだ送っていないもの
    new_points: usize,
    dirty_procs: bool,
    dirty_info: bool,
    dirty_state: bool,
}

impl NodeLive {
    fn new(cap: usize) -> Self {
        NodeLive {
            ring: Ring::new(cap),
            procs: None,
            info: None,
            session_epoch: None,
            phase: Phase::Stopped,
            reason: None,
            detail: None,
            attempt: 0,
            load: None,
            rss_mb: None,
            me_prev: None,
            new_points: 0,
            dirty_procs: false,
            dirty_info: false,
            dirty_state: false,
        }
    }

    fn set(&mut self, phase: Phase, reason: Option<&'static str>, detail: Option<String>, attempt: u32) {
        if (self.phase, self.reason, &self.detail, self.attempt) != (phase, reason, &detail, attempt) {
            self.dirty_state = true;
        }
        (self.phase, self.reason, self.detail, self.attempt) = (phase, reason, detail, attempt);
    }

    /// 画面へ渡す形。full: 開始したとき（全部、間引いて）、そうでなければ前回から変わった分だけ
    fn view(&mut self, full: bool, ui_points: usize) -> Option<Value> {
        if !full && self.new_points == 0 && !self.dirty_procs && !self.dirty_info && !self.dirty_state {
            return None;
        }
        let mut m = Map::new();
        m.insert("state".into(), Value::from(self.phase.as_str()));
        m.insert("reason".into(), self.reason.map_or(Value::Null, Value::from));
        m.insert("detail".into(), self.detail.clone().map_or(Value::Null, Value::from));
        m.insert("attempt".into(), Value::from(self.attempt));
        let n = if full { self.ring.len() } else { self.new_points.min(self.ring.len()) };
        if n > 0 {
            let pts: Vec<Value> = self.ring.tail(n).map(Sample::point).collect();
            m.insert("points".into(), Value::Array(if full { downsample(&pts, ui_points) } else { pts }));
            if let Some(last) = self.ring.last() {
                m.insert("cores".into(), json!(last.cores));
                m.insert("mem".into(), json!(last.mem));
                if last.gpu.is_some() {
                    m.insert("gpus".into(), json!(last.gpu));
                }
            }
        }
        if (full || self.dirty_procs)
            && let Some(p) = &self.procs
        {
            m.insert("procs".into(), p.clone());
        }
        if (full || self.dirty_info)
            && let Some(h) = &self.info
        {
            m.insert("info".into(), json!(h));
        }
        m.insert("load".into(), json!(self.load.map(|x| (x * 100.0).round() / 100.0)));
        m.insert("rss_mb".into(), json!(self.rss_mb));
        if full {
            // 開始したときの全体。送っていない差分の印は残す（画面は同じ時刻の点を重ねない）
            m.insert("full".into(), Value::Bool(true));
        } else {
            self.new_points = 0;
            self.dirty_procs = false;
            self.dirty_info = false;
            self.dirty_state = false;
        }
        Some(Value::Object(m))
    }
}

pub(super) struct Stream {
    pub(super) epoch: u64,
    owners: u8,
    pub(super) last_hb: Instant,
    stop: watch::Sender<Option<Stop>>,
    task: Option<JoinHandle<()>>,
    stopping: bool,
}

#[derive(Default)]
pub(super) struct Shared {
    pub(super) streams: HashMap<String, Stream>,
    nodes: HashMap<String, NodeLive>,
    epoch: u64,
    closed: bool,
}

/// 画面へイベントを送る関数（Tauri なら emit("live", …)）
pub type Sink = Box<dyn Fn(Value) + Send + Sync>;

/// ライブ表示の管理。アプリに 1 つ
pub struct Live {
    route: Arc<dyn Route>,
    opts: Opts,
    sink: Sink,
    st: Mutex<Shared>,
    journal: OnceLock<Journal>,
}

impl Live {
    pub fn new(route: Arc<dyn Route>, opts: Opts, sink: Sink) -> Arc<Live> {
        Arc::new(Live { route, opts, sink, st: Mutex::new(Shared::default()), journal: OnceLock::new() })
    }

    pub fn bind_journal(&self, journal: Journal) {
        let _ = self.journal.set(journal);
    }

    fn record(&self, id: &str, epoch: u64, attempt: u32, kind: Kind, mut data: Data) {
        if self.journal.get().is_none() {
            return;
        }
        {
            let st = self.lock();
            if Self::current(&st, id, epoch) {
                data.phase = st.nodes.get(id).map(|n| match n.phase {
                    Phase::Connecting => StatePhase::Connecting,
                    Phase::Running => StatePhase::Running,
                    Phase::Retrying => StatePhase::Retrying,
                    Phase::Stopped => StatePhase::Stopped,
                });
            }
        }
        if kind == Kind::Stopped {
            data.phase = Some(StatePhase::Stopped);
        }
        if let Some(j) = self.journal.get() {
            j.record(Some(id), epoch, attempt, kind, data);
        }
    }

    pub fn opts(&self) -> &Opts {
        &self.opts
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Shared> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn node<'a>(&self, st: &'a mut Shared, id: &str) -> &'a mut NodeLive {
        let cap = self.opts.ring;
        st.nodes.entry(id.to_string()).or_insert_with(|| NodeLive::new(cap))
    }

    /// 流し始める。すでに流れている機体は合図（heartbeat）として扱い、自動停止までの時間を延ばす。
    /// 戻り値: `{ started, running, refused: [{ id, reason, detail }], limit, snapshot: { id: 画面へ渡す形 } }`。
    /// unknown は台帳に無かった id（理由つきで断る）。非同期の実行環境（tokio）の中から呼ぶ
    pub fn start(self: &Arc<Self>, nodes: &[Node], unknown: &[String]) -> Value {
        self.start_with_owner(None, nodes, unknown)
    }

    /// Atomically claim and start/renew only the nodes accepted as started/running.
    pub fn start_owned(self: &Arc<Self>, owner: Owner, nodes: &[Node], unknown: &[String]) -> Value {
        self.start_with_owner(Some(owner), nodes, unknown)
    }

    fn start_with_owner(self: &Arc<Self>, owner: Option<Owner>, nodes: &[Node], unknown: &[String]) -> Value {
        let excluded: Vec<String> = nodes.iter().filter(|n| !n.is_active()).map(|n| n.id.clone()).chain(unknown.iter().cloned()).collect();
        let mut st = self.lock();
        Self::stop_locked(&mut st, Some(&excluded), Stop::Excluded);
        let now = Instant::now();
        let owners = owner.map_or(0, Owner::bit);
        let (mut started, mut running, mut refused) = (Vec::new(), Vec::new(), Vec::new());
        for id in unknown {
            refused.push(json!({ "id": id, "reason": "unknown", "detail": "台帳に無い機体" }));
        }
        let mut active = st.streams.values().filter(|s| !s.stopping).count();
        for n in nodes {
            // 運用対象でない機体（retired など）には接続しない
            if let Some(detail) = n.refusal() {
                refused.push(json!({ "id": n.id, "reason": "excluded", "detail": detail }));
                continue;
            }
            if st.closed {
                refused.push(json!({ "id": n.id, "reason": Stop::AppExit.code(), "detail": Stop::AppExit.detail(&self.opts) }));
                continue;
            }
            if let Some(s) = st.streams.get_mut(&n.id).filter(|s| !s.stopping) {
                s.owners |= owners;
                s.last_hb = now;
                running.push(n.id.clone());
                continue;
            }
            if active >= self.opts.max_streams {
                let detail = Stop::Limit.detail(&self.opts);
                refused.push(json!({ "id": n.id, "reason": Stop::Limit.code(), "detail": detail }));
                self.node(&mut st, &n.id).set(Phase::Stopped, Some(Stop::Limit.code()), Some(detail), 0);
                continue;
            }
            st.epoch += 1;
            let epoch = st.epoch;
            let (tx, rx) = watch::channel(None);
            let task = tokio::spawn(run_stream(self.clone(), n.clone(), epoch, rx));
            st.streams.insert(n.id.clone(), Stream { epoch, owners, last_hb: now, stop: tx, task: Some(task), stopping: false });
            let nl = self.node(&mut st, &n.id);
            nl.set(Phase::Connecting, None, None, 0);
            nl.me_prev = None;
            active += 1;
            started.push(n.id.clone());
        }
        let ui = self.opts.ui_points;
        // 新しく流し始めた機体には、覚えている点（前に流したときの分）を渡す。流れ続けている機体には差分が届いている
        let mut snapshot = Map::new();
        for id in &started {
            if let Some(v) = self.node(&mut st, id).view(true, ui) {
                snapshot.insert(id.clone(), v);
            }
        }
        json!({ "started": started, "running": running, "refused": refused, "limit": self.opts.max_streams, "snapshot": snapshot })
    }

    /// Renew only an existing running stream. This never starts a new sampler.
    pub fn heartbeat(&self, id: &str) -> bool {
        let mut state = self.lock();
        if state.closed {
            return false;
        }
        let Some(stream) = state.streams.get_mut(id).filter(|s| !s.stopping) else { return false };
        stream.last_hb = Instant::now();
        true
    }

    /// Renew an existing claim only. This cannot claim, start or reset retries.
    pub fn heartbeat_owned(&self, owner: Owner, id: &str) -> bool {
        let mut st = self.lock();
        if st.closed {
            return false;
        }
        let Some(stream) = st.streams.get_mut(id).filter(|s| !s.stopping && s.owners & owner.bit() != 0) else { return false };
        stream.last_hb = Instant::now();
        true
    }

    /// Release only this consumer. Last-owner detection and stop share the lock.
    /// Returns `{ stopped: [id] }` for samplers stopped by this release.
    pub fn release_owned(&self, owner: Owner, ids: &[String], why: Stop) -> Value {
        let mut st = self.lock();
        let mut stopped = Vec::new();
        for (id, s) in st.streams.iter_mut() {
            if s.stopping || !ids.contains(id) || s.owners & owner.bit() == 0 {
                continue;
            }
            s.owners &= !owner.bit();
            if s.owners == 0 {
                s.stopping = true;
                let _ = s.stop.send(Some(why.clone()));
                stopped.push(id.clone());
            }
        }
        stopped.sort();
        json!({ "stopped": stopped })
    }

    /// 止める（ids が None なら全部）。所有者に優先する管理操作。戻り値は `{ stopped: [id] }`。
    pub fn stop(&self, ids: Option<&[String]>, why: Stop) -> Value {
        Self::stop_locked(&mut self.lock(), ids, why)
    }

    fn stop_locked(st: &mut Shared, ids: Option<&[String]>, why: Stop) -> Value {
        let mut stopped = Vec::new();
        for (id, s) in st.streams.iter_mut() {
            if s.stopping || ids.is_some_and(|ids| !ids.contains(id)) {
                continue;
            }
            s.owners = 0;
            s.stopping = true;
            let _ = s.stop.send(Some(why.clone()));
            stopped.push(id.clone());
        }
        stopped.sort();
        json!({ "stopped": stopped })
    }

    /// 流れている（止めている途中を除く）機体
    pub fn active(&self) -> Vec<String> {
        let mut v: Vec<String> = self.lock().streams.iter().filter(|(_, s)| !s.stopping).map(|(k, _)| k.clone()).collect();
        v.sort();
        v
    }

    /// 最新の全件台帳から外れた既存ストリームも止める（常時監視・画面の合図で再確認）
    pub fn retain_active(&self, nodes: &[Node]) {
        let mut st = self.lock();
        let excluded: Vec<String> = st.streams.keys().filter(|id| !nodes.iter().any(|n| n.id == **id && n.is_active())).cloned().collect();
        Self::stop_locked(&mut st, Some(&excluded), Stop::Excluded);
    }

    /// 機体ごとの段階と保持している点の数（確認用）
    pub fn status(&self) -> Value {
        let st = self.lock();
        let mut m = Map::new();
        for (id, n) in &st.nodes {
            m.insert(
                id.clone(),
                json!({ "state": n.phase.as_str(), "reason": n.reason, "detail": n.detail, "attempt": n.attempt, "points": n.ring.len(), "streaming": st.streams.contains_key(id), "load": n.load }),
            );
        }
        Value::Object(m)
    }

    /// ダッシュボード用の短い形: 段階・サンプラーの実装と負荷・最新の 1 点・上位プロセス 3 件ずつ
    pub fn brief(&self, id: &str) -> Option<Value> {
        let st = self.lock();
        let n = st.nodes.get(id)?;
        let top = |k: &str| n.procs.as_ref().and_then(|p| p.get(k)).and_then(Value::as_array).map(|a| a.iter().take(3).cloned().collect::<Vec<_>>());
        Some(json!({
            "state": n.phase.as_str(),
            "reason": n.reason,
            "streaming": st.streams.get(id).is_some_and(|s| !s.stopping),
            "detail": n.detail,
            "impl": n.info.as_ref().map(|h| h.implementation.as_deref().unwrap_or("script")),
            "agent": n.info.as_ref().and_then(|h| h.agent.clone()),
            "load": n.load.map(|x| (x * 100.0).round() / 100.0),
            "rss_mb": n.rss_mb,
            "point": n.ring.last().map(Sample::point),
            "top_cpu": top("top_cpu"),
            "top_mem": top("top_mem"),
        }))
    }

    /// 最新の上位プロセス（常時監視の集計に付ける）
    pub fn procs(&self, id: &str) -> Option<Value> {
        self.lock().nodes.get(id)?.procs.clone()
    }

    /// 画面へ渡す形の全体（間引いた点・最新のコア・上位プロセス・hello）。確認用で、送っていない差分の印は変えない
    pub fn view(&self, id: &str) -> Option<Value> {
        let ui = self.opts.ui_points;
        self.lock().nodes.get_mut(id).and_then(|n| n.view(true, ui))
    }

    /// 保持している点（古い順）。確認用
    pub fn samples(&self, id: &str) -> Vec<Sample> {
        self.lock().nodes.get(id).map(|n| n.ring.iter().cloned().collect()).unwrap_or_default()
    }

    /// flush ごとに呼ぶ: 合図が途切れたものを止め、変わった分をまとめて画面へ送る
    pub fn tick(&self) {
        let now = Instant::now();
        let ev = {
            let mut st = self.lock();
            for s in st.streams.values_mut() {
                if !s.stopping && now.duration_since(s.last_hb) >= self.opts.idle_stop {
                    s.stopping = true;
                    let _ = s.stop.send(Some(Stop::Idle));
                }
            }
            let ui = self.opts.ui_points;
            let mut nodes = Map::new();
            for (id, n) in st.nodes.iter_mut() {
                if let Some(v) = n.view(false, ui) {
                    nodes.insert(id.clone(), v);
                }
            }
            (!nodes.is_empty()).then(|| json!({ "at": now_ms(), "nodes": nodes }))
        };
        if let Some(ev) = ev {
            (self.sink)(ev);
        }
    }

    /// tick を flush ごとに呼び続ける（終了したら抜ける）。呼び出し側の実行環境で spawn する
    pub async fn run_ticker(self: Arc<Self>) {
        let mut iv = tokio::time::interval(self.opts.flush);
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            iv.tick().await;
            if self.lock().closed {
                return;
            }
            self.tick();
        }
    }

    /// アプリの終了: 全部止め、子プロセスが終わるのを timeout まで待つ。以後は開始を断る
    pub async fn shutdown(&self, timeout: Duration) {
        let tasks: Vec<JoinHandle<()>> = {
            let mut st = self.lock();
            st.closed = true;
            st.streams
                .values_mut()
                .filter_map(|s| {
                    if !s.stopping {
                        s.stopping = true;
                        let _ = s.stop.send(Some(Stop::AppExit));
                    }
                    s.task.take()
                })
                .collect()
        };
        let deadline = tokio::time::Instant::now() + timeout;
        for t in tasks {
            let _ = tokio::time::timeout_at(deadline, t).await;
        }
    }

    // ---- 流れ（run_stream）から呼ぶもの。epoch が今の流れと違えば何もしない（止めた後の古い流れ） ----

    fn current(st: &Shared, id: &str, epoch: u64) -> bool {
        st.streams.get(id).is_some_and(|s| s.epoch == epoch)
    }

    fn set_phase(&self, id: &str, epoch: u64, phase: Phase, detail: Option<String>, attempt: u32) {
        let mut st = self.lock();
        if Self::current(&st, id, epoch) {
            self.node(&mut st, id).set(phase, None, detail, attempt);
        }
    }

    fn on_hello(&self, id: &str, epoch: u64, h: Hello) {
        let mut st = self.lock();
        if Self::current(&st, id, epoch) {
            let n = self.node(&mut st, id);
            n.session_epoch = Some(format!("{epoch}:{}", h.session_epoch_ms.unwrap_or_default()));
            n.info = Some(h);
            n.dirty_info = true;
        }
    }

    fn on_sample(&self, id: &str, epoch: u64, raw: RawSample) {
        let now = now_ms();
        let mut st = self.lock();
        if !Self::current(&st, id, epoch) {
            return;
        }
        let n = self.node(&mut st, id);
        let (mut s, procs, me) = Sample::from_raw(raw, now);
        s.session_epoch = n.session_epoch.clone();
        n.ring.push(s);
        n.new_points = (n.new_points + 1).min(n.ring.cap());
        if let Some(p) = procs {
            let mut v = json!(p);
            if let Value::Object(m) = &mut v {
                m.insert("at".into(), Value::from(now));
            }
            n.procs = Some(v);
            n.dirty_procs = true;
        }
        if let Some(cpu_s) = me.as_ref().and_then(|m| m.cpu_s).filter(|x| x.is_finite()) {
            if let Some((t0, c0)) = n.me_prev
                && now > t0
                && cpu_s >= c0
            {
                n.load = Some((cpu_s - c0) / ((now - t0) as f64 / 1000.0) * 100.0);
            }
            n.me_prev = Some((now, cpu_s));
            n.rss_mb = me.and_then(|m| m.rss_mb);
        }
        if n.phase != Phase::Running {
            n.set(Phase::Running, None, None, 0);
        }
    }

    pub(super) fn finish(&self, id: &str, epoch: u64, why: Stop) {
        let mut st = self.lock();
        if Self::current(&st, id, epoch) {
            st.streams.remove(id);
        }
        // 止めた直後に流し直していれば、新しい流れの状態を上書きしない
        if !st.streams.contains_key(id) {
            let detail = why.detail(&self.opts);
            self.node(&mut st, id).set(Phase::Stopped, Some(why.code()), Some(detail), 0);
        }
    }
}

/// 止める指示が来たか（送り手が無くなったらアプリの終了とみなす）
pub(super) fn stop_requested(rx: &watch::Receiver<Option<Stop>>) -> Option<Stop> {
    rx.borrow().clone()
}

pub(super) fn end_category(timed_out: bool, samples: u64, ended: &Ended) -> Option<Category> {
    if timed_out {
        Some(if samples == 0 { Category::FirstSampleTimeout } else { Category::StallTimeout })
    } else if ended.code.is_some_and(|c| c != 0) {
        Some(Category::ExitNonzero)
    } else if ended.wait_observed != Some(true) {
        Some(Category::WaitUnknown)
    } else if ended.code.is_none() {
        Some(Category::StatusWithoutCode)
    } else {
        None
    }
}

/// 1 台の流れ: 開く → 読む → 切れたらつなぎ直す（続けての失敗が max_retries を超えたら止める）
pub(super) async fn run_stream(live: Arc<Live>, node: Node, epoch: u64, mut stop: watch::Receiver<Option<Stop>>) {
    let o = live.opts.clone();
    let mut failures = 0u32;
    let mut attempt = 0u32;
    let why: Stop = 'outer: loop {
        if let Some(r) = stop_requested(&stop) {
            break r;
        }
        attempt = attempt.saturating_add(1);
        let opened_at = Instant::now();
        live.record(&node.id, epoch, attempt, Kind::Open, Data { configured_local: Some(node.local), ..Data::default() });
        let mut err = String::new();
        match live.route.open(&node, &o) {
            Err(e) => {
                err = e;
                live.record(
                    &node.id,
                    epoch,
                    attempt,
                    Kind::End,
                    Data { category: Some(Category::OpenFailed), duration_ms: Some(elapsed_ms(opened_at)), ..Data::default() },
                );
            }
            Ok(mut src) => {
                let started = Instant::now();
                let mut got = 0u64;
                let mut planned = false;
                let mut declared = None;
                let mut last_seq = None;
                let mut last_at = None;
                let mut rejected = 0u64;
                let mut timed_out = false;
                let mut last_sample = None;
                let mut timeout_sample_age = None;
                let mut checkpoint = Instant::now();
                let transport_pid = src.transport_pid;
                let stopped: Option<Stop> = loop {
                    let wait = if got == 0 { o.first_line_timeout } else { o.stall_timeout };
                    tokio::select! {
                        r = stop.changed() => {
                            match r {
                                Err(_) => break Some(Stop::AppExit),
                                Ok(()) => if let Some(s) = stop_requested(&stop) { break Some(s) },
                            }
                        }
                        r = tokio::time::timeout(wait, src.lines.recv()) => match r {
                            Ok(Some(line)) => match parse_line(&line) {
                                Some(Msg::Hello(h)) => {
                                    live.record(&node.id, epoch, attempt, Kind::Hello, Data { native_rust: match h.implementation.as_deref() { Some("rust") => Some(true), _ => None }, reported_version: crate::monitor_journal::reported_version(h.agent.as_deref()), ..Data::default() });
                                    live.on_hello(&node.id, epoch, *h);
                                }
                                Some(Msg::Sample(s)) => {
                                    got += 1;
                                    last_sample = Some(Instant::now());
                                    last_seq = s.seq; last_at = Some(now_ms());
                                    live.on_sample(&node.id, epoch, *s);
                                    if got == 1 { live.record(&node.id, epoch, attempt, Kind::FirstSample, Data { seq: last_seq, last_sample_at: last_at, transport_pid, duration_ms: Some(elapsed_ms(opened_at)), ..Data::default() }); }
                                    if checkpoint.elapsed() >= Duration::from_secs(60) {
                                        live.record(&node.id, epoch, attempt, Kind::Checkpoint, Data { samples: Some(got), rejected: Some(rejected), seq: last_seq, last_sample_at: last_at, duration_ms: Some(elapsed_ms(opened_at)), ..Data::default() });
                                        checkpoint = Instant::now();
                                    }
                                }
                                Some(Msg::End(reason)) => { planned = reason == "max_age"; declared = Some(Reason::end(&reason)); },
                                None => { rejected += 1; }
                            },
                            Ok(None) => break None,
                            Err(_) => {
                                timed_out = true;
                                timeout_sample_age = last_sample.map(|at: Instant| at.elapsed());
                                err = format!("{} 秒間データが来ない", secs(wait));
                                break None;
                            }
                        }
                    }
                };
                let trigger = if timed_out {
                    CloseTrigger::Timeout
                } else if stopped.is_some() {
                    CloseTrigger::Stop
                } else {
                    CloseTrigger::ChannelEnd
                };
                let ended = src.close_with(trigger, timeout_sample_age).await;
                let category = end_category(timed_out, got, &ended);
                live.record(
                    &node.id,
                    epoch,
                    attempt,
                    Kind::End,
                    Data {
                        reason: declared,
                        category,
                        exit_code: ended.code,
                        wait_observed: ended.wait_observed,
                        stdout: Some(ended.stdout),
                        stderr: Some(ended.stderr),
                        stdin: Some(ended.stdin),
                        transport_pid,
                        duration_ms: Some(elapsed_ms(opened_at)),
                        samples: Some(got),
                        rejected: Some(rejected),
                        seq: last_seq,
                        last_sample_at: last_at,
                        io: ended.io.clone().map(StreamIo::Observed),
                        ..Data::default()
                    },
                );
                if let Some(s) = stopped {
                    break 'outer s;
                }
                if planned {
                    // サンプラーが区切りで終わった。数えずに、少しだけ置いてつなぎ直す
                    failures = 0;
                    live.record(
                        &node.id,
                        epoch,
                        attempt,
                        Kind::Retry,
                        Data {
                            reason: Some(Reason::MaxAge),
                            policy: Some(Policy::PlannedRecycle),
                            delay_ms: Some(o.backoff_for(1).min(Duration::from_millis(500)).as_millis() as u64),
                            ..Data::default()
                        },
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(o.backoff_for(1).min(Duration::from_millis(500))) => {}
                        r = stop.changed() => {
                            match r {
                                Err(_) => break 'outer Stop::AppExit,
                                Ok(()) => if let Some(s) = stop_requested(&stop) { break 'outer s },
                            }
                        }
                    }
                    continue;
                }
                // しばらく続いていたなら、続けての失敗は数え直す
                if got > 0 && started.elapsed() >= Duration::from_secs(30) {
                    failures = 0;
                }
                if err.is_empty() {
                    err = ended.describe();
                }
            }
        }
        failures += 1;
        if failures > o.max_retries {
            break Stop::Disconnected(err);
        }
        live.set_phase(&node.id, epoch, Phase::Retrying, Some(err), failures);
        let wait = o.backoff_for(failures);
        live.record(
            &node.id,
            epoch,
            attempt,
            Kind::Retry,
            Data { policy: Some(Policy::StreamBackoff), delay_ms: Some(wait.as_millis() as u64), ..Data::default() },
        );
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            r = stop.changed() => {
                match r {
                    Err(_) => break Stop::AppExit,
                    Ok(()) => if let Some(s) = stop_requested(&stop) { break s },
                }
            }
        }
        live.set_phase(&node.id, epoch, Phase::Connecting, None, failures);
    };
    let reason = match &why {
        Stop::User => Reason::User,
        Stop::Idle => Reason::Idle,
        Stop::Hidden => Reason::Hidden,
        Stop::Disconnected(_) => Reason::Disconnected,
        Stop::Limit => Reason::Limit,
        Stop::AppExit => Reason::AppExit,
        Stop::Restart => Reason::Restart,
        Stop::Excluded => Reason::Excluded,
    };
    live.record(&node.id, epoch, attempt, Kind::Stopped, Data { reason: Some(reason), ..Data::default() });
    live.finish(&node.id, epoch, why);
}
