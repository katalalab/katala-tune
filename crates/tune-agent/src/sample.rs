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
//! macOS は probes/live_mac.py のまま（すでに 1 コア比 0.5% 未満）。
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
            "procs" => o.procs_every = num()?.max(1.0),
            "max" => o.max_seconds = num()?.max(1.0),
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
fn emit(out: &mut impl Write, v: &Value) -> bool {
    let mut s = v.to_string();
    s.push('\n');
    out.write_all(s.as_bytes()).and_then(|_| out.flush()).is_ok()
}

/// 標準入力を読み捨て、閉じたら印を立てる（ローカル実行ではアプリが標準入力を開いたまま持つ。
/// SSH 経由では予備のスクリプトのバイト列も届くので、それも読み捨てる）
fn watch_stdin(gone: Arc<AtomicBool>) {
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

#[cfg(not(windows))]
fn run(_a: &Args) -> Result<(), String> {
    Err("この OS のネイティブのサンプラーはまだ無い（macOS は probes/live_mac.py を使う）".into())
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

/// nvidia-smi dmon の 1 行（見出しで列を覚えてから読む）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuNow {
    pub util: Option<f64>,
    pub mem_mb: Option<f64>,
    pub power_w: Option<f64>,
    pub temp_c: Option<f64>,
    pub graphics_mhz: Option<f64>,
    pub sm_mhz: Option<f64>,
    pub memory_mhz: Option<f64>,
}

/// dmon の出力を 1 行ずつ。`# gpu …` の見出しで列を覚え、数の行を (gpu 番号, 値) にする
#[derive(Default)]
pub struct Dmon {
    cols: Vec<String>,
}

impl Dmon {
    pub fn line(&mut self, line: &str) -> Option<(u32, GpuNow)> {
        let t = line.trim();
        if let Some(h) = t.strip_prefix('#') {
            let h = h.trim();
            if h.starts_with("gpu") {
                self.cols = h.split_whitespace().map(str::to_string).collect();
            }
            return None;
        }
        let f: Vec<&str> = t.split_whitespace().collect();
        if self.cols.is_empty() || f.len() != self.cols.len() {
            return None;
        }
        let at = |k: &str| self.cols.iter().position(|c| c == k).and_then(|i| f[i].parse::<f64>().ok()).filter(|x| x.is_finite());
        let gpu = self.cols.iter().position(|c| c == "gpu").and_then(|i| f[i].parse::<u32>().ok())?;
        Some((
            gpu,
            GpuNow {
                util: at("sm"),
                mem_mb: at("fb"),
                power_w: at("pwr"),
                temp_c: at("gtemp"),
                graphics_mhz: at("gclk").or_else(|| at("pclk")),
                sm_mhz: at("smclk"),
                memory_mhz: at("mclk"),
            },
        ))
    }
}

pub fn gpu_json(name: Option<&str>, total_mb: Option<f64>, g: &GpuNow) -> Value {
    let n = |x: Option<f64>, d| x.map_or(Value::Null, |v| round(v, d));
    json!({
        "name": name,
        "util": n(g.util, 0),
        "mem_used_mb": n(g.mem_mb, 0),
        "mem_total_mb": n(total_mb, 0),
        "power_w": n(g.power_w, 1),
        "temp_c": n(g.temp_c, 0),
        "clocks_graphics_mhz": n(g.graphics_mhz, 0),
        "clocks_sm_mhz": n(g.sm_mhz, 0),
        "clocks_memory_mhz": n(g.memory_mhz, 0),
        "source": "nvidia-smi dmon",
        "available": true,
    })
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
    fn dmon_reads_columns_from_the_header() {
        let mut d = Dmon::default();
        assert_eq!(
            d.line("    0     26     31      -     29      3      0      0      0      0  10501   2520   9001    11      0"),
            None,
            "見出しの前は読まない"
        );
        assert_eq!(d.line("# gpu    pwr  gtemp  mtemp     sm    mem    enc    dec    jpg    ofa   mclk   pclk     fb   bar1   ccpm"), None);
        assert_eq!(d.line("# Idx      W      C      C      %      %      %      %      %      %    MHz    MHz     MB     MB     MB"), None);
        let (i, g) = d.line("    0     26     31      -     29      3      0      0      0      0  10501   2520   9001    11      0").unwrap();
        assert_eq!(i, 0);
        assert_eq!(
            g,
            GpuNow {
                util: Some(29.0),
                mem_mb: Some(9001.0),
                power_w: Some(26.0),
                temp_c: Some(31.0),
                graphics_mhz: Some(2520.0),
                sm_mhz: None,
                memory_mhz: Some(10501.0)
            }
        );
        assert_eq!(d.line("    0     26"), None, "列の数が合わない行は捨てる");
        let v = gpu_json(Some("RTX"), Some(24564.0), &g);
        assert_eq!((v["util"].clone(), v["power_w"].clone(), v["clocks_sm_mhz"].clone()), (json!(29.0), json!(26.0), Value::Null));
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
