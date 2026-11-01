//! リポジトリの衛生監査（lib/hygiene.js）を仕様として、tune-core の hygiene が同じ入力に同じ出力を返すかを確かめる。
//! 入力は決まった種から作る架空のパス・origin・件数（実在のリポジトリ名などは使わない）。
//! node（24 以上）が要る。無い環境では失敗する（KATALA_TUNE_PARITY=skip で飛ばせる）。

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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
fn normalize_metadata_preserves_unknown() {
    assert!(analyze(&json!({ "repos": [] })).iter().any(|finding| finding["id"] == "repo-unobserved"));
    let raw = json!({ "roots": [], "repos": [], "enumerated": null, "skipped": null, "truncated": null });
    let normalized = normalize(&raw);
    assert!(normalized["enumerated"].is_null());
    assert!(normalized["skipped"].is_null());
    assert!(normalized["truncated"].is_null());
    assert!(analyze(&normalized).iter().any(|finding| finding["id"] == "repo-unobserved"));
}

#[test]
fn unknown_error_is_redacted() {
    let marker = "fixture-only-sensitive-value";
    for error in [json!(""), json!(format!("https://unit-user:{marker}@example.test/r?token={marker}")), json!({ "detail": marker })] {
        let raw =
            json!({ "roots": [{ "path": "~/fixture", "source": "opt" }], "repos": [], "enumerated": 0, "skipped": 0, "truncated": false, "error": error });
        let normalized = normalize(&raw);
        assert_eq!(normalized["error"], "unknown_error");
        assert!(normalized["incomplete_fields"].as_array().unwrap().contains(&json!("error")));
        assert!(analyze(&raw).iter().any(|finding| finding["id"] == "repo-unobserved"));
        assert!(!normalized.to_string().contains(marker));
        assert!(!json!(analyze(&raw)).to_string().contains(marker));
        assert!(!tune_core::hygiene::progress(&raw, 1).1.contains(marker));
    }
    for error in ["git_missing", "walk_incomplete", "excluded", "no_python", "timeout_or_unreachable", "probe_failed", "internal", "unknown_error"] {
        assert_eq!(normalize(&json!({ "error": error }))["error"], error);
    }
}

