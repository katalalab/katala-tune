//! リポジトリの衛生監査（lib/hygiene.js と同じ）: 各機体の開発リポジトリの dirty・未 push・stash・ghq のパスと origin の不一致を、
//! 読み取り専用で集める。調査は probes/repo_hygiene.py（git の読み取りコマンドだけ。fetch しない・内容や差分を読まない・stderr を捨てる）。
//! 台帳の機体ごとの設定: "repo_hygiene": false で対象外（書いていなければ共用機は対象外）、"repo_roots": ["~/work"] で ghq root のほかの置き場を足す。
//! 保存はしない（アプリの分析・自動スキャンには入っていない。`tune hygiene` から呼ぶ）。結果は JS 版と同じ（tests/parity_hygiene.rs）。

use std::cmp::Ordering;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::ai_sessions::{NO_PYTHON, missing_python, run_python};
use crate::collect::last_json_line;
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
fn text(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
}
fn opt_s(v: Option<&Value>) -> Value {
    text(v).map_or(Value::Null, Value::from)
}
fn opt_i(v: Option<&Value>) -> Value {
    int(v).map_or(Value::Null, Value::from)
}
/// UTF-16 の単位で比べる（JS の `<`）
fn cmp16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
fn lower_ascii(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// 台帳の機体で集めるか。書いていなければ共用機は集めない（他の人のリポジトリのパスを集めないため）
pub fn enabled(node: &Node) -> bool {
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
    let started = Instant::now();
    let mut o = opts.clone();
    let mut roots: Vec<Value> = js::arr(js::get(Some(opts), "roots")).to_vec();
    roots.extend(js::arr(node.get("repo_roots")).iter().cloned());
    o["roots"] = Value::Array(roots);
    let budget = clamp(js::get(Some(opts), "budget_s"), 5, 300, DEFAULT_BUDGET_S);
    let res = run_python(node, &script(&o), Duration::from_secs((budget + 40) as u64)).await;
    let wall_s = started.elapsed().as_secs_f64();
    match last_json_line(&res.out).filter(|d| text(d.get("probe")) == Some("repo_hygiene")) {
        Some(data) => {
            let mut v = normalize(&data);
            v["node_id"] = node.id.clone().into();
            v["ok"] = true.into();
            v["wall_s"] = json!(wall_s);
            v["at"] = now_ms().into();
            v
        }
        None => {
            // 標準エラーには何が混ざるか分からないので、理由の種類だけを残す
            let error = if res.out.contains(NO_PYTHON) || missing_python(&res) {
                "no_python"
            } else if res.code.is_none() {
                "timeout_or_unreachable"
            } else {
                "probe_failed"
            };
            json!({ "node_id": node.id, "ok": false, "error": error, "wall_s": wall_s, "at": now_ms() })
        }
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
    let ok = text(g("state")) == Some("ok");
    let reason = if ok {
        Value::Null
    } else {
        let t = text(g("reason")).unwrap_or("");
        json!(if matches!(t, "timeout" | "git_failed" | "over_limit") { t } else { "git_failed" })
    };
    let base = Obj::new()
        .set("path", text(g("path")).unwrap_or(""))
        .set("rel", opt_s(g("rel")))
        .set("linked", g("linked") == Some(&Value::Bool(true)))
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
    let roots: Vec<Value> = js::arr(g("roots"))
        .iter()
        .map(|r| json!({ "path": text(js::get(Some(r), "path")).unwrap_or(""), "source": text(js::get(Some(r), "source")).unwrap_or("") }))
        .collect();
    json!({
        "version": opt_s(g("version")), "os": opt_s(g("os")), "roots": roots,
        "enumerated": int(g("enumerated")).unwrap_or(0), "skipped": int(g("skipped")).unwrap_or(0),
        "truncated": g("truncated") == Some(&Value::Bool(true)),
        "elapsed_s": g("elapsed_s").filter(|v| v.is_number()).cloned().unwrap_or(Value::Null),
        "error": opt_s(g("error")),
        "repos": js::arr(g("repos")).iter().map(normalize_repo).collect::<Vec<_>>(),
    })
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

/// 所見（rules の finding と同じ形）。audit は [`normalize`] の出力
pub fn analyze(audit: &Value) -> Vec<Value> {
    let repos: Vec<&Value> = js::arr(audit.get("repos")).iter().collect();
    let mut out = Vec::new();
    let is_ok = |r: &&Value| text(r.get("state")) == Some("ok");
    let seen: Vec<&Value> = repos.iter().copied().filter(is_ok).collect();
    let gone: Vec<&Value> = repos.iter().copied().filter(|r| !is_ok(r)).collect();

    let mut miss: Vec<String> = Vec::new();
    if let Some(e) = text(audit.get("error")).filter(|e| !e.is_empty()) {
        miss.push(format!("調査が失敗した（{e}）"));
    }
    if !gone.is_empty() {
        let mut by: std::collections::BTreeMap<String, usize> = Default::default();
        for r in &gone {
            *by.entry(Row(r).s("reason").to_string()).or_default() += 1;
        }
        // JS の Object.keys(by).sort()（UTF-16 の順）
        let mut keys: Vec<&String> = by.keys().collect();
        keys.sort_by(|a, b| cmp16(a, b));
        miss.push(format!("git を読めなかった {} 件（{}）", gone.len(), keys.iter().map(|k| format!("{k} {}", by[*k])).collect::<Vec<_>>().join("・")));
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
            "読めない理由は、git の所有者違い（safe.directory）・壊れたリポジトリ・巨大で時間切れのどれかが多い。上限は repo_roots の絞り込みか調査の max_repos / budget_s で調整する。",
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
}
