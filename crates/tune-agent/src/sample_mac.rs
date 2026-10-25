//! macOS のサンプラーの取得部分（sample.rs から使う）。行・値・止まり方は probes/live_mac.py と同じ:
//!
//! - CPU・コア: host_processor_info のコアごとの tick（user・system・idle・nice）。差は 2^32 で割った余り
//! - メモリ: sysctl の kern.memorystatus_level（使える割合）・vm.swapusage・kern.memorystatus_vm_pressure_level
//! - ディスク: IOBlockStorageDriver の Statistics（Activity Monitor のディスクと同じ元）
//! - ネットワーク: getifaddrs の en* の if_data（utun・bridge などは同じ通信を二重に数えるので除く）
//! - プロセス: 自分のプロセスは libproc で毎回。root や他のユーザーのプロセスは libproc では読めないので、
//!   setuid の /bin/ps を PS_EVERY 秒ごとに読む（Python 版と同じ間隔）
//! - 電力・GPU: 任意の macmon があるときだけ 1 子プロセス（`macmon pipe`）を読む。終わるときに子も止める
//!
//! 取り出すのは数と、上位プロセスの PID・実行ファイル名・アプリ名だけ（引数・環境・パス・ファイルの中身は出さない）。
//! 丸めは Python の round と同じ（小数は正確な値の偶数丸め、整数は偶数丸め）。Python 版との突き合わせは
//! この下のテストと tests/live_mac_parity.rs。
#![allow(unsafe_code)] // sysctl・mach・libproc・getifaddrs・IOKit・CoreFoundation の呼び出しだけ。前提は各所に書く

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::sample::{Args, emit, watch_stdin};

const TOP: usize = 8;
const GB: f64 = 1_073_741_824.0;
const MB: f64 = 1_048_576.0;

pub struct Control {
    gone: Arc<AtomicBool>,
    deadline: Instant,
}

impl Control {
    fn stopped(&self) -> bool {
        self.gone.load(Ordering::SeqCst) || Instant::now() >= self.deadline
    }
}

/// The owner must be able to clean up children even if a reader keeps a full pipe open.
/// At most one row is in flight. Only this worker owns stdout, never sampler resources.
/// It is deliberately not joined on cancellation: main returns after owned-child cleanup,
/// and process exit also ends a blocked writer without altering shared stdout flags.
struct Output {
    requests: SyncSender<(Value, Sender<bool>)>,
}

impl Output {
    fn open() -> Output {
        Self::with_writer(|| std::io::stdout().lock())
    }

    fn with_writer<W: Write>(make_writer: impl FnOnce() -> W + Send + 'static) -> Output {
        let (requests, rows) = mpsc::sync_channel::<(Value, Sender<bool>)>(1);
        std::thread::spawn(move || {
            let mut out = make_writer();
            while let Ok((value, ack)) = rows.recv() {
                let ok = emit(&mut out, &value);
                if ack.send(ok).is_err() || !ok {
                    break;
                }
            }
        });
        Output { requests }
    }

