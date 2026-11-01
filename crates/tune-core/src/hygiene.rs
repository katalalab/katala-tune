//! リポジトリの衛生監査（lib/hygiene.js と同じ）: 各機体の開発リポジトリの dirty・未 push・stash・ghq のパスと origin の不一致を、
//! 読み取り専用で集める。調査は probes/repo_hygiene.py（git の読み取りコマンドだけ。fetch しない・内容や差分を読まない・stderr を捨てる）。
//! 台帳の機体ごとの設定: "repo_hygiene": false で対象外（書いていなければ共用機は対象外）、"repo_roots": ["~/work"] で ghq root のほかの置き場を足す。
//! 保存はしない（アプリの分析・自動スキャンには入っていない。`tune hygiene` から呼ぶ）。結果は JS 版と同じ（tests/parity_hygiene.rs）。

use std::cmp::Ordering;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::ai_sessions::{NO_PYTHON, run_python_strict};
use crate::collect::{CompletionFailure, StrictRunResult, last_json_line};
use crate::db::now_ms;
use crate::js::{self, Obj};
use crate::nodes::Node;

pub const PROBE: &str = include_str!("../../../probes/repo_hygiene.py");

const DAY: i64 = 86_400;
const STALE_DIRTY_DAYS: i64 = 7;
const LIST_MAX: usize = 5;
const DEFAULT_MAX_REPOS: i64 = 1000;
const DEFAULT_BUDGET_S: i64 = 60;
const DEFAULT_CMD_S: i64 = 12;

fn int(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite()).map(|x| x.trunc() as i64)
}
fn count(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_f64).filter(|n| n.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(n) && n.fract() == 0.0).map(|n| n as i64)
}
fn text(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
}
fn opt_s(v: Option<&Value>) -> Value {
    text(v).map_or(Value::Null, Value::from)
}
fn opt_i(v: Option<&Value>) -> Value {
    int(v).map_or(Value::Null, Value::from)
}

/// Do not present the number of unexplored repositories as known.
pub fn walk_coverage_note(audit: &Value) -> &'static str {
    if text(audit.get("error")) == Some("walk_incomplete") || audit.get("truncated") == Some(&Value::Bool(true)) {
        "・探索未確認あり（件数不明）"
    } else {
        ""
    }
}

/// UTF-16 の単位で比べる（JS の `<`）
fn cmp16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
fn lower_ascii(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// 台帳の機体で集めるか。運用対象（online）でなければ集めない。書いていなければ共用機は集めない（他の人のリポジトリのパスを集めないため）
pub fn enabled(node: &Node) -> bool {
    if !node.is_active() {
        return false;
    }
    let h = node.get("repo_hygiene");
    h == Some(&Value::Bool(true)) || (h != Some(&Value::Bool(false)) && node.get("shared") != Some(&Value::Bool(true)))
}

fn clamp(v: Option<&Value>, lo: i64, hi: i64, d: i64) -> i64 {
    int(v).map_or(d, |n| n.clamp(lo, hi))
}

/// 調査スクリプトに上限と置き場を埋め込む。数と文字列だけなので、JSON がそのまま Python の辞書として読める
pub fn script(opts: &Value) -> String {
    let g = |k: &str| js::get(Some(opts), k);
    let roots: Vec<Value> = js::arr(g("roots")).iter().filter(|r| r.as_str().is_some_and(|s| !js::trim(s).is_empty())).take(8).cloned().collect();
    let mut lit = Map::new();
    lit.insert("max_repos".into(), clamp(g("max_repos"), 1, 5000, DEFAULT_MAX_REPOS).into());
    lit.insert("budget_s".into(), clamp(g("budget_s"), 5, 300, DEFAULT_BUDGET_S).into());
    lit.insert("cmd_s".into(), clamp(g("cmd_s"), 1, 60, DEFAULT_CMD_S).into());
    lit.insert("roots".into(), Value::Array(roots));
    if let Some(r) = text(g("ghq_root")).filter(|s| !js::trim(s).is_empty()) {
        lit.insert("ghq_root".into(), r.into());
    }
    let line = format!("KT_OPTS = {}", Value::Object(lit));
    let mut out = String::with_capacity(PROBE.len() + line.len());
    for (i, l) in PROBE.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(if l.starts_with("KT_OPTS = {}") { &line } else { l });
    }
    out
}

/// 1台を調べる（この機体ならローカル、ほかは ssh で標準入力へ）。opts は [`script`] と同じ（roots は台帳の repo_roots を足す）
pub async fn audit_node(node: &Node, opts: &Value) -> Value {
    // 運用対象でない機体（retired など）には接続しない
    if node.refusal().is_some() {
        return json!({ "node_id": node.id, "ok": false, "error": "excluded", "wall_s": 0.0, "at": now_ms() });
    }
    let started = Instant::now();
    let mut o = opts.clone();
    let mut roots: Vec<Value> = js::arr(js::get(Some(opts), "roots")).to_vec();
    roots.extend(js::arr(node.get("repo_roots")).iter().cloned());
    o["roots"] = Value::Array(roots);
    let budget = clamp(js::get(Some(opts), "budget_s"), 5, 300, DEFAULT_BUDGET_S);
    let res = run_python_strict(node, &script(&o), Duration::from_secs((budget + 40) as u64)).await;
    let wall_s = started.elapsed().as_secs_f64();
    let mut v = parse_run_result(&res);
    v["node_id"] = node.id.clone().into();
    v["wall_s"] = json!(wall_s);
    v["at"] = now_ms().into();
    v
}

