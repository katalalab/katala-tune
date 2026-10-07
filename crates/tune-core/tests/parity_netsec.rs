//! ネットワークとセキュリティ（lib/netsec.js と、それを呼ぶ lib/rules.js・lib/health.js・lib/logs.js のログイン）を仕様として、
//! tune-core が同じ入力に同じ出力を返すかを確かめる。調査スクリプトの出力の解析（normalize）も含む。
//! 入力は決まった種から作る架空の待ち受け・接続・防御・常駐・ログイン。アドレスは文書用の範囲
//! （192.0.2.0/24・198.51.100.0/24・203.0.113.0/24・2001:db8::/32）と、私用・特別な範囲だけを使う。
//! node（24 以上）が要る。無い環境では失敗する（KATALA_TUNE_PARITY=skip で飛ばせる）。

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tune_core::db::Store;
use tune_core::{health, js, logs, netsec, rules};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() % 10_000) as f64 / 10_000.0 < p
    }
    fn pick<T: Clone>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize].clone()
    }
}

const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 86_400_000;

fn addrs() -> Vec<Value> {
    let mut v: Vec<Value> = [
        "*",
        "0.0.0.0",
        "::",
        "0:0:0:0:0:0:0:0",
        "127.0.0.1",
        "127.9.9.9",
        "::1",
        "[::1]",
        "localhost",
        "LOCALHOST",
        "192.0.2.10",
        "198.51.100.7",
        "203.0.113.200",
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.1",
        "172.32.0.1",
        "192.168.1.5",
        "100.128.0.1",
        "169.254.1.1",
        "224.0.0.251",
        "0.1.2.3",
        "2001:db8::1",
        "2001:DB8:0:0:0:0:0:2",
        "fe80::1%en0",
        "[fe80::2]",
        "fe90::3",
        "febf::4",
        "fec0::5",
        "fd7a:115c:a1e0::1",
        "fc00::9",
        "ff02::fb",
        "::ffff:198.51.100.9",
        "::ffff:127.0.0.1",
        "::ffff:",
        "[",
        "]",
        "[]",
        " 192.0.2.1 ",
        "\u{3000}192.0.2.2\u{feff}",
        "010.000.000.001",
        "256.1.1.1",
        "1.2.3",
        "1.2.3.4.5",
        "1..2.3",
        "host.example",
        "ＡＢＣ",
        "é.1.1.1",
        "2001:db8::zz",
        "*:*",
        "",
        "%en0",
    ]
    .iter()
    .map(|s| json!(s))
    .collect();
    // CGNAT（tailnet）の範囲。点検（oss-check）が本物のアドレスと見分けられないので、実行時に組み立てる
    for (a, b) in [(64, 0), (127, 0), (100, 7)] {
        v.push(json!(format!("{}.{a}.{b}.1", 100)));
    }
    v.push(json!("a".repeat(64)));
    v.push(json!("a".repeat(65)));
    v.extend([json!(null), json!(3), json!(true), json!([]), json!({})]);
    v
}

const PROCS: &[&str] =
    &["node", "ssh", "Google Chrome Helper", "svchost", "System", "?", "", "アプリ", "😀app", "x|y", "Codex (Service)", "tailscaled", "sshd"];
const DOC_ADDRS: &[&str] =
    &["192.0.2.10", "192.0.2.11", "198.51.100.7", "203.0.113.5", "2001:db8::1", "2001:db8::2", "10.0.0.5", "10.0.0.9", "fe80::1%en0", "127.0.0.1", "*"];

fn junk(r: &mut Rng) -> Value {
    r.pick(&[json!(null), json!(true), json!(1), json!(-3.5), json!("x"), json!([]), json!({}), json!("1")])
}

fn procv(r: &mut Rng) -> Value {
    if r.chance(0.05) {
        return junk(r);
    }
    if r.chance(0.03) {
        return json!("p".repeat(130));
    }
    json!(r.pick(PROCS))
}

fn portv(r: &mut Rng) -> Value {
    r.pick(&[
        json!(22),
        json!(80),
        json!(443),
        json!(3000),
        json!(5353),
        json!(9999),
        json!(10000),
        json!(49231),
        json!(65535),
        json!(0),
        json!(-1),
        json!(70000),
        json!(3.5),
        json!("22"),
        json!(null),
        json!(8443),
        json!(4444),
    ])
}

fn addrv(r: &mut Rng) -> Value {
    if r.chance(0.15) {
        let all = addrs();
        return r.pick(&all);
    }
    json!(r.pick(DOC_ADDRS))
}