    fn emit(&self, value: Value, control: &Control) -> bool {
        if control.stopped() {
            return false;
        }
        let (ack, result) = mpsc::channel();
        if self.requests.try_send((value, ack)).is_err() {
            return false;
        }
        loop {
            if control.stopped() {
                return false;
            }
            match result.recv_timeout(Duration::from_millis(20)) {
                Ok(ok) => return ok,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    fn end(&self, reason: &str, control: &Control) {
        // Give a reading consumer the end row even after max expires. A full pipe must
        // not extend the lifetime indefinitely just to deliver that final notification.
        let final_write = Control { gone: control.gone.clone(), deadline: Instant::now() + Duration::from_millis(100) };
        self.emit(json!({"type": "end", "reason": reason}), &final_write);
    }

    fn end_if_expired(&self, control: &Control) {
        if Instant::now() >= control.deadline {
            self.end("max_age", control);
        }
    }
}

/// Only the ps child belongs to this guard. Always reap it, including early returns.
struct PsChild(Child);

impl Drop for PsChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn ps_output(command: &mut Command, control: &Control, timeout: Duration) -> Option<Vec<u8>> {
    if control.stopped() {
        return None;
    }
    let mut child = PsChild(command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?);
    let mut pipe = child.0.stdout.take()?;
    let fd = pipe.as_raw_fd();
    // SAFETY: fd is this child's private stdout read end, valid while pipe lives.
    // Unlike changing our stdout flags, this does not change a caller's shared descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return None;
    }
    let deadline = control.deadline.min(Instant::now() + timeout);
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    let mut eof = false;
    loop {
        if control.stopped() || Instant::now() >= deadline {
            return None;
        }
        if !eof {
            match pipe.read(&mut buf) {
                Ok(0) => eof = true,
                Ok(n) => {
                    // A broken producer must not allocate indefinitely before its deadline.
                    if bytes.len() + n > 16 * 1024 * 1024 {
                        return None;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        }
        // EOF alone does not prove the child exited; root exit alone does not prove EOF.
        let exited = child.0.try_wait().ok()?.is_some();
        if eof && exited {
            return Some(bytes);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Python の round(x, n)（正確な値を偶数丸め。Rust の書式化と同じ規則）
pub fn py_round(x: f64, n: usize) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    format!("{x:.n$}").parse::<f64>().map_or(Value::Null, |v| json!(v))
}

/// Python の round(x)（整数を返す。偶数丸め）
pub fn py_int(x: f64) -> Value {
    if !x.is_finite() { Value::Null } else { json!(x.round_ties_even() as i64) }
}

// ---------------------------------------------------------------------------
// OS の呼び出し（mach の関数は libc では非推奨なので、ここで宣言する）
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Default)]
struct Timebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    static mach_task_self_: u32;
    fn mach_host_self() -> u32;
    fn host_processor_info(host: u32, flavor: c_int, count: *mut u32, info: *mut *mut i32, info_count: *mut u32) -> c_int;
    fn vm_deallocate(task: u32, address: usize, size: usize) -> c_int;
    fn mach_timebase_info(info: *mut Timebase) -> c_int;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main_port: u32, matching: *mut c_void, iter: *mut u32) -> c_int;
    fn IOIteratorNext(iter: u32) -> u32;
    fn IOIteratorIsValid(iter: u32) -> u32;
    fn IOObjectRelease(obj: u32) -> c_int;
    fn IORegistryEntryCreateCFProperty(entry: u32, key: *const c_void, allocator: *const c_void, options: u32) -> *const c_void;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> c_int;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> *const c_void;
    fn CFDictionaryGetValue(d: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(n: *const c_void, kind: c_int, out: *mut c_void) -> bool;
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFRelease(cf: *const c_void);
}

const PROCESSOR_CPU_LOAD_INFO: c_int = 2;
const CF_UTF8: u32 = 0x0800_0100;
const CF_SINT64: c_int = 4;

/// sysctlbyname で固定長の値を 1 つ読む。大きさが合わなければ None
fn sysctl<T: Copy + Default>(name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    let mut v = T::default();
    let mut n = std::mem::size_of::<T>();
    // SAFETY: c は NUL 終端。v は大きさ n の書き込み先で、呼び出しの間生きている
    let r = unsafe { libc::sysctlbyname(c.as_ptr(), (&mut v as *mut T).cast(), &mut n, std::ptr::null_mut(), 0) };
    (r == 0 && n == std::mem::size_of::<T>()).then_some(v)
}

// ---------------------------------------------------------------------------
// メモリ
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XswUsage {
    total: u64,
    avail: u64,
    used: u64,
    pagesize: u32,
    encrypted: i32,
}

pub fn pressure_name(level: i32) -> &'static str {
    match level {
        1 => "normal",
        2 => "warn",
        4 => "critical",
        _ => "unknown",
    }
}

fn memory() -> Value {
    // 使える割合（%）。mac_probe.py の available_pct と同じ値
    let level = sysctl::<i32>("kern.memorystatus_level");
    let sw = sysctl::<XswUsage>("vm.swapusage");
    let pressure = sysctl::<i32>("kern.memorystatus_vm_pressure_level");
    let n = |v: Option<Value>| v.unwrap_or(Value::Null);
    json!({
        "used_pct": n(level.map(|l| py_round(100.0 - l as f64, 1))),
        "swap_used_gb": n(sw.map(|s| py_round(s.used as f64 / GB, 2))),
        "swap_total_gb": n(sw.map(|s| py_round(s.total as f64 / GB, 2))),
        "pressure": n(pressure.map(|p| json!(pressure_name(p)))),
    })
}

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

type Ticks = Vec<[u32; 4]>;

fn cpu_ticks(host: u32) -> Option<Ticks> {
    let (mut n, mut cnt) = (0u32, 0u32);
    let mut info: *mut i32 = std::ptr::null_mut();
    // SAFETY: 書き込み先はすべて有効。成功時は info に cnt 個の整数が割り当てられ、下で解放する
    if unsafe { host_processor_info(host, PROCESSOR_CPU_LOAD_INFO, &mut n, &mut info, &mut cnt) } != 0 || info.is_null() {
        return None;
    }
    // SAFETY: info は cnt 個の i32 を指す（カーネルが割り当てた領域）
    let vals = unsafe { std::slice::from_raw_parts(info, cnt as usize) };
    let out = (0..n as usize).filter(|i| (i + 1) * 4 <= vals.len()).map(|i| std::array::from_fn(|k| vals[i * 4 + k] as u32)).collect();
    // SAFETY: host_processor_info が割り当てた領域を 1 回だけ返す
    unsafe { vm_deallocate(mach_task_self_, info as usize, cnt as usize * 4) };
    Some(out)
}

/// (全体, コアごと)。tick は 32 ビットで一周するので差は 2^32 で割った余り
pub fn cpu_pct(prev: Option<&Ticks>, cur: Option<&Ticks>) -> (Value, Vec<Value>) {
    let (Some(prev), Some(cur)) = (prev, cur) else { return (Value::Null, vec![]) };
    if prev.is_empty() || cur.is_empty() || prev.len() != cur.len() {
        return (Value::Null, vec![]);
    }
    let (mut cores, mut busy_all, mut total_all) = (Vec::new(), 0u64, 0u64);
    for (a, b) in prev.iter().zip(cur) {
        let d: [u64; 4] = std::array::from_fn(|k| b[k].wrapping_sub(a[k]) as u64);
        let (busy, total) = (d[0] + d[1] + d[3], d.iter().sum::<u64>());
        busy_all += busy;
        total_all += total;
        cores.push(if total > 0 { py_int(busy as f64 / total as f64 * 100.0) } else { json!(0) });
    }
    (if total_all > 0 { py_round(busy_all as f64 / total_all as f64 * 100.0, 1) } else { Value::Null }, cores)
}

// ---------------------------------------------------------------------------
// ネットワーク
// ---------------------------------------------------------------------------

pub type NetBytes = HashMap<String, (u32, u32)>;

fn net_bytes() -> Option<NetBytes> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: head は書き込み先。成功時は下で freeifaddrs する
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return None;
    }
    let mut out = HashMap::new();
    let mut p = head;
    while !p.is_null() {
        // SAFETY: p は getifaddrs の連結リストの要素で、freeifaddrs まで有効
        let a = unsafe { &*p };
        // SAFETY: ifa_addr が null でなければ sockaddr を指す。AF_LINK のときだけ ifa_data は if_data を指す
        let link = !a.ifa_addr.is_null() && unsafe { (*a.ifa_addr).sa_family } as c_int == libc::AF_LINK;
        if link && !a.ifa_data.is_null() && !a.ifa_name.is_null() {
            // SAFETY: ifa_name は NUL 終端
            let name = unsafe { CStr::from_ptr(a.ifa_name) }.to_string_lossy().into_owned();
            if name.starts_with("en") {
                // SAFETY: 上の条件で ifa_data は if_data を指す
                let d = unsafe { &*(a.ifa_data as *const libc::if_data) };
                out.insert(name, (d.ifi_ibytes, d.ifi_obytes));
            }
        }
        p = a.ifa_next;
    }
    // SAFETY: getifaddrs で得たリストを 1 回だけ返す
    unsafe { libc::freeifaddrs(head) };
    Some(out)
}

/// if_data のカウンタは 32 ビット。前回に無いインターフェースは数えない
pub fn net_rate(prev: Option<&NetBytes>, cur: Option<&NetBytes>, dt: f64) -> Value {
    let (Some(prev), Some(cur)) = (prev, cur) else { return Value::Null };
    if dt <= 0.0 {
        return Value::Null;
    }
    let (mut rx, mut tx) = (0u64, 0u64);
    for (k, (i, o)) in cur {
        if let Some((pi, po)) = prev.get(k) {
            rx += i.wrapping_sub(*pi) as u64;
            tx += o.wrapping_sub(*po) as u64;
        }
    }
    json!({"rx_bps": py_int(rx as f64 / dt), "tx_bps": py_int(tx as f64 / dt)})
}

// ---------------------------------------------------------------------------
// ディスク
// ---------------------------------------------------------------------------

type DiskBytes = HashMap<u64, (Option<i64>, Option<i64>)>;

/// Identities stay internal: replacement/reset or incomplete coverage is unknown,
/// not an idle disk. Each direction can recover independently after two valid points.
fn disk_rate(prev: &DiskBytes, cur: &DiskBytes, dt: f64) -> Value {
    let mut result = json!({"read_bps":null,"write_bps":null});
    if prev.is_empty() || cur.is_empty() || prev.len() != cur.len() || !cur.keys().all(|k| prev.contains_key(k)) || !dt.is_finite() || dt <= 0.0 {
        return result;
    }
    for (direction, key) in ["read_bps", "write_bps"].into_iter().enumerate() {
        let delta = cur.iter().try_fold(0i64, |sum, (id, after)| {
            let before = prev[id];
            let (a, b) = if direction == 0 { (before.0?, after.0?) } else { (before.1?, after.1?) };
            if a < 0 || b < a { None } else { sum.checked_add(b - a) }
        });
        if let Some(delta) = delta {
            let rate = delta as f64 / dt;
            if rate.is_finite() && rate < i64::MAX as f64 {
                result[key] = py_int(rate);
            }
        }
    }
    result
}

struct Disk {
    k_stats: *const c_void,
    k_read: *const c_void,
    k_write: *const c_void,
    services: Vec<u32>,
    found_at: Option<Instant>,
}

impl Disk {
    fn open() -> Result<Disk, String> {
        let key = |s: &CStr| {
            // SAFETY: s は NUL 終端の UTF-8。返る CFString は drop で解放する
            unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), CF_UTF8) }
        };
        let d = Disk { k_stats: key(c"Statistics"), k_read: key(c"Bytes (Read)"), k_write: key(c"Bytes (Write)"), services: vec![], found_at: None };
        if d.k_stats.is_null() || d.k_read.is_null() || d.k_write.is_null() { Err("CFString を作れない".into()) } else { Ok(d) }
    }

    fn enumerate(&mut self) -> bool {
        for s in self.services.drain(..) {
            // SAFETY: IOIteratorNext で得た参照を 1 回だけ返す
            unsafe { IOObjectRelease(s) };
        }
        let mut it = 0u32;
        // SAFETY: 名前は NUL 終端。matching は IOServiceGetMatchingServices が受け取って解放する
        if unsafe { IOServiceGetMatchingServices(0, IOServiceMatching(c"IOBlockStorageDriver".as_ptr()), &mut it) } != 0 {
            return false;
        }
        loop {
            // SAFETY: it は有効なイテレータ
            let s = unsafe { IOIteratorNext(it) };
            if s == 0 {
                break;
            }
            self.services.push(s);
        }
        // SAFETY: it is owned until release; invalidation means partial enumeration.
        let valid = unsafe { IOIteratorIsValid(it) } != 0;
        // SAFETY: イテレータを 1 回だけ返す
        unsafe { IOObjectRelease(it) };
        valid
    }

    fn num(d: *const c_void, k: *const c_void) -> Option<i64> {
        let mut v = 0i64;
        // SAFETY: d は CFDictionary、k は CFString。値が無ければ null。v は 64 ビットの書き込み先
        let p = unsafe { CFDictionaryGetValue(d, k) };
        // SAFETY: borrowed non-null CF value; check its type before using CFNumber.
        if !p.is_null() && unsafe { CFGetTypeID(p) == CFNumberGetTypeID() && CFNumberGetValue(p, CF_SINT64, (&mut v as *mut i64).cast()) } && v >= 0 {
            Some(v)
        } else {
            None
        }
    }

    fn bytes(&mut self, now: Instant) -> DiskBytes {
        // 外付けの抜き差しに追従する
        if self.found_at.is_none_or(|t| now.duration_since(t) > Duration::from_secs(30)) {
            let valid = self.enumerate();
            self.found_at = Some(now);
            if !valid {
                for s in self.services.drain(..) {
                    // SAFETY: partial enumeration still owns each returned service.
                    unsafe { IOObjectRelease(s) };
                }
                return HashMap::new();
            }
        }
        let mut counters = HashMap::new();
        for s in &self.services {
            let mut id = 0u64;
            // SAFETY: s is owned; id is writable. Failure cannot identify coverage.
            if unsafe { IORegistryEntryGetRegistryEntryID(*s, &mut id) } != 0 || counters.contains_key(&id) {
                return HashMap::new();
            }
            // SAFETY: s は有効な IORegistry の項目。返る辞書は下で解放する
            let st = unsafe { IORegistryEntryCreateCFProperty(*s, self.k_stats, std::ptr::null(), 0) };
            if st.is_null() {
                counters.insert(id, (None, None));
                continue;
            }
            // SAFETY: property is a live CF value; use it as a dictionary only after type validation.
            let values =
                if unsafe { CFGetTypeID(st) == CFDictionaryGetTypeID() } { (Self::num(st, self.k_read), Self::num(st, self.k_write)) } else { (None, None) };
            counters.insert(id, values);
            // SAFETY: Create で得た参照を 1 回だけ返す
            unsafe { CFRelease(st) };
        }
        counters
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        for s in self.services.drain(..) {
            // SAFETY: 持っている参照を 1 回だけ返す
            unsafe { IOObjectRelease(s) };
        }
        for k in [self.k_stats, self.k_read, self.k_write] {
            if !k.is_null() {
                // SAFETY: open で作った CFString を 1 回だけ返す
                unsafe { CFRelease(k) };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// プロセス
// ---------------------------------------------------------------------------

/// pid → (CPU 秒, rss バイト, ps の comm。libproc で読んだものは None)
pub type ProcSnap = HashMap<i32, (f64, u64, Option<String>)>;

/// 上位プロセスの候補（pid, 1 コア換算の %, rss バイト, comm）
pub type Row = (i32, f64, u64, Option<String>);

/// ps の time（[dd-][hh:]mm:ss.cc）を秒に
pub fn cpu_seconds(s: &str) -> Option<f64> {
    let (days, rest) = match s.rsplit_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, s),
    };
    let mut sec = 0.0;
    for part in rest.split(':') {
        sec = sec * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(sec + days * 86400.0)
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// /Applications/Google Chrome.app/.../Google Chrome Helper.app/... を Google Chrome にまとめる（mac_probe.py と同じ）
pub fn app_name(path: &str) -> String {
    match path.find(".app/") {
        Some(i) if i > 0 => basename(&path[..i]).to_string(),
        _ => Some(basename(path)).filter(|b| !b.is_empty()).unwrap_or(path).to_string(),
    }
}

/// 2 回分から pid → (1 コア換算の %, rss バイト, comm)。前回が無い・名前が変わったものは 0
pub fn rates(cur: &ProcSnap, prev: &ProcSnap, dt: f64) -> ProcSnap {
    cur.iter()
        .map(|(pid, (cpu_s, rss, comm))| {
            let pct = match prev.get(pid) {
                Some(p) if dt > 0.0 && &p.2 == comm => ((cpu_s - p.0) / dt * 100.0).max(0.0),
                _ => 0.0,
            };
            (*pid, (pct, *rss, comm.clone()))
        })
        .collect()
}

/// `ps -axo pid=,time=,rss=,comm=` の出力（rss は KiB）
pub fn parse_ps(text: &str) -> ProcSnap {
    let mut out = HashMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(time), Some(rss)) = (it.next(), it.next(), it.next()) else { continue };
        // comm は空白を含みうる（Python の split(None, 3) と同じく残り全部）
        let rest = line.trim_start();
        let comm = [pid, time, rss].iter().fold(rest, |r, f| r[f.len()..].trim_start());
        if comm.is_empty() {
            continue;
        }
        if let (Ok(pid), Some(cpu), Ok(rss)) = (pid.parse::<i32>(), cpu_seconds(time), rss.parse::<u64>()) {
            out.insert(pid, (cpu, rss * 1024, Some(comm.to_string())));
        }
    }
    out
}

struct Procs {
    ns: f64,
    ps_every: Duration,
    procs_every: Duration,
    own_prev: ProcSnap,
    own_t: Option<Instant>,
    ps_prev: ProcSnap,
    ps_t: Option<Instant>,
    next_ps: Option<Instant>,
    others: ProcSnap,
}

impl Procs {
    fn open(a: &Args) -> Procs {
        let mut tb = Timebase::default();
        // SAFETY: tb は書き込み先
        unsafe { mach_timebase_info(&mut tb) };
        // Apple シリコンでは CPU 時間が mach の時間単位
        let ns = if tb.denom > 0 { tb.numer as f64 / tb.denom as f64 } else { 1.0 };
        Procs {
            ns,
            ps_every: Duration::from_secs_f64(3.0 * a.procs_every),
            procs_every: Duration::from_secs_f64(a.procs_every),
            own_prev: HashMap::new(),
            own_t: None,
            ps_prev: HashMap::new(),
            ps_t: None,
            next_ps: None,
            others: HashMap::new(),
        }
    }

    fn own(&self) -> ProcSnap {
        let mut pids = vec![0 as c_int; 16384];
        // SAFETY: pids は大きさ（バイト）を渡した書き込み先
        let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as c_int) };
        let mut out = HashMap::new();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as c_int;
        for &pid in pids.iter().take(n.max(0) as usize) {
            // SAFETY: proc_taskinfo はただの整数の構造体
            let mut ti: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
            // SAFETY: ti は size バイトの書き込み先
            if pid > 0 && unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&mut ti as *mut libc::proc_taskinfo).cast(), size) } == size {
                out.insert(pid, ((ti.pti_total_user + ti.pti_total_system) as f64 * self.ns / 1e9, ti.pti_resident_size, None));
            }
        }
        out
    }

    /// 名前は上位に入ったものだけ引く（全プロセス分は引かない）
    fn comm(pid: i32, comm: &Option<String>) -> String {
        if let Some(c) = comm {
            return c.clone();
        }
        let mut buf = vec![0u8; 4096];
        // SAFETY: buf は 4096 バイトの書き込み先
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if n > 0 { String::from_utf8_lossy(&buf[..n as usize]).into_owned() } else { pid.to_string() }
    }

    fn ps(control: &Control) -> Option<ProcSnap> {
        ps_output(Command::new("/bin/ps").args(["-axo", "pid=,time=,rss=,comm="]), control, Duration::from_secs(10))
            .map(|o| parse_ps(&String::from_utf8_lossy(&o)))
    }

    fn sample(&mut self, now: Instant, control: &Control) -> Value {
        let cur = self.own();
        let mine = match self.own_t {
            Some(t) => rates(&cur, &self.own_prev, now.duration_since(t).as_secs_f64()),
            None => HashMap::new(),
        };
        if self.next_ps.is_none_or(|t| now >= t) {
            let Some(allp) = Self::ps(control) else {
                self.own_prev = cur;
                self.own_t = Some(now);
                self.next_ps = Some(now + self.procs_every);
                return Value::Null;
            };
            if let Some(t) = self.ps_t {
                self.others = rates(&allp, &self.ps_prev, now.duration_since(t).as_secs_f64()).into_iter().filter(|(pid, _)| !cur.contains_key(pid)).collect();
            }
            self.ps_prev = allp;
            self.ps_t = Some(now);
            self.next_ps = Some(now + if self.others.is_empty() { self.procs_every } else { self.ps_every });
        }
        self.own_prev = cur;
        self.own_t = Some(now);
        if mine.is_empty() {
            return Value::Null;
        }
        let mut rows: Vec<Row> = self.others.iter().filter(|(pid, _)| !mine.contains_key(pid)).map(|(p, (c, r, n))| (*p, *c, *r, n.clone())).collect();
        rows.extend(mine.into_iter().map(|(p, (c, r, n))| (p, c, r, n)));
        top_json(&rows, TOP, Self::comm)
    }
}

