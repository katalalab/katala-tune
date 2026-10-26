//! WSL の中の観測（probes/win_probe.ps1 の `snap.wsl.distros`）から所見を作る（lib/wsl.js と同じ。副作用なし）。
//! distro の state: running（中を観測した。観測できなければ reason）/ stopped_unobserved（停止中。起動も実行もせず、観測していない）/
//! unknown（走っている一覧を読めなかった。やはり何も実行しない）。停止中は「正常」でも「空」でもなく「観測していない」。

use serde_json::{Value, json};

use crate::js::{self, Jv, get, to_fixed};

const MEM_WARN_PCT: f64 = 90.0;
const HELD_MIN_MB: f64 = 4096.0;

fn gb(mb: f64) -> String {
    to_fixed(mb / 1024.0, 1)
}

fn fin(v: Jv<'_>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite())
}

fn reason_text(r: Jv<'_>) -> String {
    match r.and_then(Value::as_str) {
        Some("not-running") => "停止中（起動しないので観測していない）".into(),
        Some("running-list-failed") => "動いているかを読めなかった（何も実行していない）".into(),
        Some("internal-distro-skipped") => "Docker Desktop の内部 distro（観測の対象外）".into(),
        Some("observation-limit") => "観測する distro の数の上限".into(),
        Some("timeout") => "時間切れ".into(),
        Some("failed") => "実行できなかった".into(),
        Some("no-data") => "内側から値を読めなかった".into(),
        Some(other) => js::slice16(other, 60),
        None => "不明".into(),
    }
}

fn distros(snap: &Value) -> Vec<&Value> {
    js::arr(get(get(Some(snap), "wsl"), "distros")).iter().filter(|d| d.is_object()).collect()
}

/// 内側で使われているメモリ（MB）。WSL2 の distro は1つの VM を共有するので、足さずに最大を取る
pub fn used_mb(inside: Jv<'_>) -> Option<f64> {
    let total = fin(get(inside, "mem_total_mb"))?;
    let avail = fin(get(inside, "mem_available_mb"))?;
    (total > 0.0).then(|| (total - avail).max(0.0))
}

fn name(d: &Value) -> String {
    js::string(d.get("name"))
}

fn is_state(d: &Value, s: &str) -> bool {
    js::is_str(d.get("state"), s)
}

fn seen(d: &Value) -> bool {
    matches!(d.get("observed"), Some(Value::Bool(true))) && used_mb(d.get("inside")).is_some()
}

