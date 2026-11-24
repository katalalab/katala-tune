use super::*;

/// 1 行の上限（壊れた出力でメモリを使い切らない）
pub(super) const MAX_LINE: usize = 256 * 1024;

pub(super) const MAX_CORES: usize = 1024;

pub(super) const MAX_TOP: usize = 20;

pub(crate) const MAX_GPUS: usize = 16;

pub(super) const MAX_NAME: usize = 120;

/// 設定。既定は 1 秒ごと・上位プロセスは 5 秒ごと・直近 300 点・同時に 6 台まで・合図が 2 分途切れたら止める
#[derive(Clone, Debug)]
pub struct Opts {
    /// サンプラーの間隔（1〜2 秒）
    pub interval: Duration,
    /// 上位プロセスの間隔
    pub procs_every: Duration,
    /// サンプラーはこの時間で自分から終わる（end: max_age）。続けるなら数えずにつなぎ直す
    pub sampler_max_age: Duration,
    /// 機体ごとに覚えておく点の数
    pub ring: usize,
    /// 同時に流せる機体の数
    pub max_streams: usize,
    /// 画面からの合図（liveStart の呼び直し）がこの時間途切れたら止める
    pub idle_stop: Duration,
    /// 続けて失敗したときにつなぎ直す回数（超えたら「接続切れ」で止める）
    pub max_retries: u32,
    /// 最初の 1 行を待つ時間（SSH の接続とサンプラーの起動を含む）
    pub first_line_timeout: Duration,
    /// 2 行目以降が来ないとき、切れたとみなすまでの時間
    pub stall_timeout: Duration,
    /// つなぎ直すまでの待ち（失敗の回数ごと。足りなければ最後の値）
    pub backoff: Vec<Duration>,
    /// 画面へまとめて送る間隔
    pub flush: Duration,
    /// 開始したときに画面へ渡す点の上限（多ければ間引く）
    pub ui_points: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            interval: Duration::from_secs(1),
            procs_every: Duration::from_secs(5),
            sampler_max_age: Duration::from_secs(900),
            ring: 300,
            max_streams: 6,
            idle_stop: Duration::from_secs(120),
            max_retries: 3,
            first_line_timeout: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(10),
            backoff: vec![Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10)],
            flush: Duration::from_secs(1),
            ui_points: 180,
        }
    }
}