/// `{"count", "top_cpu", "top_mem"}`。名前は実行ファイル名と .app の名前だけ（パスは出さない）
pub fn top_json(rows: &[Row], n: usize, comm: impl Fn(i32, &Option<String>) -> String) -> Value {
    let row = |r: &&Row| {
        let c = comm(r.0, &r.3);
        json!({"pid": r.0, "name": basename(&c), "app": app_name(&c), "cpu": py_round(r.1, 1), "mem_mb": py_int(r.2 as f64 / MB)})
    };
    let mut by_cpu: Vec<_> = rows.iter().collect();
    by_cpu.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut by_mem: Vec<_> = rows.iter().collect();
    by_mem.sort_by_key(|r| std::cmp::Reverse(r.2));
    json!({
        "count": rows.len(),
        "top_cpu": by_cpu.iter().take(n).map(row).collect::<Vec<_>>(),
        "top_mem": by_mem.iter().take(n).map(row).collect::<Vec<_>>(),
    })
}

/// このサンプラー自身の負荷（呼んだ ps を含む CPU 秒と最大 RSS）。呼び出し側が差分から CPU% を出す
fn self_usage() -> Value {
    let get = |who| {
        // SAFETY: rusage はただの構造体。書き込み先として渡す
        let mut r: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: r は書き込み先
        unsafe { libc::getrusage(who, &mut r) };
        r
    };
    let (me, kids) = (get(libc::RUSAGE_SELF), get(libc::RUSAGE_CHILDREN));
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    let cpu = secs(me.ru_utime) + secs(me.ru_stime) + secs(kids.ru_utime) + secs(kids.ru_stime);
    // macOS の ru_maxrss はバイト
    json!({"cpu_s": py_round(cpu, 3), "rss_mb": py_round(me.ru_maxrss as f64 / MB, 1)})
}