#[test]
fn metadata_and_progress_match_js() {
    let base = json!({ "roots": [{ "path": "~/fixture", "source": "opt" }], "repos": [], "enumerated": 0, "skipped": 0, "truncated": false });
    let row =
        json!({ "state": "ok", "path": "~/fixture", "linked": false, "tracked": 1, "untracked": 0, "conflicted": 0, "detached": false, "upstream": false });
    let mut cases = vec![base.clone()];
    for field in ["enumerated", "skipped", "truncated", "roots", "repos"] {
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove(field);
        cases.push(missing);
        for value in [json!(null), json!("bad"), json!({})] {
            let mut raw = base.clone();
            raw[field] = value;
            cases.push(raw);
        }
    }
    for field in ["enumerated", "skipped"] {
        for value in [json!(-1), json!(0.5), json!(9_007_199_254_740_992_u64)] {
            let mut raw = base.clone();
            raw[field] = value;
            cases.push(raw);
        }
    }
    let mut boundary = base.clone();
    boundary["enumerated"] = json!(9_007_199_254_740_991_u64);
    boundary["skipped"] = json!(9_007_199_254_740_991_u64);
    cases.push(boundary);
    let mut roots_partial = base.clone();
    roots_partial["roots"] = json!([{ "path": "~/fixture", "source": "opt" }, null]);
    cases.push(roots_partial);
    let mut partial = base.clone();
    partial["enumerated"] = json!(1.0);
    partial["repos"] = json!([row.clone()]);
    partial["error"] = json!("walk_incomplete");
    cases.push(partial.clone());
    let mut missing_git = base.clone();
    missing_git["error"] = json!("git_missing");
    cases.push(missing_git);
    let mut truncated = base.clone();
    truncated["truncated"] = json!(true);
    cases.push(truncated);
    for (n, s) in [(json!(0), json!(0)), (json!(3), json!(1)), (json!(1), json!(2)), (json!(0), json!(null))] {
        let mut raw = base.clone();
        raw["enumerated"] = n;
        raw["skipped"] = s;
        raw["repos"] = json!([row.clone()]);
        cases.push(raw);
    }
    for value in [json!(false), json!(true), json!(0), json!({}), json!([])] {
        let mut raw = base.clone();
        raw["error"] = value;
        cases.push(raw);
    }
    for value in [
        json!(""),
        json!("https://unit-user:fixture-only-sensitive-value@example.test/r?token=fixture-only-sensitive-value"),
        json!({ "detail": "fixture-only-sensitive-value" }),
        json!("no_python"),
        json!("timeout_or_unreachable"),
        json!("probe_failed"),
        json!("excluded"),
        json!("internal"),
        json!("unknown_error"),
    ] {
        let mut raw = base.clone();
        raw["error"] = value;
        cases.push(raw);
    }
    for value in [json!(null), json!("roots"), json!(false), json!({}), json!(["unknown-marker"]), json!(["roots", "unknown-marker"])] {
        let mut raw = base.clone();
        raw["incomplete_fields"] = value;
        cases.push(raw);
    }
    for root in [json!({ "path": "", "source": "opt" }), json!({ "path": "~/fixture", "source": "" })] {
        let mut raw = base.clone();
        raw["roots"] = json!([root]);
        cases.push(raw);
    }
    let mut no_roots = base.clone();
    no_roots["roots"] = json!([]);
    cases.push(no_roots.clone());
    no_roots["error"] = json!("git_missing");
    cases.push(no_roots);

    let js_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("lib").join("hygiene.js");
    let mut child = Command::new("node")
        .arg("--no-warnings")
        .arg("-e")
        .arg("const h=require(process.argv[1]);const fs=require('node:fs');const a=JSON.parse(fs.readFileSync(0,'utf8'));console.log(JSON.stringify(a.map(x=>{const n=h.normalize(x);const f=h.analyze(n);return {normalized:n,findings:f,progress:h.progress({...n,node_id:'node-a',wall_s:1.25},f.length)}})))")
        .arg(js_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("node が必要");
    child.stdin.take().unwrap().write_all(serde_json::to_string(&cases).unwrap().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "JS 評価が失敗");
    let js: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(js.len(), cases.len());
    for (i, (raw, j)) in cases.iter().zip(&js).enumerate() {
        let normalized = normalize(raw);
        let findings = analyze(&normalized);
        let mut audit = normalized.clone();
        audit["node_id"] = json!("node-a");
        audit["wall_s"] = json!(1.25);
        let (observed, line) = tune_core::hygiene::progress(&audit, findings.len());
        let rust = json!({ "normalized": normalized, "findings": findings, "progress": { "observed": observed, "line": &line } });
        assert!(same(j, &rust), "合成入力 #{i} の JS と Rust が不一致");
        assert!(!rust.to_string().contains("fixture-only-sensitive-value"));
        if i == 0 {
            assert!(line.contains("見つけた 0 件・未観測 0 件"));
        } else if i != 27 {
            assert!(line.contains("未観測 件数不明"), "合成入力 #{i} の進捗が不明を示さない");
        }
    }
    let normalized = normalize(&partial);
    assert_eq!(normalized["error"], "walk_incomplete");
    assert_eq!(normalized["repos"][0]["tracked"], 1);
    let findings = analyze(&normalized);
    assert!(findings.iter().any(|v| v["id"] == "repo-unobserved"));
    assert!(findings.iter().any(|v| v["id"] == "repo-dirty"));
}

#[test]
fn progress_raw_inputs_match_js() {
    let base = json!({
        "roots": [{ "path": "~/fixture", "source": "opt" }], "repos": [], "enumerated": 0, "skipped": 0, "truncated": false,
        "node_id": "node-a", "wall_s": 1.25,
    });
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("wall_s");
    let mut cases = vec![("valid", base.clone()), ("time_missing", missing)];
    for value in [json!(null), json!("1"), json!(false), json!(-1), json!({}), json!([])] {
        let mut raw = base.clone();
        raw["wall_s"] = value;
        cases.push(("time_invalid", raw));
    }
    let mut zero = base.clone();
    zero["wall_s"] = json!(0);
    cases.push(("time_zero", zero));
    for value in [json!(false), json!(true), json!(0), json!({}), json!([])] {
        let mut raw = base.clone();
        raw["error"] = value;
        cases.push(("error_invalid", raw));
    }
    let mut unknown = base.clone();
    unknown["error"] = json!("https://unit-user:fixture-only-sensitive-value@example.test/r");
    cases.push(("error_invalid", unknown));
    let mut no_roots = base.clone();
    no_roots["roots"] = json!([]);
    cases.push(("no_roots", no_roots));
    let raw: Vec<&Value> = cases.iter().map(|(_, v)| v).collect();
    let js_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("lib").join("hygiene.js");
    let mut child = Command::new("node")
        .arg("--no-warnings")
        .arg("-e")
        .arg("const h=require(process.argv[1]);const fs=require('node:fs');const a=JSON.parse(fs.readFileSync(0,'utf8'));console.log(JSON.stringify(a.map(x=>h.progress(x,0))))")
        .arg(js_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("node が必要");
    child.stdin.take().unwrap().write_all(serde_json::to_string(&raw).unwrap().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "JS 評価が失敗");
    let js: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(js.len(), cases.len());
    for ((kind, raw), j) in cases.iter().zip(js) {
        let (observed, line) = tune_core::hygiene::progress(raw, 0);
        assert!(same(&j, &json!({ "observed": observed, "line": &line })), "{kind} の JS と Rust が不一致");
        assert!(line.starts_with("node-a:"));
        match *kind {
            "valid" => assert!(line.contains("未観測 0 件・1.3s")),
            "time_zero" => assert!(line.contains("0.0s")),
            "time_missing" | "time_invalid" => assert!(line.contains("時間不明")),
            "error_invalid" | "no_roots" => assert!(line.contains("未観測 件数不明・1.3s")),
            _ => unreachable!(),
        }
    }
}

#[test]
fn normalize_observation_matches_js() {
    let valid = json!({
        "path": "~/fixture", "rel": "example.com/acme/fixture", "linked": false, "state": "ok",
        "branch": null, "detached": false, "upstream": false, "ahead": null, "behind": null, "local_only": null,
        "tracked": 0, "untracked": 0, "conflicted": 0, "dirty_age_s": null, "head_age_s": null, "stash": null,
        "origin": null, "origin_host": null, "origin_path": null, "has_origin": null,
    });
    let mut second = valid.clone();
    second["tracked"] = json!(1.0);
    second["untracked"] = json!(2);
    second["conflicted"] = json!(1);
    second["linked"] = json!(true);
    second["detached"] = json!(true);
    second["upstream"] = json!(true);
    let mut boundary = valid.clone();
    boundary["tracked"] = json!(9_007_199_254_740_991_u64);
    let mut missing_optional = valid.clone();
    for key in ["ahead", "behind", "local_only", "stash"] {
        missing_optional.as_object_mut().unwrap().remove(key);
    }
    missing_optional["dirty_age_s"] = json!(-10);
    missing_optional["head_age_s"] = json!(-20);
    let mut optional_boundary = valid.clone();
    optional_boundary["ahead"] = json!(9_007_199_254_740_991_u64);
    optional_boundary["behind"] = json!(1.0);
    let mut whitespace_path = valid.clone();
    whitespace_path["path"] = json!(" ");
    let mut cases = vec![
        json!({ "repos": [valid.clone()] }),
        json!({ "repos": [second] }),
        json!({ "repos": [boundary] }),
        json!({ "repos": [missing_optional] }),
        json!({ "repos": [optional_boundary] }),
        json!({ "repos": [whitespace_path] }),
    ];
    let counts = vec![json!(null), json!("0"), json!(false), json!(-1), json!(-0.5), json!(1.5), json!(9_007_199_254_740_992_u64), json!({}), json!([])];
    let flags = vec![json!(null), json!("false"), json!(0), json!(1), json!({}), json!([])];
    for (fields, values) in [(&["tracked", "untracked", "conflicted"][..], &counts), (&["upstream", "detached", "linked"][..], &flags)] {
        for field in fields {
            for value in std::iter::once(None).chain(values.iter().cloned().map(Some)) {
                let mut row = valid.clone();
                if let Some(value) = value {
                    row[*field] = value;
                } else {
                    row.as_object_mut().unwrap().remove(*field);
                }
                row["reason"] = json!("timeout");
                cases.push(json!({ "repos": [row] }));
            }
        }
    }
    let mut inconsistent = valid.clone();
    inconsistent["conflicted"] = json!(1);
    cases.push(json!({ "repos": [inconsistent] }));
    let invalid_optional = vec![json!("1"), json!(false), json!(-1), json!(-0.5), json!(0.5), json!(9_007_199_254_740_992_u64), json!({}), json!([])];
    for field in ["ahead", "behind", "local_only", "stash"] {
        for value in &invalid_optional {
            let mut row = valid.clone();
            row[field] = value.clone();
            cases.push(json!({ "repos": [row] }));
        }
    }
    for value in [None, Some(json!(null)), Some(json!("")), Some(json!(0)), Some(json!({}))] {
        let mut row = valid.clone();
        if let Some(value) = value {
            row["path"] = value;
        } else {
            row.as_object_mut().unwrap().remove("path");
        }
        cases.push(json!({ "repos": [row] }));
    }

    // stdin を使い、実リポジトリの調査や一時ファイルの作成を行わない。
    let js_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("lib").join("hygiene.js");
    let mut child = Command::new("node")
        .arg("--no-warnings")
        .arg("-e")
        .arg("const h=require(process.argv[1]);const fs=require('node:fs');const a=JSON.parse(fs.readFileSync(0,'utf8'));console.log(JSON.stringify(a.map(x=>{const n=h.normalize(x);return {normalized:n,findings:h.analyze(n),matrix:h.matrix([{node_id:'node-a',...n}])}})))")
        .arg(js_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("node が必要");
    child.stdin.take().unwrap().write_all(serde_json::to_string(&cases).unwrap().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "JS 評価が失敗");
    let js: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let rust: Vec<Value> = cases
        .iter()
        .map(|raw| {
            let normalized = normalize(raw);
            let findings = analyze(&normalized);
            let mut audit = normalized.clone();
            audit["node_id"] = json!("node-a");
            json!({ "normalized": normalized, "findings": findings, "matrix": matrix(&[audit]) })
        })
        .collect();
    assert_eq!(js.len(), rust.len());
    for (i, (j, r)) in js.iter().zip(&rust).enumerate() {
        assert!(same(j, r), "合成入力 #{i} の JS と Rust が不一致");
        let row = &r["normalized"]["repos"][0];
        if i < 6 {
            assert_eq!(row["state"], "ok");
            for key in ["branch", "local_only", "stash", "origin"] {
                assert!(row[key].is_null(), "任意項目 {key} の null が失われた");
            }
            if i != 4 {
                assert!(row["ahead"].is_null());
                assert!(row["behind"].is_null());
            }
            if i == 3 {
                assert_eq!(row["dirty_age_s"], -10);
                assert_eq!(row["head_age_s"], -20);
            } else {
                assert!(row["dirty_age_s"].is_null());
                assert!(row["head_age_s"].is_null());
            }
        } else {
            assert_eq!(row["state"], "unobserved", "合成入力 #{i}");
            assert_eq!(row["reason"], "git_failed", "合成入力 #{i}");
            assert_eq!(r["findings"][0]["id"], "repo-unobserved", "合成入力 #{i}");
            assert_eq!(r["findings"].as_array().unwrap().len(), 1);
        }
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