fn parse_run_result(res: &StrictRunResult) -> Value {
    let r = &res.result;
    let no_python = r.out.lines().any(|line| js::trim(line) == NO_PYTHON);
    if res.failure.is_some() || r.code != Some(0) {
        let error = match res.failure {
            Some(CompletionFailure::Timeout) => "timeout",
            Some(CompletionFailure::SpawnMissing) if no_python => "no_python",
            Some(_) => "incomplete",
            None if r.code.is_none() => "incomplete",
            None => "probe_failed",
        };
        return json!({ "ok": false, "error": error });
    }
    match last_json_line(&r.out).filter(|d| text(d.get("probe")) == Some("repo_hygiene")) {
        Some(data) => {
            let mut v = normalize(&data);
            v["ok"] = true.into();
            v
        }
        None => json!({ "ok": false, "error": if no_python { "no_python" } else { "probe_failed" } }),
    }
}

/// 対象の機体をまとめて並列に調べる（結果は nodes と同じ順）
pub async fn audit_all(nodes: &[Node], opts: &Value) -> Vec<Value> {
    let mut set = tokio::task::JoinSet::new();
    for (i, n) in nodes.iter().cloned().enumerate() {
        let o = opts.clone();
        set.spawn(async move { (i, audit_node(&n, &o).await) });
    }
    let mut out: Vec<Option<Value>> = vec![None; nodes.len()];
    while let Some(Ok((i, r))) = set.join_next().await {
        out[i] = Some(r);
    }
    out.into_iter().enumerate().map(|(i, r)| r.unwrap_or_else(|| json!({ "node_id": nodes[i].id, "ok": false, "error": "internal", "at": now_ms() }))).collect()
}

static RE_SCHEME_AUTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([A-Za-z][A-Za-z0-9+.-]*://)[^/]*@").expect("scheme"));
static RE_SCP_AUTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^/@:\s]+(?::[^@/\s]*)?@([^/@\s]+:)").expect("scp"));
static RE_QUERY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[?#].*$").expect("query"));

/// 認証部（user:pass@）・クエリ・フラグメントを URL から落とす（stripUrl）
pub fn strip_url(s: Option<&str>) -> Option<String> {
    let v = s?;
    let a = RE_SCHEME_AUTH.replace(v, "$1");
    let b = RE_SCP_AUTH.replace(&a, "$1");
    let c = RE_QUERY.replace(&b, "");
    Some(js::slice16(&c, 300))
}

fn normalize_repo(r: &Value) -> Value {
    let x = Some(r);
    let g = |k: &str| js::get(x, k);
    let state_ok = text(g("state")) == Some("ok");
    // JS で正確に表せる非負整数だけを受け入れ、表記上の 1.0 も 1 と同じように扱う。
    let is_count = |k: &str| g(k).and_then(Value::as_f64).is_some_and(|n| n.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&n) && n.fract() == 0.0);
    let ok = state_ok
        && text(g("path")).is_some_and(|p| !p.is_empty())
        && g("linked").is_some_and(Value::is_boolean)
        && ["tracked", "untracked", "conflicted"].iter().all(|k| is_count(k))
        && ["ahead", "behind", "local_only", "stash"].iter().all(|k| g(k).is_none_or(Value::is_null) || is_count(k))
        && g("conflicted").and_then(Value::as_f64).zip(g("tracked").and_then(Value::as_f64)).is_some_and(|(c, t)| c <= t)
        && g("detached").is_some_and(Value::is_boolean)
        && g("upstream").is_some_and(Value::is_boolean);
    let reason = if ok {
        Value::Null
    } else {
        let t = text(g("reason")).unwrap_or("");
        json!(if !state_ok && matches!(t, "timeout" | "git_failed" | "over_limit" | "probe_failed") { t } else { "git_failed" })
    };
    let base = Obj::new()
        .set("path", text(g("path")).unwrap_or(""))
        .set("rel", opt_s(g("rel")))
        .set("linked", g("linked").and_then(Value::as_bool).map_or(Value::Null, Value::Bool))
        .set("state", if ok { "ok" } else { "unobserved" })
        .set("reason", reason);
    if !ok {
        return base.build();
    }
    base.set("branch", opt_s(g("branch")))
        .set("detached", g("detached") == Some(&Value::Bool(true)))
        .set("upstream", g("upstream") == Some(&Value::Bool(true)))
        .set("ahead", opt_i(g("ahead")))
        .set("behind", opt_i(g("behind")))
        .set("local_only", opt_i(g("local_only")))
        .set("tracked", int(g("tracked")).unwrap_or(0))
        .set("untracked", int(g("untracked")).unwrap_or(0))
        .set("conflicted", int(g("conflicted")).unwrap_or(0))
        .set("dirty_age_s", opt_i(g("dirty_age_s")))
        .set("head_age_s", opt_i(g("head_age_s")))
        .set("stash", opt_i(g("stash")))
        .set("origin", strip_url(text(g("origin"))).map_or(Value::Null, Value::from))
        .set("origin_host", opt_s(g("origin_host")))
        .set("origin_path", opt_s(g("origin_path")))
        .set(
            "has_origin",
            match g("has_origin") {
                Some(Value::Bool(b)) => Value::Bool(*b),
                _ => Value::Null,
            },
        )
        .build()
}