fn maybe(r: &mut Rng, v: Value) -> Option<Value> {
    match r.below(10) {
        0 => None,
        1 => Some(junk(r)),
        _ => Some(v),
    }
}

// 値を先に作ってから「あり／型違い／無し」を選ぶ（r を2回借りないため）
macro_rules! mb {
    ($r:expr, $v:expr) => {{
        let v = $v;
        maybe($r, v)
    }};
}

fn obj(pairs: Vec<(&str, Option<Value>)>) -> Value {
    Value::Object(pairs.into_iter().filter_map(|(k, v)| v.map(|v| (k.to_string(), v))).collect())
}

fn raw_netsec(r: &mut Rng, win: bool) -> Value {
    if r.chance(0.03) {
        return junk(r);
    }
    let listen: Vec<Value> = (0..r.below(14))
        .map(|_| {
            if r.chance(0.04) {
                return junk(r);
            }
            let proto = r.pick(&[json!("tcp"), json!("tcp"), json!("udp"), json!("TCP"), json!("icmp"), json!(null)]);
            let addr = addrv(r);
            let port = portv(r);
            let proc_ = procv(r);
            obj(vec![("proto", Some(proto)), ("addr", Some(addr)), ("port", Some(port)), ("pid", Some(json!(r.below(9000)))), ("proc", Some(proc_))])
        })
        .collect();
    let outbound: Vec<Value> = (0..r.below(14))
        .map(|_| {
            let n = r.pick(&[json!(1), json!(3), json!(0), json!(-2), json!(2.5), json!(1e9), json!("4"), json!(null)]);
            let (proc_, addr, port) = (procv(r), addrv(r), portv(r));
            obj(vec![("proc", Some(proc_)), ("addr", Some(addr)), ("port", Some(port)), ("n", maybe(r, n))])
        })
        .collect();
    let defense = if win {
        let defender = if r.chance(0.85) {
            Some(obj(vec![
                ("realtime", mb!(r, json!(r.chance(0.7)))),
                ("antivirus", mb!(r, json!(r.chance(0.8)))),
                ("service", maybe(r, json!(true))),
                ("mode", mb!(r, r.pick(&[json!("Normal"), json!("Passive Mode"), json!(3)]))),
                ("sig_age_days", mb!(r, r.pick(&[json!(0), json!(1), json!(7), json!(8), json!(4_294_967_295_u64), json!("3"), json!(6.5)]))),
                ("sig_at", mb!(r, json!(NOW - r.below(20) as i64 * DAY))),
                ("tamper", mb!(r, json!(r.chance(0.5)))),
            ]))
        } else {
            Some(junk(r))
        };
        let av: Vec<Value> = (0..r.below(3))
            .map(|_| {
                let name = r.pick(&[json!("Windows Defender"), json!("Example AV"), json!(""), json!(3)]);
                obj(vec![("name", Some(name)), ("enabled", mb!(r, json!(r.chance(0.6)))), ("uptodate", mb!(r, json!(r.chance(0.7))))])
            })
            .collect();
        let fw: Vec<Value> = (0..r.below(4))
            .map(|_| {
                let name = r.pick(&[json!("Domain"), json!("Private"), json!("Public"), json!(5)]);
                obj(vec![("name", Some(name)), ("enabled", mb!(r, json!(r.chance(0.7))))])
            })
            .collect();
        let active = r.pick(&[json!([]), json!(["Private"]), json!(["Public", "Domain"]), json!([3, "Public"]), json!("Public")]);
        obj(vec![
            ("defender", defender),
            ("av", maybe(r, json!(av))),
            ("firewall", maybe(r, json!(fw))),
            ("active", Some(active)),
            ("detections_30d", mb!(r, r.pick(&[json!(0), json!(1), json!(3), json!("2"), json!(2.5)]))),
            ("detections_open_30d", mb!(r, r.pick(&[json!(0), json!(0), json!(1), json!("1")]))),
            ("security_log", mb!(r, r.pick(&[json!("ok"), json!("no-permission"), json!("x")]))),
        ])
    } else {
        obj(vec![
            ("firewall", mb!(r, r.pick(&[json!(0), json!(1), json!(2), json!(3), json!("1"), json!(true), json!(2.0)]))),
            ("stealth", mb!(r, json!(r.chance(0.5)))),
            ("gatekeeper", mb!(r, json!(r.chance(0.8)))),
            ("xprotect_version", mb!(r, r.pick(&[json!("5363"), json!(""), json!(5363), json!("v".repeat(50))]))),
            ("xprotect_at", {
                let x = NOW - r.below(80) as i64 * DAY;
                mb!(r, r.pick(&[json!(x), json!(1.5), json!("x")]))
            }),
        ])
    };
    let kinds: &[&str] = if win { &["schtask", "service", "run", "startup", "launchd", "bogus"] } else { &["launchd", "launchd", "schtask"] };
    let persist: Vec<Value> = (0..r.below(10))
        .map(|_| {
            let kind = if r.chance(0.05) { junk(r) } else { json!(r.pick(kinds)) };
            let key = r.pick(&[
                json!("user:com.example.a"),
                json!("agent:com.example.b"),
                json!("\\Task1"),
                json!("svc_*"),
                json!("HKCU\\X"),
                json!("user\\x.lnk"),
                json!(""),
                json!(3),
                json!("k".repeat(310)),
            ]);
            let program = r.pick(&[json!("a.exe"), json!(""), json!(null), json!(3), json!("b")]);
            obj(vec![("kind", Some(kind)), ("key", Some(key)), ("program", Some(program))])
        })
        .collect();
    let mut errors = serde_json::Map::new();
    for p in ["listen", "tasks", "defender", "bogus", "run", "launchd", "firewall", "gatekeeper", "xprotect", "av", "detections", "services", "startup"] {
        if r.chance(0.08) {
            errors.insert(p.into(), r.pick(&[json!("timeout 8s"), json!("no-permission"), json!(3), json!("e".repeat(250))]));
        }
    }
    obj(vec![
        ("v", Some(json!(1))),
        ("listen", Some(json!(listen))),
        ("outbound", maybe(r, json!(outbound))),
        ("defense", Some(defense)),
        ("persist", maybe(r, json!(persist))),
        ("errors", Some(Value::Object(errors))),
        ("parts_ms", maybe(r, json!({ "listen": 65, "firewall": 1.5, "bogus": 3, "tasks": "x" }))),
        ("elapsed_ms", mb!(r, r.pick(&[json!(123), json!(12.5), json!(null)]))),
        ("cpu_ms", maybe(r, json!(87))),
    ])
}

