//! 「状態」の判定。アプリ自身の機能と、各機体の機能を ok / warn / fail / unknown で返す。副作用なし。仕様は lib/health.js。
//! 操作者が一目で判定できるよう、どの項目も「何を見て」「なぜその判定か」を detail に書く。

use serde_json::Value;

use crate::db::Check;
use crate::js::{self, Jv, arr, get, num, present, string, truthy};

/// タスクスケジューラの「失敗ではない」結果コード（SCHED_S_*）。0x41300〜0x4130F と 0
fn sched_ok(x: f64) -> bool {
    x == 0.0 || (x.fract() == 0.0 && (f64::from(0x41300)..=f64::from(0x4130F)).contains(&x))
}

/// 前回失敗した定期処理
pub fn failing_jobs(jobs: &[Value]) -> Vec<&Value> {
    jobs.iter()
        .filter(|j| {
            let state = j.get("state");
            if ["disabled", "unknown", "running"].iter().any(|s| js::is_str(state, s)) {
                return false;
            }
            let lr = j.get("last_result");
            if js::is_str(j.get("kind"), "schtask") {
                return present(lr) && !sched_ok(num(lr));
            }
            js::is_str(j.get("scope"), "user") && present(lr) && !matches!(lr, Some(Value::Number(n)) if n.as_f64() == Some(0.0))
        })
        .collect()
}

fn age_min(ms: Jv<'_>, now: f64) -> String {
    if truthy(ms) { js::num_str(js::round((now - num(ms)) / 60000.0)) } else { "null".into() }
}

fn age_min_f(ms: f64, now: f64) -> String {
    if ms != 0.0 && !ms.is_nan() { js::num_str(js::round((now - ms) / 60000.0)) } else { "null".into() }
}

struct Out(Vec<Check>);

impl Out {
    fn add(&mut self, id: impl Into<String>, name: impl Into<String>, status: &str, detail: impl Into<String>) {
        self.0.push(Check { id: id.into(), name: name.into(), status: status.into(), detail: Some(detail.into()) });
    }
}