/// 調査の出力を、知っている項目だけの形に揃える（余分な項目は捨てる）
pub fn normalize(data: &Value) -> Value {
    let g = |k: &str| js::get(Some(data), k);
    let enumerated = count(g("enumerated"));
    let skipped = count(g("skipped"));
    let old_incomplete = js::arr(g("incomplete_fields"));
    let old_has = |field: &str| old_incomplete.iter().any(|v| v.as_str() == Some(field));
    let labels = ["roots", "repos", "counts", "error", "metadata"];
    let bad_metadata =
        g("incomplete_fields").is_some_and(|v| v.as_array().is_none_or(|fields| fields.iter().any(|v| v.as_str().is_none_or(|s| !labels.contains(&s)))));
    let bad_roots = g("roots").and_then(Value::as_array).is_none_or(|roots| {
        roots.iter().any(|r| text(js::get(Some(r), "path")).is_none_or(str::is_empty) || text(js::get(Some(r), "source")).is_none_or(str::is_empty))
    });
    let bad_repos = g("repos").and_then(Value::as_array).is_none();
    let bad_error = g("error").is_some_and(|v| {
        !v.is_null()
            && !matches!(
                v.as_str(),
                Some(
                    "git_missing"
                        | "walk_incomplete"
                        | "excluded"
                        | "no_python"
                        | "timeout"
                        | "incomplete"
                        | "timeout_or_unreachable"
                        | "probe_failed"
                        | "internal"
                        | "unknown_error"
                )
            )
    });
    let bad_counts = enumerated.is_some_and(|n| {
        g("repos").and_then(Value::as_array).is_some_and(|repos| {
            let n = n as u128;
            let len = repos.len() as u128;
            n < len || skipped.is_some_and(|s| n != len + s as u128)
        })
    });
    let incomplete: Vec<&str> = [("roots", bad_roots), ("repos", bad_repos), ("counts", bad_counts), ("error", bad_error), ("metadata", bad_metadata)]
        .into_iter()
        .filter_map(|(field, bad)| (bad || old_has(field)).then_some(field))
        .collect();
    let roots: Vec<Value> = js::arr(g("roots"))
        .iter()
        .map(|r| json!({ "path": text(js::get(Some(r), "path")).unwrap_or(""), "source": text(js::get(Some(r), "source")).unwrap_or("") }))
        .collect();
    let mut result = json!({
        "version": opt_s(g("version")), "os": opt_s(g("os")), "roots": roots,
        "enumerated": enumerated, "skipped": skipped,
        "truncated": g("truncated").filter(|v| v.is_boolean()).cloned().unwrap_or(Value::Null),
        "elapsed_s": g("elapsed_s").filter(|v| v.is_number()).cloned().unwrap_or(Value::Null),
        "error": if bad_error { json!("unknown_error") } else { opt_s(g("error")) },
        "repos": js::arr(g("repos")).iter().map(normalize_repo).collect::<Vec<_>>(),
    });
    if !incomplete.is_empty() {
        result["incomplete_fields"] = json!(incomplete);
    }
    result
}

/// ghq の期待パス（<host>/<owner>/<repo>、小文字）。origin から導けなければ None
pub fn expected_rel(r: &Value) -> Option<String> {
    let x = Some(r);
    match (text(js::get(x, "origin_host")), text(js::get(x, "origin_path"))) {
        (Some(h), Some(p)) if !h.is_empty() && !p.is_empty() => Some(lower_ascii(&format!("{h}/{p}"))),
        _ => None,
    }
}

/// "linked" | "outside" | "unknown" | "no_origin" | "ok" | "mismatch"。リンク worktree と ghq root の外・.worktrees 配下は照合しない
pub fn path_status(r: &Value) -> &'static str {
    let x = Some(r);
    if js::get(x, "linked") == Some(&Value::Bool(true)) {
        return "linked";
    }
    if js::get(x, "linked") != Some(&Value::Bool(false)) {
        return "unknown";
    }
    let Some(rel) = text(js::get(x, "rel")) else { return "outside" };
    if rel.starts_with(".worktrees/") {
        return "outside";
    }
    if text(js::get(x, "state")) != Some("ok") {
        return "unknown";
    }
    if js::get(x, "has_origin") == Some(&Value::Bool(false)) {
        return "no_origin";
    }
    match expected_rel(r) {
        None => "unknown",
        Some(e) if e == lower_ascii(rel) => "ok",
        Some(_) => "mismatch",
    }
}