// ---------------------------------------------------------------------------
// 任意のセンサー（macmon）
// ---------------------------------------------------------------------------

fn sensor_number(v: Option<&Value>) -> Option<f64> {
    v.filter(|v| v.is_number()).and_then(Value::as_f64).filter(|x| x.is_finite() && *x >= 0.0)
}

/// macmon の 1 行から power と gpu。数でない・負の値は欠測
pub fn macmon_metrics(data: &Value) -> Option<Map<String, Value>> {
    let d = data.as_object()?;
    let num = |k: &str| sensor_number(d.get(k));
    let n = |v: Option<f64>| v.map_or(Value::Null, |x| json!(x));
    let platform = num("sys_power").filter(|p| *p > 0.0);
    let ratio = num("gpu_scaled_ratio");
    let temp = sensor_number(d.get("temp").and_then(Value::as_object).and_then(|t| t.get("gpu_temp_avg")));
    let mut m = Map::new();
    m.insert(
        "power".into(),
        json!({"package_w": n(num("cpu_power")), "package_source": "macmon IOReport CPU",
               "soc_w": n(num("all_power")), "soc_source": "macmon CPU+GPU+ANE",
               "platform_w": n(platform), "platform_source": "macmon SMC estimate",
               "available": num("all_power").is_some()}),
    );
    m.insert(
        "gpu".into(),
        json!([{"name": "Apple GPU", "power_w": n(num("gpu_power")), "util": n(ratio.map(|r| (r * 100.0).min(100.0))),
                "clocks_graphics_mhz": n(num("gpu_freq_mhz")), "temp_c": n(temp), "source": "macmon IOReport"}]),
    );
    Some(m)
}

fn macmon_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let managed = home.join(".katala-tune/bin/macmon");
    let exec = |p: &PathBuf| std::fs::metadata(p).is_ok_and(|m| m.is_file() && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0);
    if exec(&managed) {
        return managed;
    }
    let found = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join("macmon")).find(exec));
    found.unwrap_or(managed)
}

/// 最新の行と、それを読んだ時刻。読み手のスレッドが書き、サンプラーが読む
type Latest = Arc<Mutex<(Option<Map<String, Value>>, Option<Instant>)>>;

fn read_macmon(out: impl Read, latest: &Latest) {
    // 1 行は 256KiB まで。越えたら捨てて欠測にする
    let mut r = BufReader::new(out);
    let mut line = Vec::new();
    loop {
        line.clear();
        match (&mut r).take(262_145).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line.len() > 262_144 => {
                *latest.lock().unwrap_or_else(|e| e.into_inner()) = (None, None);
                // Drain only the rest of this frame, in bounded chunks. Its tail must
                // never be parsed as a new frame, but the next valid line can recover.
                while line.last() != Some(&b'\n') {
                    line.clear();
                    match (&mut r).take(8192).read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                }
                continue;
            }
            Ok(_) => {}
        }
        let parsed = serde_json::from_slice::<Value>(&line).ok().and_then(|v| macmon_metrics(&v));
        let mut g = latest.lock().unwrap_or_else(|e| e.into_inner());
        *g = match parsed {
            Some(m) => (Some(m), Some(Instant::now())),
            None => (None, g.1),
        };
    }
}

struct Macmon {
    child: Child,
    latest: Latest,
    alive: Arc<AtomicBool>,
    ttl: Duration,
}

impl Macmon {
    fn open(interval: f64) -> Result<Macmon, String> {
        let path = macmon_path();
        if !path.is_file() {
            return Err("macmon unavailable".into());
        }
        let ms = (interval * 1000.0).round_ties_even() as u64;
        let mut child = Command::new(&path)
            .args(["pipe", "--interval", &ms.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("macmon: {e}"))?;
        let out = child.stdout.take().ok_or("macmon: no stdout")?;
        let latest: Latest = Arc::new(Mutex::new((None, None)));
        let alive = Arc::new(AtomicBool::new(true));
        let (l, a) = (latest.clone(), alive.clone());
        std::thread::spawn(move || {
            read_macmon(out, &l);
            a.store(false, Ordering::SeqCst);
        });
        Ok(Macmon { child, latest, alive, ttl: Duration::from_secs_f64((interval * 3.0).max(3.0)) })
    }

    fn sample(&self, now: Instant) -> Option<Map<String, Value>> {
        if !self.alive.load(Ordering::SeqCst) {
            return None;
        }
        let g = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            (Some(m), Some(at)) if now.saturating_duration_since(*at) <= self.ttl => Some(m.clone()),
            _ => None,
        }
    }
}

