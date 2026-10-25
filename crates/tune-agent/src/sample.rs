//! `tune-agent sample`: ライブ表示のサンプラー（probes/live_win.ps1 と同じ行・同じ値）。
//!
//!   tune-agent sample [interval=1] [procs=5] [max=900] [count=0] [watch=1]
//!
//! 1 行目に hello、以後 1 行 1 JSON（`{"type":"s",…}`）を標準出力へ。読み取り専用で、機体に何も書かない。
//! 止まるのは: 標準入力が閉じたとき（watch=1）・自分の上にいる sshd が終わったとき（watch=1、Windows）・
//! 標準出力に書けなくなったとき・`max` 秒（`{"type":"end","reason":"max_age"}`）・`count` 回（`reason":"count"`）。
//! 契約は tune-core の `live::parse_line`。hello に `impl: "rust"` と版を足す（読み手は知らない欄を捨てる）。
//!
//! PowerShell 版は 1 回ごとに .NET の性能カウンタを読み、起動と常駐に CPU とメモリを使う（実機で 1 コア比 4〜7%・
//! 120〜150MB）。ここでは同じカウンタを PDH で直接読み、プロセスは NtQuerySystemInformation 1 回で全部読む。
//! macOS は live_mac.py と同じ値を sysctl・mach・libproc・IOKit で直接読む（sample_mac.rs）。Python の起動と
//! ctypes の呼び出しが要らなくなる。アプリが macOS でこれを使うのは、各機体へ置く手順ができてから（それまでは live_mac.py）。
// 下の部品（行の組み立て・GPU・プロセス）は Windows の取得部分（sample_win.rs）だけが使う。テストはどの OS でも動かす
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub interval: f64,
    pub procs_every: f64,
    pub max_seconds: f64,
    pub count: u64,
    pub watch: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args { interval: 1.0, procs_every: 5.0, max_seconds: 900.0, count: 0, watch: false }
    }
}

/// `interval=1 procs=5 max=900 count=0 watch=1`（live_mac.py と同じ書き方）。範囲は PowerShell 版と同じに収める
pub fn parse_args(a: &[String]) -> Result<Args, String> {
    let mut o = Args::default();
    for s in a {
        let (k, v) = s.split_once('=').ok_or_else(|| format!("引数は key=value: {s}"))?;
        let num = || v.parse::<f64>().ok().filter(|x| x.is_finite()).ok_or_else(|| format!("{k} の値が数でない: {v}"));
        match k {
            "interval" => o.interval = num()?.clamp(0.2, 10.0),
            // Duration に直しても溢れない範囲に収める（1 時間・1 日まで）
            "procs" => o.procs_every = num()?.clamp(1.0, 3600.0),
            "max" => o.max_seconds = num()?.clamp(1.0, 86_400.0),
            "count" => o.count = num()?.max(0.0) as u64,
            "watch" => o.watch = v == "1",
            _ => return Err(format!("知らない引数: {k}")),
        }
    }
    Ok(o)
}

/// 小数 d 桁に丸める（JSON の数。有限でなければ null）
pub fn round(x: f64, d: i32) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    let m = 10f64.powi(d);
    json!((x * m).round() / m)
}

/// 1 行書いて flush。書けなければ false（読み手が居ない）
pub(crate) fn emit(out: &mut impl Write, v: &Value) -> bool {
    let mut s = v.to_string();
    s.push('\n');
    out.write_all(s.as_bytes()).and_then(|_| out.flush()).is_ok()
}

/// 標準入力を読み捨て、閉じたら印を立てる（ローカル実行ではアプリが標準入力を開いたまま持つ。
/// SSH 経由では予備のスクリプトのバイト列も届くので、それも読み捨てる）
pub(crate) fn watch_stdin(gone: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut si = std::io::stdin().lock();
        while matches!(si.read(&mut buf), Ok(n) if n > 0) {}
        gone.store(true, Ordering::SeqCst);
    });
}

