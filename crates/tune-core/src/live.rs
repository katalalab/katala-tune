//! ライブ表示（1〜2 秒ごとの CPU・メモリ・ディスク・ネットワーク・GPU と、5 秒ごとの上位プロセス）。
//!
//! 画面が見ているあいだだけ、各機体で軽いサンプラー（`probes/live_mac.py`・`probes/live_win.ps1`）を動かし、
//! 1 行 1 JSON を読み続ける。機体に常駐はさせない。サンプラーは読み取り専用で、読み手が居なくなると
//! （SSH の切断・アプリの終了）次の書き込みで終わり、長くても `sampler_max_age` で終わる（必要ならつなぎ直す）。
//!
//! 将来は常駐（tune-agent）の暗号化通信に載せ替えるので、取得の経路と、解析・保持・管理を分けている:
//!
//! | 層 | 中身 |
//! |---|---|
//! | 取得の経路 [`Route`] | 行を流す元（[`Source`]）を開く。今は [`ProcessRoute`]（この機体はローカル実行、ほかは SSH。collect.rs と同じ作法） |
//! | 解析 [`parse_line`] | サンプラーの 1 行を [`Msg`] に。壊れた行・知らない行は捨てる。値は範囲に収める |
//! | 保持 [`Ring`] | 機体ごとの直近 `ring` 点（既定 300 点 = 1 秒ごとで 5 分）と、最新の上位プロセス |
//! | 管理 [`Live`] | 開始・停止・画面からの合図（heartbeat）が途切れたときの自動停止・同時に流す台数の上限・つなぎ直しの上限・後始末 |
//!
//! 画面へは [`Live::tick`] が `flush` ごとに、変わった機体の新しい点だけをまとめて 1 つのイベントで送る
//! （元データは Rust 側。画面にはスパークラインに要る値だけ。docs/architecture.md の決まり）。

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, ReadBuf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::collect;
use crate::db::now_ms;
use crate::monitor_journal::{
    Category, CloseTrigger, Data, InputEnd, Journal, Kind, PipeEnd, Policy, Reason, StatePhase, StreamBoundary, StreamIo, WaitOrigin, elapsed_ms,
};
use crate::nodes::Node;

pub const MAC_LIVE: &str = include_str!("../../../probes/live_mac.py");
/// PS 5.1 のため BOM 付き ASCII。バイトのまま渡す
pub const WIN_LIVE: &[u8] = include_bytes!("../../../probes/live_win.ps1");

/// 1 行の上限（壊れた出力でメモリを使い切らない）
const MAX_LINE: usize = 256 * 1024;
const MAX_CORES: usize = 1024;
const MAX_TOP: usize = 20;
pub(crate) const MAX_GPUS: usize = 16;
const MAX_NAME: usize = 120;

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
    fn backoff_for(&self, failures: u32) -> Duration {
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
enum Line {
    #[serde(rename = "hello")]
    Hello(Hello),
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
    Hello(Hello),
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

fn pct(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite()).map(|x| x.clamp(0.0, 100.0))
}

fn nonneg(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite() && *x >= 0.0)
}

fn short(s: &str) -> String {
    s.chars().take(MAX_NAME).collect()
}

fn gpu_slot(gpus: &[Gpu], i: usize) -> Result<u8, &'static str> {
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

