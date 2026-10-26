//! macOS のネイティブのサンプラー（`tune-agent sample`）と probes/live_mac.py を同じ機体で同時に動かし、
//! 行の形と値を突き合わせる。同じ時刻の同じ OS の統計なので、変わらない値（コア数・メモリ量・スワップの総量・
//! 圧迫の段階・欄の有無）は一致し、動く値（CPU・メモリの使用率・プロセス数）は近い範囲に収まる。
#![cfg(target_os = "macos")]

use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../probes/live_mac.py");
const ARGS: [&str; 3] = ["interval=0.5", "procs=1", "count=8"];

fn lines(out: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(out).lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("読めない行 {e}: {l}"))).collect()
}

/// 両方を同時に起こし、終わるまで待つ
fn run_both() -> (Vec<Value>, Vec<Value>) {
    let native = Command::new(env!("CARGO_BIN_EXE_tune-agent")).arg("sample").args(ARGS).stdin(Stdio::null()).stdout(Stdio::piped()).spawn().unwrap();
    let mut py = Command::new("/usr/bin/python3").arg("-").args(ARGS).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    py.stdin.take().unwrap().write_all(&std::fs::read(SCRIPT).unwrap()).unwrap();
    let (n, p) = (native.wait_with_output().unwrap(), py.wait_with_output().unwrap());
    assert!(n.status.success() && p.status.success(), "native {:?} python {:?}", n.status, p.status);
    (lines(&n.stdout), lines(&p.stdout))
}

/// 欄の名前と型の形（数は数、文字列は文字列）。配列は最初の要素だけ見る。
/// センサーとdiskの数は、そのときの読みで欠測（null）になりうるので区別しない
fn shape(v: &Value, path: &str, out: &mut BTreeSet<String>) {
    let sensor = path.starts_with(".power") || path.starts_with(".gpu") || path.starts_with(".disk.");
    let kind = match v {
        Value::Null | Value::Number(_) if sensor => "number|null",
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    out.insert(format!("{path}: {kind}"));
    match v {
        Value::Object(m) => m.iter().for_each(|(k, v)| shape(v, &format!("{path}.{k}"), out)),
        Value::Array(a) => {
            if let Some(x) = a.first() {
                shape(x, &format!("{path}[]"), out)
            }
        }
        _ => {}
    }
}

fn shapes<'a>(rows: impl Iterator<Item = &'a Value>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    rows.for_each(|r| shape(r, "", &mut out));
    out
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("数でない: {v}"))
}