pub fn main(rest: &[String]) -> std::process::ExitCode {
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::from(2);
        }
    };
    match run(&a) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn run(a: &Args) -> Result<(), String> {
    use crate::sample_win::Sampler;
    let mut out = std::io::stdout().lock();
    let started = Instant::now();
    let mut s = Sampler::open(a)?;
    let stdin_gone = Arc::new(AtomicBool::new(false));
    if a.watch {
        watch_stdin(stdin_gone.clone());
        s.watch_session();
    }
    // 最初の 1 点は 250ms の窓で出す（率のカウンタは 2 回読まないと値にならない）。以後は interval ごと
    std::thread::sleep(Duration::from_millis(250));
    let first = s.sample(true);
    if !emit(&mut out, &s.hello()) {
        return Ok(());
    }
    let interval = Duration::from_secs_f64(a.interval);
    let procs_every = Duration::from_secs_f64(a.procs_every);
    let max_age = Duration::from_secs_f64(a.max_seconds);
    let mut seq = 1u64;
    if !emit(&mut out, &with_seq(first, seq)) {
        return Ok(());
    }
    let mut next = Instant::now();
    let mut next_procs = Instant::now() + procs_every;
    loop {
        if a.count > 0 && seq >= a.count {
            emit(&mut out, &json!({"type": "end", "reason": "count"}));
            return Ok(());
        }
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > interval * 2 {
            // 眠っていた・止められていた: 追いつこうとせず、ここから数え直す
            next = now;
        }
        if stdin_gone.load(Ordering::SeqCst) || s.session_gone() {
            return Ok(());
        }
        if started.elapsed() >= max_age {
            emit(&mut out, &json!({"type": "end", "reason": "max_age"}));
            return Ok(());
        }
        let procs = Instant::now() >= next_procs;
        if procs {
            next_procs = Instant::now() + procs_every;
        }
        seq += 1;
        if !emit(&mut out, &with_seq(s.sample(procs), seq)) {
            return Ok(());
        }
    }
}

#[cfg(target_os = "macos")]
fn run(a: &Args) -> Result<(), String> {
    crate::sample_mac::run(a)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run(_a: &Args) -> Result<(), String> {
    Err("この OS のネイティブのサンプラーはまだ無い".into())
}

/// s 行に種類・時刻・番号を付ける（時刻は書き出す直前）
fn with_seq(mut v: Value, seq: u64) -> Value {
    if let Some(m) = v.as_object_mut() {
        m.insert("type".into(), json!("s"));
        m.insert("t".into(), json!(tune_link::now_ms()));
        m.insert("seq".into(), json!(seq));
    }
    v
}

/// 物理でないネットワーク（同じ通信を二重に数える）。live_win.ps1 の $NET_SKIP と同じ（大文字小文字は区別しない）
pub fn virtual_adapter(name: &str) -> bool {
    const SKIP: [&str; 18] = [
        "loopback",
        "isatap",
        "teredo",
        "6to4",
        "virtual",
        "hyper-v",
        "vethernet",
        "tailscale",
        "wireguard",
        "wintun",
        "vpn",
        "tap-",
        "npcap",
        "wan miniport",
        "bluetooth",
        "vmware",
        "virtualbox",
        "kernel debug",
    ];
    let n = name.to_lowercase();
    SKIP.iter().any(|s| n.contains(s))
}

/// 上位プロセスの 1 行（名前は実行ファイル名から .exe を除いたもの。PowerShell 版の性能カウンタの名前と同じ）
#[derive(Clone, Debug, PartialEq)]
pub struct ProcRow {
    pub pid: u32,
    pub name: String,
    /// 1 コア換算の %（前回が無いときは None）
    pub cpu: Option<f64>,
    pub mem_bytes: u64,
}

pub fn display_name(image: &str) -> String {
    let n = image.rsplit(['\\', '/']).next().unwrap_or(image);
    match n.len().checked_sub(4) {
        Some(i) if n.is_char_boundary(i) && n[i..].eq_ignore_ascii_case(".exe") => n[..i].to_string(),
        _ => n.to_string(),
    }
}

/// `{"count", "top_cpu", "top_mem"}`（上位 n 件ずつ）
pub fn procs_json(rows: &[ProcRow], n: usize) -> Value {
    let row = |r: &ProcRow| {
        json!({
            "pid": r.pid,
            "name": r.name,
            "cpu": r.cpu.map_or(Value::Null, |c| round(c, 1)),
            "mem_mb": (r.mem_bytes as f64 / 1_048_576.0).round(),
        })
    };
    let mut by_cpu: Vec<&ProcRow> = rows.iter().collect();
    by_cpu.sort_by(|a, b| b.cpu.unwrap_or(-1.0).total_cmp(&a.cpu.unwrap_or(-1.0)));
    let mut by_mem: Vec<&ProcRow> = rows.iter().collect();
    by_mem.sort_by_key(|r| std::cmp::Reverse(r.mem_bytes));
    json!({
        "count": rows.len(),
        "top_cpu": by_cpu.iter().take(n).map(|r| row(r)).collect::<Vec<_>>(),
        "top_mem": by_mem.iter().take(n).map(|r| row(r)).collect::<Vec<_>>(),
    })
}

/// NVIDIA query parser shared by the native sampler and cross-platform tests.
pub fn parse_gpu_query(text: &str, sampled_at: i64) -> Vec<Value> {
    text.lines().take(16).filter_map(|line| {
        let mut f: Vec<&str> = line.split(',').map(str::trim).collect();
        if f.len() == 5 { f = vec![f[0],f[1],"N/A",f[2],f[3],f[4],"N/A","N/A","N/A","N/A","N/A","N/A","N/A","N/A","N/A","N/A","N/A","N/A"]; }
        if f.len() != 18 { return None; }
        let index = f[0].parse::<u32>().ok()?;
        let num = |i: usize| f[i].parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0);
        let total = num(4).filter(|v| *v > 0.0);
        let used = num(3).filter(|v| total.is_none_or(|t| *v <= t));
        let free = num(5).filter(|v| total.is_none_or(|t| *v <= t));
        let pct = used.zip(total).map(|(u,t)| u/t*100.0);
        let available = pct.is_some() && free.is_some();
        let n = |v: Option<f64>, d| v.map_or(Value::Null, |x| round(x,d));
        let string = |i: usize| (!matches!(f[i],"N/A"|"[N/A]"|"Not Supported"|"")).then(|| f[i].chars().take(120).collect::<String>());
        Some(json!({"adapter_index":index,"name":string(1),"util":n(num(2).filter(|v|*v<=100.0),1),
            "mem_used_mb":n(used,0),"mem_total_mb":n(total,0),"mem_free_mb":n(free,0),"mem_used_pct":n(pct,1),
            "temp_c":n(num(6),1),"power_w":n(num(7),1),"clocks_graphics_mhz":n(num(8),0),"clocks_memory_mhz":n(num(9),0),
            "uuid":string(10),"pstate":string(11),"power_limit_w":n(num(12),1),"power_min_w":n(num(13),1),"power_max_w":n(num(14),1),"clocks_sm_mhz":n(num(15),0),"driver":string(16),
            "source":"nvidia-smi query (MiB)","sampled_at":sampled_at,"memory_kind":"dedicated","memory_status":if available {"available"}else{"partial"},
            "memory_reason":if available {None}else{Some("driver returned unavailable or invalid memory fields")},"available":true}))
    }).collect()
}