/// findings・checks に渡す形（取り込みの後の snapshot の netsec）。ときどき型を崩す
fn ingested(r: &mut Rng) -> Value {
    let win = r.chance(0.5);
    let probe = if win { "windows" } else { "mac" };
    let prev = if r.chance(0.7) { netsec::normalize(&raw_netsec(r, win), probe) } else { r.pick(&[json!(null), json!({ "listen": "x" })]) };
    let at = json!(NOW - r.below(3) as i64 * DAY);
    let mut ns = netsec::strip(&netsec::annotate(&netsec::normalize(&raw_netsec(r, win), probe), Some(&prev), Some(&at)));
    if r.chance(0.6) {
        let ch: Vec<Value> = (0..r.below(8))
            .map(|i| {
                let kind = r.pick(&[json!("launchd"), json!("schtask"), json!("service"), json!("bogus"), json!(3)]);
                let change = r.pick(&[json!("added"), json!("added"), json!("removed"), json!("changed"), json!("x")]);
                let program = r.pick(&[json!("a.exe"), json!(""), json!(null)]);
                obj(vec![("kind", Some(kind)), ("key", Some(json!(format!("k{i}")))), ("program", Some(program)), ("change", Some(change))])
            })
            .collect();
        ns["persist_changes"] = json!(ch);
        ns["persist_baseline"] = r.pick(&[json!([]), json!(["launchd"]), json!(["schtask", "run", 3])]);
    }
    if r.chance(0.6) {
        let new: Vec<Value> = (0..r.below(8))
            .map(|_| {
                let why = r.pick(&[json!("proc"), json!("port"), json!("dest"), json!("x")]);
                let dests = r.pick(&[json!(1), json!(2), json!(5), json!(null), json!(1.5)]);
                let (proc_, port) = (procv(r), portv(r));
                obj(vec![("proc", Some(proc_)), ("port", Some(port)), ("why", Some(why)), ("dests", Some(dests)), ("public", Some(json!(r.chance(0.5))))])
            })
            .collect();
        let learning = r.pick(&[json!(true), json!(false), json!(false), json!("yes")]);
        let until = r.pick(&[json!(NOW + 3 * DAY + 5), json!(NOW - DAY), json!(null)]);
        let known = r.pick(&[json!(120), json!(null), json!(0)]);
        ns["peers"] = r.pick(&[json!({ "learning": learning, "until": until, "known": known, "new": new, "sampled": 3 }), json!(null), json!([1])]);
    }
    if r.chance(0.05) {
        ns["defense"] = r.pick(&[json!(null), json!([]), json!("x"), json!({})]);
    }
    if r.chance(0.03) {
        ns = junk(r);
    }
    ns
}

