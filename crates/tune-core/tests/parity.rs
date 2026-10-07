//! JS 版（lib/*.js）を仕様として、同じ入力に同じ出力を返すかを確かめる。
//! 入力は test/*.test.js の例と、決まった種から作る架空の snapshot・ログ・操作（実在の機体名・ホスト名・ユーザー名は使わない）。
//! node（24 以上。node:sqlite を使う）が要る。無い環境では失敗する（KATALA_TUNE_PARITY=skip で飛ばせる）。

use std::path::Path;
use std::process::Command;

use serde_json::{Map, Value, json};
use tune_core::db::{Check, LogRow, Store};
use tune_core::nodes::Node;
use tune_core::{actions, health, js, logs, rules};

// ---- 決まった種の乱数 ----
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
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
    /// 0..max の数。ちょうど半分（.5・.25・.125）や整数も混ぜる（toFixed・Math.round の境目を突く）
    fn num(&mut self, max: f64) -> Value {
        let base = (self.below((max * 8.0) as u64 + 1) as f64) / 8.0;
        match self.below(6) {
            0 => json!(base.floor() as i64),
            1 => json!(base),
            2 => json!((self.below(1_000_000) as f64) / 1_000_000.0 * max),
            3 => json!(((self.below((max * 100.0) as u64 + 1)) as f64) / 100.0),
            4 => json!(base + 0.05),
            _ => json!(base.floor() + 0.5),
        }
    }
    /// 値か null か無し（None = キーを入れない＝JS の undefined）
    fn maybe(&mut self, v: Value) -> Option<Value> {
        match self.below(12) {
            0 => None,
            1 => Some(Value::Null),
            _ => Some(v),
        }
    }
}

// 値を先に作ってから「あり／null／無し」を選ぶ（r を2回借りないため）
macro_rules! mb {
    ($r:expr, $v:expr) => {{
        let v = $v;
        $r.maybe(v)
    }};
}

fn obj(pairs: Vec<(&str, Option<Value>)>) -> Value {
    Value::Object(pairs.into_iter().filter_map(|(k, v)| v.map(|v| (k.to_string(), v))).collect())
}

const PROC_NAMES: &[&str] = &[
    "Google Drive",
    "WindowServer",
    "claude",
    "python3.12",
    "python3.x",
    "mdworker_shared",
    "com.apple.WebKit.Networking",
    "Google Chrome",
    "chrome",
    "MsMpEng",
    "PresentMon_x64",
    "vmmemWSL",
    "vmmem",
    "rust-analyzer",
    "Slack",
    "node",
    "ollama_llama_server",
    "Code Helper",
    "kernel_task",
    "BiomeAgent",
    "fileproviderd",
    "アプリ",
    "Kelvin",
    "op",
    "opx",
    "SearchIndexer",
    "Discord",
    "explorer",
    "Unknown.exe",
    "docker-proxy",
];

fn proc(r: &mut Rng, win: bool) -> Value {
    let name = r.pick(PROC_NAMES).to_string();
    let app = if r.chance(0.5) {
        Some(json!(name.clone()))
    } else if r.chance(0.5) {
        Some(json!(r.pick(PROC_NAMES).to_string()))
    } else {
        None
    };
    let start = if win { json!(format!("2026100{}0041201{:02}", r.below(9) + 1, r.below(100))) } else { json!("Thu Oct  1 08:12:03 2026") };
    obj(vec![
        ("pid", Some(json!(r.below(90_000) + 1))),
        ("name", Some(json!(name))),
        ("app", app),
        ("cpu", Some(if win { r.num(20.0) } else { r.num(220.0) })),
        ("avg_core", if win { mb!(r, r.num(120.0)) } else { None }),
        ("etime", if r.chance(0.5) { Some(json!(format!("{:02}:{:02}", r.below(24), r.below(60)))) } else { None }),
        ("start", mb!(r, start)),
        ("mem_mb", Some(r.num(30_000.0))),
    ])
}

fn app_row(r: &mut Rng) -> Value {
    obj(vec![
        ("app", Some(json!(r.pick(PROC_NAMES).to_string()))),
        ("mem_mb", Some(r.num(40_000.0))),
        ("count", Some(json!(r.below(30) + 1))),
        ("cpu", Some(r.num(150.0))),
    ])
}

fn jobs(r: &mut Rng, win: bool) -> Value {
    let n = r.below(6);
    let states = ["ready", "running", "disabled", "queued", "loaded", "not-loaded", "unknown"];
    Value::Array(
        (0..n)
            .map(|i| {
                let lr = match r.below(6) {
                    0 => Value::Null,
                    1 => json!(0),
                    2 => json!(267_011),
                    3 => json!(0x41303),
                    4 => json!("0"),
                    _ => json!(r.below(5)),
                };
                obj(vec![
                    ("kind", Some(json!(if win { "schtask" } else { "launchd" }))),
                    ("id", Some(json!(if win { format!("\\job{i}") } else { format!("com.example.job{i}") }))),
                    ("name", Some(json!(format!("job{i}")))),
                    ("state", Some(json!(r.pick(&states).to_string()))),
                    ("scope", if win { None } else { Some(json!(*r.pick(&["user", "system"]))) }),
                    ("last_result", mb!(r, lr)),
                ])
            })
            .collect(),
    )
}