impl Drop for Macmon {
    fn drop(&mut self) {
        // SAFETY: 自分が起こした子に SIGTERM を送る。0.5 秒で終わらなければ kill する
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// サンプラー
// ---------------------------------------------------------------------------

/// GPU の欄を live_mac.py と同じに埋める（macmon が無ければ欠測の印だけ）
pub fn fill_gpu(s: &mut Map<String, Value>, apple: bool, t: i64) {
    if !s.contains_key("gpu") {
        s.insert("gpu".into(), json!([]));
        s.insert("power".into(), json!({"available": false}));
    }
    let gpus = s.get_mut("gpu").and_then(Value::as_array_mut).expect("gpu は配列");
    if gpus.is_empty() {
        gpus.push(json!({"name": if apple { "Apple GPU" } else { "GPU" }, "source": "sysctl hw.optional.arm64"}));
    }
    for g in gpus.iter_mut().filter_map(Value::as_object_mut) {
        g.insert("adapter_index".into(), json!(0));
        g.insert("memory_kind".into(), json!(if apple { "unified" } else { "unknown" }));
        g.insert("memory_status".into(), json!(if apple { "not-applicable" } else { "unsupported" }));
        g.insert(
            "memory_reason".into(),
            json!(if apple { "unified memory shared with CPU; dedicated VRAM not applicable" } else { "no supported dedicated VRAM provider" }),
        );
        for k in ["mem_used_mb", "mem_total_mb", "mem_free_mb", "mem_used_pct"] {
            g.insert(k.into(), Value::Null);
        }
        g.insert("sampled_at".into(), json!(t));
    }
}

pub struct Sampler {
    a: Args,
    host: u32,
    apple: bool,
    errors: Vec<String>,
    disk: Option<Disk>,
    procs: Option<Procs>,
    monitor: Option<Macmon>,
    cpu_prev: Option<Ticks>,
    net_prev: Option<NetBytes>,
    disk_prev: DiskBytes,
    started_ms: i64,
}

impl Sampler {
    pub fn open(a: &Args, t0: Instant, control: &Control) -> Sampler {
        let mut errors = Vec::new();
        let mut init = |name: &str, r: Result<(), String>| {
            if let Err(e) = r {
                errors.push(format!("{name}: {e}").chars().take(200).collect());
            }
        };
        // SAFETY: 引数なし。1 回だけ取る（呼ぶたびに送信権が増える）
        let host = unsafe { mach_host_self() };
        let mut disk = Disk::open();
        let disk_err = disk.as_ref().err().cloned();
        init("disk", disk_err.map_or(Ok(()), Err));
        let procs = Some(Procs::open(a));
        let monitor = Macmon::open(a.interval);
        let monitor = match monitor {
            Ok(m) => Some(m),
            Err(e) => {
                init("power", Err(e));
                None
            }
        };
        let disk_prev = disk.as_mut().ok().map(|d| d.bytes(t0)).unwrap_or_default();
        let mut s = Sampler {
            a: a.clone(),
            host,
            apple: sysctl::<i32>("hw.optional.arm64") == Some(1),
            errors,
            disk: disk.ok(),
            procs,
            monitor,
            cpu_prev: cpu_ticks(host),
            net_prev: net_bytes(),
            disk_prev,
            started_ms: tune_link::now_ms(),
        };
        if let Some(p) = s.procs.as_mut() {
            p.sample(t0, control);
        }
        s
    }

    pub fn hello(&self) -> Value {
        let has = json!({
            "cpu": self.cpu_prev.is_some(),
            "mem": true,
            "disk": self.disk.is_some(),
            "net": self.net_prev.is_some(),
            "procs": self.procs.is_some(),
            "gpu": self.monitor.is_some(),
        });
        json!({
            "type": "hello",
            "v": 1,
            "os": "macos",
            "cores": sysctl::<i32>("hw.ncpu").map_or(Value::Null, |n| json!(n)),
            "interval": self.a.interval,
            "procs_every": self.a.procs_every,
            "mem_total_gb": sysctl::<u64>("hw.memsize").map_or(Value::Null, |b| py_round(b as f64 / GB, 1)),
            "session_epoch_ms": self.started_ms,
            "has": has,
            "errors": self.errors,
            "impl": "rust",
            "agent": env!("CARGO_PKG_VERSION"),
        })
    }

    /// 1 点。dt は前回からの秒。procs のときだけ上位プロセスと自分の負荷も
    pub fn sample(&mut self, seq: u64, now: Instant, dt: f64, procs: bool, control: &Control) -> Value {
        let t = tune_link::now_ms();
        let mut s = Map::new();
        s.insert("type".into(), json!("s"));
        s.insert("t".into(), json!(t));
        s.insert("seq".into(), json!(seq));
        if let Some(m) = self.monitor.as_ref().and_then(|m| m.sample(now)) {
            s.extend(m);
        }
        // 明示的な欠測。以前の GPU 値を画面に残さない
        fill_gpu(&mut s, self.apple, t);

        let cur = self.cpu_prev.as_ref().and_then(|_| cpu_ticks(self.host));
        let (cpu, cores) = cpu_pct(self.cpu_prev.as_ref(), cur.as_ref());
        s.insert("cpu".into(), cpu);
        s.insert("cores".into(), Value::Array(cores));
        if cur.is_some() {
            self.cpu_prev = cur;
        }
        s.insert("mem".into(), memory());
        if let Some(d) = self.disk.as_mut() {
            let cur = d.bytes(now);
            s.insert("disk".into(), disk_rate(&self.disk_prev, &cur, dt));
            self.disk_prev = if dt.is_finite() && dt > 0.0 { cur } else { HashMap::new() };
        }
        if self.net_prev.is_some() {
            let cur = net_bytes();
            s.insert("net".into(), net_rate(self.net_prev.as_ref(), cur.as_ref(), dt));
            if cur.is_some() {
                self.net_prev = cur;
            }
        }
        if procs && let Some(p) = self.procs.as_mut() {
            s.insert("procs".into(), p.sample(now, control));
            s.insert("self".into(), self_usage());
        }
        Value::Object(s)
    }
}

/// live_mac.py の main と同じ順: hello、以後 interval ごとに s。最初の上位プロセスは 1 点目に付ける
pub fn run(a: &Args) -> Result<(), String> {
    let t0 = Instant::now();
    let control = Control { gone: Arc::new(AtomicBool::new(false)), deadline: t0 + Duration::from_secs_f64(a.max_seconds) };
    if a.watch {
        watch_stdin(control.gone.clone());
    }
    let mut s = Sampler::open(a, t0, &control);
    if control.gone.load(Ordering::SeqCst) {
        return Ok(());
    }
    let out = Output::open();
    // Initialization may have exhausted max while waiting for ps. Preserve hello-first
    // for a writable consumer, with only the same bounded notification grace as end.
    let hello_control = Control { gone: control.gone.clone(), deadline: control.deadline.max(Instant::now() + Duration::from_millis(100)) };
    if !out.emit(s.hello(), &hello_control) {
        return Ok(());
    }
    if control.stopped() {
        out.end_if_expired(&control);
        return Ok(());
    }
    let interval = Duration::from_secs_f64(a.interval);
    let procs_every = Duration::from_secs_f64(a.procs_every);
    let max_age = Duration::from_secs_f64(a.max_seconds);
    let (mut seq, mut last, mut next) = (0u64, t0, t0);
    let mut next_procs = t0 + interval;
    loop {
        next += interval;
        // 標準入力が閉じたらすぐ終わるよう、細かく区切って待つ
        loop {
            if control.gone.load(Ordering::SeqCst) {
                return Ok(());
            }
            let now = Instant::now();
            if now >= next || now >= control.deadline {
                // スリープ明けなどで遅れたら、まとめて出さずに今から数え直す
                if now - next > interval * 2 {
                    next = now;
                }
                break;
            }
            std::thread::sleep((next - now).min(Duration::from_millis(50)));
        }
        let now = Instant::now();
        if now - t0 >= max_age {
            out.end("max_age", &control);
            return Ok(());
        }
        let dt = (now - last).as_secs_f64();
        last = now;
        seq += 1;
        let procs = now >= next_procs;
        if procs {
            next_procs = now + procs_every;
        }
        if !out.emit(s.sample(seq, now, dt, procs, &control), &control) {
            out.end_if_expired(&control);
            return Ok(());
        }
        if a.count > 0 && seq >= a.count {
            out.end("count", &control);
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;

    #[test]
    fn disk_empty_provider_is_unknown_instead_of_idle() {
        let now = Instant::now();
        let mut d = Disk { k_stats: std::ptr::null(), k_read: std::ptr::null(), k_write: std::ptr::null(), services: vec![], found_at: Some(now) };
        assert_eq!(json!(d.bytes(now)), json!({}));
    }

    #[test]
    fn disk_rates_match_missing_recovery_fixtures_and_python() {
        let cases: Value = serde_json::from_str(include_str!("../../../test/fixtures/disk-rates.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let mut prev = HashMap::new();
            for frame in case["frames"].as_array().unwrap() {
                let cur = serde_json::from_value(frame["c"].clone()).unwrap();
                let dt = frame["dt"].as_f64().unwrap_or(1.0);
                let want = json!({"read_bps": frame["want"][0], "write_bps": frame["want"][1]});
                assert_eq!(disk_rate(&prev, &cur, dt), want, "{}", case["name"]);
                let py = python("print(json.dumps(m.disk_rate(I[0],I[1],I[2])))", &json!([prev, cur, dt]));
                assert_eq!(py, want, "Python {}", case["name"]);
                prev = if dt.is_finite() && dt > 0.0 { cur } else { HashMap::new() };
            }
        }
        let counters = HashMap::from([(1, (Some(0), Some(0)))]);
        for dt in [f64::NAN, f64::INFINITY] {
            assert_eq!(disk_rate(&counters, &counters, dt), json!({"read_bps":null,"write_bps":null}));
        }
        let prev = HashMap::from([(1, (Some(0), Some(0))), (2, (Some(0), Some(0)))]);
        let cur = HashMap::from([(1, (Some(i64::MAX), Some(1))), (2, (Some(i64::MAX), Some(1)))]);
        assert_eq!(disk_rate(&prev, &cur, 1.0), json!({"read_bps":null,"write_bps":2}));
    }

    fn fixture_marker(path: &std::path::Path, value: &[u8]) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, value).unwrap();
        std::fs::rename(tmp, path).unwrap();
    }

    struct FixtureStdout<W> {
        inner: W,
        started: PathBuf,
    }

    impl<W: Write> Write for FixtureStdout<W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.started.exists() {
                fixture_marker(&self.started, b"writing");
            }
            self.inner.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    /// Fill only this subprocess's piped stdout, ending at an actual one-byte
    /// WouldBlock. Restore its flags before exercising the production writer.
    fn fill_fixture_stdout(deadline: Instant) -> usize {
        let fd = std::io::stdout().as_raw_fd();
        // SAFETY: the fixture alone owns this pipe write end; no caller shares it.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) }, 0);
        let filled = (|| -> std::io::Result<usize> {
            let chunk = [b'x'; 8192];
            let mut total = 0;
            for width in [chunk.len(), 1] {
                loop {
                    if Instant::now() >= deadline || total >= 16 * 1024 * 1024 {
                        return Err(std::io::Error::other("fixture stdout did not fill within setup budget"));
                    }
                    // SAFETY: chunk is readable for width bytes and fd is this fixture's pipe.
                    let written = unsafe { libc::write(fd, chunk.as_ptr().cast(), width) };
                    if written > 0 {
                        total += written as usize;
                    } else {
                        let error = std::io::Error::last_os_error();
                        match error.kind() {
                            std::io::ErrorKind::WouldBlock => break,
                            std::io::ErrorKind::Interrupted => continue,
                            _ => return Err(error),
                        }
                    }
                }
            }
            Ok(total)
        })();
        // SAFETY: restore the exact original flags even when prefill failed.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }, 0);
        filled.unwrap()
    }

    #[test]
    #[ignore = "owned subprocess invoked by expired startup test"]
    fn expired_startup_fixture() {
        let a = Args { interval: 0.2, procs_every: 1.0, max_seconds: 0.0, count: 0, watch: false };
        run(&a).unwrap();
        std::process::exit(0);
    }

    #[test]
    fn startup_deadline_preserves_hello_before_end() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "sample_mac::tests::expired_startup_fixture", "--ignored", "--nocapture"])
            .env("HOME", std::env::temp_dir().join(format!("sampler-expired-{}", std::process::id())))
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success());
        let rows: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0]["type"], "hello");
        assert_eq!(rows[1], json!({"type": "end", "reason": "max_age"}));
    }

    #[test]
    fn macmon_recovers_after_oversized_frames() {
        for size in [262_144, 262_145, 524_288] {
            let latest: Latest = Arc::new(Mutex::new((None, None)));
            let mut input = vec![b'x'; size];
            input.extend_from_slice(b"\n{\"all_power\":2.5}\n");
            read_macmon(std::io::Cursor::new(input), &latest);
            let g = latest.lock().unwrap();
            assert_eq!(g.0.as_ref().map(|m| m["power"]["soc_w"].clone()), Some(json!(2.5)), "oversized frame length {size}");
        }
    }

    #[test]
    fn macmon_oversized_frame_clears_previous_metrics_even_at_eof() {
        let latest: Latest = Arc::new(Mutex::new((macmon_metrics(&json!({"all_power": 1})), Some(Instant::now()))));
        read_macmon(std::io::Cursor::new(vec![b'x'; 524_288]), &latest);
        assert!(latest.lock().unwrap().0.is_none());
    }

    /// Subprocess entry: a real sampler owns real fixture macmon while stdout is full.
    #[test]
    #[ignore = "owned subprocess invoked by stdout cancellation tests"]
    fn blocked_stdout_fixture() {
        let dir = PathBuf::from(std::env::var_os("PR87_FIXTURE_DIR").unwrap());
        let a = Args { interval: 0.2, procs_every: 1.0, max_seconds: std::env::var("PR87_FIXTURE_MAX").unwrap().parse().unwrap(), count: 0, watch: true };
        let t0 = Instant::now();
        // Setup has its own finite budget. This is the blocked-writer test, not
        // the separate run/startup max regression: never spend its 2s on setup.
        let setup = Control { gone: Arc::new(AtomicBool::new(false)), deadline: t0 + Duration::from_secs(15) };
        watch_stdin(setup.gone.clone());
        let setup_ms = std::env::var("PR87_FIXTURE_SETUP_MS").unwrap_or_else(|_| "0".into()).parse::<u64>().unwrap();
        std::thread::sleep(Duration::from_millis(setup_ms));
        let sampler = Sampler::open(&a, t0, &setup);
        assert!(sampler.monitor.is_some());
        let filled = fill_fixture_stdout(setup.deadline);
        let paths = dir.clone();
        let out = Output::with_writer(move || {
            let inner = std::io::stdout().lock();
            fixture_marker(&paths.join("worker-ready"), b"ready");
            FixtureStdout { inner, started: paths.join("write-started") }
        });
        fixture_marker(&dir.join("ready"), filled.to_string().as_bytes());
        while !dir.join("arm").exists() {
            assert!(!setup.stopped(), "fixture setup deadline elapsed before measurement arm");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Set this deadline once, after the parent confirmed the full pipe and
        // live owned sensor. Never restart it while waiting for writer/cleanup.
        let measured = Instant::now();
        let control = Control { gone: setup.gone.clone(), deadline: measured + Duration::from_secs_f64(a.max_seconds) };
        assert!(!out.emit(json!({"payload": "x".repeat(1024 * 1024)}), &control));
        assert!(control.stopped(), "an output error is not evidence of cancellation or max expiry");
        drop(sampler);
        if !control.gone.load(Ordering::SeqCst) {
            assert!(
                measured.elapsed() < Duration::from_secs_f64(a.max_seconds) + Duration::from_millis(1500),
                "max cleanup exceeded its original measurement budget"
            );
        }
        fixture_marker(&dir.join("cleanup-done"), b"done");
        // libtest would print its result to the deliberately full stdout pipe.
        std::process::exit(0);
    }

    struct OutputFixture {
        child: Child,
        dir: PathBuf,
        exited: bool,
    }

    impl Drop for OutputFixture {
        fn drop(&mut self) {
            if !self.exited {
                // SAFETY: this unreaped child is the leader of a new group made only for
                // this test. Kill that owned group before wait, including on RED/panic.
                unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
                let _ = self.child.wait();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl OutputFixture {
        fn open(max: u64, name: &str) -> OutputFixture {
            Self::with_setup_delay(max, name, 0)
        }

        fn with_setup_delay(max: u64, name: &str, setup_ms: u64) -> OutputFixture {
            let dir = std::env::temp_dir().join(format!("sampler-output-{}-{name}-{}", std::process::id(), tune_link::now_ms()));
            let bin = dir.join(".katala-tune/bin");
            std::fs::create_dir_all(&bin).unwrap();
            let script = bin.join("macmon");
            std::fs::write(&script, "#!/usr/bin/python3\nimport os,signal,time\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\np=os.path.join(os.environ['HOME'],'macmon-pid')\nwith open(p+'.tmp','w') as f: f.write(str(os.getpid()))\nos.replace(p+'.tmp',p)\nuntil=time.monotonic()+30\nwhile time.monotonic()<until: time.sleep(0.05)\n").unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "sample_mac::tests::blocked_stdout_fixture", "--ignored", "--nocapture"])
                .env("HOME", &dir)
                .env("PR87_FIXTURE_DIR", &dir)
                .env("PR87_FIXTURE_MAX", max.to_string())
                .env("PR87_FIXTURE_SETUP_MS", setup_ms.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap();
            let fixture = OutputFixture { child, dir, exited: false };
            let until = Instant::now() + Duration::from_secs(15);
            loop {
                let mut queued = 0;
                // SAFETY: stdout read end stays owned by fixture. FIONREAD only reads its byte count.
                let r = unsafe { libc::ioctl(fixture.child.stdout.as_ref().unwrap().as_raw_fd(), libc::FIONREAD, &mut queued) };
                assert_eq!(r, 0);
                let filled = std::fs::read_to_string(fixture.dir.join("ready")).ok().and_then(|s| s.parse::<usize>().ok());
                let pid = std::fs::read_to_string(fixture.dir.join("macmon-pid")).ok().and_then(|s| s.parse::<u32>().ok());
                if let Some(pid) = pid
                    && filled.is_some_and(|n| n > 0 && queued as usize >= n)
                    && fixture.dir.join("worker-ready").exists()
                {
                    let pid = pid.to_string();
                    assert!(
                        Command::new("/bin/kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success(),
                        "owned sensor must be alive before measurement"
                    );
                    break;
                }
                assert!(Instant::now() < until, "fixture did not prepare a full stdout pipe: queued={queued} filled={filled:?} pid={pid:?}");
                std::thread::sleep(Duration::from_millis(10));
            }
            fixture_marker(&fixture.dir.join("arm"), b"measure");
            while !fixture.dir.join("write-started").exists() {
                assert!(Instant::now() < until, "production writer did not start against the prepared full pipe");
                std::thread::sleep(Duration::from_millis(10));
            }
            fixture
        }

        fn wait_and_check_cleanup(&mut self, timeout: Duration) {
            let until = Instant::now() + timeout;
            while !self.dir.join("cleanup-done").exists() {
                assert!(Instant::now() < until, "sampler stayed blocked on stdout after cancellation/deadline");
                std::thread::sleep(Duration::from_millis(10));
            }
            // Check the owned sensor before reaping the root, so failure still
            // leaves an unreaped process-group anchor for Drop's exact cleanup.
            let pid = std::fs::read_to_string(self.dir.join("macmon-pid")).unwrap();
            assert!(!Command::new("/bin/kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success(), "owned macmon was not reaped");
            let status = loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    self.exited = true;
                    break status;
                }
                assert!(Instant::now() < until, "sampler stayed blocked on stdout after cancellation/deadline");
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(status.success(), "fixture failed: {status}");
        }
    }

    #[test]
    fn stdout_backpressure_does_not_block_stdin_cleanup() {
        let mut fixture = OutputFixture::open(30, "stdin");
        drop(fixture.child.stdin.take());
        fixture.wait_and_check_cleanup(Duration::from_secs(2));
    }

    #[test]
    fn stdout_backpressure_does_not_block_max_cleanup() {
        let mut fixture = OutputFixture::open(2, "max");
        fixture.wait_and_check_cleanup(Duration::from_millis(3500));
    }

    #[test]
    fn stdout_backpressure_max_survives_slow_setup() {
        let mut fixture = OutputFixture::with_setup_delay(2, "slow-max", 2500);
        fixture.wait_and_check_cleanup(Duration::from_millis(3500));
    }

    // Reverting the ps deadline/cancellation must leave this owned child alive long enough to fail.
    fn sleeping_ps() -> Command {
        let mut c = Command::new("/usr/bin/python3");
        c.args(["-I", "-c", "import time; print('1 0:00.00 1 fixture', flush=True); time.sleep(1)"]);
        c
    }

    #[test]
    fn ps_deadline_interrupts_output_wait() {
        let control = Control { gone: Arc::new(AtomicBool::new(false)), deadline: Instant::now() + Duration::from_millis(100) };
        let started = Instant::now();
        assert!(ps_output(&mut sleeping_ps(), &control, Duration::from_secs(10)).is_none());
        assert!(started.elapsed() < Duration::from_millis(700));
    }

    #[test]
    fn ps_stdin_cancellation_interrupts_output_wait() {
        let control = Control { gone: Arc::new(AtomicBool::new(false)), deadline: Instant::now() + Duration::from_secs(10) };
        let gone = control.gone.clone();
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            gone.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let out = ps_output(&mut sleeping_ps(), &control, Duration::from_secs(10));
        cancel.join().unwrap();
        assert!(out.is_none());
        assert!(started.elapsed() < Duration::from_millis(700));
    }

    #[test]
    fn ps_local_timeout_preserves_session() {
        let control = Control { gone: Arc::new(AtomicBool::new(false)), deadline: Instant::now() + Duration::from_secs(10) };
        assert!(ps_output(&mut sleeping_ps(), &control, Duration::from_millis(100)).is_none());
        assert!(!control.stopped());
        let mut fast = Command::new("/usr/bin/printf");
        fast.arg("1 0:00.25 4 fixture\n");
        assert_eq!(ps_output(&mut fast, &control, Duration::from_secs(1)).unwrap(), b"1 0:00.25 4 fixture\n");
    }

    #[test]
    fn ps_eof_does_not_bypass_child_deadline() {
        let control = Control { gone: Arc::new(AtomicBool::new(false)), deadline: Instant::now() + Duration::from_secs(10) };
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-I", "-c", "import os,time; os.close(1); time.sleep(1)"]);
        let started = Instant::now();
        assert!(ps_output(&mut command, &control, Duration::from_millis(100)).is_none());
        assert!(started.elapsed() < Duration::from_millis(700));
    }

    #[test]
    fn ps_cancel_reaps_a_running_owned_child() {
        let dir = std::env::temp_dir().join(format!("sampler-ps-{}-{}", std::process::id(), tune_link::now_ms()));
        std::fs::create_dir(&dir).unwrap();
        let marker = dir.join("pid");
        let control = Control { gone: Arc::new(AtomicBool::new(false)), deadline: Instant::now() + Duration::from_secs(5) };
        let gone = control.gone.clone();
        let path = marker.clone();
        let cancel = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(4);
            while !path.exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            let pid = std::fs::read_to_string(path).unwrap();
            gone.store(true, Ordering::SeqCst);
            pid
        });
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-I", "-c", "import os,sys,time; open(sys.argv[1],'w').write(str(os.getpid())); time.sleep(1)"]).arg(&marker);
        let out = ps_output(&mut command, &control, Duration::from_secs(5));
        let pid = cancel.join().unwrap();
        let alive = Command::new("/bin/kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(out.is_none());
        assert!(!alive, "ps fixture must be reaped before cancellation returns");
    }

    /// live_mac.py を import して（main は動かない）、同じ入力の結果を JSON で返す
    fn python(code: &str, input: &Value) -> Value {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../probes/live_mac.py");
        let boot = format!(
            "import importlib.util,json,sys\nspec=importlib.util.spec_from_file_location('live_mac',{script:?})\nm=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)\nI=json.loads(sys.stdin.read())\n{code}"
        );
        let mut c = Command::new("/usr/bin/python3").args(["-I", "-c", &boot]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        std::io::Write::write_all(c.stdin.as_mut().unwrap(), input.to_string().as_bytes()).unwrap();
        drop(c.stdin.take());
        let o = c.wait_with_output().unwrap();
        assert!(o.status.success(), "python が失敗した");
        serde_json::from_slice(&o.stdout).unwrap()
    }

    #[test]
    fn rounding_matches_python() {
        let xs = [0.125, 0.375, 2.5, 0.25, 0.35, 2.675, 1.0005, 12.25, 0.05, 99.95, 33.333_333, 1e-9, 1234567.891];
        let py = python("print(json.dumps([[round(x,n) for n in (1,2,3)]+[round(x)] for x in I]))", &json!(xs));
        let rust: Vec<Value> = xs.iter().map(|x| json!([py_round(*x, 1), py_round(*x, 2), py_round(*x, 3), py_int(*x)])).collect();
        assert_eq!(json!(rust), py);
    }

    #[test]
    fn cpu_and_network_rates_match_python() {
        let w = u32::MAX;
        let prev = vec![[100, 50, 1000, 0], [w - 10, 0, w - 5, 3], [0, 0, 0, 0]];
        let cur = vec![[180, 70, 1900, 5], [20, 7, 400, 3], [0, 0, 0, 0]];
        let py = python("print(json.dumps(m.cpu_pct(I[0],I[1])))", &json!([prev, cur]));
        let (cpu, cores) = cpu_pct(Some(&prev), Some(&cur));
        assert_eq!(json!([cpu, cores]), py);
        assert_eq!(cpu_pct(Some(&prev), Some(&cur[..2].to_vec())), (Value::Null, vec![]));

        let a: NetBytes = [("en0".into(), (w - 100, 10)), ("en1".into(), (5, 5))].into();
        let b: NetBytes = [("en0".into(), (900, 2010)), ("en1".into(), (5, 5)), ("en9".into(), (7777, 1))].into();
        let to_py = |n: &NetBytes| json!(n.iter().map(|(k, v)| (k.clone(), json!([v.0, v.1]))).collect::<Map<_, _>>());
        for dt in [0.7, 1.5, 0.0] {
            let py = python(
                "print(json.dumps(m.net_rate({k:tuple(v) for k,v in I[0].items()},{k:tuple(v) for k,v in I[1].items()},I[2])))",
                &json!([to_py(&a), to_py(&b), dt]),
            );
            assert_eq!(net_rate(Some(&a), Some(&b), dt), py, "dt={dt}");
        }
    }

    #[test]
    fn process_helpers_match_python() {
        let paths = [
            "/Applications/Google Chrome.app/Contents/Frameworks/x/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper",
            "/usr/sbin/cfprefsd",
            "launchd",
            "/",
            ".app/x",
            "/a/B.app/",
        ];
        let times = ["0:00.01", "12:34.56", "1:02:03.04", "3-04:05:06.07"];
        let py = python("print(json.dumps([[m.app_name(p) for p in I[0]],[m.cpu_seconds(t) for t in I[1]]]))", &json!([paths, times]));
        let rust = json!([paths.iter().map(|p| app_name(p)).collect::<Vec<_>>(), times.iter().map(|t| cpu_seconds(t).unwrap()).collect::<Vec<_>>()]);
        assert_eq!(rust, py);

        let prev: ProcSnap = [(1, (10.0, 100, Some("a".into()))), (2, (5.0, 200, Some("b".into()))), (3, (1.0, 300, None))].into();
        let cur: ProcSnap = [(1, (10.5, 110, Some("a".into()))), (2, (6.0, 220, Some("c".into()))), (3, (0.5, 300, None)), (4, (9.0, 1, None))].into();
        let enc = |s: &ProcSnap| json!(s.iter().map(|(k, v)| (k.to_string(), json!([v.0, v.1, v.2]))).collect::<Map<_, _>>());
        let py = python(
            "c={int(k):tuple(v) for k,v in I[0].items()};p={int(k):tuple(v) for k,v in I[1].items()}\nprint(json.dumps({str(k):v for k,v in m.rates(c,p,I[2]).items()}))",
            &json!([enc(&cur), enc(&prev), 0.5]),
        );
        assert_eq!(enc(&rates(&cur, &prev, 0.5)), py);
    }

    #[test]
    fn macmon_lines_match_python() {
        let lines = [
            json!({"cpu_power": 1.5, "gpu_power": 3, "all_power": 4.25, "sys_power": 0, "gpu_freq_mhz": 500, "gpu_scaled_ratio": 0.42, "temp": {"gpu_temp_avg": 41.5}}),
            json!({"cpu_power": -1, "gpu_power": "x", "all_power": null, "sys_power": 12.0, "gpu_scaled_ratio": 1.7, "temp": 3}),
            json!({}),
            json!([1, 2]),
        ];
        let py = python("print(json.dumps([m.macmon_metrics(x) for x in I]))", &json!(lines));
        for (i, l) in lines.iter().enumerate() {
            let rust = macmon_metrics(l).map_or(json!({}), Value::Object);
            // 数の型（3 と 3.0）は JSON の読み手にとって同じ。値で比べる
            assert_eq!(normalize(&rust), normalize(&py[i]), "{l}");
        }
    }

    fn normalize(v: &Value) -> Value {
        match v {
            Value::Number(n) => json!(n.as_f64()),
            Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), normalize(v))).collect()),
            _ => v.clone(),
        }
    }

    #[test]
    fn ps_output_keeps_names_with_spaces() {
        let s = parse_ps(
            "    1   1:02.50   12345 /sbin/launchd\n  812   0:00.01     64 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\nbad line\n 9 x 1 y\n",
        );
        assert_eq!(s.len(), 2);
        assert_eq!(s[&1], (62.5, 12345 * 1024, Some("/sbin/launchd".into())));
        assert_eq!(s[&812].2.as_deref(), Some("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
    }

    #[test]
    fn gpu_is_filled_like_the_script() {
        let mut s = Map::new();
        fill_gpu(&mut s, true, 5);
        assert_eq!(s["power"], json!({"available": false}));
        assert_eq!(s["gpu"][0]["name"], "Apple GPU");
        assert_eq!(s["gpu"][0]["memory_kind"], "unified");
        assert_eq!(s["gpu"][0]["sampled_at"], 5);
        let mut s = Map::new();
        s.extend(macmon_metrics(&json!({"all_power": 1})).unwrap());
        fill_gpu(&mut s, false, 7);
        assert_eq!(s["power"]["available"], true);
        assert_eq!(s["gpu"][0]["source"], "macmon IOReport");
        assert_eq!(s["gpu"][0]["memory_status"], "unsupported");
    }

    #[test]
    fn sampler_reads_this_machine() {
        let a = Args { interval: 0.3, procs_every: 1.0, max_seconds: 10.0, count: 0, watch: false };
        let t0 = Instant::now();
        let control = Control { gone: Arc::new(AtomicBool::new(false)), deadline: t0 + Duration::from_secs(10) };
        let mut s = Sampler::open(&a, t0, &control);
        std::thread::sleep(Duration::from_millis(300));
        let v = s.sample(1, Instant::now(), 0.3, true, &control);
        let h = s.hello();
        assert_eq!((h["os"].as_str(), h["impl"].as_str()), (Some("macos"), Some("rust")));
        assert!(h["cores"].as_u64().unwrap() > 0);
        assert_eq!(v["cores"].as_array().unwrap().len() as u64, h["cores"].as_u64().unwrap(), "{v}");
        assert!(v["cpu"].as_f64().is_some_and(|x| (0.0..=100.0).contains(&x)), "{v}");
        assert!(v["mem"]["used_pct"].as_f64().is_some_and(|x| x > 0.0 && x < 100.0), "{v}");
        assert!(v["mem"]["pressure"].is_string(), "{v}");
        assert!(v["procs"]["count"].as_u64().unwrap() > 10, "{v}");
        assert!(v["procs"]["top_mem"][0]["name"].as_str().is_some_and(|n| !n.contains('/')), "{v}");
        assert!(v["self"]["rss_mb"].as_f64().is_some());
        assert!(v["gpu"][0]["memory_kind"].is_string());
    }
}
