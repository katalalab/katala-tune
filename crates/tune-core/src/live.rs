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
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::collect;
use crate::db::now_ms;
use crate::nodes::Node;

pub const MAC_LIVE: &str = include_str!("../../../probes/live_mac.py");
/// PS 5.1 のため BOM 付き ASCII。バイトのまま渡す
pub const WIN_LIVE: &[u8] = include_bytes!("../../../probes/live_win.ps1");

/// 1 行の上限（壊れた出力でメモリを使い切らない）
const MAX_LINE: usize = 256 * 1024;
const MAX_CORES: usize = 1024;
const MAX_TOP: usize = 20;
const MAX_GPUS: usize = 16;
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
            g.into_iter()
                .take(MAX_GPUS)
                .map(|x| Gpu {
                    uuid: x.uuid.map(|s| short(&s)),
                    name: x.name.map(|n| short(&n)),
                    util: pct(x.util),
                    mem_used_mb: nonneg(x.mem_used_mb),
                    mem_total_mb: nonneg(x.mem_total_mb),
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
                .collect()
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
            let (used, total) = g.iter().fold((0.0, 0.0), |(u, t), x| (u + x.mem_used_mb.unwrap_or(0.0), t + x.mem_total_mb.unwrap_or(0.0)));
            put("gmem", (total > 0.0).then(|| used / total * 100.0), 1);
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
}

impl Source {
    pub fn new(lines: mpsc::Receiver<String>, stop: oneshot::Sender<()>, done: oneshot::Receiver<Ended>) -> Source {
        Source { lines, stop: Some(stop), done: Some(done) }
    }

    /// 止めて、終わるのを待つ
    pub async fn close(mut self) -> Ended {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        match self.done.take() {
            Some(d) => d.await.unwrap_or_default(),
            None => Ended::default(),
        }
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

/// 各機体に置くネイティブのサンプラー（`tune-agent sample`。Windows だけ。置き方は docs/architecture.md）
pub const NATIVE_REMOTE: &str = "~/.katala-tune/bin/tune-agent.exe";

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

/// この機体のネイティブのサンプラー: アプリ（tune・katala-tune）の隣、無ければ ~/.katala-tune/bin
fn local_native() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("tune-agent.exe")));
    let home = dirs::home_dir().map(|h| h.join(".katala-tune").join("bin").join("tune-agent.exe"));
    [beside, home].into_iter().flatten().find(|p| p.is_file())
}

/// 機体ごとの起動方法。macOS / Windows とも標準入力でスクリプトを渡す。
/// 渡した後も stdin を開き、停止時に閉じる。SSH は多重化しない。
pub fn launch(node: &Node, o: &Opts) -> Result<Launch, String> {
    if node.is_mac() {
        let a = format!("interval={} procs={} max={} watch=1", secs(o.interval), secs(o.procs_every), secs(o.sampler_max_age));
        let boot = python_boot(MAC_LIVE.len());
        let stdin = MAC_LIVE.as_bytes().to_vec();
        if node.local {
            let mut args = vec!["python3".to_string(), "-c".to_string(), boot];
            args.extend(a.split(' ').map(str::to_string));
            return Ok(Launch { program: "/usr/bin/env".into(), args, stdin, keep_stdin: true, cleanup: None });
        }
        let remote = format!("command -v python3 >/dev/null && exec python3 -c '{boot}' {a} || exec /usr/bin/python3 -c '{boot}' {a}");
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
    let mut stdin = child.stdin.take();
    let input = l.stdin;
    let keep = l.keep_stdin;
    // keep のとき、標準入力は止めるとき（closed の送り手が無くなったとき）まで開いておく
    let (closed_tx, closed_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        if let Some(si) = stdin.as_mut() {
            let _ = si.write_all(&input).await;
            let _ = si.flush().await;
            if !keep {
                let _ = si.shutdown().await;
            }
        }
        if keep {
            let _ = closed_rx.await;
        }
        drop(stdin);
    });
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(read_lines(child.stdout.take(), tx));
    let err_task = tokio::spawn(read_tail(child.stderr.take(), 4096));
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (done_tx, done_rx) = oneshot::channel();
    let cleanup = l.cleanup;
    tokio::spawn(async move {
        let mut closed_tx = Some(closed_tx);
        // stop_rx は止める指示か、Source が捨てられた（送り手が無くなった）ときに終わる
        let status = tokio::select! {
            st = child.wait() => st.ok(),
            _ = stop_rx => {
                // まず標準入力を閉じる。サンプラーは自分で後始末して終わる（Windows は nvidia-smi も止める）。
                // 終わらなければ（応答の無い機体など）強制的に止める
                drop(closed_tx.take());
                let grace = if keep { Duration::from_millis(2500) } else { Duration::ZERO };
                match tokio::time::timeout(grace, child.wait()).await {
                    Ok(st) => st.ok(),
                    Err(_) => {
                        let _ = child.start_kill();
                        child.wait().await.ok()
                    }
                }
            }
        };
        drop(closed_tx);
        // 孫プロセスがパイプを持ち続けても止まらないよう、読み終わりを待つのは少しだけ
        let err = tokio::time::timeout(Duration::from_secs(2), err_task).await.ok().and_then(Result::ok).unwrap_or_default();
        if let Some(f) = cleanup {
            let _ = std::fs::remove_file(f);
        }
        let _ = done_tx.send(Ended { code: status.and_then(|s| s.code()), err });
    });
    Ok(Source::new(rx, stop_tx, done_rx))
}