fn snapshot(r: &mut Rng) -> Value {
    let win = r.chance(0.5);
    let total = *r.pick(&[8, 16, 18, 32, 64, 128, 0]);
    let top: Vec<Value> = (0..r.below(7)).map(|_| proc(r, win)).collect();
    let apps: Vec<Value> = (0..r.below(5)).map(|_| app_row(r)).collect();
    let apps_cpu: Vec<Value> = (0..r.below(4)).map(|_| obj(vec![("app", Some(json!(r.pick(PROC_NAMES).to_string()))), ("cpu", Some(r.num(30.0)))])).collect();
    let mut disks = vec![obj(vec![
        ("mount", Some(json!(if win { *r.pick(&["C:", "c:\\", "D:"]) } else { *r.pick(&["/", "/Volumes/Data"]) }))),
        ("total_gb", Some(r.num(2000.0))),
        ("free_gb", Some(r.num(500.0))),
        ("free_pct", Some(r.num(30.0))),
    ])];
    if r.chance(0.3) {
        disks.push(obj(vec![
            ("mount", Some(json!("E:"))),
            ("total_gb", Some(r.num(4000.0))),
            ("free_gb", Some(r.num(100.0))),
            ("free_pct", Some(r.num(15.0))),
        ]));
    }
    let caches: Vec<Value> = (0..r.below(5))
        .map(|_| {
            let p = *r.pick(&[
                "~/Library/Developer/CoreSimulator/Devices",
                "~/Library/Developer/Xcode/DerivedData",
                "~/.npm/_cacache",
                "~/Library/Caches/Homebrew",
                "~/.colima",
                "~/other",
            ]);
            obj(vec![("path", Some(json!(p))), ("gb", mb!(r, r.num(12.0)))])
        })
        .collect();
    let runs: Vec<Value> = (0..r.below(6)).map(|_| r.num(300.0)).collect();
    let mut s = vec![
        ("probe", Some(json!(if win { "windows" } else { "mac" }))),
        ("host", Some(obj(vec![("cores", mb!(r, json!(r.below(32) + 1))), ("uptime_h", mb!(r, r.num(900.0))), ("cpu", Some(json!("Test CPU")))]))),
        ("cpu_busy", mb!(r, r.num(100.0))),
        (
            "memory",
            Some(obj(vec![
                ("total_gb", mb!(r, json!(total))),
                ("available_pct", mb!(r, r.num(60.0))),
                ("pressure", if win { None } else { mb!(r, json!(*r.pick(&["normal", "warn", "critical"]))) }),
                ("swap_used_gb", mb!(r, r.num(24.0))),
                ("swap_total_gb", Some(r.num(30.0))),
                ("compressed_gb", Some(r.num(8.0))),
                ("commit_pct", if win { mb!(r, r.num(100.0)) } else { None }),
                ("free_gb", Some(r.num(64.0))),
                ("pagefile_alloc_mb", Some(json!(r.below(65536)))),
                ("pagefile_peak_mb", Some(json!(r.below(65536)))),
            ])),
        ),
        (
            "processes",
            Some(obj(vec![
                ("top_cpu", Some(Value::Array(top))),
                ("top_mem", Some(json!([]))),
                ("apps", Some(Value::Array(apps))),
                ("apps_cpu", Some(Value::Array(apps_cpu))),
                ("agent_processes", mb!(r, json!(r.below(120)))),
                ("count", Some(json!(r.below(900)))),
            ])),
        ),
        ("disk", Some(Value::Array(disks))),
        ("caches", Some(Value::Array(caches))),
        ("bench", mb!(r, obj(vec![("runs_ms", Some(Value::Array(runs))), ("median_ms", Some(r.num(200.0)))]))),
        ("jobs", Some(jobs(r, win))),
        (
            "third_party_services",
            Some(
                json!([{ "name": "svc-a", "state": *r.pick(&["running", "stopped"]), "start": "auto" }, { "name": "Svc-B", "state": "running", "start": "manual" }]),
            ),
        ),
    ];
    if win {
        let guid = *r.pick(&[rules::BALANCED_GUID, rules::POWER_SAVER_GUID, rules::HIGH_PERF_GUID, "381B4222-F694-41F0-9685-FF5BB260DF2E", "x"]);
        let plans = match r.below(3) {
            0 => None,
            1 => Some(json!([rules::BALANCED_GUID, rules::HIGH_PERF_GUID])),
            _ => Some(json!([rules::BALANCED_GUID])),
        };
        s.extend([
            ("cpu_perf_pct", mb!(r, r.num(120.0))),
            ("power", Some(obj(vec![("plan_guid", mb!(r, json!(guid))), ("plan_name", Some(json!("バランス"))), ("plans", plans)]))),
            ("gpus", Some(json!([{ "name": "Test GPU", "temp_c": r.num(100.0), "util": r.num(100.0), "power_w": r.num(400.0), "power_limit_w": 450 }]))),
            (
                "stability_7d",
                mb!(
                    r,
                    obj(vec![
                        ("bugcheck_1001", mb!(r, json!(r.below(8)))),
                        ("kernel_power_41", mb!(r, json!(r.below(8)))),
                        ("unexpected_6008", Some(json!(r.below(8))))
                    ])
                ),
            ),
            ("wsl", if r.chance(0.6) { Some(obj(vec![("config", Some(json!(r.chance(0.5)))), ("memory", mb!(r, json!("16GB")))])) } else { None }),
            ("defender", mb!(r, obj(vec![("realtime", Some(json!(r.chance(0.7)))), ("exclusions", mb!(r, json!(r.below(20))))]))),
            ("startup_items", Some(Value::Array((0..r.below(30)).map(|i| json!(format!("item{i}"))).collect()))),
        ]);
    } else {
        let colima: Vec<Value> = (0..r.below(3))
            .map(|i| json!({ "name": format!("vm{i}"), "status": *r.pick(&["Running", "Stopped"]), "cpus": r.below(12) + 1, "memory_gb": r.num(64.0) }))
            .collect();
        s.extend([
            ("power", Some(obj(vec![("cpu_speed_limit", mb!(r, json!(*r.pick(&[100, 80, 55])))), ("low_power_mode", mb!(r, json!(r.chance(0.3))))]))),
            (
                "containers",
                Some(obj(vec![
                    ("colima", Some(Value::Array(colima))),
                    ("docker_desktop", if r.chance(0.3) { Some(json!({ "memory_gb": r.num(32.0) })) } else { None }),
                ])),
            ),
            ("time_machine_running", mb!(r, json!(r.chance(0.3)))),
        ]);
    }
    obj(s)
}

// ---- test/*.test.js の例（手で書いた入力） ----
fn mac_fixture(over: Value) -> Value {
    let mut b = json!({
        "probe": "mac", "host": { "cores": 10, "uptime_h": 10 }, "cpu_busy": 20,
        "memory": { "total_gb": 16, "available_pct": 50, "pressure": "normal", "swap_used_gb": 0, "swap_total_gb": 0 },
        "processes": { "top_cpu": [], "apps": [], "apps_cpu": [], "agent_processes": 3 },
        "disk": [{ "mount": "/", "total_gb": 500, "free_gb": 200, "free_pct": 40 }],
        "power": {}, "containers": {}, "caches": [], "bench": { "runs_ms": [100, 101, 99], "median_ms": 100 }
    });
    merge(&mut b, over);
    b
}

