//! Windows のサンプラーの取得部分（sample.rs から使う）。値の定義は probes/live_win.ps1 と同じ:
//!
//! - CPU・コア・ディスク・ネットワーク・CPU パッケージ電力: 性能カウンタ（PDH、英語名なので日本語版でも同じ）
//! - メモリ: used = 1 - Available Bytes / 物理メモリ、commit = Committed Bytes / Commit Limit
//! - プロセス: NtQuerySystemInformation(SystemProcessInformation) 1 回で全プロセスの CPU 時間・ワーキングセット・
//!   名前・親。ハンドルを開かないので、保護されたプロセス（vmmemWSL など）も数えられる
//! - GPU: nvidia-smi があるときだけ、120 回で自分から終わる `nvidia-smi dmon` を 1 つ（終わったら次の回でつなぎ直す）
//!
//! 取り出すのは数と、上位プロセスの PID・実行ファイル名だけ（コマンドライン・環境・ファイルの中身は読まない）。
#![allow(unsafe_code)] // PDH・NtQuerySystemInformation・GlobalMemoryStatusEx・OpenProcess の呼び出しだけ。前提は各所に書く

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Map, Value, json};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, UNICODE_STRING, WAIT_OBJECT_0};
use windows_sys::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

use crate::sample::{Args, Dmon, GpuNow, ProcRow, display_name, gpu_json, procs_json, round, virtual_adapter};

/// 100 を超える値をそのまま返す（コアの % は丸めない側で収める）
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const TOP: usize = 8;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// PDH
// ---------------------------------------------------------------------------

struct Pdh {
    query: PDH_HQUERY,
}

impl Pdh {
    fn open() -> Result<Pdh, String> {
        let mut q: PDH_HQUERY = std::ptr::null_mut();
        // SAFETY: q は書き込み先として有効。データ源は null（実時間）
        let r = unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut q) };
        if r != 0 { Err(format!("PdhOpenQuery 0x{r:08x}")) } else { Ok(Pdh { query: q }) }
    }

    fn add(&self, path: &str) -> Result<PDH_HCOUNTER, String> {
        let w = wide(path);
        let mut c: PDH_HCOUNTER = std::ptr::null_mut();
        // SAFETY: w は NUL 終端の UTF-16 で、呼び出しの間生きている。c は書き込み先
        let r = unsafe { PdhAddEnglishCounterW(self.query, w.as_ptr(), 0, &mut c) };
        if r != 0 { Err(format!("{path}: PDH 0x{r:08x}")) } else { Ok(c) }
    }

    fn collect(&self) -> bool {
        // SAFETY: query は open で得た有効なハンドル
        unsafe { PdhCollectQueryData(self.query) == 0 }
    }

    fn value(&self, c: PDH_HCOUNTER) -> Option<f64> {
        let mut v = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: c は add で得たこのクエリのカウンタ。v は書き込み先
        let r = unsafe { PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, std::ptr::null_mut(), &mut v) };
        if r != 0 || !(v.CStatus == PDH_CSTATUS_VALID_DATA || v.CStatus == PDH_CSTATUS_NEW_DATA) {
            return None;
        }
        // SAFETY: PDH_FMT_DOUBLE を頼んだので doubleValue が入っている
        Some(unsafe { v.Anonymous.doubleValue }).filter(|x| x.is_finite())
    }

    /// ワイルドカードのカウンタの (インスタンス名, 値)。値が無いインスタンスは捨てる
    fn array(&self, c: PDH_HCOUNTER) -> Vec<(String, f64)> {
        let fmt = PDH_FMT_DOUBLE | PDH_FMT_NOCAP100;
        let (mut size, mut count) = (0u32, 0u32);
        // SAFETY: 大きさを聞くだけ（バッファは null）
        let r = unsafe { PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut count, std::ptr::null_mut()) };
        if r != PDH_MORE_DATA || size == 0 {
            return Vec::new();
        }
        // 8 バイト境界のバッファ（項目は f64 と pointer を含む）
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        // SAFETY: buf は size バイト以上で、項目の並び（と名前の文字列）がその中に書かれる
        let r = unsafe { PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut count, items) };
        if r != 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            // SAFETY: count 個の項目が items から並んでいる（PDH がそう書いた）
            let it = unsafe { &*items.add(i) };
            if !(it.FmtValue.CStatus == PDH_CSTATUS_VALID_DATA || it.FmtValue.CStatus == PDH_CSTATUS_NEW_DATA) {
                continue;
            }
            // SAFETY: 名前は buf の中の NUL 終端の UTF-16、値は doubleValue
            let (name, v) = unsafe { (wide_cstr(it.szName), it.FmtValue.Anonymous.doubleValue) };
            if v.is_finite() {
                out.push((name, v));
            }
        }
        out
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        // SAFETY: open で得たハンドルを 1 回だけ閉じる
        unsafe { PdhCloseQuery(self.query) };
    }
}