type GpuRead = std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, &'static str>> + Send>>;

/// One owned child, at most every five seconds; no detached readers or resident dmon.
pub struct GpuQuery {
    smi: std::path::PathBuf,
    every: Duration,
    runtime: Option<tokio::runtime::Runtime>,
    child: Option<tokio::process::Child>,
    output: Option<GpuRead>,
    read: Option<Result<String, &'static str>>,
    started: Option<Instant>,
    sampled: Option<Instant>,
    fallback: bool,
    rows: Vec<Value>,
    reason: &'static str,
}
impl GpuQuery {
    pub fn open(smi: std::path::PathBuf, every: f64) -> Self {
        Self {
            smi,
            every: Duration::from_secs_f64(every.max(5.0)),
            runtime: tokio::runtime::Builder::new_current_thread().enable_all().build().ok(),
            child: None,
            output: None,
            read: None,
            started: None,
            sampled: None,
            fallback: false,
            rows: vec![],
            reason: "query-pending",
        }
    }
    fn stop(&mut self) {
        // Dropping the async read closes its pipe, including when a grandchild holds the other end.
        self.output = None;
        self.read = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Some(rt) = &self.runtime {
                let _ = rt.block_on(async { tokio::time::timeout(Duration::from_millis(200), child.wait()).await });
            }
        }
    }
    fn start(&mut self, memory_only: bool) {
        let Some(rt) = &self.runtime else {
            self.reason = "query-runtime-unavailable";
            return;
        };
        let fields = if memory_only {
            "index,name,memory.used,memory.total,memory.free"
        } else {
            "index,name,utilization.gpu,memory.used,memory.total,memory.free,temperature.gpu,power.draw,clocks.current.graphics,clocks.current.memory,uuid,pstate,power.limit,power.min_limit,power.max_limit,clocks.current.sm,driver_version,clocks_throttle_reasons.active"
        };
        let mut command = tokio::process::Command::new(&self.smi);
        command
            .args([format!("--query-gpu={fields}"), "--format=csv,noheader,nounits".into()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            command.creation_flags(0x0800_0000);
        }
        self.started = Some(Instant::now());
        self.fallback = memory_only;
        let _guard = rt.enter();
        match command.spawn() {
            Ok(mut child) => {
                let out = child.stdout.take();
                self.output = Some(Box::pin(async move {
                    use tokio::io::AsyncReadExt;
                    let out = out.ok_or("output-unavailable")?;
                    let mut buf = Vec::new();
                    out.take(65537).read_to_end(&mut buf).await.map_err(|_| "output-unavailable")?;
                    if buf.len() > 65536 {
                        return Err("output-limit");
                    };
                    Ok(String::from_utf8_lossy(&buf).into_owned())
                }));
                self.read = None;
                self.child = Some(child);
            }
            Err(_) => {
                self.rows.clear();
                self.reason = "query-unavailable";
            }
        }
    }
    pub fn poll(&mut self) {
        if self.child.is_some() {
            if let (Some(rt), Some(output)) = (&self.runtime, self.output.as_mut())
                && let Ok(result) = rt.block_on(async { tokio::time::timeout(Duration::from_millis(1), output).await })
            {
                self.read = Some(result);
                self.output = None;
            }
            if let Some(Err(reason)) = &self.read {
                self.reason = reason;
                self.rows.clear();
                self.stop();
                return;
            }
            match self.child.as_mut().unwrap().try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        self.stop();
                        self.rows.clear();
                        self.reason = "query-failed";
                        if !self.fallback {
                            self.start(true)
                        };
                        return;
                    }
                    if let Some(Ok(text)) = self.read.take() {
                        self.rows = parse_gpu_query(&text, tune_link::now_ms());
                        self.sampled = Some(Instant::now());
                        self.reason = "invalid-query-row";
                        self.stop();
                        return;
                    }
                }
                Err(_) => {
                    self.stop();
                    self.rows.clear();
                    self.reason = "query-failed";
                    return;
                }
                Ok(None) => {}
            }
            if self.started.is_some_and(|t| t.elapsed() >= Duration::from_secs(4)) {
                self.stop();
                self.rows.clear();
                self.reason = "query-timeout";
            }
            return;
        }
        if self.started.is_none_or(|t| t.elapsed() >= self.every) {
            self.start(false);
        }
    }
    pub fn json(&self) -> Option<Value> {
        if !self.rows.is_empty() && self.sampled.is_some_and(|t| t.elapsed() <= self.every + Duration::from_millis(4500)) {
            return Some(json!(self.rows));
        }
        Some(
            json!([{"name":"GPU","source":"nvidia-smi query","memory_kind":"unknown","memory_status":"unavailable","memory_reason":self.reason,"available":false}]),
        )
    }
}
impl Drop for GpuQuery {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn args_are_clamped_like_the_powershell_sampler() {
        assert_eq!(parse_args(&[]).unwrap(), Args::default());
        let o = parse_args(&a("interval=0.01 procs=0 max=0 count=3 watch=1")).unwrap();
        assert_eq!((o.interval, o.procs_every, o.max_seconds, o.count, o.watch), (0.2, 1.0, 1.0, 3, true));
        assert_eq!(parse_args(&a("interval=99")).unwrap().interval, 10.0);
        let big = parse_args(&a("procs=1e300 max=1e300")).unwrap();
        assert_eq!((big.procs_every, big.max_seconds), (3600.0, 86_400.0), "Duration に直しても panic しない範囲");
        let _ = (std::time::Duration::from_secs_f64(big.procs_every), std::time::Duration::from_secs_f64(big.max_seconds));
        assert!(parse_args(&a("interval=x")).is_err());
        assert!(parse_args(&a("interval=NaN")).is_err());
        assert!(parse_args(&a("what=1")).is_err());
        assert!(parse_args(&a("interval")).is_err());
    }