fn win_fixture(over: Value) -> Value {
    let mut b = json!({
        "probe": "windows", "host": { "cores": 20, "uptime_h": 10 }, "cpu_busy": 10, "cpu_perf_pct": 100,
        "memory": { "total_gb": 64, "available_pct": 60, "commit_pct": 40 },
        "processes": { "top_cpu": [], "apps": [], "apps_cpu": [], "agent_processes": 1 },
        "disk": [{ "mount": "C:", "total_gb": 1000, "free_gb": 500, "free_pct": 50 }],
        "power": { "plan_guid": rules::HIGH_PERF_GUID, "plan_name": "高パフォーマンス" }, "wsl": { "config": true, "memory": "16GB" },
        "stability_7d": { "bugcheck_1001": 0, "kernel_power_41": 0, "unexpected_6008": 0 }, "defender": { "realtime": true }
    });
    merge(&mut b, over);
    b
}

fn merge(b: &mut Value, over: Value) {
    if let (Value::Object(b), Value::Object(o)) = (b, over) {
        b.extend(o);
    }
}

fn fixtures() -> Vec<(Value, Value)> {
    let bal = rules::BALANCED_GUID;
    vec![
        (mac_fixture(json!({})), json!({})),
        (win_fixture(json!({})), json!({})),
        (
            mac_fixture(json!({ "processes": { "top_cpu": [
                { "pid": 500, "name": "Google Drive", "app": "Google Drive", "cpu": 105 },
                { "pid": 414, "name": "WindowServer", "app": "WindowServer", "cpu": 90 },
                { "pid": 900, "name": "claude", "app": "claude", "cpu": 120 }
            ], "apps": [], "apps_cpu": [] } })),
            json!({}),
        ),
        (
            win_fixture(
                json!({ "processes": { "top_cpu": [{ "pid": 10, "name": "ChatGPT", "cpu": 8, "avg_core": 5 }, { "pid": 11, "name": "PresentMon_x64", "cpu": 8, "avg_core": 74 }], "apps": [], "apps_cpu": [] } }),
            ),
            json!({}),
        ),
        (
            mac_fixture(
                json!({ "disk": [{ "mount": "/", "total_gb": 460, "free_gb": 16, "free_pct": 3.5 }], "caches": [{ "path": "~/.npm/_cacache", "gb": 4 }] }),
            ),
            json!({}),
        ),
        (win_fixture(json!({ "power": { "plan_guid": bal, "plan_name": "バランス" } })), json!({})),
        (win_fixture(json!({ "power": { "plan_guid": bal, "plan_name": "バランス" } })), json!({ "shared": true })),
        (win_fixture(json!({ "stability_7d": { "bugcheck_1001": 6, "kernel_power_41": 6, "unexpected_6008": 7 } })), json!({})),
        (
            win_fixture(
                json!({ "wsl": { "config": false, "memory": null }, "processes": { "top_cpu": [], "apps": [{ "app": "vmmemWSL", "mem_mb": 20000, "count": 1 }], "apps_cpu": [] } }),
            ),
            json!({}),
        ),
        (win_fixture(json!({ "power": { "plan_guid": bal, "plan_name": "バランス", "plans": [bal] } })), json!({})),
        (mac_fixture(json!({ "disk": [{ "mount": "/", "total_gb": 460, "free_gb": 108, "free_pct": 23.4, "raw_free_gb": 19 }] })), json!({})),
    ]
}

// ---- JS を呼ぶ ----
fn run_js(cases: &Value) -> Option<Value> {
    if std::env::var("KATALA_TUNE_PARITY").as_deref() == Ok("skip") {
        eprintln!("KATALA_TUNE_PARITY=skip のため JS との比較を飛ばした");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("kt-parity-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("cases.json");
    std::fs::write(&f, cases.to_string()).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("js").join("eval.js");
    let out = Command::new("node").arg("--no-warnings").arg(&script).arg(&f).output().expect("node が要る（KATALA_TUNE_PARITY=skip で飛ばせる）");
    assert!(out.status.success(), "eval.js が失敗: {}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    Some(serde_json::from_slice(&out.stdout).expect("eval.js の出力が JSON ではない"))
}

// ---- 比べる（数は値で比べる。1 と 1.0 は同じ） ----
fn same(a: &Value, b: &Value, loose_throw: bool) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q, loose_throw)),
        (Value::Object(x), Value::Object(y)) => {
            if loose_throw && x.contains_key("__throw") && y.contains_key("__throw") {
                return true;
            }
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w, loose_throw)))
        }
        _ => a == b,
    }
}

