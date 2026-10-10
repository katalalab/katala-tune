//! リポジトリの衛生監査（lib/hygiene.js）を仕様として、tune-core の hygiene が同じ入力に同じ出力を返すかを確かめる。
//! 入力は決まった種から作る架空のパス・origin・件数（実在のリポジトリ名などは使わない）。
//! node（24 以上）が要る。無い環境では失敗する（KATALA_TUNE_PARITY=skip で飛ばせる）。

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tune_core::hygiene::{analyze, matrix, normalize, path_status, script, strip_url};

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
}

const NODES: &[&str] = &["node-a", "node-b", "node-c"];
const OWNERS: &[&str] = &["acme", "Acme", "widgets", "日本語"];
const REPOS: &[&str] = &["alpha", "Alpha", "beta", "gamma-tool", "デルタ", "x"];
const HOSTS: &[&str] = &["example.com", "Example.com", "gitlab.example", "git.example.org"];
const URLS: &[&str] = &[
    "https://example.com/acme/alpha.git",
    "https://user:tok@example.com/acme/alpha.git?x=1#f",
    "git@example.com:acme/alpha.git",
    "usr:pw@example.com:acme/alpha.git",
    "ssh://git@example.com:22/acme/alpha.git",
    "(local)",
    "(unparsed)",
    "",
];

fn raw_repo(r: &mut Rng) -> Value {
    let host = *r.pick(HOSTS);
    let owner = *r.pick(OWNERS);
    let repo = *r.pick(REPOS);
    let linked = r.chance(0.15);
    let rel: Value = match r.below(10) {
        0 => Value::Null,
        1 => json!(format!(".worktrees/{repo}")),
        2 => json!(repo),
        _ => json!(format!("{}/{}/{}", r.pick(HOSTS), r.pick(OWNERS), r.pick(REPOS))),
    };
    let path = format!("~/ghq/{}", rel.as_str().unwrap_or("outside/x"));
    if r.chance(0.12) {
        let reason = *r.pick(&["timeout", "git_failed", "over_limit", "weird", ""]);
        return json!({ "state": "unobserved", "reason": reason, "path": path, "rel": rel, "linked": linked });
    }
    let upstream = r.chance(0.7);
    let (ahead, behind, local_only): (Value, Value, Value) = if upstream && r.chance(0.9) {
        (json!(r.below(4)), json!(r.below(4)), Value::Null)
    } else {
        (Value::Null, Value::Null, if r.chance(0.8) { json!(r.below(3)) } else { Value::Null })
    };
    let has_origin = match r.below(10) {
        0 => json!(false),
        1 => Value::Null,
        _ => json!(true),
    };
    let (oh, op) = if r.chance(0.8) { (json!(host.to_lowercase()), json!(format!("{owner}/{repo}"))) } else { (Value::Null, Value::Null) };
    json!({
        "state": "ok", "path": path, "rel": rel, "linked": linked, "branch": if r.chance(0.9) { json!("main") } else { Value::Null }, "detached": r.chance(0.05),
        "upstream": upstream, "ahead": ahead, "behind": behind, "local_only": local_only,
        "tracked": if r.chance(0.4) { r.below(5) } else { 0 }, "untracked": if r.chance(0.4) { r.below(9) } else { 0 }, "conflicted": if r.chance(0.05) { 1 } else { 0 },
        "dirty_age_s": match r.below(5) { 0 => Value::Null, 1 => json!(r.below(3600)), 2 => json!(r.below(90_000)), _ => json!(r.below(40 * 86_400)) },
        "head_age_s": r.below(1_000_000), "stash": match r.below(6) { 0 => Value::Null, 1 => json!(r.below(4)), _ => json!(0) },
        "origin": *r.pick(URLS), "origin_host": oh, "origin_path": op, "has_origin": has_origin,
        "diff": "must-not-appear",
    })
}

fn raw_audit(r: &mut Rng) -> Value {
    let n = r.below(14);
    let mut repos: Vec<Value> = (0..n).map(|_| raw_repo(r)).collect();
    if r.chance(0.1) {
        repos.push(Value::Null);
    }
    json!({
        "probe": "repo_hygiene", "version": "x", "os": "darwin", "roots": [{ "path": "~/ghq", "source": "ghq" }, 5],
        "enumerated": n + r.below(3), "skipped": if r.chance(0.3) { r.below(50) } else { 0 }, "truncated": r.chance(0.15),
        "elapsed_s": if r.chance(0.8) { json!(1.5) } else { Value::Null },
        "error": if r.chance(0.08) { json!("git_missing") } else { Value::Null }, "repos": repos, "extra": "dropped",
    })
}