/// NUL 終端の UTF-16 を String に（長すぎるものは 256 文字で切る）
///
/// # Safety
/// p は null か、NUL 終端の UTF-16 文字列を指す
unsafe fn wide_cstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: 呼び出し側の約束（NUL まで読める）
    while n < 256 && unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: 上で数えた n 文字は読める
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

// ---------------------------------------------------------------------------
// プロセス（NtQuerySystemInformation）
// ---------------------------------------------------------------------------

/// SYSTEM_PROCESS_INFORMATION の先頭（スレッドの並びより前）。NT 以来の並びで、ntapi・Process Hacker と同じ
#[repr(C)]
struct SpiHead {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UNICODE_STRING,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::offset_of!(SpiHead, image_name) == 56 && std::mem::offset_of!(SpiHead, working_set_size) == 144);

#[derive(Clone, Debug)]
struct ProcRaw {
    pid: u32,
    ppid: u32,
    image: String,
    /// CPU 時間（100ns 単位。カーネル＋ユーザー）
    cpu: i64,
    ws: u64,
}

fn processes(buf: &mut Vec<u64>) -> Result<Vec<ProcRaw>, String> {
    use windows_sys::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
    let mut tries = 0;
    let len = loop {
        let bytes = (buf.len() * 8) as u32;
        let mut need = 0u32;
        // SAFETY: buf は bytes バイトの書き込み先。need は書き込み先
        let st = unsafe { NtQuerySystemInformation(SystemProcessInformation, buf.as_mut_ptr().cast(), bytes, &mut need) };
        if st == 0 {
            break bytes as usize;
        }
        tries += 1;
        if st != STATUS_INFO_LENGTH_MISMATCH || tries > 5 {
            return Err(format!("NtQuerySystemInformation 0x{:08x}", st as u32));
        }
        // 呼ぶ間にもプロセスは増えるので余裕を持たせる
        buf.resize((need as usize + 64 * 1024).div_ceil(8), 0);
    };
    let base = buf.as_ptr().cast::<u8>();
    let end = base as usize + len;
    let mut out = Vec::with_capacity(512);
    let mut off = 0usize;
    while off + std::mem::size_of::<SpiHead>() <= len {
        // SAFETY: off + 大きさ <= len（buf の中）。境界は保証されないので read_unaligned
        let e = unsafe { std::ptr::read_unaligned(base.add(off).cast::<SpiHead>()) };
        let pid = e.unique_process_id as u32;
        if pid != 0 {
            let p = e.image_name.Buffer as usize;
            let n = e.image_name.Length as usize;
            // 名前は buf の中を指しているはず。外を指していたら読まない
            let image = if p >= base as usize && p.checked_add(n).is_some_and(|x| x <= end) && n % 2 == 0 && p % 2 == 0 {
                // SAFETY: 上で buf の中・2 バイト境界・n バイトを確かめた
                String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p as *const u16, n / 2) })
            } else {
                String::new()
            };
            out.push(ProcRaw {
                pid,
                ppid: e.inherited_from_unique_process_id as u32,
                image,
                cpu: e.user_time.saturating_add(e.kernel_time),
                ws: e.working_set_size as u64,
            });
        }
        if e.next_entry_offset == 0 {
            break;
        }
        off += e.next_entry_offset as usize;
    }
    Ok(out)
}

/// 前回との差から 1 コア換算の % を出す（同じ PID で名前が変わっていたら別のプロセス）
struct ProcRates {
    prev: HashMap<u32, (String, i64)>,
    at: Option<Instant>,
}