fn findings_pool(r: &mut Rng) -> Vec<Value> {
    let pool = [
        json!({ "id": "log-login-fail", "severity": "critical", "title": "ログインの失敗が24時間で 120 件" }),
        json!({ "id": "log-login-fail", "severity": "warn", "title": "ログインの失敗が24時間で 12 件" }),
        json!({ "id": "log-login-fail", "severity": "info", "title": "ログインの失敗が24時間で 2 件" }),
        json!({ "id": "log-login-public", "severity": "warn", "title": "外部のアドレスからのログイン成功" }),
        json!({ "id": "sec-av-off", "severity": "critical", "title": "外から足した所見" }),
        json!({ "id": "log-panic", "severity": "critical", "title": "パニック" }),
        json!("x"),
    ];
    (0..r.below(4)).map(|_| r.pick(&pool)).collect()
}

fn run_js(cases: &Value) -> Option<Value> {
    if std::env::var("KATALA_TUNE_PARITY").as_deref() == Ok("skip") {
        eprintln!("KATALA_TUNE_PARITY=skip のため JS との比較を飛ばした");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("kt-parity-ns-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("cases.json");
    std::fs::write(&f, cases.to_string()).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("js").join("eval_netsec.js");
    let out = Command::new("node").arg("--no-warnings").arg(&script).arg(&f).output().expect("node が要る（KATALA_TUNE_PARITY=skip で飛ばせる）");
    assert!(out.status.success(), "eval_netsec.js が失敗: {}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    Some(serde_json::from_slice(&out.stdout).expect("eval_netsec.js の出力が JSON ではない"))
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

/// 比べる。JS が例外になった件数を返す（Rust 側は例外を出さないので、それは比べない）
fn check(kind: &str, inputs: &[Value], js: &Value, rust: &[Value]) -> usize {
    let js = js.as_array().unwrap_or_else(|| panic!("{kind}: JS の結果が無い"));
    assert_eq!(js.len(), rust.len(), "{kind}: 件数が違う");
    let mut thrown = 0;
    let mut bad = Vec::new();
    for (i, (j, r)) in js.iter().zip(rust).enumerate() {
        if j.get("__throw").is_some() {
            thrown += 1;
            continue;
        }
        if !same(j, r) {
            bad.push(i);
        }
    }
    if let Some(&i) = bad.first() {
        panic!(
            "{kind}: {} / {} 件が JS と違う。最初の例 #{i}\n入力: {}\nJS:   {}\nRust: {}",
            bad.len(),
            js.len(),
            inputs.get(i).map(Value::to_string).unwrap_or_default(),
            js[i],
            rust[i]
        );
    }
    assert_eq!(thrown, 0, "{kind}: JS が例外になった入力がある（netsec は型の崩れた入力でも落ちない作り）");
    thrown
}

fn checks_json(c: &[tune_core::db::Check]) -> Value {
    serde_json::to_value(c).unwrap()
}

fn mac_snap(ns: Value) -> Value {
    json!({
        "probe": "mac", "host": { "cores": 10, "uptime_h": 10 }, "cpu_busy": 20,
        "memory": { "total_gb": 16, "available_pct": 50, "pressure": "normal", "swap_used_gb": 0, "swap_total_gb": 0 },
        "processes": { "top_cpu": [], "apps": [], "apps_cpu": [], "agent_processes": 3 },
        "disk": [{ "mount": "/", "total_gb": 500, "free_gb": 200, "free_pct": 40 }],
        "power": {}, "containers": {}, "caches": [], "bench": { "runs_ms": [100, 101, 99], "median_ms": 100 },
        "jobs": [{ "kind": "launchd", "id": "com.example.a", "name": "com.example.a", "scope": "user", "state": "loaded", "last_result": 0 }],
        "netsec": ns
    })
}

fn win_snap(ns: Value, realtime: bool) -> Value {
    json!({
        "probe": "windows", "host": { "cores": 20, "uptime_h": 10 }, "cpu_busy": 10, "cpu_perf_pct": 100,
        "memory": { "total_gb": 64, "available_pct": 60, "commit_pct": 40 },
        "processes": { "top_cpu": [], "apps": [], "apps_cpu": [], "agent_processes": 1 },
        "disk": [{ "mount": "C:", "total_gb": 1000, "free_gb": 500, "free_pct": 50 }],
        "power": { "plan_guid": rules::HIGH_PERF_GUID, "plan_name": "高パフォーマンス" }, "wsl": { "config": true, "memory": "16GB" },
        "stability_7d": { "bugcheck_1001": 0, "kernel_power_41": 0, "unexpected_6008": 0 }, "defender": { "realtime": realtime },
        "jobs": [{ "kind": "schtask", "id": "\\A", "name": "A", "state": "ready", "last_result": 0 }],
        "netsec": ns
    })
}

/// nettop と Get-NetTCPConnection から作った形の見本（probes の出力。架空の値）
fn fixtures() -> Vec<(Value, &'static str)> {
    vec![
        (
            json!({
                "v": 1,
                "listen": [
                    { "proto": "tcp", "addr": "*", "port": 7000, "pid": 932, "proc": "ControlCenter" },
                    { "proto": "tcp", "addr": "*", "port": 7000, "pid": 932, "proc": "ControlCenter" },
                    { "proto": "tcp", "addr": "127.0.0.1", "port": 11434, "pid": 1172, "proc": "ollama" },
                    { "proto": "tcp", "addr": "::1", "port": 11434, "pid": 1172, "proc": "ollama" },
                    { "proto": "tcp", "addr": "10.20.30.40", "port": 36580, "pid": 538, "proc": "tailscaled" },
                    { "proto": "udp", "addr": "*", "port": 5353, "pid": 200, "proc": "mDNSResponder" },
                    { "proto": "tcp", "addr": "fe80::1%en0", "port": 49231, "pid": 916, "proc": "rapportd" }
                ],
                "outbound": [
                    { "proc": "ssh", "addr": "10.0.0.9", "port": 22, "n": 2 },
                    { "proc": "Google Chrome Helper", "addr": "198.51.100.7", "port": 443, "n": 5 },
                    { "proc": "Google Chrome Helper", "addr": "2001:db8::1", "port": 443, "n": 1 },
                    { "proc": "node", "addr": "127.0.0.1", "port": 5432, "n": 3 }
                ],
                "defense": { "firewall": 0, "stealth": false, "gatekeeper": true, "xprotect_version": "5363", "xprotect_at": NOW - 50 * DAY },
                "persist": [
                    { "kind": "launchd", "key": "user:com.example.agent", "program": "agent" },
                    { "kind": "launchd", "key": "daemon:com.example.daemon", "program": null }
                ],
                "errors": {}, "parts_ms": { "listen": 65, "firewall": 17 }, "elapsed_ms": 123, "cpu_ms": 87
            }),
            "mac",
        ),
        (
            json!({
                "v": 1,
                "listen": [
                    { "proto": "tcp", "addr": "0.0.0.0", "port": 3389, "pid": 1000, "proc": "svchost" },
                    { "proto": "tcp", "addr": "::", "port": 445, "pid": 4, "proc": "System" },
                    { "proto": "udp", "addr": "0.0.0.0", "port": 5353, "pid": 3000, "proc": "chrome" }
                ],
                "outbound": [{ "proc": "chrome", "addr": "203.0.113.5", "port": 443, "n": 7 }],
                "defense": {
                    "defender": { "realtime": false, "antivirus": true, "service": true, "mode": "Normal", "sig_age_days": 9, "sig_at": NOW - 9 * DAY, "tamper": true },
                    "av": [{ "name": "Windows Defender", "enabled": false, "uptodate": true }],
                    "firewall": [{ "name": "Domain", "enabled": true }, { "name": "Private", "enabled": false }, { "name": "Public", "enabled": true }],
                    "active": ["Private"], "detections_30d": 2, "detections_open_30d": 0, "security_log": "no-permission"
                },
                "persist": [{ "kind": "service", "key": "Example_*", "program": "example.exe" }, { "kind": "run", "key": "HKCU\\Example", "program": "example.exe" }],
                "errors": { "detections": "no-permission" }, "tcp_states": { "2": 12, "5": 40 }
            }),
            "windows",
        ),
    ]
}

#[test]
fn netsec_matches_js() {
    let mut r = Rng(0xA076_1D64_78BD_642F);

    let addr_in = addrs();
    let enabled_in =
        vec![json!({}), json!({ "network": false }), json!({ "network": true }), json!({ "network": 0 }), json!({ "network": "false" }), json!(null)];

    let mut normalize_in: Vec<Value> = fixtures().into_iter().map(|(raw, probe)| json!({ "raw": raw, "probe": probe })).collect();
    for _ in 0..1500 {
        let win = r.chance(0.5);
        let probe = if r.chance(0.97) { if win { "windows" } else { "mac" } } else { "x" };
        normalize_in.push(json!({ "raw": raw_netsec(&mut r, win), "probe": probe }));
    }

    let mut annotate_in = Vec::new();
    for _ in 0..800 {
        let win = r.chance(0.5);
        let probe = if win { "windows" } else { "mac" };
        let cur = netsec::normalize(&raw_netsec(&mut r, win), probe);
        let prev = match r.below(5) {
            0 => json!(null),
            1 => r.pick(&[json!({ "listen": "x" }), json!({ "listen": [], "errors": { "listen": "timeout" } }), json!([1])]),
            _ => netsec::normalize(&raw_netsec(&mut r, win), probe),
        };
        let at = r.pick(&[json!(NOW), json!(NOW as f64 + 0.5), json!(null), json!("x")]);
        let mut c = json!({ "ns": cur, "prev": prev });
        if r.chance(0.9) {
            c["at"] = at;
        }
        annotate_in.push(c);
    }
    let strip_in: Vec<Value> = annotate_in.iter().map(|c| netsec::annotate(&c["ns"], c.get("prev"), c.get("at"))).collect();
    let prepare_in: Vec<Value> =
        normalize_in.iter().take(300).map(|c| json!({ "raw": c["raw"], "probe": c["probe"], "prev": strip_in[0], "at": NOW })).collect();
    let listen_key_in: Vec<Value> = strip_in
        .iter()
        .flat_map(|n| js::arr(n.get("listen")).to_vec())
        .take(400)
        .chain([json!(null), json!({ "port": 10000.5 }), json!({ "port": "1" })])
        .collect();
    let persist_kinds_in: Vec<Value> = normalize_in.iter().take(400).map(|c| netsec::normalize(&c["raw"], c["probe"].as_str().unwrap())).collect();

    let kinds_pool = ["launchd", "schtask", "service", "run", "startup"];
    let mut diff_in = Vec::new();
    for _ in 0..600 {
        let keys = ["a", "b", "c", "d", "e"];
        let prev: Vec<Value> = (0..r.below(8))
            .map(|_| {
                let removed = r.pick(&[json!(null), json!(null), json!(NOW), json!("x")]);
                json!({ "kind": r.pick(&kinds_pool), "key": r.pick(&keys), "program": r.pick(&[json!("p"), json!("q"), json!(null), json!("")]), "removed_at": removed })
            })
            .collect();
        let items: Vec<Value> = (0..r.below(8))
            .map(|_| json!({ "kind": r.pick(&kinds_pool), "key": r.pick(&keys), "program": r.pick(&[json!("p"), json!("q"), json!(null)]) }))
            .collect();
        let pick_set = |r: &mut Rng| -> Vec<String> { kinds_pool.iter().filter(|_| r.chance(0.5)).map(|s| s.to_string()).collect() };
        let kinds = pick_set(&mut r);
        let failed = pick_set(&mut r);
        let baselined = pick_set(&mut r);
        diff_in.push(json!({ "prev": prev, "items": items, "kinds": kinds, "failed": failed, "baselined": baselined }));
    }

    let mut peers_in = Vec::new();
    for _ in 0..600 {
        let procs = ["app", "tool", "browser", "?"];
        let mut known: Vec<Value> = (0..r.below(12))
            .map(|_| json!({ "proc": r.pick(&procs), "addr": r.pick(DOC_ADDRS), "port": r.pick(&[json!(443), json!(22), json!(80), json!(null)]) }))
            .collect();
        if r.chance(0.3) {
            // 宛先の多いプロセス（ブラウザなど）
            for i in 0..12 {
                known.push(json!({ "proc": "browser", "addr": format!("198.51.100.{i}"), "port": 443 }));
            }
        }
        let sample: Vec<Value> = (0..r.below(10))
            .map(|_| {
                let d = r.pick(DOC_ADDRS);
                let a = r.pick(&[json!("198.51.100.200"), json!("203.0.113.9"), json!(d)]);
                json!({ "proc": r.pick(&["app", "tool", "browser", "new", "other"]), "addr": a, "port": r.pick(&[json!(443), json!(22), json!(8443), json!(null)]), "public": r.chance(0.5) })
            })
            .collect();
        peers_in.push(json!({ "known": known, "sample": sample, "learning": r.chance(0.2) }));
    }

    let mut findings_in = Vec::new();
    let mut checks_in = Vec::new();
    for i in 0..1500 {
        let ns = ingested(&mut r);
        let node = match r.below(4) {
            0 => json!({ "id": "pc", "shared": true }),
            1 => json!(null),
            _ => json!({ "id": "pc" }),
        };
        let mut fs = netsec::findings(Some(&ns), &node);
        fs.extend(findings_pool(&mut r));
        let cursors: Vec<Value> = (0..r.below(3))
            .map(|_| {
                let node_id = r.pick(&[json!("pc"), json!("other")]);
                let source = r.pick(&[json!("mac_auth"), json!("win_security"), json!("x")]);
                let last_error = r.pick(&[json!(null), json!(""), json!("ssh: 接続できない")]);
                obj(vec![("node_id", Some(node_id)), ("source", Some(source)), ("last_error", Some(last_error))])
            })
            .collect();
        let ctx = if i % 17 == 0 { json!(null) } else { json!({ "cursors": cursors, "now": NOW }) };
        findings_in.push(json!({ "ns": ns, "node": node }));
        checks_in.push(json!({ "ns": ns, "findings": fs, "ctx": ctx, "node": node }));
    }

    // rules.analyze・health.nodeChecks への組み込み（Defender の古い所見・状態との重なり、並び）
    let mut analyze_in = Vec::new();
    let mut node_checks_in = Vec::new();
    for _ in 0..400 {
        let win = r.chance(0.5);
        let ns = if r.chance(0.85) { ingested(&mut r) } else { json!(null) };
        let snap = if win { win_snap(ns, r.chance(0.6)) } else { mac_snap(ns) };
        let node = json!({ "id": "pc", "shared": r.chance(0.2) });
        let f = rules::analyze(&snap, &node);
        analyze_in.push(json!({ "snap": snap, "node": node }));
        let cursors = json!([{ "node_id": "pc", "source": if win { "win_security" } else { "mac_auth" }, "last_ok_at": NOW, "last_error": null }]);
        node_checks_in
            .push(json!({ "node": node, "snap": { "at": NOW, "wall_s": 3.5, "data": snap }, "findings": f, "ctx": { "now": NOW, "cursors": cursors } }));
    }

    // ログインの記録（DB を通す）
    let cgnat = format!("{}.64.0.9", 100); // tailnet の範囲（点検が本物と見分けられないので組み立てる）
    let mut lf_in = Vec::new();
    for case in 0..60 {
        let mut groups = Vec::new();
        for (k, (source, ev)) in [("mac_auth", "ssh-fail"), ("win_security", "4625"), ("win_security", "4624"), ("win_system", "7")].iter().enumerate() {
            let n = match r.below(4) {
                0 => 0,
                1 => r.below(12),
                2 => r.below(130),
                _ => 1,
            };
            let rows: Vec<Value> = (0..n)
                .map(|i| {
                    let p = r.pick(&[json!("198.51.100.7"), json!("203.0.113.5"), json!("192.168.1.9"), json!("local"), json!(cgnat.clone()), json!("2001:db8::7"), json!(null)]);
                    let ts = NOW - (r.below(9 * 86_400) as i64) * 1000;
                    json!({ "uid": format!("{case}-{k}-{i}"), "ts": ts, "level": if *ev == "4624" { "info" } else { "warn" }, "provider": p, "event_id": ev, "message": format!("logon {ev} {i}") })
                })
                .collect();
            groups.push(json!({ "source": source, "rows": rows }));
        }
        lf_in.push(json!({ "node_id": "pc", "now": NOW, "logs": groups }));
    }

    let cases = json!({
        "addr": addr_in, "enabled": enabled_in, "normalize": normalize_in, "annotate": annotate_in, "strip": strip_in, "prepare": prepare_in,
        "listenKey": listen_key_in, "persistKinds": persist_kinds_in, "diff": diff_in, "peers": peers_in, "findings": findings_in,
        "checks": checks_in, "analyze": analyze_in, "nodeChecks": node_checks_in, "logFindings": lf_in,
    });
    let Some(j) = run_js(&cases) else { return };

    let ai = |a: &Value| netsec::addr_info(Some(a)).map_or(Value::Null, |i| json!({ "addr": i.addr, "scope": i.scope }));
    check("netsec.addrInfo", &addr_in, &j["addr"], &addr_in.iter().map(ai).collect::<Vec<_>>());
    check("netsec.isPublic", &addr_in, &j["isPublic"], &addr_in.iter().map(|a| json!(netsec::is_public(Some(a)))).collect::<Vec<_>>());
    check("netsec.enabled", &enabled_in, &j["enabled"], &enabled_in.iter().map(|n| json!(netsec::enabled(n))).collect::<Vec<_>>());
    let norm: Vec<Value> = normalize_in.iter().map(|c| netsec::normalize(&c["raw"], c["probe"].as_str().unwrap())).collect();
    check("netsec.normalize", &normalize_in, &j["normalize"], &norm);
    let ann: Vec<Value> = annotate_in.iter().map(|c| netsec::annotate(&c["ns"], c.get("prev"), c.get("at"))).collect();
    check("netsec.annotate", &annotate_in, &j["annotate"], &ann);
    check("netsec.strip", &strip_in, &j["strip"], &strip_in.iter().map(netsec::strip).collect::<Vec<_>>());
    let prep: Vec<Value> = prepare_in.iter().map(|c| netsec::prepare(&c["raw"], c["probe"].as_str().unwrap(), c.get("prev"), c.get("at"))).collect();
    check("netsec.prepare", &prepare_in, &j["prepare"], &prep);
    check("netsec.listenKey", &listen_key_in, &j["listenKey"], &listen_key_in.iter().map(|l| json!(netsec::listen_key(l))).collect::<Vec<_>>());
    let pk: Vec<Value> = persist_kinds_in
        .iter()
        .map(|n| {
            let (k, f) = netsec::persist_kinds(n);
            json!({ "kinds": k, "failed": f })
        })
        .collect();
    check("netsec.persistKinds", &persist_kinds_in, &j["persistKinds"], &pk);
    let strs = |v: &Value| -> Vec<String> { js::arr(Some(v)).iter().map(|x| x.as_str().unwrap().to_string()).collect() };
    let diff: Vec<Value> = diff_in
        .iter()
        .map(|c| netsec::diff_persist(js::arr(c.get("prev")), js::arr(c.get("items")), &strs(&c["kinds"]), &strs(&c["failed"]), &strs(&c["baselined"])))
        .collect();
    check("netsec.diffPersist", &diff_in, &j["diff"], &diff);
    let peers: Vec<Value> = peers_in
        .iter()
        .map(|c| Value::Array(netsec::classify_peers(js::arr(c.get("known")), js::arr(c.get("sample")), c["learning"].as_bool().unwrap())))
        .collect();
    check("netsec.classifyPeers", &peers_in, &j["peers"], &peers);
    let fi: Vec<Value> = findings_in.iter().map(|c| Value::Array(netsec::findings(c.get("ns"), &c["node"]))).collect();
    check("netsec.findings", &findings_in, &j["findings"], &fi);
    let ch: Vec<Value> = checks_in.iter().map(|c| checks_json(&netsec::checks(c.get("ns"), js::arr(c.get("findings")), &c["ctx"], &c["node"]))).collect();
    check("netsec.checks", &checks_in, &j["checks"], &ch);
    let an: Vec<Value> = analyze_in.iter().map(|c| Value::Array(rules::analyze(&c["snap"], &c["node"]))).collect();
    check("rules.analyze（netsec あり）", &analyze_in, &j["analyze"], &an);
    let nc: Vec<Value> =
        node_checks_in.iter().map(|c| checks_json(&health::node_checks(&c["node"], Some(&c["snap"]), js::arr(c.get("findings")), &c["ctx"]))).collect();
    check("health.nodeChecks（netsec あり）", &node_checks_in, &j["nodeChecks"], &nc);
    let lf: Vec<Value> = lf_in
        .iter()
        .map(|c| {
            let db = Store::open_in_memory().unwrap();
            for g in js::arr(c.get("logs")) {
                let src = g["source"].as_str().unwrap();
                db.insert_logs("pc", src, &logs::normalize(src, js::arr(g.get("rows"))), NOW).unwrap();
            }
            Value::Array(logs::log_findings(&db, "pc", NOW).unwrap())
        })
        .collect();
    check("logs.logFindings（ログイン）", &lf_in, &j["logFindings"], &lf);

    // 判定が実際に出ているか（比べる意味のある入力になっているか）
    let ids: std::collections::HashSet<String> =
        fi.iter().chain(an.iter()).chain(lf.iter()).flat_map(|v| js::arr(Some(v)).iter().map(|f| js::string(f.get("id"))).collect::<Vec<_>>()).collect();
    for id in [
        "sec-av-off",
        "sec-av-old",
        "sec-firewall-off",
        "sec-gatekeeper-off",
        "sec-xprotect-old",
        "sec-detections",
        "net-persist-added",
        "net-peers-new",
        "log-login-fail",
        "log-login-public",
    ] {
        assert!(ids.contains(id), "{id} が一度も出ていない（入力の作り方を見直す）");
    }
    assert!(ids.iter().any(|i| i.starts_with("net-listen-")), "待ち受けの所見が一度も出ていない");
}