    #[test]
    fn virtual_adapters_are_skipped() {
        for n in ["vEthernet (WSL)", "Tailscale Tunnel", "Bluetooth Network Connection", "Hyper-V Virtual Ethernet Adapter", "WAN Miniport (IP)"] {
            assert!(virtual_adapter(n), "{n}");
        }
        for n in ["Intel[R] Ethernet Controller I226-V", "Realtek PCIe 2.5GbE Family Controller", "Intel[R] Wi-Fi 6E AX211 160MHz"] {
            assert!(!virtual_adapter(n), "{n}");
        }
    }

    #[test]
    fn names_drop_exe_only() {
        assert_eq!(display_name("chrome.exe"), "chrome");
        assert_eq!(display_name("C:\\x\\Code.EXE"), "Code");
        assert_eq!(display_name("vmmemWSL"), "vmmemWSL");
        assert_eq!(display_name("Memory Compression"), "Memory Compression");
        assert_eq!(display_name("exe"), "exe");
        assert_eq!(display_name("日本語.exe"), "日本語");
    }

    #[test]
    fn top_processes_sort_unknown_cpu_last() {
        let rows = vec![
            ProcRow { pid: 1, name: "a".into(), cpu: None, mem_bytes: 3 << 20 },
            ProcRow { pid: 2, name: "b".into(), cpu: Some(12.34), mem_bytes: 1 << 20 },
            ProcRow { pid: 3, name: "c".into(), cpu: Some(0.0), mem_bytes: 2 << 20 },
        ];
        let v = procs_json(&rows, 2);
        assert_eq!(v["count"], 3);
        assert_eq!(v["top_cpu"][0]["pid"], 2);
        assert_eq!(v["top_cpu"][0]["cpu"], 12.3);
        assert_eq!(v["top_cpu"][1]["pid"], 3);
        assert_eq!(v["top_mem"][0]["pid"], 1);
        assert_eq!(v["top_mem"][0]["mem_mb"], 3.0);
        assert_eq!(v["top_mem"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn query_rows_keep_free_memory_and_na_per_adapter() {
        let rows = parse_gpu_query(
            "0, Test GPU A, 10, 50, 100, 49, 40, 25, 500, 800, GPU-test-a, P8, 100, 80, 120, 500, 1, 0\n1, Test GPU B, N/A, N/A, 200, N/A, N/A, N/A, N/A, N/A, GPU-test-b, P8, N/A, N/A, N/A, N/A, 1, 0",
            123,
        );
        assert_eq!(rows[0]["mem_free_mb"], json!(49.0));
        assert_eq!(rows[0]["mem_used_pct"], json!(50.0));
        assert_eq!(rows[1]["mem_used_mb"], Value::Null);
        assert_eq!(rows[1]["memory_status"], "partial");
        assert_eq!(rows[0]["sampled_at"], 123);
        let fallback = parse_gpu_query("0, Test GPU, 50, 100, 49", 123);
        assert_eq!(fallback[0]["mem_used_mb"], json!(50.0));
        assert_eq!(fallback[0]["util"], Value::Null);
    }

    #[test]
    fn query_cache_expires_without_converting_missing_memory_to_zero() {
        let mut q = GpuQuery::open("missing-sensor".into(), 5.0);
        q.rows = parse_gpu_query("0, Test GPU, 0, 100, 99", 123);
        q.sampled = Some(Instant::now());
        assert_eq!(q.json().unwrap()[0]["mem_used_mb"], json!(0.0));
        q.sampled = Some(Instant::now() - Duration::from_secs(10));
        let expired = q.json().unwrap();
        assert_eq!(expired[0]["available"], false);
        assert_eq!(expired[0]["mem_used_mb"], Value::Null);
        assert_eq!(expired[0]["memory_status"], "unavailable");
    }

    #[cfg(unix)]
    #[test]
    fn pending_pipe_read_is_cancelled_on_repeated_timeout_and_drop() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tune-gpu-pipe-{}-{}", std::process::id(), tune_link::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("fake-sensor");
        std::fs::write(&exe, "#!/bin/sh\n(sleep 0.2) &\nprintf '0, Test GPU, 0, 100, 99\\n'\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        for _ in 0..5 {
            let mut q = GpuQuery::open(exe.clone(), 5.0);
            q.start(false);
            std::thread::sleep(Duration::from_millis(20));
            q.poll();
            q.started = Some(Instant::now() - Duration::from_secs(5));
            q.poll();
            assert_eq!(q.reason, "query-timeout");
            assert!(q.child.is_none() && q.output.is_none() && q.read.is_none());
            assert_eq!(q.runtime.as_ref().unwrap().metrics().num_alive_tasks(), 0, "no detached reader tasks");
        }
        std::thread::sleep(Duration::from_millis(250));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rounding_matches_the_scripts() {
        assert_eq!(round(12.345, 1), json!(12.3));
        assert_eq!(round(0.5, 0), json!(1.0));
        assert_eq!(round(f64::NAN, 1), Value::Null);
        assert_eq!(round(f64::INFINITY, 1), Value::Null);
    }

    #[test]
    fn seq_and_time_are_added() {
        let v = with_seq(json!({"cpu": 1.0}), 7);
        assert_eq!((v["type"].as_str(), v["seq"].as_u64()), (Some("s"), Some(7)));
        assert!(v["t"].as_i64().unwrap() > 1_700_000_000_000);
    }
}