impl ProcRates {
    fn rows(&mut self, raw: &[ProcRaw], now: Instant) -> Vec<ProcRow> {
        let dt = self.at.map(|t| now.duration_since(t).as_secs_f64() * 1e7).filter(|d| *d > 0.0);
        let mut next = HashMap::with_capacity(raw.len());
        let rows = raw
            .iter()
            .map(|p| {
                let cpu = match (self.prev.get(&p.pid), dt) {
                    (Some((img, c)), Some(dt)) if *img == p.image => Some(((p.cpu - c).max(0) as f64 / dt * 100.0).max(0.0)),
                    _ => None,
                };
                next.insert(p.pid, (p.image.clone(), p.cpu));
                ProcRow { pid: p.pid, name: display_name(&p.image), cpu, mem_bytes: p.ws }
            })
            .collect();
        self.prev = next;
        self.at = Some(now);
        rows
    }
}

// ---------------------------------------------------------------------------
// GPU（nvidia-smi）
// ---------------------------------------------------------------------------

fn nvidia_smi() -> Option<PathBuf> {
    let sys = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32\\nvidia-smi.exe");
    if sys.is_file() {
        return Some(sys);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("nvidia-smi.exe")).find(|p| p.is_file())
}

struct Gpus {
    smi: PathBuf,
    names: HashMap<u32, String>,
    totals: HashMap<u32, f64>,
    now: Arc<Mutex<HashMap<u32, GpuNow>>>,
    child: Option<Child>,
    secs: u64,
}

impl Gpus {
    fn open(smi: PathBuf, interval: f64) -> Gpus {
        let mut g = Gpus { smi, names: HashMap::new(), totals: HashMap::new(), now: Arc::default(), child: None, secs: interval.round().max(1.0) as u64 };
        if let Ok(o) = Command::new(&g.smi)
            .args(["--query-gpu=index,name,memory.total", "--format=csv,noheader,nounits"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let f: Vec<&str> = line.split(',').map(str::trim).collect();
                if let [i, name, total, ..] = f[..]
                    && let Ok(i) = i.parse::<u32>()
                {
                    g.names.insert(i, name.to_string());
                    if let Ok(t) = total.parse::<f64>() {
                        g.totals.insert(i, t);
                    }
                }
            }
        }
        g
    }