fn check_all(kind: &str, inputs: &[Value], js: &Value, rust: &[Value], loose_throw: bool) -> usize {
    let js = js.as_array().unwrap_or_else(|| panic!("{kind}: JS の結果が無い"));
    assert_eq!(js.len(), rust.len(), "{kind}: 件数が違う");
    let mut thrown = 0;
    let mut bad = Vec::new();
    for (i, (j, r)) in js.iter().zip(rust).enumerate() {
        if j.get("__throw").is_some() && r.get("__throw").is_none() {
            // JS が例外で落ちる入力（型の崩れたもの）は比べない。数だけ数える
            thrown += 1;
            continue;
        }
        if !same(j, r, loose_throw) {
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
    thrown
}

fn checks_json(c: &[Check]) -> Value {
    serde_json::to_value(c).unwrap()
}

fn row_json(r: &LogRow) -> Value {
    json!({ "uid": r.uid, "ts": js::jnum(r.ts), "level": r.level, "provider": r.provider, "event_id": r.event_id, "message": r.message, "fingerprint": r.fingerprint })
}

fn node_of(v: &Value) -> Node {
    let mut n = Node::from_value(v, "no-such-host");
    n.local = js::truthy(v.get("local"));
    n
}

#[test]
fn rules_health_logs_actions_match_js() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let now: i64 = 1_800_000_000_000;

    // 判定（rules）
    let mut analyze_in: Vec<Value> = fixtures().into_iter().map(|(snap, node)| json!({ "snap": snap, "node": node })).collect();
    for _ in 0..4000 {
        let shared = r.chance(0.2);
        analyze_in.push(json!({ "snap": snapshot(&mut r), "node": { "shared": shared } }));
    }
    let compare_in: Vec<Value> = (0..600)
        .map(|_| {
            let mut p = snapshot(&mut r);
            let c = snapshot(&mut r);
            if r.chance(0.1) {
                p = Value::Null;
            }
            json!({ "prev": p, "cur": c })
        })
        .collect();

    // 状態（health）
    let finding_pool = [
        json!({ "id": "runaway-x-1", "severity": "warn", "title": "暴走" }),
        json!({ "id": "log-panic", "severity": "critical", "title": "パニック" }),
        json!({ "id": "log-whea", "severity": "critical", "title": "WHEA 6 件" }),
        json!({ "id": "log-whea", "severity": "warn", "title": "WHEA 2 件" }),
        json!({ "id": "log-gpu-reset", "severity": "warn", "title": "GPU" }),
        json!({ "id": "log-disk", "severity": "warn", "title": "disk" }),
        json!({ "id": "log-flood-abc", "severity": "warn", "title": "洪水 A" }),
        json!({ "id": "log-flood-def", "severity": "warn", "title": "洪水 B" }),
    ];
    let mut node_in = Vec::new();
    for i in 0..1500 {
        let snap = if r.chance(0.15) {
            Value::Null
        } else {
            json!({ "at": now - (r.below(600) as i64) * 60_000, "wall_s": mb!(r, r.num(20.0)).unwrap_or(Value::Null), "data": snapshot(&mut r) })
        };
        let findings: Vec<Value> = (0..r.below(4)).map(|_| r.pick(&finding_pool).clone()).collect();
        let cursors: Vec<Value> = (0..r.below(4))
            .map(|k| {
                obj(vec![
                    ("node_id", Some(json!(if r.chance(0.8) { "pc" } else { "other" }))),
                    ("source", Some(json!(format!("src{k}")))),
                    ("last_ok_at", mb!(r, json!(now - (r.below(200) as i64) * 60_000))),
                    (
                        "last_error",
                        if r.chance(0.2) { Some(json!(format!("ssh: 接続できない {}", "x".repeat(r.below(120) as usize)))) } else { Some(Value::Null) },
                    ),
                ])
            })
            .collect();
        let ctx = obj(vec![
            ("now", Some(json!(now))),
            ("cursors", Some(Value::Array(cursors))),
            (
                "expect",
                if r.chance(0.6) {
                    Some(
                        json!({ "services": ["svc-a", "svc-b", "missing"], "jobs": ["\\job0", "com.example.job1", "nope"], "processes": ["Slack", "ollama", "node"] }),
                    )
                } else {
                    None
                },
            ),
            ("schedule", if r.chance(0.7) { Some(json!({ "probe_minutes": *r.pick(&[15, 60, 240]), "logs_minutes": *r.pick(&[5, 15]) })) } else { None }),
            ("lastError", if r.chance(0.1) { Some(json!(format!("line1\nssh: timeout {i}"))) } else { None }),
        ]);
        node_in.push(json!({ "node": { "id": "pc" }, "snap": snap, "findings": findings, "ctx": ctx }));
    }
    let app_in: Vec<Value> = (0..800)
        .map(|_| {
            obj(vec![
                ("now", Some(json!(now))),
                ("configError", if r.chance(0.1) { Some(json!("JSON の誤り")) } else { None }),
                ("example", Some(json!(r.chance(0.2)))),
                ("nodeCount", Some(json!(r.below(8)))),
                ("protectCount", Some(json!(r.below(4)))),
                ("dbCheck", Some(json!(*r.pick(&["ok", "ok", "*** in database main ***"])))),
                ("dbBytes", Some(json!(r.below(80_000_000)))),
                (
                    "scheduler",
                    Some(obj(vec![
                        ("enabled", Some(json!(r.chance(0.8)))),
                        ("probe_minutes", Some(json!(*r.pick(&[15, 60])))),
                        ("logs_minutes", Some(json!(15))),
                        ("lastProbeAt", mb!(r, json!(now - (r.below(400) as i64) * 60_000))),
                        ("lastLogsAt", mb!(r, json!(now - (r.below(400) as i64) * 60_000))),
                    ])),
                ),
                (
                    "fleet",
                    match r.below(4) {
                        0 => None,
                        1 => Some(Value::Null),
                        2 => Some(json!({ "error": "op: 認証が切れている" })),
                        _ => Some(json!({ "nodes": [] })),
                    },
                ),
                ("fleetAt", Some(json!(now - 120_000))),
                ("openAtLogin", Some(json!(r.chance(0.5)))),
            ])
        })
        .collect();
    let jobs_in: Vec<Value> = (0..300).map(|i| jobs(&mut r, i % 2 == 0)).collect();

    // ログ（伏せ字・指紋・正規化）
    let fake_aws = ["AKIA", "ABCDEFGHIJKLMNOP"].concat();
    let messages: Vec<String> = vec![
        format!(
            "token=abc123 Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig12345678 ghp_{} {fake_aws} sk-{} password: \"p w\"",
            "a".repeat(36),
            "x".repeat(30)
        ),
        "The device \\Device\\Harddisk1\\DR1 has a bad block at 0x1F3 in C:\\Users\\a\\x.txt".into(),
        "port {1C3A4C45-3712-47C5-BAD3-BFD302864317} failed after 3.5 s at /Users/me/Library/x".into(),
        "API_KEY = 'secret value' PWD:hunter2 Access-Key=zzz basic dGVzdHRlc3R0ZXN0".into(),
        "İstanbul ΣΑΣ 'quoted' \"double\" 12.5.6 0xABC".into(),
        format!("{}😀tail", "あ".repeat(999)),
        format!("{}😀", "a".repeat(999)),
        "tabs\tand\u{3000}ideographic\u{a0}spaces\u{feff}bom\u{85}nel".into(),
        // 秘密らしい形の偽の値は実行時に組み立てる（ソースに置かない。gitleaks が正しく反応するため。test/logs.test.js と同じ）
        format!(
            "{} {} {}",
            ["github", "_pat_", "ABCDEFGHIJKLMNOPQRSTUV_123"].concat(),
            ["ops", "_ABCDEFGHIJKLMNOPQRSTUV"].concat(),
            ["AS", "IA", "ABCDEFGHIJKLMNOP"].concat()
        ),
        "x".repeat(5000),
        String::new(),
    ];
    let mut redact_in: Vec<Value> = messages.iter().map(|m| json!(m)).collect();
    for _ in 0..400 {
        let a = r.pick(&messages).clone();
        let b = r.pick(&messages).clone();
        let cut = a.chars().count().min(r.below(60) as usize);
        redact_in.push(json!(format!("{}{} {}", a.chars().take(cut).collect::<String>(), r.pick(&["=", ": ", " token=", " Bearer "]), b)));
    }
    let fp_in: Vec<Value> = redact_in
        .iter()
        .map(|m| {
            obj(vec![
                ("source", Some(json!(*r.pick(&["win_system", "mac_diag", "x"])))),
                ("provider", mb!(r, json!(*r.pick(&["disk", "Display", "", "p\"q"])))),
                ("event_id", mb!(r, if r.chance(0.5) { json!(4101) } else { json!("7") })),
                ("message", Some(m.clone())),
            ])
        })
        .collect();
    let normalize_in: Vec<Value> = (0..300)
        .map(|_| {
            let rows: Vec<Value> = (0..r.below(5))
                .map(|k| {
                    obj(vec![
                        ("uid", mb!(r, if r.chance(0.5) { json!(k) } else { json!(format!("u{k}-{}", "x".repeat(r.below(250) as usize))) })),
                        ("ts", mb!(r, if r.chance(0.8) { json!(now - k as i64) } else { json!(format!("{}", now)) })),
                        ("level", mb!(r, json!(*r.pick(&["critical", "error", "warn", "info", "debug"])))),
                        ("provider", mb!(r, json!(*r.pick(&["disk", "", "Microsoft-Windows-WHEA-Logger"])))),
                        ("event_id", mb!(r, json!(*r.pick(&["7", "4101"])))),
                        ("message", mb!(r, json!(r.pick(&messages).clone()))),
                    ])
                })
                .collect();
            json!({ "source": *r.pick(&["win_system", "mac_diag"]), "rows": rows })
        })
        .collect();

    // 変更操作の計画（actions）
    let plist = "/Users/me/Library/LaunchAgents/com.example.job.plist";
    let action_pool = [
        json!({ "type": "rm-rf", "params": {} }),
        json!({ "type": "kill-process", "params": { "pid": 500, "name": "Google Drive", "start": "Thu Oct  1 08:12:03 2026", "min_cpu": 50 } }),
        json!({ "type": "kill-process", "params": { "pid": 700, "name": "PresentMon_x64", "start": "20261003004120123", "min_cpu": 50 } }),
        json!({ "type": "kill-process", "params": { "pid": 700, "name": "python" } }),
        json!({ "type": "kill-process", "params": { "pid": 700, "name": "code" } }),
        json!({ "type": "kill-process", "params": { "pid": "500; rm -rf ~", "name": "x" } }),
        json!({ "type": "kill-process", "params": { "pid": 500, "name": "x'; rm -rf ~; '" } }),
        json!({ "type": "kill-process", "params": { "pid": 500, "name": "x", "start": "now'; reboot" } }),
        json!({ "type": "kill-process", "params": { "pid": 500, "name": "x", "start": "2026" } }),
        json!({ "type": "kill-process", "params": { "pid": 4, "name": "System" } }),
        json!({ "type": "kill-process", "params": { "pid": 9, "name": "x", "min_cpu": "1; x" } }),
        json!({ "type": "kill-process", "params": { "pid": 9, "name": "x", "min_cpu": 0 } }),
        json!({ "type": "kill-process", "params": { "pid": 9.0, "name": "Slack", "min_cpu": null } }),
        json!({ "type": "kill-process", "params": { "pid": 12345, "name": "MsMpEng" } }),
        json!({ "type": "set-power-plan", "params": { "guid": rules::HIGH_PERF_GUID, "prev_guid": rules::BALANCED_GUID } }),
        json!({ "type": "set-power-plan", "params": { "guid": "x && shutdown" } }),
        json!({ "type": "set-power-plan", "params": { "guid": rules::HIGH_PERF_GUID, "prev_guid": "bad" } }),
        json!({ "type": "task-disable", "params": { "path": "\\", "name": "KatalaGitHubBackup" } }),
        json!({ "type": "task-enable", "params": { "path": "\\Sub\\", "name": "Job (1)" } }),
        json!({ "type": "task-disable", "params": { "path": "\\Microsoft\\Windows\\", "name": "Defrag" } }),
        json!({ "type": "task-run", "params": { "path": "\\", "name": "x\"; Remove-Item C:\\ -Recurse; \"" } }),
        json!({ "type": "task-run", "params": { "path": "C:\\", "name": "x" } }),
        json!({ "type": "task-run", "params": { "path": "\\", "name": "x" } }),
        json!({ "type": "launchd-unload", "params": { "label": "com.example.job", "plist": plist } }),
        json!({ "type": "launchd-unload", "params": { "label": "com.example.job" } }),
        json!({ "type": "launchd-unload", "params": { "label": "com.apple.Finder", "plist": plist } }),
        json!({ "type": "launchd-load", "params": { "label": "x", "plist": "/Library/LaunchDaemons/x.plist" } }),
        json!({ "type": "launchd-load", "params": { "label": "com.example.job", "plist": plist } }),
        json!({ "type": "launchd-kickstart", "params": { "label": "x; rm -rf ~" } }),
        json!({ "type": "launchd-kickstart", "params": { "label": "com.example.job" } }),
        json!({ "type": "kill-process" }),
    ];
    let mut plan_in = Vec::new();
    for a in &action_pool {
        for os in ["macos", "windows"] {
            for shared in [false, true] {
                for local in [false, true] {
                    for protect in [Some(json!(["python.exe", "Code"])), None] {
                        plan_in.push(obj(vec![
                            ("node", Some(json!({ "id": "n1", "alias": "n1", "os": os, "shared": shared, "local": local }))),
                            ("action", Some(a.clone())),
                            ("protect", protect),
                        ]));
                    }
                }
            }
        }
    }

    // ログ由来の所見（DB を通す）
    let mut lf_in = Vec::new();
    let kinds: [(&str, &str, Option<&str>, &str); 14] = [
        ("win_system", "Microsoft-Windows-WHEA-Logger", Some("17"), "warn"),
        ("win_system", "Display", Some("4101"), "warn"),
        ("win_system", "nvlddmkm", Some("14"), "error"),
        ("win_system", "Microsoft-Windows-Resource-Exhaustion-Detector", Some("2004"), "warn"),
        ("win_system", "Ntfs", Some("55"), "error"),
        ("win_system", "disk", Some("7"), "info"),
        ("win_application", "Application Error", Some("1000"), "error"),
        ("win_application", ".NET Runtime", Some("1026"), "error"),
        ("mac_diag", "Safari", Some("crash"), "error"),
        ("mac_diag", "kernel", Some("kernel panic"), "critical"),
        ("mac_diag", "Xcode", Some("jetsam (memory)"), "warn"),
        ("neonmonitor", "NeonMonitor", None, "warn"),
        ("neonmonitor", "NeonMonitor", None, "info"),
        ("win_system", "Service Control Manager", Some("7031"), "error"),
    ];
    for case in 0..40 {
        let mut groups: Vec<Value> = Vec::new();
        for (k, (source, provider, ev, level)) in kinds.iter().enumerate() {
            let n = match r.below(4) {
                0 => 0,
                1 => r.below(4),
                2 => r.below(20),
                _ => 1 + r.below(2),
            };
            let rows: Vec<Value> = (0..n)
                .map(|i| json!({ "uid": format!("{case}-{k}-{i}"), "ts": now - (r.below(9 * 86_400) as i64) * 1000, "level": level, "provider": provider, "event_id": ev, "message": format!("{provider} {} {i}", ev.unwrap_or("")) }))
                .collect();
            groups.push(json!({ "source": source, "rows": rows }));
        }
        if case % 3 == 0 {
            // 同じエラーの洪水（件数は重ならないようにする）
            for (j, n) in [(0, 250), (1, 210), (2, 5)] {
                let rows: Vec<Value> = (0..n)
                    .map(|i| json!({ "uid": format!("f{j}-{i}"), "ts": now - i * 60_000 / 4, "level": "error", "provider": format!("Flood{j}"), "event_id": "1", "message": format!("flood {j} \"same\" error {i}") }))
                    .collect();
                groups.push(json!({ "source": "win_application", "rows": rows }));
            }
        }
        let dropped = if case % 4 == 0 { json!([{ "source": "win_application", "n": 1700 }]) } else { json!([{ "source": "win_system", "n": r.below(900) }]) };
        lf_in.push(json!({ "node_id": "pc", "now": now, "logs": groups, "dropped": dropped }));
    }

    let cases = json!({
        "analyze": analyze_in, "compare": compare_in, "nodeChecks": node_in, "appChecks": app_in, "failingJobs": jobs_in,
        "redact": redact_in, "fingerprint": fp_in, "normalize": normalize_in, "plan": plan_in, "logFindings": lf_in,
    });
    let Some(js_out) = run_js(&cases) else { return };

    // Rust 側で同じ入力を評価して比べる
    let analyze_rs: Vec<Value> = analyze_in.iter().map(|c| Value::Array(rules::analyze(&c["snap"], &c["node"]))).collect();
    let thrown = check_all("rules.analyze", &analyze_in, &js_out["analyze"], &analyze_rs, false);
    let score_rs: Vec<Value> = analyze_in.iter().map(|c| json!(rules::score(&rules::analyze(&c["snap"], &c["node"])))).collect();
    check_all("rules.score", &analyze_in, &js_out["score"], &score_rs, false);
    let compare_rs: Vec<Value> = compare_in.iter().map(|c| rules::compare(Some(&c["prev"]).filter(|v| !v.is_null()), Some(&c["cur"]))).collect();
    check_all("rules.compare", &compare_in, &js_out["compare"], &compare_rs, false);
    let node_rs: Vec<Value> = node_in
        .iter()
        .map(|c| checks_json(&health::node_checks(&c["node"], Some(&c["snap"]).filter(|v| !v.is_null()), js::arr(c.get("findings")), &c["ctx"])))
        .collect();
    check_all("health.nodeChecks", &node_in, &js_out["nodeChecks"], &node_rs, false);
    let app_rs: Vec<Value> = app_in.iter().map(|c| checks_json(&health::app_checks(c))).collect();
    check_all("health.appChecks", &app_in, &js_out["appChecks"], &app_rs, false);
    let jobs_rs: Vec<Value> = jobs_in.iter().map(|j| Value::Array(health::failing_jobs(js::arr(Some(j))).into_iter().cloned().collect())).collect();
    check_all("health.failingJobs", &jobs_in, &js_out["failingJobs"], &jobs_rs, false);
    let redact_rs: Vec<Value> = redact_in.iter().map(|s| json!(logs::redact(s.as_str().unwrap()))).collect();
    check_all("logs.redact", &redact_in, &js_out["redact"], &redact_rs, false);
    let fp_rs: Vec<Value> = fp_in
        .iter()
        .map(|c| json!(logs::fingerprint(c["source"].as_str().unwrap(), c.get("provider"), c.get("event_id"), c["message"].as_str().unwrap())))
        .collect();
    check_all("logs.fingerprint", &fp_in, &js_out["fingerprint"], &fp_rs, false);
    let norm_rs: Vec<Value> = normalize_in
        .iter()
        .map(|c| Value::Array(logs::normalize(c["source"].as_str().unwrap(), js::arr(c.get("rows"))).iter().map(row_json).collect()))
        .collect();
    check_all("logs.normalize", &normalize_in, &js_out["normalize"], &norm_rs, false);
    let plan_rs: Vec<Value> = plan_in
        .iter()
        .map(|c| {
            let node = node_of(&c["node"]);
            let protect: Option<Vec<String>> = c.get("protect").map(|p| js::arr(Some(p)).iter().map(|x| js::string(Some(x))).collect());
            match actions::plan(&node, &c["action"], protect.as_deref()) {
                Ok(p) => json!({
                    "describe": p.describe, "script": p.script, "shell": p.shell, "undo": p.undo, "exits": p.has_exits,
                    "wrap": match actions::wrap(&node, &p) { Ok(s) => json!(s), Err(e) => json!({ "__throw": e }) },
                }),
                Err(e) => json!({ "__throw": e }),
            }
        })
        .collect();
    let plan_thrown = check_all("actions.plan", &plan_in, &js_out["plan"], &plan_rs, false);
    assert_eq!(plan_thrown, 0, "actions.plan: JS だけが例外になる入力があった（Rust 側の検証が JS より甘い可能性）");
    let lf_rs: Vec<Value> = lf_in
        .iter()
        .map(|c| {
            let db = Store::open_in_memory().unwrap();
            for g in js::arr(c.get("logs")) {
                let src = g["source"].as_str().unwrap();
                db.insert_logs("pc", src, &logs::normalize(src, js::arr(g.get("rows"))), now).unwrap();
            }
            for d in js::arr(c.get("dropped")) {
                db.cursor_ok("pc", d["source"].as_str().unwrap(), Some(&json!("1")), 0, &d["n"]).unwrap();
            }
            Value::Array(logs::log_findings(&db, "pc", now).unwrap())
        })
        .collect();
    check_all("logs.logFindings", &lf_in, &js_out["logFindings"], &lf_rs, false);

    // 型の崩れた入力で JS が落ちる割合（多すぎると比べた意味が薄れる）
    eprintln!("rules.analyze: {} 件中 {thrown} 件は JS が例外（比較から除外）", analyze_in.len());
    assert!(thrown * 20 < analyze_in.len(), "JS が例外になる入力が多すぎる: {thrown}");
}

// ---- DB の相互運用 ----
fn write_spec(now: i64) -> Value {
    let mut r = Rng(0xD1B5_4A32_D192_ED03);
    let snaps: Vec<Value> = (0..8)
        .map(|i| {
            let data = snapshot(&mut r);
            let f = rules::analyze(&data, &json!({}));
            let findings: Vec<Value> = f.iter().map(|x| json!({ "id": x["id"], "severity": x["severity"] })).collect();
            let summary = tune_core::engine::summarize(&data, &f);
            json!({ "node_id": if i % 2 == 0 { "n1" } else { "n2" }, "entry": { "at": now - i * 3_600_000, "wall_s": 4.25 + i as f64, "score": rules::score(&f), "findings": findings, "data": data, "summary": summary } })
        })
        .collect();
    let actions = json!([
        { "id": "1-a", "at": now - 5000, "node_id": "n1", "type": "kill-process", "params": { "pid": 500, "name": "Google Drive" }, "label": "n1: 終了", "ok": true, "output": "終了を確認した", "undo": null, "undo_of": null },
        { "id": "2-b", "at": now - 4000, "node_id": "n2", "type": "set-power-plan", "params": { "guid": rules::HIGH_PERF_GUID }, "label": "n2: 高パフォーマンス", "ok": true, "output": "完了", "undo": { "type": "set-power-plan", "params": { "guid": rules::BALANCED_GUID } }, "undo_of": null },
        { "id": "3-c", "at": now - 3000, "node_id": "n2", "type": "set-power-plan", "params": { "guid": rules::BALANCED_GUID }, "label": "n2: 元に戻す", "ok": false, "output": "失敗（exit 1）", "undo_of": "2-b" }
    ]);
    let logs: Vec<Value> = (0..6)
        .map(|k| {
            let rows: Vec<Value> = (0..30)
                .map(|i| json!({ "uid": format!("{k}-{i}"), "ts": now - (i * 3_600_000) - k, "level": (["error", "warn", "info"][i as usize % 3]), "provider": (["disk", "Display", "Microsoft-Windows-WHEA-Logger"][k as usize % 3]), "event_id": "7", "message": format!("device {} WHEA bad block at 0x{i:X} \"q\" ghp_{}", i, "a".repeat(30)) }))
                .collect();
            json!({ "node_id": if k % 2 == 0 { "n1" } else { "n2" }, "source": (["win_system", "win_application", "mac_diag"][k as usize % 3]), "rows": rows, "now": now })
        })
        .collect();
    json!({
        "snapshots": snaps, "actions": actions, "logs": logs,
        "cursors": [
            { "node_id": "n1", "source": "win_system", "cursor": "1791352417838", "count": 30, "dropped": 3 },
            { "node_id": "n1", "source": "win_system", "error": "ssh timeout" },
            { "node_id": "n2", "source": "mac_diag", "cursor": 12345, "count": 2, "dropped": 0 }
        ],
        "checks": [
            { "scope": "n1", "now": now - 1000, "checks": [{ "id": "probe", "name": "分析", "status": "ok", "detail": "1 分前に成功" }, { "id": "disk", "name": "ディスク", "status": "warn", "detail": "C: 8%" }] },
            { "scope": "n1", "now": now, "checks": [{ "id": "probe", "name": "分析", "status": "fail", "detail": "失敗" }] },
            { "scope": "_app", "now": now, "checks": [{ "id": "db", "name": "ローカル DB", "status": "ok", "detail": "整合性 ok" }] }
        ],
        "meta": { "schedule": { "enabled": false, "probe_minutes": 30 }, "lastProbeAt": now - 60_000 }
    })
}

fn read_spec(now: i64) -> Value {
    json!({
        "nodes": ["n1", "n2", "n3"],
        "queries": [{}, { "q": "WHEA" }, { "node_id": "n1" }, { "level": "error", "limit": 5 }, { "since": now - 5 * 3_600_000 }, { "source": "win_system", "q": "disk OR Display" }, { "q": "bad\"query" }, { "limit": 7 }],
        "signatures": [{ "since": 0 }, { "node_id": "n1", "limit": 3 }, { "since": now - 2 * 3_600_000 }],
        "meta": ["schedule", "lastProbeAt", "migr_win_ts_v1", "missing"]
    })
}

fn rust_write(db: &Store, w: &Value) {
    for s in js::arr(w.get("snapshots")) {
        let e = &s["entry"];
        db.add_snapshot(
            s["node_id"].as_str().unwrap(),
            e["at"].as_i64().unwrap(),
            e["wall_s"].as_f64(),
            e["score"].as_i64(),
            &e["findings"],
            &e["data"],
            &e["summary"],
        )
        .unwrap();
    }
    for a in js::arr(w.get("actions")) {
        db.add_action(a).unwrap();
    }
    for l in js::arr(w.get("logs")) {
        let src = l["source"].as_str().unwrap();
        db.insert_logs(l["node_id"].as_str().unwrap(), src, &logs::normalize(src, js::arr(l.get("rows"))), l["now"].as_i64().unwrap()).unwrap();
    }
    for c in js::arr(w.get("cursors")) {
        let (n, s) = (c["node_id"].as_str().unwrap(), c["source"].as_str().unwrap());
        match c.get("error") {
            Some(e) => db.cursor_error(n, s, e.as_str().unwrap()).unwrap(),
            None => db.cursor_ok(n, s, c.get("cursor"), c["count"].as_i64().unwrap(), &c["dropped"]).unwrap(),
        }
    }
    for c in js::arr(w.get("checks")) {
        let checks: Vec<Check> = js::arr(c.get("checks"))
            .iter()
            .map(|x| Check {
                id: js::string(x.get("id")),
                name: js::string(x.get("name")),
                status: js::string(x.get("status")),
                detail: x["detail"].as_str().map(str::to_string),
            })
            .collect();
        db.save_checks(c["scope"].as_str().unwrap(), &checks, c["now"].as_i64().unwrap()).unwrap();
    }
    for (k, v) in w["meta"].as_object().unwrap() {
        db.set_meta(k, v).unwrap();
    }
}

fn rust_dump(db: &Store, spec: &Value) -> Value {
    let mut out = Map::new();
    for id in js::arr(spec.get("nodes")) {
        let id = id.as_str().unwrap();
        let last: Vec<Value> =
            db.last_snapshots(id, 3).unwrap().into_iter().map(|s| json!({ "node_id": s.node_id, "at": s.at, "wall_s": s.wall_s, "data": s.data })).collect();
        out.insert(format!("last:{id}"), Value::Array(last));
        out.insert(format!("history:{id}"), Value::Array(db.history(id, 60).unwrap()));
        out.insert(format!("dropped:{id}"), json!(db.dropped_total(id).unwrap()));
    }
    out.insert("actions".into(), Value::Array(db.actions(100).unwrap()));
    for (i, f) in js::arr(spec.get("queries")).iter().enumerate() {
        out.insert(format!("query:{i}"), db.query_logs(f).map(Value::Array).unwrap_or_else(|e| json!({ "__throw": e.to_string() })));
    }
    for (i, f) in js::arr(spec.get("signatures")).iter().enumerate() {
        out.insert(format!("sig:{i}"), Value::Array(db.signatures(f).unwrap()));
    }
    out.insert("cursors".into(), Value::Array(db.cursors().unwrap()));
    out.insert("checks".into(), Value::Array(db.checks().unwrap()));
    let events: Vec<Value> = db
        .check_events(200)
        .unwrap()
        .into_iter()
        .map(|mut e| {
            e.as_object_mut().unwrap().remove("id");
            e
        })
        .collect();
    out.insert("events".into(), Value::Array(events));
    for k in js::arr(spec.get("meta")) {
        let k = k.as_str().unwrap();
        out.insert(format!("meta:{k}"), db.get_meta(k).unwrap().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

/// 書いた時刻で変わる列（取り込みの位置の更新時刻）を除く
fn stable(mut v: Value) -> Value {
    if let Some(Value::Array(cs)) = v.get_mut("cursors") {
        for c in cs {
            let m = c.as_object_mut().unwrap();
            m.remove("updated_at");
            m.remove("last_ok_at");
        }
    }
    v
}

#[test]
fn db_files_are_interchangeable_with_electron() {
    let now: i64 = 1_800_000_000_000;
    let base = std::env::temp_dir().join(format!("kt-db-parity-{}-{}", std::process::id(), tune_core::db::now_ms()));
    let (dir_rs, dir_js) = (base.join("rust"), base.join("js"));
    let (w, spec) = (write_spec(now), read_spec(now));

    // Rust が書いた DB
    let rust_written = {
        let db = Store::open(&dir_rs).unwrap();
        rust_write(&db, &w);
        rust_dump(&db, &spec)
    };
    let Some(js_out) = run_js(&json!({ "dbRead": { "dir": dir_rs, "spec": spec }, "dbWrite": { "dir": dir_js, "spec": spec, "write": w } })) else { return };

    let diff = |a: &Value, b: &Value| -> String {
        a.as_object()
            .unwrap()
            .iter()
            .filter(|(k, v)| !b.get(k.as_str()).is_some_and(|w| same(v, w, true)))
            .map(|(k, v)| format!("{k}:\n  左: {v}\n  右: {}", b.get(k.as_str()).cloned().unwrap_or(Value::Null)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // 1) Rust が書いた DB を JS（Electron 版の lib/db.js）で読むと、Rust で読んだのと同じ
    assert!(
        same(&js_out["dbRead"], &rust_written, true),
        "Rust が書いた DB を JS（左）と Rust（右）で読んだ結果が違う\n{}",
        diff(&js_out["dbRead"], &rust_written)
    );
    // 2) JS が書いた DB を Rust で読むと、JS で読んだのと同じ
    let js_written_by_rust = rust_dump(&Store::open(&dir_js).unwrap(), &spec);
    assert!(
        same(&js_out["dbWrite"], &js_written_by_rust, true),
        "JS が書いた DB を JS（左）と Rust（右）で読んだ結果が違う\n{}",
        diff(&js_out["dbWrite"], &js_written_by_rust)
    );
    // 3) 同じ操作で書いた2つの DB の中身が同じ（書き方も揃っている）
    assert!(
        same(&stable(rust_written.clone()), &stable(js_written_by_rust.clone()), true),
        "同じ操作で書いた DB の中身が違う（左: Rust が書いた、右: JS が書いた）\n{}",
        diff(&stable(rust_written.clone()), &stable(js_written_by_rust.clone()))
    );
    // 4) 伏せ字が効いていて、全文検索も通る
    let q = rust_written["query:1"].as_array().unwrap();
    assert!(!q.is_empty() && q.iter().all(|r| !r["message"].as_str().unwrap().contains("ghp_aaa")));
    assert!(rust_written["query:6"].get("__throw").is_some(), "壊れた検索式はエラーとして返す");
    assert_eq!(rust_written["query:7"].as_array().unwrap().len(), 7, "limit が効く");
    // ページング（Rust 側だけの拡張。offset が無ければ JS と同じ）
    let db = Store::open(&dir_rs).unwrap();
    let all = db.query_logs(&json!({ "limit": 10 })).unwrap();
    assert_eq!(db.query_logs(&json!({ "limit": 7, "offset": 3 })).unwrap(), all[3..10].to_vec());
    assert_eq!(db.query_logs(&json!({ "limit": 100_000 })).unwrap().len(), 180.min(tune_core::db::QUERY_LIMIT_MAX as usize), "上限は Rust 側で守る");
    drop(db);
    let _ = std::fs::remove_dir_all(&base);
}