fn clean_procs(p: Procs) -> Procs {
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
    fn describe(&self) -> String {
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

    async fn close_with(mut self, trigger: CloseTrigger, sample_age: Option<Duration>) -> Ended {
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

fn live_ssh_args(alias: &str, remote: &str) -> Vec<String> {
    let mut a: Vec<String> = collect::SSH_OPTS.iter().chain(LIVE_SSH_OPTS.iter()).map(|s| s.to_string()).collect();
    a.push(alias.into());
    a.push(remote.into());
    a
}

/// 標準入力の先頭 n バイトをスクリプトとして動かし、残りの標準入力は開いたままにする python3 -c の中身。
/// シングルクォートを含めない（リモートのシェルで '…' に包むため）
fn python_boot(n: usize) -> String {
    format!("import sys;exec(compile(sys.stdin.buffer.read({n}),\"live_mac.py\",\"exec\"))")
}

fn secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s.fract() == 0.0 { format!("{}", s as u64) } else { format!("{s}") }
}

/// スクリプト長だけ読む。残りの stdin は EOF 監視に残し、実行先にファイルを作らない。
fn windows_boot(n: usize, params: &str) -> String {
    format!(
        "[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);$i=[Console]::OpenStandardInput();$b=New-Object byte[] {n};$p=0;while($p -lt {n}){{$r=$i.Read($b,$p,({n}-$p));if($r -eq 0){{exit 2}};$p+=$r}};$s=[Text.Encoding]::UTF8.GetString($b);. ([ScriptBlock]::Create($s)) {params}"
    )
}

/// 各機体に置くネイティブのサンプラー（`tune-agent sample`。置き方は docs/architecture.md）
pub const NATIVE_REMOTE: &str = "~/.katala-tune/bin/tune-agent.exe";
/// macOS の置き場所（名前に .exe が付かない）
pub const NATIVE_REMOTE_MAC: &str = "~/.katala-tune/bin/tune-agent";

fn native_args(o: &Opts) -> Vec<String> {
    [
        "sample".to_string(),
        format!("interval={}", secs(o.interval)),
        format!("procs={}", secs(o.procs_every)),
        format!("max={}", secs(o.sampler_max_age)),
        "watch=1".into(),
    ]
    .to_vec()
}

/// この機体のネイティブのサンプラー: アプリの隣、無ければ helpers の namespace。
/// `KATALA_TUNE_DATA_DIR` を明示した QA では `data/bin` に閉じ、通常は従来の HOME を使う。
fn local_native() -> Option<PathBuf> {
    let name = crate::helpers::exe("tune-agent");
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
        return Ok(Launch { program: "ssh".into(), args: live_ssh_args(&node.alias, &remote), stdin, keep_stdin: true, cleanup: None });
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
        return Ok(Launch { program: "ssh".into(), args: live_ssh_args(&node.alias, &remote), stdin, keep_stdin: true, cleanup: None });
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

async fn reader_result<T: Send + 'static>(mut task: JoinHandle<T>, deadline: tokio::time::Instant) -> Option<T> {
    match tokio::time::timeout_at(deadline, &mut task).await {
        Ok(r) => r.ok(),
        Err(_) => {
            task.abort();
            let _ = task.await;
            None
        }
    }
}

struct ReadBoundary {
    began: Instant,
    data: StreamBoundary,
    wait_observed: Option<bool>,
}
impl ReadBoundary {
    fn new() -> Self {
        Self { began: Instant::now(), data: StreamBoundary::default(), wait_observed: Some(false) }
    }
    fn at_ms(&mut self) -> Option<u32> {
        let ms = u32::try_from(self.began.elapsed().as_millis()).ok();
        self.data.overflow |= ms.is_none();
        ms
    }
    fn add(&mut self, bytes: usize) {
        let n = u32::try_from(bytes).ok().and_then(|n| self.data.bytes.checked_add(n));
        self.data.overflow |= n.is_none();
        self.data.bytes = n.unwrap_or(u32::MAX);
        self.data.last_byte_ms = self.at_ms();
    }
    fn snapshot(&self) -> StreamBoundary {
        let mut data = self.data.clone();
        data.snapshot_ms = u32::try_from(self.began.elapsed().as_millis()).ok();
        data.overflow |= data.snapshot_ms.is_none();
        data.wait_before_close = self.wait_observed;
        data
    }
}

// Observe actual pipe reads, including a partial line still held by read_until/BufReader.
struct ObservedRead<R> {
    inner: R,
    boundary: Arc<Mutex<ReadBoundary>>,
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

async fn read_lines(out: Option<tokio::process::ChildStdout>, tx: mpsc::Sender<String>, boundary: Arc<Mutex<ReadBoundary>>) -> PipeEnd {
    let Some(out) = out else {
        if let Ok(mut b) = boundary.lock() {
            b.data.pre_stdout = Some(PipeEnd::Unknown);
        }
        return PipeEnd::Unknown;
    };
    read_lines_from(out, tx, boundary).await
}

async fn read_lines_from<R: AsyncRead + Unpin>(out: R, tx: mpsc::Sender<String>, boundary: Arc<Mutex<ReadBoundary>>) -> PipeEnd {
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

async fn read_tail(err: Option<tokio::process::ChildStderr>, keep: usize) -> (String, PipeEnd) {
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
struct NodeLive {
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

struct Stream {
    epoch: u64,
    last_hb: Instant,
    stop: watch::Sender<Option<Stop>>,
    task: Option<JoinHandle<()>>,
    stopping: bool,
}

#[derive(Default)]
struct Shared {
    streams: HashMap<String, Stream>,
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

    fn lock(&self) -> MutexGuard<'_, Shared> {
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
        let excluded: Vec<String> = nodes.iter().filter(|n| !n.is_active()).map(|n| n.id.clone()).chain(unknown.iter().cloned()).collect();
        self.stop(Some(&excluded), Stop::Excluded);
        let now = Instant::now();
        let mut st = self.lock();
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
            st.streams.insert(n.id.clone(), Stream { epoch, last_hb: now, stop: tx, task: Some(task), stopping: false });
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

    /// 止める（ids が None なら全部）。戻り値は `{ stopped: [id] }`。止まり終わったら画面へ state: stopped を送る
    pub fn stop(&self, ids: Option<&[String]>, why: Stop) -> Value {
        let mut st = self.lock();
        let mut stopped = Vec::new();
        for (id, s) in st.streams.iter_mut() {
            if s.stopping || ids.is_some_and(|ids| !ids.contains(id)) {
                continue;
            }
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
        let excluded: Vec<String> = self.active().into_iter().filter(|id| !nodes.iter().any(|n| n.id == *id && n.is_active())).collect();
        self.stop(Some(&excluded), Stop::Excluded);
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

    fn finish(&self, id: &str, epoch: u64, why: Stop) {
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
fn stop_requested(rx: &watch::Receiver<Option<Stop>>) -> Option<Stop> {
    rx.borrow().clone()
}

fn end_category(timed_out: bool, samples: u64, ended: &Ended) -> Option<Category> {
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
async fn run_stream(live: Arc<Live>, node: Node, epoch: u64, mut stop: watch::Receiver<Option<Stop>>) {
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
                                    live.on_hello(&node.id, epoch, h);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn node(id: &str, os: &str) -> Node {
        Node::from_value(&json!({ "id": id, "alias": id, "os": os }), "this-host")
    }

    const SAMPLE: &str = r#"{"type":"s","t":1700000000000,"seq":3,"cpu":12.5,"cores":[10,20,null,150],"mem":{"used_pct":41.2,"swap_used_gb":1.5,"swap_total_gb":2,"pressure":"normal"},"disk":{"read_bps":1000,"write_bps":-5},"net":{"rx_bps":10,"tx_bps":20},"power":{"package_w":200.8,"package_source":"Energy Meter RAPL package average","available":true},"gpu":[{"uuid":"GPU-a","name":"G","util":30,"mem_used_mb":1024,"mem_total_mb":4096,"power_w":220,"power_limit_w":320,"power_min_w":100,"power_max_w":350,"temp_c":65,"clocks_graphics_mhz":1800,"clocks_sm_mhz":1815,"clocks_memory_mhz":9500,"pstate":"P0","source":"nvidia-smi dmon","available":true}],"procs":{"count":3,"top_cpu":[{"pid":7,"name":"a","cpu":150.5,"mem_mb":10}],"top_mem":[]},"self":{"cpu_s":0.5,"rss_mb":20}}"#;

    #[test]
    fn vram_keeps_adapter_fields_and_missing_memory_is_not_zero() {
        let Some(Msg::Sample(raw)) = parse_line(
            r#"{"type":"s","t":123,"gpu":[{"adapter_index":0,"name":"A","mem_used_mb":1024,"mem_total_mb":4096,"mem_free_mb":3000,"memory_status":"available","memory_kind":"dedicated","sampled_at":123},{"adapter_index":1,"name":"B","mem_total_mb":8192,"mem_used_mb":null,"mem_free_mb":null,"memory_status":"partial"}]}"#,
        ) else {
            panic!("sample")
        };
        let (s, _, _) = Sample::from_raw(*raw, 123);
        let point = s.point();
        assert_eq!(point["gpu_0_vram_used_mib"], json!(1024.0));
        assert_eq!(point["gpu_0_vram_free_mib"], json!(3000.0));
        assert_eq!(point["gpu_0_vram_pct"], json!(25.0));
        assert_eq!(point["gpu_1_vram_used_mib"], Value::Null);
        assert_eq!(point["gmem"], json!(25.0), "available adapter percent, never dilute with unknown adapter");
        assert_eq!(serde_json::to_value(&s.gpu).unwrap()[0]["memory_status"], "available");
    }

    #[test]
    fn disk_null_partial_and_legacy_samples_preserve_unknown_and_idle() {
        for (disk, read, write) in [
            (None, None, None),
            (Some(Value::Null), None, None),
            (Some(json!({"read_bps":null,"write_bps":120})), None, Some(120.0)),
            (Some(json!({"read_bps":0,"write_bps":0})), Some(0.0), Some(0.0)),
        ] {
            let mut row = json!({"type":"s","t":1000,"cpu":10});
            if let Some(disk) = disk {
                row["disk"] = disk;
            }
            let Some(Msg::Sample(raw)) = parse_line(&row.to_string()) else { panic!("sample") };
            let (sample, _, _) = Sample::from_raw(*raw, 1000);
            let point = sample.point();
            assert_eq!(point.get("dr").and_then(Value::as_f64), read);
            assert_eq!(point.get("dw").and_then(Value::as_f64), write);
            assert_eq!(point["cpu"], json!(10.0));
        }
    }

    #[test]
    fn parses_lines_and_ignores_noise() {
        let Some(Msg::Hello(h)) = parse_line("\u{FEFF}{\"type\":\"hello\",\"v\":1,\"os\":\"windows\",\"cores\":8,\"errors\":\"x\",\"has\":{\"gpu\":true}}\r")
        else {
            panic!("hello を読めない")
        };
        assert_eq!((h.os.as_deref(), h.cores, h.has.get("gpu")), (Some("windows"), Some(8), Some(&Value::Bool(true))));
        let Some(Msg::Sample(s)) = parse_line(SAMPLE) else { panic!("s を読めない") };
        let (p, procs, me) = Sample::from_raw(*s, 1_700_000_000_120);
        assert_eq!(p.cpu, Some(12.5));
        assert_eq!(p.cores, vec![Some(10.0), Some(20.0), None, Some(100.0)], "コアは 0〜100 に収める");
        assert_eq!(p.disk.as_ref().unwrap().write_bps, None, "負の速度は捨てる");
        assert_eq!((p.src_t, p.seq), (Some(1_700_000_000_000), Some(3)));
        assert_eq!(p.power.as_ref().and_then(|x| x.package_w), Some(200.8));
        let gpu = &p.gpu.as_ref().unwrap()[0];
        assert_eq!(
            (gpu.uuid.as_deref(), gpu.pstate.as_deref(), gpu.source.as_deref(), gpu.available),
            (Some("GPU-a"), Some("P0"), Some("nvidia-smi dmon"), Some(true))
        );
        assert_eq!((gpu.power_w, gpu.power_limit_w, gpu.temp_c), (Some(220.0), Some(320.0), Some(65.0)));
        assert_eq!((gpu.power_min_w, gpu.power_max_w), (Some(100.0), Some(350.0)));
        assert_eq!((gpu.clocks_graphics_mhz, gpu.clocks_sm_mhz, gpu.clocks_memory_mhz), (Some(1800.0), Some(1815.0), Some(9500.0)));
        assert_eq!(procs.unwrap().top_cpu[0].cpu, Some(150.5), "プロセスは 1 コア換算なので 100 を超えてよい");
        assert_eq!(me.unwrap().cpu_s, Some(0.5));
        let pt = p.point();
        assert_eq!(
            (pt["cpu"].as_f64(), pt["gpu"].as_f64(), pt["gmem"].as_f64(), pt["power_cpu_w"].as_f64(), pt["power_gpu_w"].as_f64(), pt["lag"].as_f64()),
            (Some(12.5), Some(30.0), Some(25.0), Some(200.8), Some(220.0), Some(120.0))
        );
        assert!(pt.get("dw").is_none(), "無い値は点に入れない");
        assert!(matches!(parse_line(r#"{"type":"end","reason":"max_age"}"#), Some(Msg::End(r)) if r == "max_age"));
        for bad in ["", "WARNING: something", "{broken", r#"{"type":"other"}"#, r#"{"cpu":1}"#, r#"{"type":"s","cores":"x"}"#] {
            assert!(parse_line(bad).is_none(), "{bad:?} は捨てる");
        }
        assert!(parse_line(&format!("{{\"type\":\"s\",\"pad\":\"{}\"}}", "x".repeat(MAX_LINE))).is_none(), "長すぎる行は捨てる");
    }

    #[test]
    fn ring_keeps_the_latest_and_downsamples() {
        let mut r = Ring::new(3);
        for i in 0..5 {
            r.push(i);
        }
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(r.tail(2).copied().collect::<Vec<_>>(), vec![3, 4]);
        assert_eq!((r.len(), r.last()), (3, Some(&4)));
        let pts: Vec<Value> = (0..10).map(|i| json!({ "t": i, "cpu": i as f64 * 10.0 })).collect();
        let d = downsample(&pts, 4);
        assert_eq!(d.len(), 4);
        assert_eq!(d[0], json!({ "t": 2, "cpu": 10.0 }), "3 点の平均と、最後の時刻");
        assert_eq!(d[3], json!({ "t": 9, "cpu": 90.0 }));
        assert_eq!(downsample(&pts[..3], 4).len(), 3, "少なければそのまま");
    }

    struct LaunchTestDir {
        path: PathBuf,
        cleanup: bool,
    }

    impl LaunchTestDir {
        fn create(path: PathBuf) -> std::io::Result<Self> {
            std::fs::create_dir(&path)?;
            Ok(Self { path, cleanup: true })
        }

        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            Self::create(std::env::temp_dir().join(format!("kt-live-launch-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)))).unwrap()
        }
    }

    impl Drop for LaunchTestDir {
        fn drop(&mut self) {
            if self.cleanup {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }

    struct ExactChildOutput {
        status: Option<std::process::ExitStatus>,
        reason: Option<&'static str>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    }

    // Keep the exact Child handle; never rediscover or signal an arbitrary PID.
    struct RetainedExactChild {
        child: std::process::Child,
        status: Option<std::process::ExitStatus>,
        stopped: bool,
    }

    impl RetainedExactChild {
        fn reap(&mut self) -> std::io::Result<bool> {
            if self.status.is_none() {
                self.status = self.child.try_wait()?;
            }
            Ok(self.status.is_some())
        }

        fn stop_and_reap(&mut self) {
            if self.stopped || self.status.is_some() {
                return;
            }
            self.stopped = true;
            let _ = self.child.kill();
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match self.reap() {
                    Ok(true) | Err(_) => return,
                    Ok(false) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
    }

    impl Drop for RetainedExactChild {
        fn drop(&mut self) {
            self.stop_and_reap();
        }
    }

    fn capture_exact_child(command: &mut std::process::Command, dir: &mut LaunchTestDir, limit: Duration, cap: usize) -> ExactChildOutput {
        use std::io::Read as _;
        let deadline = Instant::now() + limit;
        let out_path = dir.path.join("stdout");
        let err_path = dir.path.join("stderr");
        let out_file = std::fs::OpenOptions::new().write(true).create_new(true).open(&out_path).unwrap();
        let err_file = std::fs::OpenOptions::new().write(true).create_new(true).open(&err_path).unwrap();
        command.stdout(out_file.try_clone().unwrap()).stderr(err_file.try_clone().unwrap());
        let mut child = RetainedExactChild { child: command.spawn().unwrap(), status: None, stopped: false };
        // If anything fails before confirmed reap, leave the owned directory for
        // diagnosis rather than removing files still used by a retained child.
        dir.cleanup = false;
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let reason = loop {
            if Instant::now() >= deadline {
                break Some("deadline");
            }
            let reaped = child.reap().unwrap();
            // Regular files avoid pipe-reader joins. Poll size and stop excess
            // output; only cap bytes in total are ever loaded into diagnostics.
            if out_file.metadata().unwrap().len().saturating_add(err_file.metadata().unwrap().len()) > cap as u64 {
                break Some("output limit");
            }
            if reaped {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        child.stop_and_reap();
        dir.cleanup = child.status.is_some();
        let mut stdout = Vec::new();
        std::fs::File::open(out_path).unwrap().take(cap as u64).read_to_end(&mut stdout).unwrap();
        let mut stderr = Vec::new();
        std::fs::File::open(err_path).unwrap().take((cap - stdout.len()) as u64).read_to_end(&mut stderr).unwrap();
        ExactChildOutput { status: child.status, reason, stdout, stderr }
    }

    fn launch_test_command(dir: &std::path::Path, test: &str, case: &str) -> std::process::Command {
        let app = dir.join("owned app");
        let home = dir.join("synthetic-home");
        std::fs::create_dir(&app).unwrap();
        std::fs::create_dir(&home).unwrap();
        let executable = app.join(crate::helpers::exe("live-test"));
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let mut child = std::process::Command::new(&executable);
        child
            .args(["--exact", test, "--nocapture", "--test-threads=1", "--format=terse"])
            .env("KT_LIVE_LAUNCH_TEST_ROOT", dir)
            .env("KT_LIVE_LAUNCH_TEST_CASE", case)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("KATALA_TUNE_DATA_DIR", home.join(".katala-tune"))
            .stdin(Stdio::null());
        #[cfg(target_os = "macos")]
        child.env("PATH", "/usr/bin:/bin");
        child
    }

    #[test]
    fn launch_test_directory_rejects_collisions_and_cleans_only_owned_paths() {
        let owner = LaunchTestDir::new();
        let existing = owner.path.join("existing");
        std::fs::create_dir(&existing).unwrap();
        let canary = existing.join("canary");
        std::fs::write(&canary, b"keep").unwrap();
        let collision = LaunchTestDir::create(existing.clone());
        assert!(matches!(collision, Err(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists));
        drop(collision);
        assert_eq!(std::fs::read(&canary).unwrap(), b"keep");
        let path = owner.path.clone();
        drop(owner);
        assert!(!path.exists(), "successful exclusive owner cleans its own fixture");
    }

    // Invoked only as an exact child with the test-only case environment set.
    #[test]
    #[ignore]
    fn launch_test_watchdog_fixture() {
        println!("watchdog fixture ready");
        match std::env::var("KT_LIVE_LAUNCH_TEST_CASE").unwrap().as_str() {
            "hung" => loop {
                std::thread::park();
            },
            "noisy" => {
                use std::io::Write as _;
                std::io::stdout().write_all(&vec![b'x'; 262144]).unwrap();
                std::io::stderr().write_all(&vec![b'y'; 262144]).unwrap();
                loop {
                    std::thread::park();
                }
            }
            "stderr" => {
                use std::io::Write as _;
                std::io::stderr().write_all(&vec![b'y'; 262144]).unwrap();
                loop {
                    std::thread::park();
                }
            }
            "normal" => (),
            other => panic!("unknown fixture case: {other}"),
        }
    }

    fn watchdog_case(case: &str, expected: Option<&str>) {
        let mut dir = LaunchTestDir::new();
        let mut command = launch_test_command(&dir.path, "live::tests::launch_test_watchdog_fixture", case);
        command.arg("--ignored");
        let limit = if case == "hung" { Duration::from_secs(2) } else { Duration::from_secs(5) };
        let began = Instant::now();
        let output = capture_exact_child(&mut command, &mut dir, limit, 4096);
        assert_eq!(output.reason, expected);
        assert!(output.status.is_some(), "retained child must be reaped before cleanup");
        assert!(began.elapsed() < limit + Duration::from_secs(3));
        assert!(output.stdout.len() + output.stderr.len() <= 4096, "capture must be bounded");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("watchdog fixture ready"),
            "child must reach the controlled fixture before watchdog termination"
        );
        if expected.is_none() {
            assert!(output.status.unwrap().success());
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
        let path = dir.path.clone();
        drop(dir);
        assert!(!path.exists(), "reaped child fixture can be removed");
    }

    #[test]
    fn launch_test_watchdog_stops_hung_exact_child() {
        watchdog_case("hung", Some("deadline"));
    }

    #[test]
    fn launch_test_watchdog_caps_noisy_exact_child() {
        watchdog_case("noisy", Some("output limit"));
        watchdog_case("stderr", Some("output limit"));
    }

    #[test]
    fn launch_test_watchdog_accepts_normal_exact_child() {
        watchdog_case("normal", None);
    }

    // Only child processes receive this HOME/data namespace. Copying the test
    // executable also confines the beside-app search; no installed helper runs.
    fn isolated_launch_child(test: &str, cases: &[&str]) -> Option<(PathBuf, String)> {
        const ROOT: &str = "KT_LIVE_LAUNCH_TEST_ROOT";
        const CASE: &str = "KT_LIVE_LAUNCH_TEST_CASE";
        if let Some(dir) = std::env::var_os(ROOT) {
            return Some((PathBuf::from(dir), std::env::var(CASE).unwrap()));
        }
        for case in cases {
            let mut dir = LaunchTestDir::new();
            let mut command = launch_test_command(&dir.path, test, case);
            let output = capture_exact_child(&mut command, &mut dir, Duration::from_secs(45), 65536);
            assert!(
                output.reason.is_none() && output.status.is_some_and(|status| status.success()),
                "{test} case={case}: {:?}, {:?}, fixture={}\nstdout: {}\nstderr: {}",
                output.status,
                output.reason,
                dir.path.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"), "exact child must run one test");
            eprintln!("{test} case={case}: passed");
        }
        None
    }

    fn install_launch_fixture(dir: &std::path::Path, case: &str, bytes: &[u8]) -> Option<PathBuf> {
        let name = crate::helpers::exe("tune-agent");
        let beside = dir.join("owned app").join(&name);
        let managed = dir.join("synthetic-home").join(".katala-tune").join("bin").join(&name);
        assert_eq!(std::env::current_exe().unwrap().parent().unwrap(), beside.parent().unwrap());
        assert_eq!(crate::helpers::bin_dir(), managed.parent().unwrap());
        #[cfg(not(windows))]
        assert_eq!(crate::nodes::home(), dir.join("synthetic-home"));
        assert!(!beside.exists() && !managed.exists());
        if case == "absent" {
            assert_eq!(local_native(), None);
            return None;
        }
        assert!(case == "managed" || case == "bundled");
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, bytes).unwrap();
        if case == "bundled" {
            std::fs::write(&beside, bytes).unwrap();
            assert_eq!(local_native(), Some(beside.clone()), "beside-app wins when both exist");
            Some(beside)
        } else {
            std::fs::create_dir(&beside).unwrap();
            assert_eq!(local_native(), Some(managed.clone()));
            Some(managed)
        }
    }

    #[test]
    fn launch_follows_the_probe_conventions() {
        let Some((dir, case)) = isolated_launch_child("live::tests::launch_follows_the_probe_conventions", &["absent", "managed", "bundled"]) else { return };
        let native = install_launch_fixture(&dir, &case, b"owned fixture - never execute");
        let o = Opts::default();
        let mac = launch(&node("m", "macos"), &o).unwrap();
        assert_eq!(mac.program, "ssh");
        let cmd = mac.args.last().unwrap();
        assert!(
            cmd.starts_with("[ -x ~/.katala-tune/bin/tune-agent ] && ~/.katala-tune/bin/tune-agent sample interval=1 procs=5 max=900 watch=1 || { "),
            "ネイティブを先に使う: {cmd}"
        );
        assert!(cmd.contains(&format!("exec python3 -c '{}' interval=1 procs=5 max=900 watch=1", python_boot(MAC_LIVE.len()))), "{cmd}");
        assert_eq!(cmd.matches('\'').count(), 4, "python3 -c の中身にシングルクォートを入れない");
        for opt in ["BatchMode=yes", "ControlMaster=no", "ControlPath=none"] {
            assert!(mac.args.contains(&opt.to_string()), "{opt}");
        }
        assert!(mac.keep_stdin && mac.stdin == MAC_LIVE.as_bytes());
        let win = launch(&node("w", "windows"), &Opts { interval: Duration::from_millis(1500), ..o.clone() }).unwrap();
        let cmd = win.args.last().unwrap();
        assert!(
            cmd.starts_with("if [ -x ~/.katala-tune/bin/tune-agent.exe ]; then exec ~/.katala-tune/bin/tune-agent.exe sample interval=1.5 procs=5 max=900 watch=1; else exec powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand "),
            "{cmd}"
        );
        assert!(cmd.ends_with("; fi"), "{cmd}");
        assert!(!cmd.contains("live.ps1") && !cmd.contains("mkdir") && !cmd.contains("-File"));
        let boot = windows_boot(WIN_LIVE.len() - 3, "-Interval 1.5 -ProcEvery 5 -MaxSeconds 900 -WatchStdin");
        assert!(boot.contains("OpenStandardInput") && boot.contains(".Read($b,$p,"));
        assert!(boot.ends_with("-Interval 1.5 -ProcEvery 5 -MaxSeconds 900 -WatchStdin"));
        assert!(win.keep_stdin && win.args.contains(&"ControlPath=none".to_string()));
        assert_eq!(win.stdin, WIN_LIVE[3..]);
        assert!(win.cleanup.is_none());
        let mut local_node = node("local", "windows");
        local_node.local = true;
        let local = launch(&local_node, &o).unwrap();
        assert!(local.cleanup.is_none() && local.keep_stdin);
        if let Some(native) = native.as_ref() {
            assert_eq!(local.program, native.to_str().unwrap());
            #[cfg(windows)]
            assert!(local.program.ends_with("tune-agent.exe"));
            assert!(local.stdin.is_empty());
            assert_eq!(local.args, ["sample", "interval=1", "procs=5", "max=900", "watch=1"]);
        } else {
            assert_eq!(local.program, "powershell.exe");
            assert_eq!(local.stdin, WIN_LIVE[3..]);
            assert_eq!(local.args[..4], ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand"]);
            assert_eq!(local.args.len(), 5);
        }
        local_node.os = "macos".into();
        let local = launch(&local_node, &o).unwrap();
        assert!(local.keep_stdin && local.cleanup.is_none());
        assert_eq!(local.stdin, MAC_LIVE.as_bytes());
        if let Some(native) = native {
            assert_eq!(local.program, "/bin/sh");
            assert_eq!(
                local.args,
                [
                    "-c",
                    &format!(
                        "\"$0\" sample interval=1 procs=5 max=900 watch=1 || exec /usr/bin/env python3 -c '{}' interval=1 procs=5 max=900 watch=1",
                        python_boot(MAC_LIVE.len())
                    ),
                    native.to_str().unwrap()
                ]
            );
        } else {
            assert_eq!(local.program, "/usr/bin/env");
            assert_eq!(local.args, ["python3", "-c", &python_boot(MAC_LIVE.len()), "interval=1", "procs=5", "max=900", "watch=1"]);
        }
        assert!(WIN_LIVE[3..].iter().all(|b| b.is_ascii()), "probes/live_win.ps1 は ASCII のみ");
        assert!(launch(&node("x", "linux"), &o).is_err());
    }

    /// 本番の spawn 経路で stderr flood 後の初回出力・scope・stdin 保持と回収を検証する。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_boot_preserves_script_scope_and_stdin_lifetime() {
        let script = b"[Console]::Error.Write(('E' * 131072));$value=42;function Scope {$script:value};[Console]::WriteLine((Scope));$io=[Console]::OpenStandardInput();[Console]::WriteLine($io.ReadByte());[Console]::WriteLine($io.ReadByte())";
        let boot = windows_boot(script.len(), "");
        let utf16: Vec<u8> = boot.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut stdin = script.to_vec();
        stdin.push(b'Q');
        let mut source = spawn(Launch {
            program: "powershell.exe".into(),
            args: vec!["-NoLogo".into(), "-NoProfile".into(), "-NonInteractive".into(), "-EncodedCommand".into(), crate::logs::base64(&utf16)],
            stdin,
            keep_stdin: true,
            cleanup: None,
        })
        .unwrap();
        let first_line_timeout = Duration::from_secs(15);
        let scope = match tokio::time::timeout(first_line_timeout, source.lines.recv()).await {
            Ok(Some(scope)) => scope,
            Ok(None) => panic!("PowerShell stdout closed before the first line"),
            Err(_) => {
                let ended = tokio::time::timeout(Duration::from_secs(5), source.close()).await;
                panic!("PowerShell produced no first stdout line within {first_line_timeout:?}; shutdown: {ended:?}");
            }
        };
        assert_eq!(scope.trim_end(), "42", "dot-source must preserve $script scope");
        let first_byte = tokio::time::timeout(Duration::from_secs(5), source.lines.recv())
            .await
            .expect("PowerShell should read the first byte")
            .expect("PowerShell stdout closed before the first byte");
        assert_eq!(first_byte.trim_end(), "81", "bootstrap must consume exactly script.len() bytes");
        assert!(tokio::time::timeout(Duration::from_millis(100), source.lines.recv()).await.is_err(), "must remain alive while stdin stays open");
        let ended = tokio::time::timeout(Duration::from_secs(5), source.close()).await.unwrap();
        assert_eq!(ended.code, Some(0), "closing the source should close stdin and let PowerShell exit: {ended:?}");
        assert!(ended.err.contains("EEEE"), "spawn must drain the PowerShell stderr flood: {ended:?}");
        assert!(ended.err.len() <= 4096, "spawn must retain only the bounded stderr tail: {} bytes", ended.err.len());
    }

    /// Bootstrap must retain its remaining-byte loop so short pipe reads cannot truncate the script.
    #[test]
    fn windows_boot_accumulates_partial_reads_until_script_length() {
        let script_len = 7;
        let boot = windows_boot(script_len, "");
        let read_loop = format!(";while($p -lt {script_len}){{$r=$i.Read($b,$p,({script_len}-$p));if($r -eq 0){{exit 2}};$p+=$r}};");
        assert!(boot.contains(&read_loop), "bootstrap must read remaining bytes after each partial read: {boot}");
    }

    // ---- 偽のサンプラー（一定間隔で行を出すローカルのスクリプト）で、開始・停止・自動停止・上限・つなぎ直しを確かめる ----

    /// sh で動かす偽のサンプラー。$1 に機体の id、$KT_DIR に記録の置き場所（起動ごとに pid を書く）
    struct Fake {
        script: String,
        dir: PathBuf,
        opened: AtomicUsize,
    }

    impl Route for Fake {
        fn open(&self, node: &Node, _o: &Opts) -> Result<Source, String> {
            self.opened.fetch_add(1, Ordering::SeqCst);
            let script = format!("KT_DIR='{}'; {}", self.dir.display(), self.script);
            spawn(Launch { program: "/bin/sh".into(), args: vec!["-c".into(), script, "sh".into(), node.id.clone()], ..Launch::default() })
        }
    }

    const STEADY: &str = r#"echo $$ >> "$KT_DIR/pids-$1"; echo '{"type":"hello","v":1,"os":"fake"}'; while true; do echo '{"type":"s","t":1,"cpu":5,"cores":[5,5],"mem":{"used_pct":50},"procs":{"count":1,"top_cpu":[{"pid":1,"name":"x","cpu":1}]}}'; sleep 0.05; done"#;

    fn fake(script: &str) -> (Arc<Fake>, PathBuf) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("kt-live-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        (Arc::new(Fake { script: script.into(), dir: dir.clone(), opened: AtomicUsize::new(0) }), dir)
    }

    fn quick() -> Opts {
        Opts {
            interval: Duration::from_millis(50),
            idle_stop: Duration::from_secs(60),
            first_line_timeout: Duration::from_secs(5),
            stall_timeout: Duration::from_secs(5),
            backoff: vec![Duration::from_millis(20)],
            flush: Duration::from_millis(50),
            ..Opts::default()
        }
    }

    fn events() -> (Sink, Arc<Mutex<Vec<Value>>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        (Box::new(move |v| g.lock().unwrap().push(v)), got)
    }

    async fn until(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("待っても {what} にならない");
    }

    fn pids(dir: &std::path::Path, id: &str) -> Vec<u32> {
        std::fs::read_to_string(dir.join(format!("pids-{id}"))).unwrap_or_default().lines().filter_map(|l| l.trim().parse().ok()).collect()
    }

    fn alive(pid: u32) -> bool {
        std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }

    fn state(l: &Live, id: &str) -> (String, String) {
        let s = l.status();
        (s[id]["state"].as_str().unwrap_or("").into(), s[id]["reason"].as_str().unwrap_or("").into())
    }

    #[tokio::test]
    async fn streams_samples_and_stop_kills_the_sampler() {
        if cfg!(windows) {
            return;
        }
        let (route, dir) = fake(STEADY);
        let (sink, got) = events();
        let live = Live::new(route.clone(), quick(), sink);
        let r = live.start(&[node("a", "macos")], &["ghost".into()]);
        assert_eq!(r["started"], json!(["a"]));
        assert_eq!(r["refused"][0]["reason"], "unknown");
        until("3 点たまる", || live.samples("a").len() >= 3).await;
        assert_eq!(state(&live, "a").0, "running");
        live.tick();
        let ev = got.lock().unwrap().last().cloned().unwrap();
        let a = &ev["nodes"]["a"];
        assert!(a["points"].as_array().unwrap().len() >= 3);
        assert_eq!((a["state"].as_str(), a["cores"].as_array().map(Vec::len)), (Some("running"), Some(2)));
        assert!(a["procs"]["top_cpu"].is_array() && a["info"]["os"] == "fake");
        // 次の tick には新しい点だけ（procs・info は変わったときだけ）
        let before = got.lock().unwrap().len();
        until("次の点", || live.samples("a").len() >= 6).await;
        live.tick();
        let next = got.lock().unwrap()[before]["nodes"]["a"].clone();
        assert!(next.get("info").is_none());
        // 2 回目の start は合図（つなぎ直さない）
        assert_eq!(live.start(&[node("a", "macos")], &[])["running"], json!(["a"]));
        assert_eq!(route.opened.load(Ordering::SeqCst), 1);

        let pid = pids(&dir, "a")[0];
        assert!(alive(pid));
        assert_eq!(live.stop(None, Stop::User)["stopped"], json!(["a"]));
        until("止まる", || state(&live, "a").0 == "stopped").await;
        assert_eq!(state(&live, "a").1, "user");
        until("サンプラーが終わる", || !alive(pid)).await;
        assert!(live.active().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stops_by_itself_when_the_screen_goes_quiet() {
        if cfg!(windows) {
            return;
        }
        let (route, dir) = fake(STEADY);
        let (sink, got) = events();
        let live = Live::new(route, Opts { idle_stop: Duration::from_millis(600), ..quick() }, sink);
        tokio::spawn(live.clone().run_ticker());
        live.start(&[node("a", "macos"), node("b", "windows")], &[]);
        until("流れる", || live.samples("a").len() >= 2 && live.samples("b").len() >= 2).await;
        // a にだけ合図を送り続ける
        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(150)).await;
            live.start(&[node("a", "macos")], &[]);
        }
        assert_eq!(state(&live, "b"), ("stopped".into(), "idle".into()), "合図の無い b は自動で止まる");
        assert_eq!(state(&live, "a").0, "running", "合図のある a は続く");
        until("b のサンプラーが終わる", || pids(&dir, "b").iter().all(|p| !alive(*p))).await;
        assert!(got.lock().unwrap().iter().any(|e| e["nodes"]["b"]["reason"] == "idle"), "止まった理由を画面へ送る");
        until("a も止まる", || state(&live, "a").0 == "stopped").await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn refuses_beyond_the_limit() {
        if cfg!(windows) {
            return;
        }
        let (route, dir) = fake(STEADY);
        let (sink, _) = events();
        let live = Live::new(route, Opts { max_streams: 2, ..quick() }, sink);
        let r = live.start(&[node("a", "macos"), node("b", "macos"), node("c", "macos")], &[]);
        assert_eq!((r["started"].clone(), r["refused"][0]["id"].clone(), r["refused"][0]["reason"].clone()), (json!(["a", "b"]), json!("c"), json!("limit")));
        assert_eq!(state(&live, "c"), ("stopped".into(), "limit".into()));
        // 1 台止めれば流せる
        live.stop(Some(&["a".to_string()]), Stop::User);
        assert_eq!(live.start(&[node("c", "macos")], &[])["started"], json!(["c"]));
        live.shutdown(Duration::from_secs(5)).await;
        assert!(live.active().is_empty());
        assert_eq!(live.start(&[node("a", "macos")], &[])["refused"][0]["reason"], "app_exit", "終了した後は流さない");
        for id in ["a", "b", "c"] {
            assert!(pids(&dir, id).iter().all(|p| !alive(*p)), "終了で {id} のサンプラーも終わる");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn gives_up_after_the_retry_limit() {
        if cfg!(windows) {
            return;
        }
        // 1 行出してすぐ終わる（切れる）サンプラー
        let (route, dir) = fake(r#"echo '{"type":"s","cpu":1}'; echo 'Connection closed by remote host' >&2; exit 255"#);
        let (sink, _) = events();
        let live = Live::new(route.clone(), Opts { max_retries: 2, ..quick() }, sink);
        live.start(&[node("a", "macos")], &[]);
        until("諦める", || state(&live, "a").0 == "stopped").await;
        let s = live.status();
        assert_eq!(s["a"]["reason"], "disconnected");
        assert!(s["a"]["detail"].as_str().unwrap().contains("exit 255: Connection closed"), "{}", s["a"]["detail"]);
        assert_eq!(route.opened.load(Ordering::SeqCst), 3, "最初の 1 回 + つなぎ直し 2 回");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn reconnects_without_counting_when_the_sampler_rotates() {
        if cfg!(windows) {
            return;
        }
        // max_age で区切って終わるサンプラー。つなぎ直しの上限 0 でも止まらない
        let (route, dir) = fake(r#"echo '{"type":"s","cpu":1}'; echo '{"type":"end","reason":"max_age"}'"#);
        let (sink, _) = events();
        let live = Live::new(route.clone(), Opts { max_retries: 0, ..quick() }, sink);
        live.start(&[node("a", "macos")], &[]);
        until("何度か区切る", || route.opened.load(Ordering::SeqCst) >= 4).await;
        assert_ne!(state(&live, "a").0, "stopped");
        live.stop(None, Stop::User);
        until("止まる", || state(&live, "a").0 == "stopped").await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_silent_sampler_counts_as_disconnected() {
        if cfg!(windows) {
            return;
        }
        let (route, dir) = fake(r#"echo $$ >> "$KT_DIR/pids-$1"; echo '{"type":"s","cpu":1}'; exec sleep 30"#);
        let (sink, _) = events();
        let live = Live::new(route, Opts { max_retries: 1, stall_timeout: Duration::from_millis(200), ..quick() }, sink);
        live.start(&[node("a", "macos")], &[]);
        until("諦める", || state(&live, "a").0 == "stopped").await;
        assert!(live.status()["a"]["detail"].as_str().unwrap().contains("データが来ない"));
        until("黙ったサンプラーも終わる", || pids(&dir, "a").iter().all(|p| !alive(*p))).await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// macOS の起動コマンドを実際のシェルで動かす: 置いたネイティブが動けばそれだけ、起動できなければ Python 版
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn mac_launch_prefers_native_and_falls_back_to_python() {
        use std::os::unix::fs::PermissionsExt;
        let o = Opts { sampler_max_age: Duration::from_secs(1), ..Opts::default() };
        // 古い版（macOS のサンプラーが無い）は標準入力を読まずに失敗する。動く版は hello を出して正常に終わる
        for (native, want) in [
            (
                "#!/bin/sh
exit 1
",
                None,
            ),
            (
                "#!/bin/sh
echo '{\"type\":\"hello\",\"os\":\"macos\",\"impl\":\"rust\"}'
",
                Some("rust"),
            ),
        ] {
            let (_, dir) = fake("");
            let bin = dir.join(".katala-tune/bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("tune-agent"), native).unwrap();
            std::fs::set_permissions(bin.join("tune-agent"), std::fs::Permissions::from_mode(0o700)).unwrap();
            let l = launch(&node("m", "macos"), &o).unwrap();
            let remote = l.args.last().unwrap().clone();
            let mut src = spawn(Launch {
                program: "/usr/bin/env".into(),
                args: vec![format!("HOME={}", dir.display()), "PATH=/usr/bin:/bin".into(), "/bin/sh".into(), "-c".into(), remote],
                stdin: l.stdin,
                keep_stdin: true,
                cleanup: None,
            })
            .unwrap();
            // Python 版は max=1 で自分から終わる
            let mut msgs = Vec::new();
            while let Ok(Some(line)) = tokio::time::timeout(Duration::from_secs(20), src.lines.recv()).await {
                msgs.push(parse_line(&line).unwrap_or_else(|| panic!("読めない行: {line}")));
            }
            let ended = src.close().await;
            assert_eq!(ended.code, Some(0), "{}", ended.err);
            let Some(Msg::Hello(h)) = msgs.first() else { panic!("1 行目は hello") };
            assert_eq!(h.implementation.as_deref(), want, "{native}");
            assert_eq!(msgs.iter().filter(|m| matches!(m, Msg::Hello(_))).count(), 1, "動くのはどちらか一方");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 必須のOS統計を保ち、任意センサーの有無を独立した環境で確かめる。
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn mac_sampler_runs_here() {
        use std::os::unix::fs::PermissionsExt;
        for with_sensor in [false, true] {
            let (_, dir) = fake("");
            if with_sensor {
                let sensor = dir.join("macmon");
                std::fs::write(
                    &sensor,
                    r#"#!/usr/bin/python3
import json,os,time
from pathlib import Path
Path(__file__).with_suffix('.pid').write_text(str(os.getpid()))
for _ in range(400):
    print(json.dumps({'cpu_power':0,'gpu_power':3,'all_power':4,'sys_power':0,'gpu_freq_mhz':500}),flush=True)
    time.sleep(0.05)
"#,
                )
                .unwrap();
                std::fs::set_permissions(sensor, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let mut src = spawn(Launch {
                program: "/usr/bin/env".into(),
                args: [
                    format!("PATH={}", dir.display()),
                    format!("HOME={}", dir.display()),
                    "/usr/bin/python3".into(),
                    "-".into(),
                    "interval=0.3".into(),
                    "procs=0.3".into(),
                    "count=3".into(),
                ]
                .into(),
                stdin: MAC_LIVE.as_bytes().to_vec(),
                ..Launch::default()
            })
            .unwrap();
            let mut msgs = Vec::new();
            while let Ok(Some(l)) = tokio::time::timeout(Duration::from_secs(20), src.lines.recv()).await {
                msgs.push(parse_line(&l).unwrap_or_else(|| panic!("読めない行: {l}")));
            }
            let ended = src.close().await;
            assert_eq!(ended.code, Some(0), "{}", ended.err);
            let Msg::Hello(h) = &msgs[0] else { panic!("1 行目は hello") };
            assert_eq!(h.os.as_deref(), Some("macos"));
            assert_eq!(
                h.errors,
                if with_sensor { json!([]) } else { json!(["power: FileNotFoundError: macmon unavailable"]) },
                "予期しない欠測: {:?}",
                h.errors
            );
            assert_eq!(h.has.get("gpu"), Some(&json!(with_sensor)));
            let samples: Vec<Sample> = msgs
                .iter()
                .filter_map(|m| match m {
                    Msg::Sample(s) => Some(Sample::from_raw((**s).clone(), now_ms()).0),
                    _ => None,
                })
                .collect();
            assert_eq!(samples.len(), 3);
            let last = samples.last().unwrap();
            assert!(last.cpu.is_some() && last.cores.len() == h.cores.unwrap() as usize);
            assert!(last.mem.as_ref().and_then(|m| m.used_pct).is_some());
            assert!(last.disk.is_some() && last.net.is_some());
            assert!(matches!(msgs.last(), Some(Msg::End(r)) if r == "count"));
            if with_sensor {
                let power = last.power.as_ref().unwrap();
                assert_eq!((power.package_w, power.soc_w, power.platform_w, power.available), (Some(0.0), Some(4.0), None, Some(true)));
                assert_eq!(power.package_source.as_deref(), Some("macmon IOReport CPU"));
                let gpu = &last.gpu.as_ref().unwrap()[0];
                assert_eq!((gpu.power_w, gpu.clocks_graphics_mhz), (Some(3.0), Some(500.0)));
                let pid: u32 = std::fs::read_to_string(dir.join("macmon.pid")).unwrap().parse().unwrap();
                assert!(!alive(pid), "サンプラー終了時にセンサーの子も終わる");
            } else {
                for sample in &samples {
                    assert_eq!(sample.power, Some(Power { available: Some(false), ..Power::default() }));
                    let gpu = &sample.gpu.as_ref().unwrap()[0];
                    assert_eq!(gpu.power_w, None, "欠測をGPUの0Wとして作らない");
                    assert_eq!(gpu.mem_used_mb, None, "統合メモリのGPU割当は未取得");
                    assert!(matches!(gpu.memory_status.as_deref(), Some("not-applicable") | Some("unsupported")));
                }
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// アプリと同じ起動のしかた（標準入力を開いたまま）で動かし、止めると標準入力が閉じてサンプラーが自分で終わる
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn mac_sampler_ends_by_itself_when_stdin_closes() {
        let me = Node::from_value(&json!({ "id": "me", "alias": "me", "os": "macos", "local_hostname": "this-host" }), "this-host");
        let Some((dir, case)) = isolated_launch_child("live::tests::mac_sampler_ends_by_itself_when_stdin_closes", &["absent"]) else { return };
        assert_eq!(install_launch_fixture(&dir, &case, b""), None);
        let l = launch(&me, &Opts::default()).unwrap();
        assert!(l.keep_stdin && l.args[1] == "-c");
        assert_eq!(l.program, "/usr/bin/env");
        assert_eq!(l.stdin, MAC_LIVE.as_bytes());
        let mut src = spawn(with_isolated_mac_sensor_env(l, &dir)).unwrap();
        let mut got = 0;
        while got < 2 {
            let line = tokio::time::timeout(Duration::from_secs(20), src.lines.recv()).await.unwrap().expect("行が来る");
            if matches!(parse_line(&line), Some(Msg::Sample(_))) {
                got += 1;
            }
        }
        let t0 = Instant::now();
        let ended = src.close().await;
        // 強制終了ならシグナルで終わり code は None。0 は標準入力が閉じたのを見て自分で終わったということ
        assert_eq!(ended.code, Some(0), "{}", ended.err);
        assert!(t0.elapsed() < Duration::from_millis(2500));
    }

    // collect::command enriches PATH. Set these again in the spawned process so
    // the real Python script cannot discover a normal HOME/Homebrew macmon.
    #[cfg(target_os = "macos")]
    fn with_isolated_mac_sensor_env(mut launch: Launch, dir: &std::path::Path) -> Launch {
        let mut args = vec![format!("HOME={}", dir.join("synthetic-home").display()), "PATH=/usr/bin:/bin".into(), launch.program];
        args.extend(launch.args);
        launch.program = "/usr/bin/env".into();
        launch.args = args;
        launch
    }

    /// Owned fake: this checks the native launcher's stdin/EOF contract only.
    /// Actual native sampler EOF behavior stays covered by tune-agent tests.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn mac_native_launcher_preserves_stdin_until_close() {
        use std::os::unix::fs::PermissionsExt as _;
        let Some((dir, case)) = isolated_launch_child("live::tests::mac_native_launcher_preserves_stdin_until_close", &["managed", "bundled"]) else { return };
        let script = b"#!/bin/sh\nprintf '%s\\n' '{\"type\":\"hello\",\"impl\":\"rust\",\"os\":\"macos\"}' '{\"type\":\"s\",\"seq\":1}' '{\"type\":\"s\",\"seq\":2}'\nwhile IFS= read -r line; do :; done\n";
        let native = install_launch_fixture(&dir, &case, script).unwrap();
        std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut me = node("me", "macos");
        me.local = true;
        let l = launch(&me, &Opts::default()).unwrap();
        assert_eq!(l.program, "/bin/sh");
        assert_eq!(l.args[0], "-c");
        assert_eq!(l.args[2], native.to_str().unwrap());
        assert!(l.keep_stdin && l.cleanup.is_none());
        assert_eq!(l.stdin, MAC_LIVE.as_bytes());
        let mut src = spawn(with_isolated_mac_sensor_env(l, &dir)).unwrap();
        for expected in ["hello", "s", "s"] {
            let line = tokio::time::timeout(Duration::from_secs(20), src.lines.recv()).await.unwrap().expect("fixture frame");
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["type"], expected);
            if expected == "hello" {
                assert_eq!(value["impl"], "rust");
            }
        }
        assert!(tokio::time::timeout(Duration::from_millis(100), src.lines.recv()).await.is_err(), "owned fake stays alive while stdin is open");
        let t0 = Instant::now();
        let ended = src.close().await;
        assert_eq!(ended.code, Some(0), "{}", ended.err);
        assert!(t0.elapsed() < Duration::from_millis(2500));
    }
    #[tokio::test]
    async fn journal_planned_recycle_eof_and_privacy() {
        if cfg!(windows) {
            return;
        }
        let (route, dir) = fake(
            r#"echo $$ >> "$KT_DIR/pids-$1"; echo '{"type":"hello","impl":"rust","agent":"0.6.4","errors":"dummy-private-canary"}'; echo '{"type":"s","seq":1}'; echo '{"type":"end","reason":"max_age"}'; echo dummy-private-canary >&2"#,
        );
        let db = Arc::new(Mutex::new(crate::db::Store::open_in_memory().unwrap()));
        let journal = Journal::start(db.clone());
        let live = Live::new(route.clone(), quick(), Box::new(|_| {}));
        live.bind_journal(journal.clone());
        live.start(&[node("fixture", "macos")], &[]);
        until("3 open", || route.opened.load(Ordering::SeqCst) >= 3).await;
        live.shutdown(Duration::from_secs(3)).await;
        assert!(journal.flush(Duration::from_secs(2)).await);
        let v = db.lock().unwrap().monitor_events(&crate::monitor_journal::Filter { limit: Some(500), ..Default::default() }).unwrap();
        assert!(!v.to_string().contains("dummy-private-canary"));
        let xs = v["events"].as_array().unwrap();
        assert!(xs.iter().any(|e| e["kind"] == "first_sample" && e["data"]["transport_pid"].is_number()));
        assert!(xs.iter().any(|e| e["kind"] == "retry" && e["data"]["policy"] == "planned_recycle"));
        assert!(xs.iter().any(|e| e["kind"] == "end" && e["data"]["stdout"] == "eof" && e["data"]["exit_code"] == 0));
        assert!(!xs.iter().any(|e| e["kind"] == "stopped" && e["data"]["reason"] == "disconnected"));
        assert!(journal.shutdown(Duration::from_secs(2)).await);
        assert!(pids(&dir, "fixture").iter().all(|p| !alive(*p)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn journal_distinguishes_first_sample_and_stall_timeout() {
        // Classification must depend on whether a sample arrived, not whether
        // the OS happened to start a shell within the 150 ms silence window.
        // Native source termination remains covered by the adjacent tests.
        struct SilentRoute(bool);
        impl Route for SilentRoute {
            fn open(&self, _: &Node, _: &Opts) -> Result<Source, String> {
                let (lines, received) = mpsc::channel(2);
                lines.try_send(r#"{"type":"hello"}"#.into()).unwrap();
                if self.0 {
                    lines.try_send(r#"{"type":"s","seq":1}"#.into()).unwrap();
                }
                let (stop, stopped) = oneshot::channel();
                let (done, ended) = oneshot::channel();
                tokio::spawn(async move {
                    let _ = stopped.await;
                    drop(lines);
                    let _ = done.send(Ended { code: Some(0), wait_observed: Some(true), ..Ended::default() });
                });
                Ok(Source::new(received, stop, ended))
            }
        }
        for (sample, category) in [(false, "first_sample_timeout"), (true, "stall_timeout")] {
            let db = Arc::new(Mutex::new(crate::db::Store::open_in_memory().unwrap()));
            let journal = Journal::start(db.clone());
            let live = Live::new(
                Arc::new(SilentRoute(sample)),
                Opts { first_line_timeout: Duration::from_millis(150), stall_timeout: Duration::from_millis(150), max_retries: 0, ..quick() },
                Box::new(|_| {}),
            );
            live.bind_journal(journal.clone());
            live.start(&[node("fixture", "macos")], &[]);
            until("disconnected", || state(&live, "fixture").1 == "disconnected").await;
            assert!(journal.flush(Duration::from_secs(2)).await);
            let v = db.lock().unwrap().monitor_events(&crate::monitor_journal::Filter::default()).unwrap();
            let events = v["events"].as_array().unwrap();
            assert!(events.iter().any(|e| e["kind"] == "hello"));
            assert_eq!(events.iter().any(|e| e["kind"] == "first_sample"), sample);
            assert!(events.iter().any(|e| e["kind"] == "end" && e["data"]["category"] == category && e["data"]["samples"] == u64::from(sample)));
            live.shutdown(Duration::from_secs(3)).await;
            assert!(journal.shutdown(Duration::from_secs(2)).await);
        }
    }

    // No remote sampler: the owned local child exits zero only after Source closes stdin.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn journal_separates_timeout_cleanup_from_natural_zero_exit() {
        struct StdinRoute(bool);
        impl Route for StdinRoute {
            fn open(&self, _: &Node, _: &Opts) -> Result<Source, String> {
                let tail = if self.0 { "cat >/dev/null" } else { "exit 0" };
                spawn(Launch {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), format!("echo '{{\"type\":\"s\",\"seq\":1}}'; {tail}")],
                    keep_stdin: true,
                    ..Launch::default()
                })
            }
        }
        for silent in [true, false] {
            let db = Arc::new(Mutex::new(crate::db::Store::open_in_memory().unwrap()));
            let journal = Journal::start(db.clone());
            let live = Live::new(Arc::new(StdinRoute(silent)), Opts { stall_timeout: Duration::from_millis(150), max_retries: 0, ..quick() }, Box::new(|_| {}));
            live.bind_journal(journal.clone());
            live.start(&[node("fixture", "macos")], &[]);
            until("disconnected", || state(&live, "fixture").1 == "disconnected").await;
            assert!(journal.flush(Duration::from_secs(2)).await);
            let rows = db.lock().unwrap().monitor_events(&crate::monitor_journal::Filter::default()).unwrap();
            let end = rows["events"].as_array().unwrap().iter().find(|e| e["kind"] == "end").unwrap();
            let d = &end["data"];
            assert_eq!(d["exit_code"], 0);
            assert_eq!(d["wait_observed"], true);
            assert_eq!(d["category"], if silent { json!("stall_timeout") } else { Value::Null });
            assert_eq!(d["io"]["close_trigger"], if silent { "timeout" } else { "channel_end" });
            assert_eq!(d["io"]["kill_attempted"], false);
            assert_eq!(d["io"]["forwarded"], 1);
            if silent {
                assert_eq!(d["io"]["wait_origin"], "close");
                assert_eq!(d["io"]["wait_before_close"], false);
                assert!(d["io"]["sample_age_ms"].as_u64().unwrap() >= 150);
                assert_eq!(d["io"]["pre_stdout"], Value::Null);
            } else {
                assert_eq!(d["io"]["pre_stdout"], "eof");
                assert_eq!(d["io"]["sample_age_ms"], Value::Null);
            }
            assert_eq!(d["producer_pid"], Value::Null);
            assert_eq!(d["actual_source_sha256"], Value::Null);
            live.shutdown(Duration::from_secs(2)).await;
            assert!(journal.shutdown(Duration::from_secs(2)).await);
        }
    }

    #[tokio::test]
    async fn reader_boundary_retains_partial_discard_and_error_without_payload() {
        let boundary = Arc::new(Mutex::new(ReadBoundary::new()));
        let (mut writer, reader) = tokio::io::duplex(4096);
        let (tx, mut rx) = mpsc::channel(2);
        let task = tokio::spawn(read_lines_from(reader, tx, boundary.clone()));
        writer.write_all(b"partial-secret-canary").await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(30), rx.recv()).await.is_err());
        let pre = boundary.lock().unwrap().snapshot();
        assert_eq!(pre.bytes, 21);
        assert_eq!(pre.forwarded, 0);
        assert!(pre.last_byte_ms.is_some() && pre.last_line_ms.is_none());
        assert!(pre.snapshot_ms >= pre.last_byte_ms);
        assert_eq!(pre.pre_stdout, None);
        writer.write_all(&vec![b'x'; MAX_LINE]).await.unwrap();
        writer.write_all(b"\n{\"type\":\"s\",\"seq\":1}\n").await.unwrap();
        assert!(rx.recv().await.unwrap().contains("\"seq\":1"));
        drop(writer);
        assert_eq!(task.await.unwrap(), PipeEnd::Eof);
        let post = boundary.lock().unwrap().snapshot();
        assert_eq!(post.discarded, 1);
        assert_eq!(post.forwarded, 1);
        assert_eq!(post.pre_stdout, Some(PipeEnd::Eof));
        assert!(!serde_json::to_string(&post).unwrap().contains("secret-canary"));

        struct Broken;
        impl tokio::io::AsyncRead for Broken {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::other("secret-canary")))
            }
        }
        let boundary = Arc::new(Mutex::new(ReadBoundary::new()));
        let (tx, _) = mpsc::channel(1);
        assert_eq!(read_lines_from(Broken, tx, boundary.clone()).await, PipeEnd::ReadError);
        let d = boundary.lock().unwrap().snapshot();
        assert_eq!(d.pre_stdout, Some(PipeEnd::ReadError));
        assert_eq!(d.bytes, 0);
        assert!(!serde_json::to_string(&d).unwrap().contains("secret-canary"));

        // Keep the existing final unterminated-frame behavior, but do not call it a newline.
        let boundary = Arc::new(Mutex::new(ReadBoundary::new()));
        let (tx, mut rx) = mpsc::channel(1);
        assert_eq!(read_lines_from(&b"{\"type\":\"s\",\"seq\":1}"[..], tx, boundary.clone()).await, PipeEnd::Eof);
        assert!(matches!(parse_line(&rx.recv().await.unwrap()), Some(Msg::Sample(_))));
        let partial = boundary.lock().unwrap().snapshot();
        assert_eq!(partial.forwarded, 1);
        assert_eq!(partial.last_line_ms, None);
        assert_eq!(partial.pre_stdout, Some(PipeEnd::Eof));
    }

    #[tokio::test]
    async fn missing_cleanup_report_retains_unknown_diagnostics() {
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let (stop, _stopped) = oneshot::channel();
        let (done, report) = oneshot::channel();
        drop(done);
        let ended = Source::new(rx, stop, report).close().await;
        assert_eq!(ended.wait_observed, None);
        assert_eq!(ended.code, None);
        assert_eq!(ended.wait_origin, None);
        assert_eq!(ended.kill_attempted, None);
        assert_eq!(ended.io, None);
        assert_eq!(end_category(false, 1, &ended), Some(Category::WaitUnknown));
        assert_eq!(end_category(true, 1, &ended), Some(Category::StallTimeout));
        let ended =
            Ended { code: Some(0), wait_observed: Some(true), io: Some(StreamBoundary { overflow: true, ..StreamBoundary::default() }), ..Ended::default() };
        assert_eq!(end_category(false, 1, &ended), None);
        assert!(ended.io.as_ref().unwrap().overflow);
        // Read errors remain explicit; diagnostics do not rewrite the pre-existing exit category.
        let ended = Ended { code: Some(0), wait_observed: Some(true), stdout: PipeEnd::ReadError, ..Ended::default() };
        assert_eq!(end_category(false, 1, &ended), None);
        assert_eq!(ended.stdout, PipeEnd::ReadError);
        assert!(ended.io.is_none());
    }

    #[test]
    fn read_boundary_saturation_is_explicit_unknown() {
        let mut b = ReadBoundary::new();
        b.add(u32::MAX as usize);
        b.add(1);
        assert_eq!(b.snapshot().bytes, u32::MAX);
        assert!(b.snapshot().overflow);
    }
}