async fn read_lines(out: Option<tokio::process::ChildStdout>, tx: mpsc::Sender<String>) {
    let Some(out) = out else { return };
    let mut r = BufReader::new(out);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = match (&mut r).take(MAX_LINE as u64).read_until(b'\n', &mut buf).await {
            Ok(n) => n,
            Err(_) => return,
        };
        if n == 0 {
            return;
        }
        if buf.last() != Some(&b'\n') && n >= MAX_LINE {
            // 長すぎる行は改行まで読み捨てる
            loop {
                let mut skip = Vec::new();
                match (&mut r).take(MAX_LINE as u64).read_until(b'\n', &mut skip).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) if skip.last() == Some(&b'\n') => break,
                    Ok(_) => {}
                }
            }
            continue;
        }
        if tx.send(collect::decode(&buf)).await.is_err() {
            return;
        }
    }
}

async fn read_tail(err: Option<tokio::process::ChildStderr>, keep: usize) -> String {
    let Some(mut err) = err else { return String::new() };
    let mut tail: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(n) = err.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&chunk[..n]);
        if tail.len() > keep {
            tail.drain(..tail.len() - keep);
        }
    }
    collect::decode(&tail)
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
}

impl Live {
    pub fn new(route: Arc<dyn Route>, opts: Opts, sink: Sink) -> Arc<Live> {
        Arc::new(Live { route, opts, sink, st: Mutex::new(Shared::default()) })
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
        let now = Instant::now();
        let mut st = self.lock();
        let (mut started, mut running, mut refused) = (Vec::new(), Vec::new(), Vec::new());
        for id in unknown {
            refused.push(json!({ "id": id, "reason": "unknown", "detail": "台帳に無い機体" }));
        }
        let mut active = st.streams.values().filter(|s| !s.stopping).count();
        for n in nodes {
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

/// 1 台の流れ: 開く → 読む → 切れたらつなぎ直す（続けての失敗が max_retries を超えたら止める）
async fn run_stream(live: Arc<Live>, node: Node, epoch: u64, mut stop: watch::Receiver<Option<Stop>>) {
    let o = live.opts.clone();
    let mut failures = 0u32;
    let why: Stop = 'outer: loop {
        if let Some(r) = stop_requested(&stop) {
            break r;
        }
        let mut err = String::new();
        match live.route.open(&node, &o) {
            Err(e) => err = e,
            Ok(mut src) => {
                let started = Instant::now();
                let mut got = 0u64;
                let mut planned = false;
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
                                Some(Msg::Hello(h)) => live.on_hello(&node.id, epoch, h),
                                Some(Msg::Sample(s)) => {
                                    got += 1;
                                    live.on_sample(&node.id, epoch, *s);
                                }
                                Some(Msg::End(reason)) => planned = reason == "max_age",
                                None => {}
                            },
                            Ok(None) => break None,
                            Err(_) => {
                                err = format!("{} 秒間データが来ない", secs(wait));
                                break None;
                            }
                        }
                    }
                };
                let ended = src.close().await;
                if let Some(s) = stopped {
                    break 'outer s;
                }
                if planned {
                    // サンプラーが区切りで終わった。数えずに、少しだけ置いてつなぎ直す
                    failures = 0;
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

    #[test]
    fn launch_follows_the_probe_conventions() {
        let o = Opts::default();
        let mac = launch(&node("m", "macos"), &o).unwrap();
        assert_eq!(mac.program, "ssh");
        let cmd = mac.args.last().unwrap();
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
        if local.program == "powershell.exe" {
            assert!(local.cleanup.is_none() && local.keep_stdin && local.stdin == WIN_LIVE[3..]);
        } else {
            assert!(local.program.ends_with("tune-agent.exe") && local.keep_stdin && local.stdin.is_empty());
            assert_eq!(local.args, ["sample", "interval=1", "procs=5", "max=900", "watch=1"]);
        }
        assert!(WIN_LIVE[3..].iter().all(|b| b.is_ascii()), "probes/live_win.ps1 は ASCII のみ");
        assert!(launch(&node("x", "linux"), &o).is_err());
    }

    /// 実 PowerShell で scope・短い stdin chunk・末尾 byte・EOF 前後を検証する。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_boot_preserves_script_scope_and_stdin_lifetime() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let script = b"$value=42;function Scope {$script:value};[Console]::WriteLine((Scope));$io=[Console]::OpenStandardInput();[Console]::WriteLine($io.ReadByte());[Console]::WriteLine($io.ReadByte())";
        let boot = windows_boot(script.len(), "");
        let utf16: Vec<u8> = boot.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut child = collect::command("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &crate::logs::base64(&utf16)])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        for chunk in script.chunks(7) {
            input.write_all(chunk).await.unwrap();
            input.flush().await.unwrap();
        }
        input.write_all(b"Q").await.unwrap();
        input.flush().await.unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        let scope = tokio::time::timeout(Duration::from_secs(15), output.next_line()).await.unwrap().unwrap();
        assert_eq!(scope.as_deref(), Some("42"), "dot-source must preserve $script scope");
        let tail = tokio::time::timeout(Duration::from_secs(5), output.next_line()).await.unwrap().unwrap();
        assert_eq!(tail.as_deref(), Some("81"), "bootstrap must consume exactly script.len() bytes");
        assert!(child.try_wait().unwrap().is_none(), "must remain alive before stdin EOF");
        drop(input);
        let eof = tokio::time::timeout(Duration::from_secs(5), output.next_line()).await.unwrap().unwrap();
        assert_eq!(eof.as_deref(), Some("-1"));
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await.unwrap().unwrap();
        assert!(status.success());
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
                    assert_eq!(sample.gpu, Some(vec![]), "欠測をGPUの0Wとして作らない");
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
        let l = launch(&me, &Opts::default()).unwrap();
        assert!(l.keep_stdin && l.args[1] == "-c");
        let mut src = spawn(l).unwrap();
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
}