#[test]
fn native_sampler_matches_live_mac_py() {
    let (n, p) = run_both();
    let (nh, ph) = (&n[0], &p[0]);
    assert_eq!((nh["type"].as_str(), ph["type"].as_str()), (Some("hello"), Some("hello")));
    assert_eq!(nh["impl"], "rust");
    for k in ["v", "os", "cores", "interval", "procs_every", "mem_total_gb", "has"] {
        assert_eq!(nh[k], ph[k], "hello.{k}");
    }
    // 取れなかったものの名前（「power: …」の前）だけ比べる。例外の型名は Python にしか無い
    let names = |h: &Value| h["errors"].as_array().unwrap().iter().map(|e| e.as_str().unwrap().split(':').next().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names(nh), names(ph), "hello.errors");

    let samples = |v: &[Value]| v.iter().filter(|x| x["type"] == "s").cloned().collect::<Vec<_>>();
    let (ns, ps) = (samples(&n), samples(&p));
    assert_eq!((ns.len(), ps.len()), (8, 8));
    assert_eq!(n.last().unwrap()["reason"], "count");
    assert_eq!(p.last().unwrap()["reason"], "count");
    let seqs = |v: &[Value]| v.iter().map(|x| x["seq"].as_u64().unwrap()).collect::<Vec<_>>();
    assert_eq!(seqs(&ns), (1..=8).collect::<Vec<_>>());

    // 形: procs の有無で null と object が混ざるので、プロセスの付いた行・付かない行の両方を集めて比べる
    let (nshape, pshape) = (shapes(ns.iter()), shapes(ps.iter()));
    assert_eq!(
        nshape,
        pshape,
        "\nネイティブだけ: {:?}\nPython だけ: {:?}",
        nshape.difference(&pshape).collect::<Vec<_>>(),
        pshape.difference(&nshape).collect::<Vec<_>>()
    );
    // 上位プロセスは両方とも 1 点目に付き、以後は procs 秒ごと（間隔 0.5 秒なら 2〜3 点ごと）
    let with_procs = |v: &[Value]| v.iter().filter(|x| x.get("procs").is_some()).count();
    assert!(ns[0].get("procs").is_some() && ps[0].get("procs").is_some());
    assert!(with_procs(&ns).abs_diff(with_procs(&ps)) <= 1, "{} {}", with_procs(&ns), with_procs(&ps));

    let cores = nh["cores"].as_u64().unwrap() as usize;
    for (a, b) in ns.iter().zip(&ps) {
        assert_eq!(a["cores"].as_array().unwrap().len(), cores);
        assert_eq!(b["cores"].as_array().unwrap().len(), cores);
        for c in a["cores"].as_array().unwrap() {
            assert!(c.is_u64() && c.as_u64().unwrap() <= 100, "コアは 0〜100 の整数: {c}");
        }
        assert!((0.0..=100.0).contains(&f(&a["cpu"])));
        assert_eq!(a["mem"]["swap_total_gb"], b["mem"]["swap_total_gb"]);
        assert!((f(&a["mem"]["used_pct"]) - f(&b["mem"]["used_pct"])).abs() <= 5.0, "{} {}", a["mem"], b["mem"]);
        assert_eq!(a["gpu"][0]["name"], b["gpu"][0]["name"]);
        assert_eq!(a["gpu"][0]["memory_kind"], b["gpu"][0]["memory_kind"]);
        for k in ["read_bps", "write_bps"] {
            assert!(a["disk"][k].is_null() || a["disk"][k].is_u64(), "{}", a["disk"]);
        }
        for k in ["rx_bps", "tx_bps"] {
            assert!(a["net"][k].is_u64(), "{}", a["net"]);
        }
    }
    // 両方が同時に動く 4 秒の平均は近い（どちらも同じ tick を読む）
    let mean = |v: &[Value]| v.iter().map(|x| f(&x["cpu"])).sum::<f64>() / v.len() as f64;
    assert!((mean(&ns) - mean(&ps)).abs() <= 15.0, "CPU の平均 {} {}", mean(&ns), mean(&ps));

    let procs = |v: &[Value]| v.iter().filter_map(|x| x.get("procs").filter(|p| p.is_object())).cloned().collect::<Vec<_>>();
    let (np, pp) = (procs(&ns), procs(&ps));
    let (nc, pc) = (np.last().unwrap()["count"].as_u64().unwrap(), pp.last().unwrap()["count"].as_u64().unwrap());
    assert!(nc.abs_diff(pc) <= pc / 10 + 5, "プロセス数 {nc} {pc}");
    // 名前はパスを含まない。メモリの上位は動きが遅いので、どちらの上位にも同じプロセスが居る
    for row in np.iter().flat_map(|p| p["top_cpu"].as_array().unwrap().iter().chain(p["top_mem"].as_array().unwrap())) {
        assert!(!row["name"].as_str().unwrap().contains('/'), "{row}");
        assert!(!row["app"].as_str().unwrap().contains('/'), "{row}");
    }
    let top = |p: &Value| p["top_mem"].as_array().unwrap().iter().map(|r| r["pid"].as_u64().unwrap()).collect::<BTreeSet<_>>();
    assert!(!top(np.last().unwrap()).is_disjoint(&top(pp.last().unwrap())), "メモリの上位が重ならない");
    for x in ns.iter().filter_map(|x| x.get("self")) {
        assert!(f(&x["rss_mb"]) > 0.0 && f(&x["cpu_s"]) >= 0.0);
    }
}

/// 標準入力が閉じたら（watch=1）、間隔を待たずに終わる
#[test]
fn native_sampler_stops_when_stdin_closes() {
    let mut c =
        Command::new(env!("CARGO_BIN_EXE_tune-agent")).args(["sample", "interval=5", "watch=1"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    // 読み手は最後まで持つ（標準出力に書けなくなって終わるのと区別する）
    let mut out = std::io::BufReader::new(c.stdout.take().unwrap());
    let mut first = String::new();
    std::io::BufRead::read_line(&mut out, &mut first).unwrap();
    assert!(first.contains("\"hello\""), "{first}");
    let t = std::time::Instant::now();
    drop(c.stdin.take());
    let st = c.wait().unwrap();
    assert!(st.success());
    assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
    drop(out);
}

#[test]
fn native_sampler_max_expires_before_long_interval() {
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_tune-agent"))
        .args(["sample", "interval=5", "max=1"])
        .env("HOME", std::env::temp_dir().join(format!("sampler-timer-{}", std::process::id())))
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
        .wait_with_output()
        .unwrap();
    assert!(out.status.success());
    // Distinguish the old five-second interval wait while allowing scheduler/startup
    // overhead on a loaded CI runner. Sensor cleanup has separate controlled fixtures.
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
    let rows = lines(&out.stdout);
    assert_eq!(rows[0]["type"], "hello");
    assert_eq!(rows.last().unwrap()["reason"], "max_age");
    assert!(!rows.iter().any(|row| row["type"] == "s"));
}