fn age_text(s: Option<i64>) -> String {
    match s {
        None => "経過不明".into(),
        Some(s) if s >= DAY => format!("{} 日", s / DAY),
        Some(s) if s >= 3600 => format!("{} 時間", s / 3600),
        Some(_) => "1 時間未満".into(),
    }
}

fn more(n: usize) -> String {
    if n > LIST_MAX { format!(" ほか {} 件", n - LIST_MAX) } else { String::new() }
}

struct Row<'a>(&'a Value);
impl Row<'_> {
    fn s(&self, k: &str) -> &str {
        text(self.0.get(k)).unwrap_or("")
    }
    fn i(&self, k: &str) -> Option<i64> {
        int(self.0.get(k))
    }
    fn n(&self, k: &str) -> i64 {
        self.i(k).unwrap_or(0)
    }
    fn path(&self) -> &str {
        self.s("path")
    }
}

fn finding(out: &mut Vec<Value>, id: &str, severity: &str, title: String, detail: String, advice: &str) {
    out.push(json!({ "category": "repo", "id": id, "severity": severity, "title": title, "detail": detail, "advice": advice }));
}

/// 所見（rules の finding と同じ形）。欠損を正しく扱うため、直接渡された入力も整形する。
pub fn analyze(input: &Value) -> Vec<Value> {
    let normalized = normalize(input);
    let audit = &normalized;
    let repos: Vec<&Value> = js::arr(audit.get("repos")).iter().collect();
    let mut out = Vec::new();
    let is_ok = |r: &&Value| text(r.get("state")) == Some("ok");
    let seen: Vec<&Value> = repos.iter().copied().filter(is_ok).collect();
    let gone: Vec<&Value> = repos.iter().copied().filter(|r| !is_ok(r)).collect();

    let mut miss: Vec<String> = Vec::new();
    if let Some(e) = text(audit.get("error")).filter(|e| !e.is_empty()) {
        miss.push(if e == "walk_incomplete" { "探索先の一部を確認できない".into() } else { format!("調査が失敗した（{e}）") });
    }
    if ["enumerated", "skipped", "truncated"].iter().any(|k| audit.get(*k) == Some(&Value::Null)) || !js::arr(audit.get("incomplete_fields")).is_empty() {
        miss.push("探索結果の集計または一覧が不明".into());
    }
    if text(audit.get("error")).is_none_or(str::is_empty) && js::arr(audit.get("incomplete_fields")).is_empty() && js::arr(audit.get("roots")).is_empty() {
        miss.push("探索先がない".into());
    }
    if !gone.is_empty() {
        let mut by: std::collections::BTreeMap<String, usize> = Default::default();
        for r in &gone {
            *by.entry(Row(r).s("reason").to_string()).or_default() += 1;
        }
        // JS の Object.keys(by).sort()（UTF-16 の順）
        let mut keys: Vec<&String> = by.keys().collect();
        keys.sort_by(|a, b| cmp16(a, b));
        let action = if by.contains_key("probe_failed") { "確認できなかった" } else { "git を読めなかった" };
        miss.push(format!("{action} {} 件（{}）", gone.len(), keys.iter().map(|k| format!("{k} {}", by[*k])).collect::<Vec<_>>().join("・")));
    }
    let skipped = audit.get("skipped").and_then(Value::as_f64).unwrap_or(0.0);
    if skipped > 0.0 {
        miss.push(format!("件数の上限で見ていない {} 件", js::num_str(skipped)));
    }
    if audit.get("truncated") == Some(&Value::Bool(true)) {
        miss.push("探索を時間または件数で打ち切った".into());
    }
    if !miss.is_empty() {
        finding(
            &mut out,
            "repo-unobserved",
            "info",
            "リポジトリの一部を確認できていない".into(),
            format!("{}。確認できた {} 件だけで判定している（読めなかった分は「きれい」とは数えない）", miss.join("、"), seen.len()),
            if text(audit.get("error")) == Some("walk_incomplete") {
                "探索先への読み取り権限と移動・消失を確認する。ほかの未観測がある場合は、その理由も確認する。"
            } else if gone.iter().any(|r| text(r.get("reason")) == Some("probe_failed")) {
                "対象の属性と読み取り権限、移動・消失を確認する。ほかの未観測がある場合は、その理由も確認する。"
            } else {
                "読めない理由は、git の所有者違い（safe.directory）・壊れたリポジトリ・巨大で時間切れのどれかが多い。上限は repo_roots の絞り込みか調査の max_repos / budget_s で調整する。"
            },
        );
    }

    let list = |rs: &[&Value], f: &dyn Fn(&Row) -> String| rs.iter().take(LIST_MAX).map(|r| f(&Row(r))).collect::<Vec<_>>().join("、") + &more(rs.len());
    let by_path = |a: &&Value, b: &&Value| cmp16(Row(a).path(), Row(b).path());

    let mut mism: Vec<&Value> = repos.iter().copied().filter(|r| path_status(r) == "mismatch").collect();
    mism.sort_by(by_path);
    if !mism.is_empty() {
        let detail = mism
            .iter()
            .take(LIST_MAX)
            .map(|r| format!("{}（origin は {}）", Row(r).path(), expected_rel(r).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("、")
            + &more(mism.len());
        finding(
            &mut out,
            "repo-path-mismatch",
            "warn",
            format!("ghq のパスと origin が食い違うリポジトリが {} 件", mism.len()),
            detail,
            "別のリポジトリの中身を置いていると、取り違えて別の origin へ変更を入れる事故になる。内容を確かめてから、置き場所を origin に合わせて移す（このアプリは移さない）。",
        );
    }
    let mut no_origin: Vec<&Value> = repos.iter().copied().filter(|r| path_status(r) == "no_origin").collect();
    no_origin.sort_by(by_path);
    if !no_origin.is_empty() {
        finding(
            &mut out,
            "repo-no-origin",
            "info",
            format!("origin の無い ghq 配下のリポジトリが {} 件", no_origin.len()),
            list(&no_origin, &|r| r.path().to_string()),
            "push 先が無いので、機体が壊れると失われる。残すなら remote を付け、要らなければ消す。",
        );
    }

    let weight = |r: &Row| r.i("ahead").unwrap_or(0) + r.i("local_only").unwrap_or(0);
    let mut unpushed: Vec<&Value> = seen.iter().copied().filter(|r| Row(r).i("ahead").unwrap_or(0) > 0 || Row(r).i("local_only").unwrap_or(0) > 0).collect();
    unpushed.sort_by(|a, b| weight(&Row(b)).cmp(&weight(&Row(a))).then_with(|| by_path(a, b)));
    if !unpushed.is_empty() {
        let detail = list(&unpushed, &|r| match r.i("ahead") {
            Some(a) => format!("{}（upstream より {a} 件先）", r.path()),
            None => format!("{}（remote に無いコミット {} 件・upstream なし）", r.path(), r.i("local_only").map_or("null".into(), |n| n.to_string())),
        });
        finding(
            &mut out,
            "repo-unpushed",
            "warn",
            format!("push していないコミットのあるリポジトリが {} 件", unpushed.len()),
            detail,
            "その機体だけにある作業。必要なものは push するか別の機体へ退避する。upstream の無いブランチは、先に push 先を決める。",
        );
    }

    let mut dirty: Vec<&Value> = seen.iter().copied().filter(|r| Row(r).n("tracked") > 0 || Row(r).n("untracked") > 0).collect();
    dirty.sort_by(|a, b| Row(b).i("dirty_age_s").unwrap_or(-1).cmp(&Row(a).i("dirty_age_s").unwrap_or(-1)).then_with(|| by_path(a, b)));
    if !dirty.is_empty() {
        let stale = dirty.iter().filter(|r| Row(r).i("dirty_age_s").unwrap_or(0) >= STALE_DIRTY_DAYS * DAY).count();
        let title_tail = if stale > 0 { format!("（{STALE_DIRTY_DAYS} 日以上前からが {stale} 件）") } else { String::new() };
        let detail = list(&dirty, &|r| {
            let conflict = if r.n("conflicted") > 0 { format!("・衝突 {}", r.n("conflicted")) } else { String::new() };
            format!("{}（変更 {}・未追跡 {}・{}{conflict}）", r.path(), r.n("tracked"), r.n("untracked"), age_text(r.i("dirty_age_s")))
        });
        finding(
            &mut out,
            "repo-dirty",
            if stale > 0 { "warn" } else { "info" },
            format!("未コミットの変更が残るリポジトリが {} 件{title_tail}", dirty.len()),
            detail,
            "長く残った変更は、作業の持ち主が分からなくなり、別の作業と混ざる。コミットして push するか、不要なら破棄する前に内容を確かめる（このアプリは変更を戻さない）。",
        );
    }

    let mut stashed: Vec<&Value> = seen.iter().copied().filter(|r| Row(r).n("stash") > 0).collect();
    stashed.sort_by(|a, b| Row(b).n("stash").cmp(&Row(a).n("stash")).then_with(|| by_path(a, b)));
    if !stashed.is_empty() {
        let total: i64 = stashed.iter().map(|r| Row(r).n("stash")).sum();
        finding(
            &mut out,
            "repo-stash",
            "info",
            format!("stash が残るリポジトリが {} 件（合計 {total} 件）", stashed.len()),
            list(&stashed, &|r| format!("{}（{} 件）", r.path(), r.n("stash"))),
            "stash は push されず、リポジトリを消すと一緒に失われる。必要なら branch にしてコミットする。",
        );
    }

    let mut behind: Vec<&Value> = seen.iter().copied().filter(|r| Row(r).n("behind") > 0).collect();
    behind.sort_by(|a, b| Row(b).n("behind").cmp(&Row(a).n("behind")).then_with(|| by_path(a, b)));
    if !behind.is_empty() {
        let detail =
            list(&behind, &|r| format!("{}（{} 件遅れ）", r.path(), r.n("behind"))) + "。遅れは最後に fetch した時点のもの（このアプリは fetch しない）";
        finding(
            &mut out,
            "repo-behind",
            "info",
            format!("upstream より遅れているリポジトリが {} 件", behind.len()),
            detail,
            "作業の前に pull（または fetch して確認）する。dirty のまま遅れていると、あとで衝突しやすい。",
        );
    }
    out
}

/// CLI の観測数と進捗。探索が不完全なら未観測の総数を 0 と表示しない。
pub fn progress(input: &Value, finding_count: usize) -> (Option<usize>, String) {
    let normalized = normalize(input);
    let audit = &normalized;
    let repos = js::arr(audit.get("repos"));
    let fields = js::arr(audit.get("incomplete_fields"));
    let observed = repos.iter().filter(|r| text(r.get("state")) == Some("ok")).count();
    let repos_known = audit.get("repos").is_some_and(Value::is_array) && !fields.iter().any(|v| v.as_str() == Some("repos"));
    let enumerated = count(audit.get("enumerated"));
    let skipped = count(audit.get("skipped"));
    let incomplete = text(audit.get("error")).is_some_and(|e| !e.is_empty())
        || enumerated.is_none()
        || skipped.is_none()
        || audit.get("truncated") != Some(&Value::Bool(false))
        || !fields.is_empty()
        || js::arr(audit.get("roots")).is_empty();
    let found = if fields.iter().any(|v| v.as_str() == Some("counts")) {
        "見つけた 件数不明".to_string()
    } else {
        enumerated.map_or_else(|| "見つけた 件数不明".to_string(), |n| format!("見つけた {n} 件"))
    };
    let coverage = walk_coverage_note(audit);
    let known = enumerated.is_some()
        && skipped.is_some()
        && fields.is_empty()
        && !js::arr(audit.get("roots")).is_empty()
        && audit.get("truncated").is_some_and(Value::is_boolean)
        && matches!(text(audit.get("error")), None | Some("walk_incomplete"));
    let known_unseen = (repos.len() - observed) as u128 + skipped.unwrap_or(0) as u128;
    let unseen = if !incomplete {
        format!("未観測 {known_unseen} 件")
    } else if known {
        format!("未観測 既知 {known_unseen} 件{}", if coverage.is_empty() { "・ほか件数不明" } else { coverage })
    } else {
        format!("未観測 件数不明{coverage}")
    };
    let wall = input
        .get("wall_s")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map_or_else(|| "時間不明".to_string(), |n| format!("{}s", js::to_fixed(n, 1)));
    let id = text(input.get("node_id")).unwrap_or("?");
    (repos_known.then_some(observed), format!("{id}: {found}・{unseen}・{wall}・所見 {finding_count}"))
}

/// 機体×リポジトリの表（matrix）。キーは origin から導いた <host>/<owner>/<repo>（小文字）。リンク worktree と origin の無いものは含めない
pub fn matrix(audits: &[Value]) -> Vec<Value> {
    let mut rows: Vec<(String, Map<String, Value>)> = Vec::new();
    for a in audits {
        let node_id = js::string(a.get("node_id"));
        let mut mine: Vec<(String, &Value, i64)> = Vec::new();
        for r in js::arr(a.get("repos")) {
            let Some(key) = expected_rel(r) else { continue };
            if js::get(Some(r), "linked") == Some(&Value::Bool(true)) {
                continue;
            }
            match mine.iter_mut().find(|m| m.0 == key) {
                None => mine.push((key, r, 1)),
                Some(cur) => {
                    cur.2 += 1;
                    if cmp16(Row(r).path(), Row(cur.1).path()) == Ordering::Less {
                        cur.1 = r;
                    }
                }
            }
        }
        for (key, r, copies) in mine {
            let cell = Obj::new()
                .set("state", r.get("state").cloned().unwrap_or(Value::Null))
                .opt("tracked", r.get("tracked").cloned())
                .opt("untracked", r.get("untracked").cloned())
                .opt("ahead", r.get("ahead").cloned())
                .opt("behind", r.get("behind").cloned())
                .opt("stash", r.get("stash").cloned())
                .opt("branch", r.get("branch").cloned())
                .set("copies", copies)
                .build();
            let idx = match rows.iter().position(|x| x.0 == key) {
                Some(i) => i,
                None => {
                    rows.push((key, Map::new()));
                    rows.len() - 1
                }
            };
            rows[idx].1.insert(node_id.clone(), cell);
        }
    }
    let dirty = |c: &Value| int(c.get("tracked")).unwrap_or(0) > 0 || int(c.get("untracked")).unwrap_or(0) > 0;
    let mut out: Vec<(i64, i64, String, Value)> = rows
        .into_iter()
        .map(|(key, nodes)| {
            let node_count = nodes.len() as i64;
            let dirty_nodes = nodes.values().filter(|c| dirty(c)).count() as i64;
            (dirty_nodes, node_count, key.clone(), json!({ "key": key, "nodes": nodes, "node_count": node_count, "dirty_nodes": dirty_nodes }))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then_with(|| cmp16(&a.2, &b.2)));
    out.into_iter().map(|x| x.3).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_hygiene_result_matches_shared_fixtures() {
        let fixtures: Value = serde_json::from_str(include_str!("../../../test/fixtures/hygiene-process-result.json")).unwrap();
        for case in fixtures["results"].as_array().unwrap() {
            let failure = match case["failure"].as_str() {
                Some("timeout") => Some(crate::collect::CompletionFailure::Timeout),
                Some("incomplete") => Some(crate::collect::CompletionFailure::Incomplete),
                Some("spawn_missing") => Some(crate::collect::CompletionFailure::SpawnMissing),
                Some("spawn_failed") => Some(crate::collect::CompletionFailure::SpawnFailed),
                None => None,
                _ => unreachable!(),
            };
            let run = crate::collect::StrictRunResult {
                result: crate::collect::RunResult {
                    code: case["code"].as_i64().map(|n| n as i32),
                    out: fixtures["outputs"][case["output"].as_str().unwrap()].as_str().unwrap().into(),
                    err: "fixture-private-stderr".into(),
                },
                failure,
            };
            let parsed = parse_run_result(&run);
            assert_eq!(parsed["ok"], case["ok"], "{}", case["name"]);
            assert_eq!(parsed.get("error").unwrap_or(&Value::Null), &case["error"], "{}", case["name"]);
            if case["ok"] == false {
                assert!(parsed.get("repos").is_none(), "{}", case["name"]);
                assert_eq!(normalize(&parsed)["error"], case["error"], "{}", case["name"]);
            }
            assert!(!parsed.to_string().contains("fixture-private-stderr"));
            let mut other = StrictRunResult { result: run.result.clone(), failure: run.failure };
            other.result.err = "Python was not found @@KATALA_TUNE_NO_PYTHON@@".into();
            assert_eq!(parse_run_result(&other), parsed, "{}", case["name"]);
        }
    }

    #[test]
    fn incomplete_walk_is_distinct_from_truncation_and_keeps_observed_results() {
        for truncated in [false, true] {
            let input = json!({
                "probe": "repo_hygiene", "error": "walk_incomplete", "truncated": truncated, "enumerated": 1, "skipped": 0,
                "roots": [{"path": "fixture", "source": "opt"}],
                "repos": [{"state": "ok", "path": "fixture/project", "linked": false, "tracked": 1, "untracked": 0, "conflicted": 0, "upstream": false, "detached": false}]
            });
            let data = normalize(&input);
            let findings = analyze(&data);
            let missing = findings.iter().find(|finding| finding["id"] == "repo-unobserved").unwrap();
            let detail = missing["detail"].as_str().unwrap();
            assert!(detail.contains("探索先の一部を確認できない"));
            assert_eq!(detail.contains("探索を時間または件数で打ち切った"), truncated);
            assert!(findings.iter().any(|finding| finding["id"] == "repo-dirty"));
            assert_eq!(walk_coverage_note(&data), "・探索未確認あり（件数不明）");
        }
        assert_eq!(walk_coverage_note(&json!({ "error": null, "truncated": true })), "・探索未確認あり（件数不明）");
        assert_eq!(walk_coverage_note(&json!({ "error": "git_missing" })), "");
    }

    #[test]
    fn metadata_unknown_remains_visible_with_unknown_state() {
        let normal = json!({
            "path": "fixture/normal", "rel": "example.invalid/group/item", "linked": false, "state": "ok",
            "tracked": 0, "untracked": 0, "conflicted": 0, "upstream": false, "detached": false,
            "origin_host": "example.invalid", "origin_path": "group/item"
        });
        let mut linked = normal.clone();
        linked["path"] = json!("fixture/linked");
        linked["linked"] = json!(true);
        let mut unknown = normal.clone();
        unknown["path"] = json!("fixture/unknown");
        unknown["linked"] = Value::Null;
        unknown["state"] = json!("unobserved");
        unknown["reason"] = json!("probe_failed");
        unknown["origin_path"] = json!("group/unknown");
        let input = json!({
            "roots": [{"path": "fixture", "source": "opt"}], "enumerated": 3, "skipped": 0, "truncated": false,
            "repos": [normal, linked, unknown]
        });
        let data = normalize(&input);
        assert_eq!(data["repos"][0]["linked"], false);
        assert_eq!(data["repos"][1]["linked"], true);
        assert_eq!(data["repos"][2]["linked"], Value::Null);
        assert_eq!(data["repos"][2]["reason"], "probe_failed");
        assert_eq!(normalize(&data), data);
        assert_eq!(path_status(&data["repos"][2]), "unknown");
        let findings = analyze(&data);
        let finding = findings.iter().find(|item| item["id"] == "repo-unobserved").unwrap();
        let detail = finding["detail"].as_str().unwrap();
        assert!(detail.contains("確認できなかった 1 件"));
        assert!(detail.contains("probe_failed 1"));
        assert!(!detail.contains("git を読めなかった"));
        let (observed, line) = progress(&input, 1);
        assert_eq!(observed, Some(2));
        assert!(line.contains("未観測 1 件"));
        assert_eq!(
            json!({"linked": [data["repos"][0]["linked"], data["repos"][1]["linked"], data["repos"][2]["linked"]],
                "reason": data["repos"][2]["reason"], "detail": finding["detail"], "advice": finding["advice"],
                "observed": observed, "progress": line}),
            json!({"linked": [false, true, null], "reason": "probe_failed",
                "detail": "確認できなかった 1 件（probe_failed 1）。確認できた 2 件だけで判定している（読めなかった分は「きれい」とは数えない）",
                "advice": "対象の属性と読み取り権限、移動・消失を確認する。ほかの未観測がある場合は、その理由も確認する。",
                "observed": 2, "progress": "?: 見つけた 3 件・未観測 1 件・時間不明・所見 1"})
        );
        let mut truncated = input.clone();
        truncated["truncated"] = json!(true);
        assert!(analyze(&truncated)[0]["detail"].as_str().unwrap().contains("探索を時間または件数で打ち切った"));
        assert!(progress(&truncated, 1).1.contains("未観測 既知 1 件・探索未確認あり"));
        let cells = matrix(&[json!({"node_id": "node-a", "repos": input["repos"]})]);
        assert_eq!(cells.len(), 2);
        let known = cells.iter().find(|cell| cell["key"] == "example.invalid/group/item").unwrap();
        let missing = cells.iter().find(|cell| cell["key"] == "example.invalid/group/unknown").unwrap();
        assert_eq!(known["nodes"]["node-a"]["copies"], 1);
        assert_eq!(missing["nodes"]["node-a"]["state"], "unobserved");
        for value in [Value::Null, json!("invalid")] {
            let mut bad = input.clone();
            bad["repos"][0]["linked"] = value.clone();
            assert_eq!(path_status(&bad["repos"][0]), "unknown");
            assert_eq!(normalize(&bad)["repos"][0]["state"], "unobserved");
            bad["repos"][2]["linked"] = value;
            assert_eq!(normalize(&bad)["repos"][2]["linked"], Value::Null);
        }
        let mut omitted = input;
        omitted["repos"][2].as_object_mut().unwrap().remove("linked");
        assert_eq!(normalize(&omitted)["repos"][2]["linked"], Value::Null);
        omitted["repos"][0].as_object_mut().unwrap().remove("linked");
        assert_eq!(path_status(&omitted["repos"][0]), "unknown");
        assert_eq!(normalize(&omitted)["repos"][0]["state"], "unobserved");
        let mut bad_reason = omitted;
        bad_reason["repos"][2]["reason"] = json!("synthetic-private-reason");
        let scrubbed = normalize(&bad_reason);
        assert_eq!(scrubbed["repos"][2]["reason"], "git_failed");
        assert!(!scrubbed.to_string().contains("synthetic-private-reason"));
        assert!(!json!(analyze(&scrubbed)).to_string().contains("synthetic-private-reason"));
    }

    #[test]
    fn urls_lose_credentials() {
        assert_eq!(strip_url(Some("https://user:tok@example.com/o/r.git?x=1#f")).as_deref(), Some("https://example.com/o/r.git"));
        assert_eq!(strip_url(Some("user:tok@example.com:o/r.git")).as_deref(), Some("example.com:o/r.git"));
        assert_eq!(strip_url(Some("git@example.com:o/r.git")).as_deref(), Some("example.com:o/r.git"));
    }

    #[test]
    fn script_embeds_clamped_opts() {
        let s = script(&json!({ "max_repos": 99999, "budget_s": 1, "cmd_s": "x", "roots": ["~/a", 5, "", "日本語"], "ghq_root": "/r" }));
        let line = s.lines().find(|l| l.starts_with("KT_OPTS = ")).expect("line");
        let v: Value = serde_json::from_str(line.trim_start_matches("KT_OPTS = ")).expect("json");
        assert_eq!(v, json!({ "max_repos": 5000, "budget_s": 5, "cmd_s": 12, "roots": ["~/a", "日本語"], "ghq_root": "/r" }));
        assert_eq!(s.lines().filter(|l| l.starts_with("KT_OPTS = ")).count(), 1);
    }

    #[test]
    fn shared_nodes_are_off_unless_opted_in() {
        let n = |v: Value| Node::from_value(&v, "other-host");
        assert!(enabled(&n(json!({ "id": "a", "alias": "a", "os": "macos" }))));
        assert!(!enabled(&n(json!({ "id": "a", "alias": "a", "os": "macos", "repo_hygiene": false }))));
        assert!(!enabled(&n(json!({ "id": "s", "alias": "s", "os": "windows", "shared": true }))));
        assert!(enabled(&n(json!({ "id": "s", "alias": "s", "os": "windows", "shared": true, "repo_hygiene": true }))));
    }

    #[tokio::test]
    async fn inactive_nodes_are_never_contacted() {
        let n = |v: Value| Node::from_value(&v, "other-host");
        for status in ["retired", "no-ssh", "paused", "out-of-scope", "bogus"] {
            let node = n(json!({ "id": "x", "alias": "x", "os": "macos", "status": status, "repo_hygiene": true }));
            assert!(!enabled(&node), "{status} must not be audited");
            let r = audit_node(&node, &json!({})).await;
            assert_eq!((r["ok"].clone(), r["error"].clone()), (json!(false), json!("excluded")), "{status}");
        }
        assert!(enabled(&n(json!({ "id": "a", "alias": "a", "os": "macos", "status": "online" }))));
    }
}