    /// dmon が動いていなければ（初回・120 回で終わった）起動する。読むのは別のスレッド
    fn poll(&mut self) {
        if let Some(c) = self.child.as_mut()
            && matches!(c.try_wait(), Ok(None))
        {
            return;
        }
        self.child = None;
        let Ok(mut c) = Command::new(&self.smi)
            .args(["dmon", "-s", "pucm", "-d", &self.secs.to_string(), "-c", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        else {
            return;
        };
        let out = c.stdout.take();
        let now = self.now.clone();
        std::thread::spawn(move || {
            let Some(out) = out else { return };
            let mut d = Dmon::default();
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { return };
                if let Some((i, g)) = d.line(&line)
                    && let Ok(mut m) = now.lock()
                {
                    m.insert(i, g);
                }
            }
        });
        self.child = Some(c);
    }

    fn json(&self) -> Option<Value> {
        let m = self.now.lock().ok()?;
        if m.is_empty() {
            return None;
        }
        let mut keys: Vec<&u32> = m.keys().collect();
        keys.sort();
        Some(Value::Array(keys.into_iter().map(|k| gpu_json(self.names.get(k).map(String::as_str), self.totals.get(k).copied(), &m[k])).collect()))
    }
}

impl Drop for Gpus {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// サンプラー
// ---------------------------------------------------------------------------

struct Counters {
    cpu_total: Option<PDH_HCOUNTER>,
    cpu_cores: Option<PDH_HCOUNTER>,
    avail: Option<PDH_HCOUNTER>,
    committed: Option<PDH_HCOUNTER>,
    commit_limit: Option<PDH_HCOUNTER>,
    disk_read: Option<PDH_HCOUNTER>,
    disk_write: Option<PDH_HCOUNTER>,
    net_rx: Option<PDH_HCOUNTER>,
    net_tx: Option<PDH_HCOUNTER>,
    energy: Option<PDH_HCOUNTER>,
}

pub struct Sampler {
    a: Args,
    pdh: Pdh,
    c: Counters,
    total_bytes: Option<f64>,
    errors: Vec<String>,
    buf: Vec<u64>,
    rates: ProcRates,
    gpus: Option<Gpus>,
    session: Option<SessionWatch>,
    cores: usize,
    power_seen: bool,
    started_ms: i64,
}

fn total_physical() -> Option<f64> {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: dwLength を入れた MEMORYSTATUSEX への書き込み
    (unsafe { GlobalMemoryStatusEx(&mut m) } != 0 && m.ullTotalPhys > 0).then_some(m.ullTotalPhys as f64)
}

/// Energy Meter の RAPL パッケージ（rapl_package0_pkg など）だけ
fn rapl_package(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.strip_prefix("rapl_package").and_then(|r| r.strip_suffix("_pkg")).is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

impl Sampler {
    pub fn open(a: &Args) -> Result<Sampler, String> {
        let pdh = Pdh::open()?;
        let mut errors = Vec::new();
        let mut add = |p: &str| match pdh.add(p) {
            Ok(c) => Some(c),
            Err(e) => {
                errors.push(e);
                None
            }
        };
        let c = Counters {
            cpu_total: add("\\Processor(_Total)\\% Processor Time"),
            cpu_cores: add("\\Processor(*)\\% Processor Time"),
            avail: add("\\Memory\\Available Bytes"),
            committed: add("\\Memory\\Committed Bytes"),
            commit_limit: add("\\Memory\\Commit Limit"),
            disk_read: add("\\PhysicalDisk(_Total)\\Disk Read Bytes/sec"),
            disk_write: add("\\PhysicalDisk(_Total)\\Disk Write Bytes/sec"),
            net_rx: add("\\Network Interface(*)\\Bytes Received/sec"),
            net_tx: add("\\Network Interface(*)\\Bytes Sent/sec"),
            // 無い機体（Energy Meter を持たない CPU・VM）では電力を出さないだけ。エラーには数えない
            energy: pdh.add("\\Energy Meter(*)\\Power").ok(),
        };
        let total_bytes = total_physical();
        if total_bytes.is_none() {
            errors.push("memory: total unknown".into());
        }
        let mut s = Sampler {
            a: a.clone(),
            pdh,
            c,
            total_bytes,
            errors,
            buf: vec![0u64; 512 * 1024 / 8],
            rates: ProcRates { prev: HashMap::new(), at: None },
            gpus: nvidia_smi().map(|p| Gpus::open(p, a.interval)),
            session: None,
            cores: 0,
            power_seen: false,
            started_ms: tune_link::now_ms(),
        };
        // 率のカウンタの 1 回目（値にはならない）と、プロセスの CPU 時間の基準
        s.pdh.collect();
        if let Ok(raw) = processes(&mut s.buf) {
            s.rates.rows(&raw, Instant::now());
        }
        if let Some(g) = s.gpus.as_mut() {
            g.poll();
        }
        Ok(s)
    }

    /// 自分の上にいる sshd（sshd-session・sshd）を親をたどって探し、終わったかを見張る。
    /// Git Bash 経由の SSH では、セッションが終わっても標準入出力が閉じないことがある（実機で確認）
    pub fn watch_session(&mut self) {
        let Ok(raw) = processes(&mut self.buf) else { return };
        let by_pid: HashMap<u32, &ProcRaw> = raw.iter().map(|p| (p.pid, p)).collect();
        let mut cur = std::process::id();
        for _ in 0..16 {
            let Some(p) = by_pid.get(&cur) else { return };
            let Some(parent) = by_pid.get(&p.ppid) else { return };
            if parent.pid == p.pid {
                return;
            }
            if parent.image.to_ascii_lowercase().starts_with("sshd") {
                self.session = SessionWatch::open(parent.pid);
                return;
            }
            cur = parent.pid;
        }
    }

    pub fn session_gone(&self) -> bool {
        self.session.as_ref().is_some_and(SessionWatch::gone)
    }

    pub fn hello(&self) -> Value {
        let has = json!({
            "cpu": self.c.cpu_total.is_some(),
            "mem": self.c.avail.is_some(),
            "disk": self.c.disk_read.is_some(),
            "net": self.c.net_rx.is_some(),
            "procs": true,
            "gpu": self.gpus.is_some(),
            "power": self.power_seen,
        });
        let cores = if self.cores > 0 { self.cores } else { std::thread::available_parallelism().map_or(0, |n| n.get()) };
        json!({
            "type": "hello",
            "v": 1,
            "os": "windows",
            "cores": cores,
            "interval": self.a.interval,
            "procs_every": self.a.procs_every,
            "mem_total_gb": self.total_bytes.map_or(Value::Null, |b| round(b / 1_073_741_824.0, 1)),
            "session_epoch_ms": self.started_ms,
            "has": has,
            "errors": self.errors,
            "impl": "rust",
            "agent": env!("CARGO_PKG_VERSION"),
        })
    }

    /// 1 点（type・t・seq は呼び出し側が付ける）。procs のときだけ上位プロセスと自分の負荷も
    pub fn sample(&mut self, procs: bool) -> Value {
        let mut m = Map::new();
        self.pdh.collect();
        let p = &self.pdh;
        let cpu = self.c.cpu_total.and_then(|c| p.value(c));
        m.insert("cpu".into(), cpu.map_or(Value::Null, |v| round(v.clamp(0.0, 100.0), 1)));
        let mut cores: Vec<(usize, f64)> =
            self.c.cpu_cores.map(|c| p.array(c)).unwrap_or_default().into_iter().filter_map(|(n, v)| Some((n.parse::<usize>().ok()?, v))).collect();
        cores.sort_by_key(|(i, _)| *i);
        self.cores = self.cores.max(cores.len());
        m.insert("cores".into(), Value::Array(cores.iter().map(|(_, v)| round(v.clamp(0.0, 100.0), 0)).collect()));

        let avail = self.c.avail.and_then(|c| p.value(c));
        let committed = self.c.committed.and_then(|c| p.value(c));
        let limit = self.c.commit_limit.and_then(|c| p.value(c)).filter(|l| *l > 0.0);
        let gb = 1_073_741_824.0;
        m.insert(
            "mem".into(),
            json!({
                "used_pct": match (avail, self.total_bytes) { (Some(a), Some(t)) => round((1.0 - a / t) * 100.0, 1), _ => Value::Null },
                "commit_pct": match (committed, limit) { (Some(c), Some(l)) => round(c / l * 100.0, 1), _ => Value::Null },
                "commit_gb": committed.map_or(Value::Null, |c| round(c / gb, 2)),
                "commit_limit_gb": limit.map_or(Value::Null, |l| round(l / gb, 2)),
            }),
        );

        if let Some(e) = self.c.energy {
            let pkgs: Vec<f64> = p.array(e).into_iter().filter(|(n, _)| rapl_package(n)).map(|(_, v)| v).collect();
            // 全パッケージが有限で 0 以上のときだけ足す（1 つでも欠けたら出さない）
            let valid = !pkgs.is_empty() && pkgs.iter().all(|v| *v >= 0.0);
            if valid {
                self.power_seen = true;
            }
            if valid || self.power_seen {
                m.insert(
                    "power".into(),
                    json!({
                        "package_w": if valid { round(pkgs.iter().sum::<f64>() / 1000.0, 1) } else { Value::Null },
                        "package_source": "Energy Meter RAPL package average (mW / 1000)",
                        "available": valid,
                    }),
                );
            }
        }

        if let (Some(r), Some(w)) = (self.c.disk_read, self.c.disk_write) {
            m.insert(
                "disk".into(),
                json!({"read_bps": p.value(r).map_or(Value::Null, |v| round(v, 0)), "write_bps": p.value(w).map_or(Value::Null, |v| round(v, 0))}),
            );
        }
        if let (Some(rx), Some(tx)) = (self.c.net_rx, self.c.net_tx) {
            let sum = |c| p.array(c).into_iter().filter(|(n, _)| !virtual_adapter(n)).map(|(_, v)| v).sum::<f64>();
            m.insert("net".into(), json!({"rx_bps": round(sum(rx), 0), "tx_bps": round(sum(tx), 0)}));
        }

        if let Some(g) = self.gpus.as_mut() {
            g.poll();
            if let Some(v) = g.json() {
                m.insert("gpu".into(), v);
            }
        }

        if procs && let Ok(raw) = processes(&mut self.buf) {
            let me = std::process::id();
            if let Some(s) = raw.iter().find(|r| r.pid == me) {
                m.insert("self".into(), json!({"cpu_s": round(s.cpu as f64 / 1e7, 3), "rss_mb": round(s.ws as f64 / 1_048_576.0, 1)}));
            }
            let rows = self.rates.rows(&raw, Instant::now());
            m.insert("procs".into(), procs_json(&rows, TOP));
        }
        Value::Object(m)
    }
}

/// 見張っている sshd のハンドル（PID の使い回しにだまされないよう、開いたまま持つ）
struct SessionWatch {
    h: HANDLE,
}

impl SessionWatch {
    fn open(pid: u32) -> Option<SessionWatch> {
        // SAFETY: 待つ権限だけで開く。失敗は null
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        (!h.is_null()).then_some(SessionWatch { h })
    }

    fn gone(&self) -> bool {
        // SAFETY: open で得た有効なハンドル。待たずに状態だけ見る
        unsafe { WaitForSingleObject(self.h, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for SessionWatch {
    fn drop(&mut self) {
        // SAFETY: open で得たハンドルを 1 回だけ閉じる
        unsafe { CloseHandle(self.h) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapl_names() {
        assert!(rapl_package("rapl_package0_pkg"));
        assert!(rapl_package("RAPL_Package12_PKG"));
        assert!(!rapl_package("rapl_package0_dram"));
        assert!(!rapl_package("rapl_package_pkg"));
        assert!(!rapl_package("rapl_packagex_pkg"));
    }

    #[test]
    fn process_snapshot_sees_this_process_and_its_parent() {
        let mut buf = vec![0u64; 8];
        let raw = processes(&mut buf).unwrap();
        assert!(raw.len() > 10, "全プロセスが読める");
        let me = raw.iter().find(|p| p.pid == std::process::id()).expect("自分が居る");
        assert!(me.image.to_ascii_lowercase().ends_with(".exe"), "{}", me.image);
        assert!(me.ws > 0 && me.cpu >= 0);
        assert!(raw.iter().any(|p| p.pid == 4), "System（保護されたプロセス）もハンドル無しで見える");
    }

    #[test]
    fn rates_need_two_snapshots_and_same_image() {
        let mut r = ProcRates { prev: HashMap::new(), at: None };
        let t0 = Instant::now();
        let a = |cpu, img: &str| vec![ProcRaw { pid: 7, ppid: 1, image: img.into(), cpu, ws: 1 << 20 }];
        assert_eq!(r.rows(&a(0, "x.exe"), t0)[0].cpu, None);
        // 1 秒で 0.5 秒分の CPU 時間 = 1 コア換算 50%
        let rows = r.rows(&a(5_000_000, "x.exe"), t0 + std::time::Duration::from_secs(1));
        assert!((rows[0].cpu.unwrap() - 50.0).abs() < 1e-6);
        assert_eq!(rows[0].name, "x");
        // 同じ PID でも名前が変われば別のプロセス
        assert_eq!(r.rows(&a(9_000_000, "y.exe"), t0 + std::time::Duration::from_secs(2))[0].cpu, None);
    }

    #[test]
    fn sampler_reads_this_machine() {
        let a = Args { interval: 1.0, procs_every: 5.0, max_seconds: 10.0, count: 0, watch: false };
        let mut s = Sampler::open(&a).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let v = s.sample(true);
        let h = s.hello();
        assert_eq!(h["os"], "windows");
        assert!(h["cores"].as_u64().unwrap() > 0);
        assert!(v["cpu"].as_f64().is_some(), "{v}");
        assert!(v["mem"]["used_pct"].as_f64().is_some_and(|x| x > 0.0 && x < 100.0), "{v}");
        assert!(v["mem"]["commit_pct"].as_f64().is_some(), "{v}");
        assert!(v["procs"]["count"].as_u64().unwrap() > 10);
        assert!(v["self"]["rss_mb"].as_f64().is_some());
    }
}