impl Opts {
    pub(super) fn backoff_for(&self, failures: u32) -> Duration {
        let i = (failures.max(1) - 1) as usize;
        self.backoff.get(i).or(self.backoff.last()).copied().unwrap_or(Duration::from_secs(2))
    }
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/// サンプラーの 1 行目（何が取れるか）
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Hello {
    pub v: Option<u32>,
    pub os: Option<String>,
    pub cores: Option<u32>,
    pub interval: Option<f64>,
    pub procs_every: Option<f64>,
    pub mem_total_gb: Option<f64>,
    pub session_epoch_ms: Option<i64>,
    #[serde(default)]
    pub has: Map<String, Value>,
    /// 取れなかったもの（PS の ConvertTo-Json は 1 件だと配列にしないことがあるので Value のまま受ける）
    #[serde(default)]
    pub errors: Value,
    /// サンプラーの実装（"rust" はネイティブの `tune-agent sample`。スクリプトは書かない）と、その版
    #[serde(rename = "impl", default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Mem {
    pub used_pct: Option<f64>,
    pub swap_used_gb: Option<f64>,
    pub swap_total_gb: Option<f64>,
    pub commit_pct: Option<f64>,
    pub commit_gb: Option<f64>,
    pub commit_limit_gb: Option<f64>,
    pub pressure: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Disk {
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Net {
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Gpu {
    pub uuid: Option<String>,
    pub name: Option<String>,
    pub util: Option<f64>,
    pub mem_used_mb: Option<f64>,
    pub mem_total_mb: Option<f64>,
    pub mem_free_mb: Option<f64>,
    pub mem_used_pct: Option<f64>,
    /// Physical provider index, preserved independently of the bounded metric slot.
    pub adapter_index: Option<u32>,
    /// Validated storage key (0..MAX_GPUS). Never trust a slot supplied by a sampler.
    #[serde(skip_deserializing)]
    pub storage_slot: Option<u8>,
    pub sampled_at: Option<i64>,
    pub memory_kind: Option<String>,
    pub memory_status: Option<String>,
    pub memory_reason: Option<String>,
    pub power_w: Option<f64>,
    pub power_limit_w: Option<f64>,
    pub power_min_w: Option<f64>,
    pub power_max_w: Option<f64>,
    pub temp_c: Option<f64>,
    pub clocks_graphics_mhz: Option<f64>,
    pub clocks_sm_mhz: Option<f64>,
    pub clocks_memory_mhz: Option<f64>,
    pub pstate: Option<String>,
    pub source: Option<String>,
    pub available: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Power {
    pub package_w: Option<f64>,
    pub package_source: Option<String>,
    pub soc_w: Option<f64>,
    pub soc_source: Option<String>,
    pub platform_w: Option<f64>,
    pub platform_source: Option<String>,
    pub available: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Proc {
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub name: String,
    pub app: Option<String>,
    /// 1 コア換算の %
    pub cpu: Option<f64>,
    pub mem_mb: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Procs {
    pub count: Option<u64>,
    #[serde(default)]
    pub top_cpu: Vec<Proc>,
    #[serde(default)]
    pub top_mem: Vec<Proc>,
}

/// サンプラー自身の負荷（それまでの CPU 秒と RSS）
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct SelfUse {
    pub cpu_s: Option<f64>,
    pub rss_mb: Option<f64>,
}

/// サンプラーの s 行（そのまま）
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawSample {
    pub t: Option<f64>,
    pub seq: Option<u64>,
    pub cpu: Option<f64>,
    #[serde(default)]
    pub cores: Vec<Option<f64>>,
    pub mem: Option<Mem>,
    pub disk: Option<Disk>,
    pub net: Option<Net>,
    pub power: Option<Power>,
    pub gpu: Option<Vec<Gpu>>,
    pub procs: Option<Procs>,
    #[serde(rename = "self")]
    pub me: Option<SelfUse>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum Line {
    #[serde(rename = "hello")]
    Hello(Box<Hello>),
    #[serde(rename = "s")]
    Sample(Box<RawSample>),
    #[serde(rename = "end")]
    End {
        #[serde(default)]
        reason: Option<String>,
    },
}

/// 解析した 1 行
#[derive(Clone, Debug)]
pub enum Msg {
    Hello(Box<Hello>),
    Sample(Box<RawSample>),
    /// サンプラーが自分から終わる（max_age: 長く動いたので区切る、count: 確認用）
    End(String),
}

/// サンプラーの 1 行を読む。JSON でない行（警告など）・知らない種類・壊れた行は None
pub fn parse_line(line: &str) -> Option<Msg> {
    let s = line.trim_start_matches('\u{FEFF}').trim();
    if !s.starts_with('{') || s.len() > MAX_LINE {
        return None;
    }
    match serde_json::from_str::<Line>(s).ok()? {
        Line::Hello(h) => Some(Msg::Hello(h)),
        Line::Sample(r) => Some(Msg::Sample(r)),
        Line::End { reason } => Some(Msg::End(reason.unwrap_or_default())),
    }
}

pub(super) fn pct(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite()).map(|x| x.clamp(0.0, 100.0))
}

pub(super) fn nonneg(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite() && *x >= 0.0)
}

pub(super) fn short(s: &str) -> String {
    s.chars().take(MAX_NAME).collect()
}

pub(super) fn gpu_slot(gpus: &[Gpu], i: usize) -> Result<u8, &'static str> {
    match gpus[i].adapter_index {
        Some(id) if id >= MAX_GPUS as u32 => Err("adapter-index-out-of-range"),
        Some(id) if gpus.iter().filter(|g| g.adapter_index == Some(id)).count() > 1 => Err("duplicate-adapter-index"),
        Some(id) => Ok(id as u8),
        // Older single-GPU samplers have no index. Multiple unindexed rows cannot
        // be mapped by array order without mixing their history after a reorder.
        None if gpus.len() == 1 => Ok(0),
        None => Err("missing-adapter-index"),
    }
}

pub(super) fn clean_procs(p: Procs) -> Procs {
    let fix = |xs: Vec<Proc>| -> Vec<Proc> {
        xs.into_iter()
            .take(MAX_TOP)
            .map(|x| Proc { pid: x.pid, name: short(&x.name), app: x.app.as_deref().map(short), cpu: nonneg(x.cpu), mem_mb: nonneg(x.mem_mb) })
            .collect()
    };
    Procs { count: p.count, top_cpu: fix(p.top_cpu), top_mem: fix(p.top_mem) }
}

/// リングに入れる 1 点（範囲に収めた値と、受け取った時刻）
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Sample {
    /// この機体（アプリ）で受け取った時刻（ms）。機体どうしの時計のずれに左右されない
    pub t: i64,
    /// サンプラーの時刻（ms）。t との差が遅延の目安（時計が合っているとき）
    pub src_t: Option<i64>,
    pub seq: Option<u64>,
    pub session_epoch: Option<String>,
    /// 機体全体の CPU（%）
    pub cpu: Option<f64>,
    /// コアごとの CPU（%）
    pub cores: Vec<Option<f64>>,
    pub mem: Option<Mem>,
    pub disk: Option<Disk>,
    pub net: Option<Net>,
    pub power: Option<Power>,
    pub gpu: Option<Vec<Gpu>>,
}

impl Sample {
    /// 受け取った行から、範囲に収めた点と、付いていれば上位プロセス・サンプラー自身の負荷を取り出す
    pub fn from_raw(r: RawSample, received: i64) -> (Sample, Option<Procs>, Option<SelfUse>) {
        let mem = r.mem.map(|m| Mem {
            used_pct: pct(m.used_pct),
            swap_used_gb: nonneg(m.swap_used_gb),
            swap_total_gb: nonneg(m.swap_total_gb),
            commit_pct: pct(m.commit_pct),
            commit_gb: nonneg(m.commit_gb),
            commit_limit_gb: nonneg(m.commit_limit_gb),
            pressure: m.pressure.map(|p| short(&p)),
        });
        let gpu = r.gpu.map(|g| {
            let mut gpus: Vec<Gpu> = g
                .into_iter()
                .take(MAX_GPUS)
                .map(|x| Gpu {
                    uuid: x.uuid.map(|s| short(&s)),
                    name: x.name.map(|n| short(&n)),
                    util: pct(x.util),
                    mem_used_mb: nonneg(x.mem_used_mb),
                    mem_total_mb: nonneg(x.mem_total_mb),
                    mem_free_mb: nonneg(x.mem_free_mb),
                    mem_used_pct: pct(x.mem_used_pct),
                    adapter_index: x.adapter_index,
                    storage_slot: None,
                    sampled_at: x.sampled_at,
                    memory_kind: x.memory_kind.map(|s| short(&s)),
                    memory_status: x.memory_status.map(|s| short(&s)),
                    memory_reason: x.memory_reason.map(|s| short(&s)),
                    power_w: nonneg(x.power_w),
                    power_limit_w: nonneg(x.power_limit_w),
                    power_min_w: nonneg(x.power_min_w),
                    power_max_w: nonneg(x.power_max_w),
                    temp_c: nonneg(x.temp_c),
                    clocks_graphics_mhz: nonneg(x.clocks_graphics_mhz),
                    clocks_sm_mhz: nonneg(x.clocks_sm_mhz),
                    clocks_memory_mhz: nonneg(x.clocks_memory_mhz),
                    pstate: x.pstate.map(|s| short(&s)),
                    source: x.source.map(|s| short(&s)),
                    available: x.available,
                })
                .collect();
            for i in 0..gpus.len() {
                let slot = gpu_slot(&gpus, i);
                let g = &mut gpus[i];
                match slot {
                    Ok(slot) => g.storage_slot = Some(slot),
                    Err(reason) => {
                        g.mem_used_mb = None;
                        g.mem_total_mb = None;
                        g.mem_free_mb = None;
                        g.mem_used_pct = None;
                        g.memory_status = Some("unavailable".into());
                        g.memory_reason = Some(reason.into());
                    }
                }
            }
            gpus
        });
        let power = r.power.map(|x| Power {
            package_w: nonneg(x.package_w),
            package_source: x.package_source.map(|s| short(&s)),
            soc_w: nonneg(x.soc_w),
            soc_source: x.soc_source.map(|s| short(&s)),
            platform_w: nonneg(x.platform_w),
            platform_source: x.platform_source.map(|s| short(&s)),
            available: x.available,
        });
        let s = Sample {
            t: received,
            src_t: r.t.filter(|t| t.is_finite() && *t > 0.0).map(|t| t as i64),
            seq: r.seq,
            session_epoch: None,
            cpu: pct(r.cpu),
            cores: r.cores.into_iter().take(MAX_CORES).map(pct).collect(),
            mem,
            disk: r.disk.map(|d| Disk { read_bps: nonneg(d.read_bps), write_bps: nonneg(d.write_bps) }),
            net: r.net.map(|n| Net { rx_bps: nonneg(n.rx_bps), tx_bps: nonneg(n.tx_bps) }),
            power,
            gpu,
        };
        (s, r.procs.map(clean_procs), r.me)
    }

    /// 画面へ渡す 1 点（スパークラインに要る値だけ。無い値は入れない）
    pub fn point(&self) -> Value {
        let mut m = Map::new();
        m.insert("t".into(), Value::from(self.t));
        if let Some(seq) = self.seq {
            m.insert("seq".into(), Value::from(seq));
        }
        if let Some(epoch) = &self.session_epoch {
            m.insert("epoch".into(), Value::from(epoch.clone()));
        }
        let mut put = |k: &str, v: Option<f64>, digits: i32| {
            if let Some(x) = v {
                let f = 10f64.powi(digits);
                m.insert(k.into(), json!((x * f).round() / f));
            }
        };
        let mem = self.mem.as_ref();
        put("cpu", self.cpu, 1);
        put("mem", mem.and_then(|x| x.used_pct), 1);
        put("swap", mem.and_then(|x| x.swap_used_gb), 2);
        put("commit", mem.and_then(|x| x.commit_pct), 1);
        put("dr", self.disk.as_ref().and_then(|d| d.read_bps), 0);
        put("dw", self.disk.as_ref().and_then(|d| d.write_bps), 0);
        put("rx", self.net.as_ref().and_then(|n| n.rx_bps), 0);
        put("tx", self.net.as_ref().and_then(|n| n.tx_bps), 0);
        put("power_cpu_w", self.power.as_ref().and_then(|p| p.package_w), 1);
        put("power_soc_w", self.power.as_ref().and_then(|p| p.soc_w), 2);
        put("power_platform_w", self.power.as_ref().and_then(|p| p.platform_w), 2);
        let power_source = self.power.as_ref().and_then(|p| p.package_source.as_ref());
        if let Some(g) = &self.gpu {
            put("gpu", g.iter().filter_map(|x| x.util).reduce(f64::max), 1);
            let memory_pct = |x: &Gpu| x.mem_used_mb.zip(x.mem_total_mb).filter(|(u, t)| *t > 0.0 && *u <= *t).map(|(u, t)| u / t * 100.0);
            put("gmem", g.iter().enumerate().filter(|(i, _)| gpu_slot(g, *i).is_ok()).filter_map(|(_, x)| memory_pct(x)).reduce(f64::max), 1);
            for (i, x) in g.iter().enumerate() {
                let Ok(id) = gpu_slot(g, i) else { continue };
                put(&format!("gpu_{id}_vram_used_mib"), x.mem_used_mb, 0);
                put(&format!("gpu_{id}_vram_total_mib"), x.mem_total_mb, 0);
                put(&format!("gpu_{id}_vram_free_mib"), x.mem_free_mb, 0);
                put(&format!("gpu_{id}_vram_pct"), memory_pct(x), 1);
            }
            put("gtemp", g.iter().filter_map(|x| x.temp_c).reduce(f64::max), 0);
            if !g.is_empty() && g.iter().all(|x| x.power_w.is_some()) {
                put("power_gpu_w", Some(g.iter().map(|x| x.power_w.unwrap_or_default()).sum()), 1);
            }
        }
        put("lag", self.src_t.map(|s| (self.t - s) as f64), 0);
        if let Some(source) = power_source {
            m.insert("power_cpu_source".into(), Value::from(source.clone()));
        }
        if let Some(source) = self.power.as_ref().and_then(|p| p.soc_source.as_ref()) {
            m.insert("power_soc_source".into(), Value::from(source.clone()));
        }
        if let Some(g) = self.gpu.as_ref()
            && !g.is_empty()
        {
            m.insert("power_gpu_source".into(), Value::from(g.iter().map(|g| g.source.as_deref().unwrap_or("unknown")).collect::<Vec<_>>().join("+")));
        }
        Value::Object(m)
    }
}

// ---------------------------------------------------------------------------
// 保持
// ---------------------------------------------------------------------------

/// 直近 cap 個だけを覚えておく入れ物
#[derive(Clone, Debug)]
pub struct Ring<T> {
    buf: VecDeque<T>,
    cap: usize,
}

impl<T> Ring<T> {
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Ring { buf: VecDeque::with_capacity(cap.min(4096)), cap }
    }

    pub fn push(&mut self, x: T) {
        if self.buf.len() >= self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(x);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn last(&self) -> Option<&T> {
        self.buf.back()
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.buf.iter()
    }

    /// 新しいほうから n 個（古い順に並べて返す）
    pub fn tail(&self, n: usize) -> impl Iterator<Item = &T> {
        self.buf.iter().skip(self.buf.len().saturating_sub(n))
    }
}

/// 点（`Sample::point` の形）を max 個以下に間引く。続く点をまとめ、値は平均、時刻はまとめた最後の点
pub fn downsample(points: &[Value], max: usize) -> Vec<Value> {
    let max = max.max(1);
    if points.len() <= max {
        return points.to_vec();
    }
    let k = points.len().div_ceil(max);
    points
        .chunks(k)
        .map(|c| {
            // キーごとの (合計, 個数)。無い値は数えない
            let mut acc: HashMap<&str, (f64, usize)> = HashMap::new();
            for p in c {
                for (key, v) in p.as_object().into_iter().flatten() {
                    if let (false, Some(x)) = (key == "t", v.as_f64()) {
                        let e = acc.entry(key.as_str()).or_default();
                        *e = (e.0 + x, e.1 + 1);
                    }
                }
            }
            let mut out = Map::new();
            out.insert("t".into(), c.last().and_then(|p| p.get("t")).cloned().unwrap_or(Value::Null));
            for (key, (sum, n)) in acc {
                out.insert(key.to_string(), json!((sum / n as f64 * 10.0).round() / 10.0));
            }
            Value::Object(out)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 取得の経路
// ---------------------------------------------------------------------------

/// 終わり方（終了コードと、標準エラーの最後の部分）
#[derive(Clone, Debug, Default)]
pub struct Ended {
    pub code: Option<i32>,
    pub err: String,
    pub wait_observed: Option<bool>,
    pub stdout: PipeEnd,
    pub stderr: PipeEnd,
    pub stdin: InputEnd,
    pub wait_origin: Option<WaitOrigin>,
    pub kill_attempted: Option<bool>,
    pub io: Option<StreamBoundary>,
}

impl Ended {
    pub(super) fn describe(&self) -> String {
        let tail: Vec<&str> = self.err.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let last = tail[tail.len().saturating_sub(2)..].join(" / ");
        match (self.code, last.is_empty()) {
            (Some(c), false) => format!("exit {c}: {last}"),
            (Some(c), true) => format!("exit {c}"),
            (None, false) => last,
            (None, true) => "サンプラーが終わった".into(),
        }
    }
}

/// 行を流す元。`lines` が閉じたら終わり。`close` で止めて後始末する（drop しても止まる）
pub struct Source {
    pub lines: mpsc::Receiver<String>,
    stop: Option<oneshot::Sender<()>>,
    done: Option<oneshot::Receiver<Ended>>,
    pub transport_pid: Option<u32>,
    boundary: Option<Arc<Mutex<ReadBoundary>>>,
}

impl Source {
    pub fn new(lines: mpsc::Receiver<String>, stop: oneshot::Sender<()>, done: oneshot::Receiver<Ended>) -> Source {
        Source { lines, stop: Some(stop), done: Some(done), transport_pid: None, boundary: None }
    }

    /// 止めて、終わるのを待つ
    pub async fn close(self) -> Ended {
        self.close_with(CloseTrigger::Stop, None).await
    }

    pub(super) async fn close_with(mut self, trigger: CloseTrigger, sample_age: Option<Duration>) -> Ended {
        // Snapshot before sending stop: EOF caused by closing stdin must not become pre-close EOF.
        let mut io = self.boundary.as_ref().and_then(|b| b.lock().ok().map(|s| s.snapshot()));
        if let Some(d) = io.as_mut() {
            d.close_trigger = Some(trigger);
            d.sample_age_ms = sample_age.and_then(|age| u32::try_from(age.as_millis()).ok());
            d.overflow |= sample_age.is_some() && d.sample_age_ms.is_none();
        }
        let began = Instant::now();
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        let mut ended = match self.done.take() {
            Some(d) => d.await.unwrap_or_default(),
            None => Ended::default(),
        };
        if let Some(d) = io.as_mut() {
            d.close_ms = u32::try_from(began.elapsed().as_millis()).ok();
            d.overflow |= d.close_ms.is_none();
            d.wait_origin = ended.wait_origin;
            d.kill_attempted = ended.kill_attempted;
        }
        ended.io = io;
        ended
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

/// 取得の経路。今は子プロセス（ローカル・SSH）、将来は tune-agent の暗号化通信
pub trait Route: Send + Sync + 'static {
    fn open(&self, node: &Node, opts: &Opts) -> Result<Source, String>;
}

/// 子プロセスとして動かすもの
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    /// 標準入力に渡すもの（スクリプト本体）
    pub stdin: Vec<u8>,
    /// 渡した後も標準入力を開いておく。サンプラーは標準入力が閉じたら終わるので、
    /// こちらが止めた・落ちた・SSH が切れたときに機体側に残らない
    pub keep_stdin: bool,
    /// 終わったら消す一時ファイル（Windows のローカル実行）
    pub cleanup: Option<PathBuf>,
}

/// SSH の多重化（ControlMaster）を使わない。多重化の親が接続を持ち続けると、こちらで ssh を止めても
/// 機体側のサンプラーに切断が伝わらない（実機で確認）。ライブ表示は自分の接続を持ち、止めたら接続ごと閉じる
pub const LIVE_SSH_OPTS: [&str; 4] = ["-o", "ControlMaster=no", "-o", "ControlPath=none"];

pub(super) fn live_ssh_args(node: &Node, remote: &str) -> Vec<String> {
    let mut a: Vec<String> = collect::SSH_OPTS.iter().chain(LIVE_SSH_OPTS.iter()).map(|s| s.to_string()).collect();
    a.push(node.alias.clone());
    a.push(collect::remote_command(node, remote));
    a
}

/// 標準入力の先頭 n バイトをスクリプトとして動かし、残りの標準入力は開いたままにする python3 -c の中身。
/// シングルクォートを含めない（リモートのシェルで '…' に包むため）
pub(super) fn python_boot(n: usize) -> String {
    format!("import sys;exec(compile(sys.stdin.buffer.read({n}),\"live_mac.py\",\"exec\"))")
}

pub(super) fn secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s.fract() == 0.0 { format!("{}", s as u64) } else { format!("{s}") }
}

/// スクリプト長だけ読む。残りの stdin は EOF 監視に残し、実行先にファイルを作らない。
pub(super) fn windows_boot(n: usize, params: &str) -> String {
    format!(
        "[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);$i=[Console]::OpenStandardInput();$b=New-Object byte[] {n};$p=0;while($p -lt {n}){{$r=$i.Read($b,$p,({n}-$p));if($r -eq 0){{exit 2}};$p+=$r}};$s=[Text.Encoding]::UTF8.GetString($b);. ([ScriptBlock]::Create($s)) {params}"
    )
}

/// 各機体に置くネイティブのサンプラー（`tune-agent sample`。置き方は docs/architecture.md）
pub const NATIVE_REMOTE: &str = "~/.katala-tune/bin/tune-agent.exe";

/// macOS の置き場所（名前に .exe が付かない）
pub const NATIVE_REMOTE_MAC: &str = "~/.katala-tune/bin/tune-agent";

pub(super) fn native_args(o: &Opts) -> Vec<String> {
    [
        "sample".to_string(),
        format!("interval={}", secs(o.interval)),
        format!("procs={}", secs(o.procs_every)),
        format!("max={}", secs(o.sampler_max_age)),
        "watch=1".into(),
    ]
    .to_vec()
}

/// GPUI は束縛済み data/bin だけ。未束縛の CLI/Tauri は従来どおり
/// アプリの隣、無ければ helpers の namespace を探す。
pub(super) fn local_native() -> Option<PathBuf> {
    let name = crate::helpers::exe("tune-agent");
    if let Some(bin) = crate::helpers::bound_bin_dir() {
        let native = bin.join(&name);
        return native.is_file().then_some(native);
    }
    let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(&name)));
    let managed = crate::helpers::bin_dir().join(&name);
    [beside, Some(managed)].into_iter().flatten().find(|p| p.is_file())
}

/// 機体ごとの起動方法。macOS / Windows とも標準入力でスクリプトを渡す。
/// 渡した後も stdin を開き、停止時に閉じる。SSH は多重化しない。
pub fn launch(node: &Node, o: &Opts) -> Result<Launch, String> {
    if let Some(refused) = node.refusal() {
        return Err(refused);
    }
    if node.is_mac() {
        let a = format!("interval={} procs={} max={} watch=1", secs(o.interval), secs(o.procs_every), secs(o.sampler_max_age));
        let boot = python_boot(MAC_LIVE.len());
        let stdin = MAC_LIVE.as_bytes().to_vec();
        // ネイティブ（tune-agent sample）を先に使い、置いていない・起動できない（古い版・合わないアーキテクチャ）なら
        // Python 版。ネイティブは起動に失敗するとき標準入力を読まずに終わるので、Python 版はそのままスクリプトを読める。
        // ネイティブが動いたときは、標準入力のスクリプトを読み捨てる
        let native = native_args(o).join(" ");
        if node.local {
            if let Some(exe) = local_native() {
                // パスは引数で渡す（空白を含む .app の中でも引用符を組み立てない）
                let script = format!("\"$0\" {native} || exec /usr/bin/env python3 -c '{boot}' {a}");
                let args = vec!["-c".to_string(), script, exe.to_string_lossy().into_owned()];
                return Ok(Launch { program: "/bin/sh".into(), args, stdin, keep_stdin: true, cleanup: None });
            }
            let mut args = vec!["python3".to_string(), "-c".to_string(), boot];
            args.extend(a.split(' ').map(str::to_string));
            return Ok(Launch { program: "/usr/bin/env".into(), args, stdin, keep_stdin: true, cleanup: None });
        }
        let remote = format!(
            "[ -x {NATIVE_REMOTE_MAC} ] && {NATIVE_REMOTE_MAC} {native} || {{ command -v python3 >/dev/null && exec python3 -c '{boot}' {a} || exec /usr/bin/python3 -c '{boot}' {a}; }}"
        );
        return Ok(Launch { program: "ssh".into(), args: live_ssh_args(node, &remote), stdin, keep_stdin: true, cleanup: None });
    }
    if node.is_windows() {
        let params = format!("-Interval {} -ProcEvery {} -MaxSeconds {} -WatchStdin", secs(o.interval), secs(o.procs_every), secs(o.sampler_max_age));
        let script = WIN_LIVE.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(WIN_LIVE);
        let boot = windows_boot(script.len(), &params);
        let utf16: Vec<u8> = boot.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = crate::logs::base64(&utf16);
        let stdin = script.to_vec();
        let native = native_args(o);
        if node.local {
            // 同じ版のサンプラー（アプリの隣・~/.katala-tune/bin）があればそれを、無ければ PowerShell 版を動かす
            if let Some(exe) = local_native() {
                return Ok(Launch { program: exe.to_string_lossy().into_owned(), args: native, stdin: Vec::new(), keep_stdin: true, cleanup: None });
            }
            let args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded].iter().map(|s| s.to_string()).collect();
            return Ok(Launch { program: "powershell.exe".into(), args, stdin, keep_stdin: true, cleanup: None });
        }
        // 既定シェルは Git Bash（collect.rs と同じ）。置いてあるネイティブのサンプラーを先に使い、無ければ PowerShell 版。
        // どちらになっても標準入力には PowerShell 版を送る（ネイティブは読み捨てる）
        let remote = format!(
            "if [ -x {NATIVE_REMOTE} ]; then exec {NATIVE_REMOTE} {}; else exec powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}; fi",
            native.join(" ")
        );
        return Ok(Launch { program: "ssh".into(), args: live_ssh_args(node, &remote), stdin, keep_stdin: true, cleanup: None });
    }
    Err(format!("この OS（{}）はライブ表示に対応していない", node.os))
}

/// 子プロセスを起動し、標準出力を 1 行ずつ流す。止めると（drop しても）子プロセスを終わらせ、一時ファイルを消す
pub fn spawn(l: Launch) -> Result<Source, String> {
    let mut c = collect::command(&l.program);
    c.args(&l.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            if let Some(f) = &l.cleanup {
                let _ = std::fs::remove_file(f);
            }
            return Err(format!("{} を起動できない: {e}", l.program));
        }
    };
    let transport_pid = child.id();
    let mut stdin = child.stdin.take();
    let input = l.stdin;
    let keep = l.keep_stdin;
    // keep のとき、標準入力は止めるとき（closed の送り手が無くなったとき）まで開いておく
    let (closed_tx, closed_rx) = oneshot::channel::<()>();
    let stdin_task = tokio::spawn(async move {
        let mut outcome = InputEnd::Unknown;
        if let Some(si) = stdin.as_mut() {
            outcome = if si.write_all(&input).await.is_ok() && si.flush().await.is_ok() { InputEnd::Written } else { InputEnd::WriteError };
            if !keep && si.shutdown().await.is_err() {
                outcome = InputEnd::WriteError;
            }
        }
        if keep {
            let _ = closed_rx.await;
        }
        drop(stdin);
        outcome
    });
    let (tx, rx) = mpsc::channel(256);
    let boundary = Arc::new(Mutex::new(ReadBoundary::new()));
    let out_task = tokio::spawn(read_lines(child.stdout.take(), tx, boundary.clone()));
    let err_task = tokio::spawn(read_tail(child.stderr.take(), 4096));
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (done_tx, done_rx) = oneshot::channel();
    let cleanup = l.cleanup;
    let wait_boundary = boundary.clone();
    tokio::spawn(async move {
        let mut closed_tx = Some(closed_tx);
        // stop_rx は止める指示か、Source が捨てられた（送り手が無くなった）ときに終わる
        let mut kill_attempted = false;
        let (status, wait_origin) = tokio::select! {
            st = child.wait() => (st.ok(), WaitOrigin::Natural),
            _ = stop_rx => {
                // まず標準入力を閉じる。サンプラーは自分で後始末して終わる（Windows は nvidia-smi も止める）。
                // 終わらなければ（応答の無い機体など）強制的に止める
                drop(closed_tx.take());
                let grace = if keep { Duration::from_millis(2500) } else { Duration::ZERO };
                let status = match tokio::time::timeout(grace, child.wait()).await {
                    Ok(st) => st.ok(),
                    Err(_) => {
                        // API attempt only; an error/already-exited race does not prove a kill.
                        kill_attempted = true;
                        let _ = child.start_kill();
                        child.wait().await.ok()
                    }
                };
                (status, WaitOrigin::Close)
            }
        };
        if let Ok(mut b) = wait_boundary.lock() {
            b.wait_observed = status.as_ref().map(|_| true);
        }
        drop(closed_tx);
        // 孫プロセスがパイプを持ち続けても止まらないよう、読み終わりを待つのは少しだけ
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let (stdout, tail, stdin) = tokio::join!(reader_result(out_task, deadline), reader_result(err_task, deadline), reader_result(stdin_task, deadline));
        let (err, stderr) = tail.unwrap_or_else(|| (String::new(), PipeEnd::Cancelled));
        if let Some(f) = cleanup {
            let _ = std::fs::remove_file(f);
        }
        let _ = done_tx.send(Ended {
            code: status.as_ref().and_then(|s| s.code()),
            wait_observed: Some(status.is_some()),
            err,
            stdout: stdout.unwrap_or(PipeEnd::Cancelled),
            stderr,
            stdin: stdin.unwrap_or(InputEnd::Cancelled),
            wait_origin: Some(wait_origin),
            kill_attempted: Some(kill_attempted),
            io: None,
        });
    });
    let mut source = Source::new(rx, stop_tx, done_rx);
    source.transport_pid = transport_pid;
    source.boundary = Some(boundary);
    Ok(source)
}

pub(super) async fn reader_result<T: Send + 'static>(mut task: JoinHandle<T>, deadline: tokio::time::Instant) -> Option<T> {
    match tokio::time::timeout_at(deadline, &mut task).await {
        Ok(r) => r.ok(),
        Err(_) => {
            task.abort();
            let _ = task.await;
            None
        }
    }
}

pub(super) struct ReadBoundary {
    began: Instant,
    pub(super) data: StreamBoundary,
    wait_observed: Option<bool>,
}

impl ReadBoundary {
    pub(super) fn new() -> Self {
        Self { began: Instant::now(), data: StreamBoundary::default(), wait_observed: Some(false) }
    }
    pub(super) fn at_ms(&mut self) -> Option<u32> {
        let ms = u32::try_from(self.began.elapsed().as_millis()).ok();
        self.data.overflow |= ms.is_none();
        ms
    }
    pub(super) fn add(&mut self, bytes: usize) {
        let n = u32::try_from(bytes).ok().and_then(|n| self.data.bytes.checked_add(n));
        self.data.overflow |= n.is_none();
        self.data.bytes = n.unwrap_or(u32::MAX);
        self.data.last_byte_ms = self.at_ms();
    }
    pub(super) fn snapshot(&self) -> StreamBoundary {
        let mut data = self.data.clone();
        data.snapshot_ms = u32::try_from(self.began.elapsed().as_millis()).ok();
        data.overflow |= data.snapshot_ms.is_none();
        data.wait_before_close = self.wait_observed;
        data
    }
}

// Observe actual pipe reads, including a partial line still held by read_until/BufReader.
pub(super) struct ObservedRead<R> {
    pub(super) inner: R,
    pub(super) boundary: Arc<Mutex<ReadBoundary>>,
}

impl<R: AsyncRead + Unpin> AsyncRead for ObservedRead<R> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Ok(mut b) = this.boundary.lock() {
            match &result {
                Poll::Ready(Ok(())) if buf.filled().len() > before => b.add(buf.filled().len() - before),
                Poll::Ready(Ok(())) => b.data.pre_stdout = Some(PipeEnd::Eof),
                Poll::Ready(Err(_)) => b.data.pre_stdout = Some(PipeEnd::ReadError),
                Poll::Pending => {}
            }
        }
        result
    }
}