/// 1台分の機能チェック。snap = 最新の分析結果 `{ at, wall_s, data }`（無ければ None）、findings = その所見、
/// ctx = `{ now, cursors, expect, schedule, lastError }`（health.js と同じキー）
pub fn node_checks(node: &Value, snap: Option<&Value>, findings: &[Value], ctx: &Value) -> Vec<Check> {
    let c = Some(ctx);
    let now = if present(get(c, "now")) { num(get(c, "now")) } else { crate::db::now_ms() as f64 };
    let minutes = |k: &str, d: f64| {
        let v = get(get(c, "schedule"), k);
        if present(v) { num(v) } else { d }
    };
    let probe_every = minutes("probe_minutes", 60.0) * 60000.0;
    let logs_every = minutes("logs_minutes", 15.0) * 60000.0;
    let d = get(snap, "data");
    let mut out = Out(Vec::new());
    let has = |prefix: &str| {
        findings.iter().find(|f| {
            let id = string(f.get("id"));
            id == prefix || id.starts_with(prefix)
        })
    };

    // 調査が通るか（SSH・スクリプト）
    let last_error = get(c, "lastError");
    if truthy(last_error) {
        let e = string(last_error);
        let tail = e.split('\n').next_back().unwrap_or_default();
        out.add("probe", "分析", "fail", format!("前回の分析が失敗: {}", js::slice16(tail, 120)));
    } else if !present(snap) {
        out.add("probe", "分析", "unknown", "まだ分析していない");
    } else if now - num(get(snap, "at")) > probe_every * 3.0 {
        out.add(
            "probe",
            "分析",
            "warn",
            format!("最後の成功が {} 分前（間隔 {} 分の3倍を超えた）", age_min(get(snap, "at"), now), js::num_str(probe_every / 60000.0)),
        );
    } else {
        let wall = match get(snap, "wall_s") {
            Some(Value::Number(n)) => js::to_fixed(n.as_f64().unwrap_or(f64::NAN), 1),
            _ => "-".into(),
        };
        out.add("probe", "分析", "ok", format!("{} 分前に成功（{wall} 秒）", age_min(get(snap, "at"), now)));
    }

    // ログの取り込み
    let node_id = get(Some(node), "id");
    let cur: Vec<&Value> = arr(get(c, "cursors")).iter().filter(|x| js::strict_eq(x.get("node_id"), node_id)).collect();
    if cur.is_empty() {
        out.add("logs", "ログ取り込み", "unknown", "まだ取り込んでいない");
    } else {
        let errs: Vec<&&Value> = cur.iter().filter(|x| truthy(x.get("last_error"))).collect();
        let stale: Vec<&&Value> =
            cur.iter().filter(|x| !truthy(x.get("last_error")) && truthy(x.get("last_ok_at")) && now - num(x.get("last_ok_at")) > logs_every * 3.0).collect();
        if !errs.is_empty() {
            let d =
                errs.iter().map(|x| format!("{}: {}", string(x.get("source")), js::slice16(&string(x.get("last_error")), 80))).collect::<Vec<_>>().join(" / ");
            out.add("logs", "ログ取り込み", "fail", d);
        } else if !stale.is_empty() {
            let oldest = js::min(&stale.iter().map(|x| num(x.get("last_ok_at"))).collect::<Vec<_>>());
            out.add(
                "logs",
                "ログ取り込み",
                "warn",
                format!(
                    "{} の最後の成功が {} 分前",
                    js::join(&stale.iter().map(|x| x.get("source").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(), ", "),
                    age_min_f(oldest, now)
                ),
            );
        } else {
            let zero = Value::from(0);
            let newest = js::max(&cur.iter().map(|x| num(js::or(x.get("last_ok_at"), Some(&zero)))).collect::<Vec<_>>());
            out.add("logs", "ログ取り込み", "ok", format!("{} か所、最新 {} 分前", cur.len(), age_min_f(newest, now)));
        }
    }
    let Some(d) = d.filter(|v| truthy(Some(v))) else { return out.0 };
    let d = Some(d);

    // 定期処理（launchd / タスクスケジューラ）
    let jobs = arr(get(d, "jobs"));
    let bad = failing_jobs(jobs);
    let (st, detail) = if jobs.is_empty() {
        ("unknown", "取得できない（古い調査スクリプト）".to_string())
    } else if bad.is_empty() {
        ("ok", format!("{} 件、前回失敗なし", jobs.len()))
    } else {
        let names = js::join(&bad.iter().take(4).map(|j| j.get("name").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(), ", ");
        ("warn", format!("{} 件が前回失敗: {names}{}", bad.len(), if bad.len() > 4 { " ほか" } else { "" }))
    };
    out.add("jobs", "定期処理", st, detail);

    // 期待する常駐（台帳の expect）
    let exp = get(c, "expect");
    for name in arr(get(exp, "services")) {
        let n = string(Some(name));
        let s = arr(get(d, "third_party_services")).iter().find(|x| string(x.get("name")).to_lowercase() == n.to_lowercase());
        let (st, detail) = match s {
            None => ("fail", "見つからない".to_string()),
            Some(s) => {
                (if js::is_str(s.get("state"), "running") { "ok" } else { "fail" }, format!("{}（起動 {}）", string(s.get("state")), string(s.get("start"))))
            }
        };
        out.add(format!("svc:{n}"), format!("サービス {n}"), st, detail);
    }
    for label in arr(get(exp, "jobs")) {
        let l = string(Some(label));
        let j = jobs.iter().find(|x| js::strict_eq(x.get("id"), Some(label)) || js::strict_eq(x.get("name"), Some(label)));
        let (st, detail) = match j {
            None => ("fail", "見つからない".to_string()),
            Some(j) => {
                let st = if js::is_str(j.get("state"), "disabled") || js::is_str(j.get("state"), "not-loaded") {
                    "fail"
                } else if !failing_jobs(std::slice::from_ref(j)).is_empty() {
                    "warn"
                } else {
                    "ok"
                };
                let lr = j.get("last_result");
                (st, format!("{}{}", string(j.get("state")), if present(lr) { format!("、前回の結果 {}", string(lr)) } else { String::new() }))
            }
        };
        out.add(format!("job:{l}"), format!("定期処理 {l}"), st, detail);
    }
    for p in arr(get(exp, "processes")) {
        let p = string(Some(p));
        let procs = get(d, "processes");
        let hit = arr(get(procs, "apps")).iter().chain(arr(get(procs, "top_cpu")).iter()).any(|x| {
            let v = js::or(js::or(x.get("app"), x.get("name")), None);
            let s = if truthy(v) { string(v) } else { String::new() };
            s.to_lowercase() == p.to_lowercase()
        });
        // apps は上位25件だけなので、見えないことがある → unknown
        out.add(
            format!("proc:{p}"),
            format!("プロセス {p}"),
            if hit { "ok" } else { "unknown" },
            if hit { "動いている" } else { "メモリ上位に見えない（動いていないか、小さい）" },
        );
    }

    // リソース
    if let Some(sys) = arr(get(d, "disk")).iter().find(|x| js::is_str(x.get("mount"), "/") || js::is_c_drive(x.get("mount"))) {
        let fp = num(sys.get("free_pct"));
        out.add(
            "disk",
            "ディスク",
            if fp < 5.0 {
                "fail"
            } else if fp < 10.0 {
                "warn"
            } else {
                "ok"
            },
            format!("{} 実効の空き {} GB（{}%）", string(sys.get("mount")), string(sys.get("free_gb")), string(sys.get("free_pct"))),
        );
    }
    let m = get(d, "memory");
    let is_win = js::is_str(get(d, "probe"), "windows");
    let avail = get(m, "available_pct");
    let commit = num(get(m, "commit_pct"));
    let mem_bad = js::is_str(get(m, "pressure"), "critical") || commit >= 90.0 || (present(avail) && is_win && num(avail) < 10.0);
    let mem_warn = js::is_str(get(m, "pressure"), "warn") || commit >= 80.0 || (is_win && num(avail) < 20.0);
    out.add(
        "memory",
        "メモリ",
        if mem_bad {
            "fail"
        } else if mem_warn {
            "warn"
        } else {
            "ok"
        },
        if is_win {
            format!("空き {}%・コミット {}%", string(avail), string(get(m, "commit_pct")))
        } else {
            format!("圧迫 {}・swap {} GB", string(get(m, "pressure")), string(get(m, "swap_used_gb")))
        },
    );
    let busy = get(d, "cpu_busy");
    out.add(
        "cpu",
        "CPU",
        if num(busy) >= 85.0 { "warn" } else { "ok" },
        format!("{}%{}", string(busy), if has("runaway-").is_some() { "、1コアを使い切るプロセスあり" } else { "" }),
    );

    // 安定性・保護
    let st7 = get(d, "stability_7d");
    if truthy(st7) {
        let zero = Value::from(0);
        let c = js::max(&[num(js::or(get(st7, "bugcheck_1001"), Some(&zero))), num(js::or(get(st7, "kernel_power_41"), Some(&zero)))]);
        out.add(
            "stability",
            "安定性",
            if c >= 3.0 {
                "fail"
            } else if c > 0.0 {
                "warn"
            } else {
                "ok"
            },
            format!("7日で予期しない停止 {} 回", js::num_str(c)),
        );
    } else if has("log-panic").is_some() {
        out.add("stability", "安定性", "fail", "カーネルパニックの記録あり");
    } else {
        out.add("stability", "安定性", "ok", "パニックの記録なし");
    }
    let def = get(d, "defender");
    // netsec（netsec.rs）があれば、防御・待ち受け・常駐の増減・ログイン・初めての接続先をそちらで判定する（Defender もそこに含む）
    if truthy(def) && !truthy(get(get(d, "netsec"), "defense")) {
        let rt = truthy(get(def, "realtime"));
        out.add("defender", "Defender", if rt { "ok" } else { "warn" }, if rt { "リアルタイム保護 有効" } else { "リアルタイム保護 無効" });
    }
    out.0.extend(crate::netsec::checks(get(d, "netsec"), findings, ctx, node));
    let flood: Vec<&Value> = findings.iter().filter(|f| string(f.get("id")).starts_with("log-flood-")).collect();
    out.add(
        "flood",
        "エラーの繰り返し",
        if flood.is_empty() { "ok" } else { "warn" },
        if flood.is_empty() {
            "24時間で200件を超える同じエラーなし".to_string()
        } else {
            js::join(&flood.iter().map(|f| f.get("title").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(), " / ")
        },
    );
    if let Some(hw) = has("log-whea").or_else(|| has("log-gpu-reset")).or_else(|| has("log-disk")) {
        out.add("hardware", "ハードウェア", if js::is_str(hw.get("severity"), "critical") { "fail" } else { "warn" }, string(hw.get("title")));
    }
    out.0
}

/// アプリ自身の機能。ctx = `{ now, configError, example, nodeCount, protectCount, dbCheck, dbBytes, scheduler, fleet?, fleetAt, openAtLogin }`
/// （fleet キーが無いときは連携の行を出さない＝JS の undefined）
pub fn app_checks(ctx: &Value) -> Vec<Check> {
    let c = Some(ctx);
    let now = if present(get(c, "now")) { num(get(c, "now")) } else { crate::db::now_ms() as f64 };
    let mut out = Out(Vec::new());
    let cfg_err = get(c, "configError");
    let example = truthy(get(c, "example"));
    out.add(
        "config",
        "機体台帳",
        if truthy(cfg_err) {
            "fail"
        } else if example {
            "warn"
        } else {
            "ok"
        },
        if truthy(cfg_err) {
            string(cfg_err)
        } else if example {
            "見本のまま".into()
        } else {
            format!("{} 台、保護 {} 件", string(get(c, "nodeCount")), string(get(c, "protectCount")))
        },
    );
    let db_ok = js::is_str(get(c, "dbCheck"), "ok");
    out.add(
        "db",
        "ローカル DB",
        if db_ok { "ok" } else { "fail" },
        if db_ok {
            format!("整合性 ok、{} MB", js::to_fixed(num(get(c, "dbBytes")) / 1_048_576.0, 1))
        } else {
            format!("整合性チェック: {}", string(get(c, "dbCheck")))
        },
    );
    let s = get(c, "scheduler");
    if !truthy(get(s, "enabled")) {
        out.add("scheduler", "自動スキャン", "warn", "止まっている");
    } else {
        let lp = get(s, "lastProbeAt");
        let ll = get(s, "lastLogsAt");
        let late = truthy(lp) && now - num(lp) > num(get(s, "probe_minutes")) * 60000.0 * 2.0;
        let when = |v: Jv<'_>| if truthy(v) { format!("{} 分前", age_min(v, now)) } else { "まだ".into() };
        out.add(
            "scheduler",
            "自動スキャン",
            if late { "warn" } else { "ok" },
            format!(
                "分析 {} 分ごと（前回 {}）、ログ {} 分ごと（前回 {}）",
                string(get(s, "probe_minutes")),
                when(lp),
                string(get(s, "logs_minutes")),
                when(ll)
            ),
        );
    }
    if ctx.get("fleet").is_some() {
        let fleet = get(c, "fleet");
        let err = get(fleet, "error");
        out.add(
            "fleet",
            "katala-fleet 連携",
            if truthy(err) {
                "warn"
            } else if truthy(fleet) {
                "ok"
            } else {
                "unknown"
            },
            if truthy(err) {
                string(err)
            } else if truthy(fleet) {
                format!("取得 {} 分前", age_min(get(c, "fleetAt"), now))
            } else {
                "まだ取得していない".into()
            },
        );
    }
    let login = truthy(get(c, "openAtLogin"));
    out.add(
        "login",
        "ログイン時に起動",
        if login { "ok" } else { "unknown" },
        if login { "有効" } else { "無効（自動スキャンはアプリを開いている間だけ動く）" },
    );
    out.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failing_jobs_skip_sched_ok_and_disabled() {
        let jobs = json!([
            { "kind": "schtask", "id": "\\ok", "name": "ok", "state": "ready", "last_result": 0 },
            { "kind": "schtask", "id": "\\never", "name": "never", "state": "ready", "last_result": 267011 },
            { "kind": "schtask", "id": "\\bad", "name": "bad", "state": "ready", "last_result": 1 },
            { "kind": "schtask", "id": "\\off", "name": "off", "state": "disabled", "last_result": 1 }
        ]);
        let names: Vec<String> = failing_jobs(jobs.as_array().unwrap()).iter().map(|j| string(j.get("name"))).collect();
        assert_eq!(names, ["bad"]);
    }
}