fn partial_text(d: &Value) -> String {
    let errors = get(d.get("inside"), "errors");
    [("swap", "スワップ"), ("psi", "PSI"), ("load", "負荷"), ("top", "上位プロセス"), ("disk", "ディスク")]
        .into_iter()
        .filter_map(|(key, label)| {
            let reason = get(errors, key).filter(|r| !r.is_null())?;
            let text = match reason.as_str() {
                Some("command-failed") => "取得コマンドが失敗",
                Some("invalid-data") => "値を読めなかった",
                Some("missing-status") => "取得結果を確認できなかった",
                _ => "不明",
            };
            Some(format!("{label}（{text}）"))
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// vm_mb: Windows 側の vmmem の実メモリ MB（無ければ None）、total_mb: Windows の物理メモリ MB
pub fn findings(snap: &Value, vm_mb: Option<f64>, total_mb: f64) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(reason) = get(get(Some(snap), "wsl"), "list_error").and_then(Value::as_str).filter(|r| !r.is_empty()) {
        out.push(json!({
            "id": "wsl-list-failed", "severity": "info", "category": "memory",
            "title": "WSL の distro の一覧を読めなかった",
            "detail": format!("一覧の取得失敗: {}（distro が無いと確認できたわけではない）", reason_text(Some(&json!(reason)))),
            "advice": "一覧を取得できないため、WSL の中は未観測。調査のために distro を起動しない。",
        }));
    }
    let all = distros(snap);
    if all.is_empty() {
        return out;
    }
    let running: Vec<&Value> = all.iter().copied().filter(|d| is_state(d, "running")).collect();
    let observed: Vec<&Value> = running.iter().copied().filter(|d| seen(d)).collect();
    let pct = |d: &Value| js::round(used_mb(d.get("inside")).unwrap_or(0.0) / fin(get(d.get("inside"), "mem_total_mb")).unwrap_or(1.0) * 100.0);

    let hot: Vec<&Value> = observed.iter().copied().filter(|d| pct(d) >= MEM_WARN_PCT).collect();
    if let Some(d) = hot.first() {
        let psi = fin(get(d.get("inside"), "psi_some_avg10"));
        let names: Vec<String> = hot.iter().map(|x| name(x)).collect();
        out.push(json!({
            "id": "wsl-inside-memory", "severity": "warn", "category": "memory",
            "title": format!("WSL の内側でメモリが {}% 使われている（{}）", js::num_str(pct(d)), names.join("、")),
            "detail": format!(
                "内側の使用 {} GB / WSL の VM の上限 {} GB{}",
                gb(used_mb(d.get("inside")).unwrap_or(0.0)),
                gb(fin(get(d.get("inside"), "mem_total_mb")).unwrap_or(0.0)),
                psi.map(|p| format!("、メモリ圧迫 PSI some {}%（直近10秒）", js::num_str(p))).unwrap_or_default()
            ),
            "advice": "WSL の VM の中で足りなくなっている（Windows 側の空きとは別）。内側の上位プロセス（機体の詳細）で、開発環境・コンテナ・学習ジョブのどれが大きいかを確かめる。上限を上げるなら .wslconfig の memory（反映には `wsl --shutdown` が要る）。この画面からは変更しない。",
        }));
    }

    // Windows 側（vmmem）が、内側の実使用より多く持ったままか
    if let Some(vm) = vm_mb.filter(|v| v.is_finite()).filter(|_| !observed.is_empty() && observed.len() == running.len()) {
        let inner = observed.iter().filter_map(|d| used_mb(d.get("inside"))).fold(f64::NEG_INFINITY, f64::max);
        let held = vm - inner;
        if held >= HELD_MIN_MB && vm >= total_mb * 0.25 {
            out.push(json!({
                "id": "wsl-reclaim", "severity": "info", "category": "memory",
                "title": format!("WSL の VM が内側の実使用より約 {} GB 多く保持している", gb(held)),
                "detail": format!("Windows 側 vmmem {} GB、内側の使用 {} GB（{} distro は同じ VM を共有）", gb(vm), gb(inner), observed.len()),
                "advice": "内側で解放されたキャッシュが Windows に返っていない。.wslconfig の autoMemoryReclaim=gradual で返りやすくなる。すぐ返すなら、使い終わってから `wsl --shutdown`。",
            }));
        }
    }

    // 観測していない distro（停止中・一覧を読めない・観測に失敗）。正常でも空でもない
    let stopped: Vec<&Value> = all.iter().copied().filter(|d| is_state(d, "stopped_unobserved")).collect();
    let missed: Vec<&Value> =
        all.iter().copied().filter(|d| !is_state(d, "stopped_unobserved") && !seen(d) && !js::is_str(d.get("reason"), "internal-distro-skipped")).collect();
    if !stopped.is_empty() || !missed.is_empty() {
        let mut parts = Vec::new();
        if !stopped.is_empty() {
            parts.push(format!("停止中（起動しないので未観測）: {}", stopped.iter().map(|d| name(d)).collect::<Vec<_>>().join("、")));
        }
        if !missed.is_empty() {
            parts.push(format!(
                "観測できなかった: {}",
                missed.iter().map(|d| format!("{}（{}）", name(d), reason_text(d.get("reason")))).collect::<Vec<_>>().join("、")
            ));
        }
        out.push(json!({
            "id": "wsl-unobserved", "severity": "info", "category": "memory",
            "title": format!("WSL の distro {} 件は中を観測していない", stopped.len() + missed.len()),
            "detail": parts.join(" / "),
            "advice": "停止中の distro は「正常」でも「空」でもなく、観測していないだけ。調べるために起動はしない。中を見たいときは、その distro を自分で起動してから分析する。",
        }));
    }
    let partial: Vec<String> = observed
        .iter()
        .filter_map(|d| {
            let text = partial_text(d);
            (!text.is_empty()).then(|| format!("{}: {}", name(d), text))
        })
        .collect();
    if !partial.is_empty() {
        out.push(json!({
            "id": "wsl-partial", "severity": "info", "category": "memory",
            "title": format!("WSL の distro {} 件で一部を取得できなかった", partial.len()),
            "detail": partial.join(" / "),
            "advice": "取得できたメモリは表示している。欠測項目は空や正常と判断せず、機体の詳細で理由を確認する。",
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(name: &str, avail: i64) -> Value {
        json!({ "name": name, "state": "running", "observed": true, "reason": null, "inside": { "mem_total_mb": 24000, "mem_available_mb": avail, "psi_some_avg10": 12.5 } })
    }

    fn stopped(name: &str) -> Value {
        json!({ "name": name, "state": "stopped_unobserved", "observed": false, "reason": "not-running", "inside": null })
    }

    fn ids(v: &[Value]) -> Vec<String> {
        v.iter().map(|f| f["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn hot_vm_reclaim_and_unobserved_are_separate_findings() {
        let snap = json!({ "wsl": { "distros": [running("dev-a", 1000), running("dev-b", 20000), stopped("old")] } });
        let f = findings(&snap, Some(30000.0), 65536.0);
        assert_eq!(ids(&f), ["wsl-inside-memory", "wsl-reclaim", "wsl-unobserved"]);
        assert!(f[0]["title"].as_str().unwrap().contains("96%") && f[0]["detail"].as_str().unwrap().contains("PSI some 12.5%"));
        assert!(f[1]["detail"].as_str().unwrap().contains("内側の使用 22.5 GB"), "{}", f[1]["detail"]);
        assert!(f[2]["detail"].as_str().unwrap().contains("停止中（起動しないので未観測）: old"));
        assert!(f[2]["title"].as_str().unwrap().starts_with("WSL の distro 1 件"));
    }

    #[test]
    fn nothing_to_say_for_healthy_empty_or_internal() {
        let internal = json!({ "name": "docker-desktop", "state": "running", "observed": false, "reason": "internal-distro-skipped", "inside": null });
        assert!(findings(&json!({ "wsl": { "distros": [] } }), None, 0.0).is_empty());
        assert!(findings(&json!({ "wsl": { "distros": [running("a", 20000), internal] } }), None, 0.0).is_empty());
        assert!(findings(&json!({}), None, 0.0).is_empty());
        // 観測できていない distro があるときは、vmmem との比較をしない
        let missed = json!({ "name": "b", "state": "running", "observed": false, "reason": "timeout", "inside": null });
        let f = findings(&json!({ "wsl": { "distros": [running("a", 20000), missed] } }), Some(20000.0), 65536.0);
        assert_eq!(ids(&f), ["wsl-unobserved"]);
        assert!(f[0]["detail"].as_str().unwrap().contains("b（時間切れ）"));
    }

    #[test]
    fn list_failure_is_distinct_from_an_empty_or_legacy_inventory() {
        for reason in ["timeout", "exit 1", "output-limit"] {
            let f = findings(
                &json!({ "wsl": { "observation_enabled": true, "installed": true, "listed": false, "list_error": reason, "distros": [] } }),
                None,
                0.0,
            );
            assert_eq!(ids(&f), ["wsl-list-failed"]);
            assert!(f[0]["title"].as_str().unwrap().contains("一覧を読めなかった"));
        }
        for wsl in [json!({}), json!({"observation_enabled": false}), json!({"installed": false}), json!({"installed": true, "listed": true, "distros": []})] {
            assert!(findings(&json!({ "wsl": wsl }), None, 0.0).is_empty());
        }
    }

    #[test]
    fn partial_observation_preserves_memory_and_reports_section_reasons() {
        let mut d = running("dev-a", 1000);
        d["inside"]["errors"] = json!({"top": "command-failed", "disk": "invalid-data", "psi": "missing-status"});
        let f = findings(&json!({ "wsl": { "distros": [d] } }), None, 0.0);
        assert_eq!(ids(&f), ["wsl-inside-memory", "wsl-partial"]);
        assert_eq!(f[1]["detail"], "dev-a: PSI（取得結果を確認できなかった）、上位プロセス（取得コマンドが失敗）、ディスク（値を読めなかった）");
        for errors in [Value::Null, json!({}), json!({"top": null}), json!({"unexpected": "secret"})] {
            let mut d = running("dev-a", 20000);
            d["inside"]["errors"] = errors;
            assert!(findings(&json!({ "wsl": { "distros": [d] } }), None, 0.0).is_empty());
        }
    }
}