fn run_js(cases: &Value) -> Option<Value> {
    if std::env::var("KATALA_TUNE_PARITY").as_deref() == Ok("skip") {
        eprintln!("KATALA_TUNE_PARITY=skip のため JS との比較を飛ばした");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("kt-parity-hyg-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("cases.json");
    std::fs::write(&f, cases.to_string()).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("js").join("eval_hygiene.js");
    let out = Command::new("node").arg("--no-warnings").arg(&script).arg(&f).output().expect("node が要る（KATALA_TUNE_PARITY=skip で飛ばせる）");
    assert!(out.status.success(), "eval_hygiene.js が失敗: {}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    Some(serde_json::from_slice(&out.stdout).expect("eval_hygiene.js の出力が JSON ではない"))
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

fn check(kind: &str, inputs: &[Value], js: &Value, rust: &[Value]) {
    let js = js[kind].as_array().unwrap_or_else(|| panic!("{kind}: JS の結果が無い"));
    assert_eq!(js.len(), rust.len(), "{kind}: 件数が違う");
    for (i, (j, r)) in js.iter().zip(rust).enumerate() {
        assert!(same(j, r), "{kind}: #{i} が JS と違う\n入力: {}\nJS:   {j}\nRust: {r}", inputs[i]);
    }
}

#[test]
fn hygiene_matches_js() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let raws: Vec<Value> = (0..300).map(|_| raw_audit(&mut r)).collect();
    let normalized: Vec<Value> = raws.iter().map(normalize).collect();
    let urls: Vec<Value> = URLS
        .iter()
        .map(|s| json!(s))
        .chain([json!("a"), json!("https://u:p@h/x?q"), json!("https://a:b@c@example.com/x"), json!(null), json!("x".repeat(400))])
        .collect();
    let rows: Vec<Value> = normalized.iter().flat_map(|a| a["repos"].as_array().cloned().unwrap_or_default()).collect();
    // 複数機体の表: 機体ごとに別々の調査結果を割り当てる（同じ機体 ID が重なる場合も含む）
    let matrices: Vec<Value> = (0..120)
        .map(|_| {
            json!(
                (0..r.below(4) + 1)
                    .map(|_| {
                        let mut a = r.pick(&normalized).clone();
                        a["node_id"] = json!(*r.pick(NODES));
                        a
                    })
                    .collect::<Vec<_>>()
            )
        })
        .collect();
    let scripts: Vec<Value> = vec![
        json!({}),
        json!({ "max_repos": 99999, "budget_s": 1, "cmd_s": "x", "roots": ["~/a", 5, "", "日本語"], "ghq_root": "/r" }),
        json!({ "max_repos": 7.9, "budget_s": 300.5, "cmd_s": 0, "roots": (0..12).map(|i| format!("/r{i}")).collect::<Vec<_>>(), "ghq_root": "   " }),
    ];

    let cases = json!({ "stripUrl": urls, "normalize": raws, "analyze": normalized, "pathStatus": rows, "matrix": matrices, "script": scripts });
    let Some(js) = run_js(&cases) else { return };

    let rs_urls: Vec<Value> = urls.iter().map(|u| strip_url(u.as_str()).map_or(Value::Null, Value::from)).collect();
    check("stripUrl", &urls, &js, &rs_urls);
    check("normalize", &raws, &js, &normalized);
    let rs_an: Vec<Value> = normalized.iter().map(|a| json!(analyze(a))).collect();
    check("analyze", &normalized, &js, &rs_an);
    let rs_ps: Vec<Value> = rows.iter().map(|x| json!(path_status(x))).collect();
    check("pathStatus", &rows, &js, &rs_ps);
    let rs_mx: Vec<Value> = matrices.iter().map(|m| json!(matrix(m.as_array().unwrap()))).collect();
    check("matrix", &matrices, &js, &rs_mx);
    // script は、埋め込んだ辞書が同じで、それ以外の行が変わっていないこと
    let opts_line =
        |s: &str| s.lines().find(|l| l.starts_with("KT_OPTS = ")).map(|l| serde_json::from_str::<Value>(l.trim_start_matches("KT_OPTS = ")).unwrap()).unwrap();
    let rs_sc: Vec<Value> = scripts.iter().map(|o| opts_line(&script(o))).collect();
    let js_sc: Vec<Value> = js["script"].as_array().unwrap().iter().map(|s| opts_line(s.as_str().unwrap())).collect();
    for (a, b) in js_sc.iter().zip(&rs_sc) {
        assert!(same(a, b), "script: {a} と {b}");
    }
    for s in &scripts {
        assert_eq!(
            script(s).replace(&format!("KT_OPTS = {}", opts_line(&script(s))), "KT_OPTS = {}").split('\n').count(),
            tune_core::hygiene::PROBE.split('\n').count()
        );
    }

    // 内容を持ち込んだ入力でも、出力に残らない
    assert!(!normalized.iter().any(|a| a.to_string().contains("must-not-appear")));
    assert!(!normalized.iter().any(|a| a.to_string().contains("tok@") || a.to_string().contains("usr:pw")));
    assert!(normalized.iter().flat_map(analyze).count() > 100, "所見がほとんど出ない入力は検証にならない");
}
