//! Codex の残り枠。各機体で probes/codex_limits.py を流し、Codex 自身の `codex app-server` に JSON-RPC の
//! `account/rateLimits/read` を問い合わせる。受け取るのは窓の長さ（分）・使用率・リセット時刻だけ。
//! 認証は Codex が持ち、Tune は Cookie・トークン・auth.json を読まない。
//!
//! - 呼ぶのは AI の取り込みのときだけ（既定 30 分ごと）。続きを読むための 2 分ごとの取り込みでは呼ばない（[`MIN_INTERVAL_MINUTES`]）
//! - 共用機（shared）では呼ばない。台帳の機体に `"codex_limits": true / false` と書けば、それに従う
//! - 窓は長さで見分ける（5 時間 = 300 分、7 日 = 10080 分）。報告に無い窓は「未報告」（無制限とは扱わない）
//! - 起動の直後は窓が空で返ることがあるので、調査スクリプトが少し待って 1 回だけ読み直す。それでも空なら empty

use std::time::Duration;

use serde_json::Value;

use crate::ai_sessions;
use crate::collect::{RunResult, last_json_line};
use crate::db::LimitWindow;
use crate::js;
use crate::nodes::Node;

pub const LIMITS_PROBE: &str = include_str!("../../../probes/codex_limits.py");
/// 同じ機体に問い合わせる間隔の下限（分）。取り込み（既定 30 分ごと）のたびに 1 回で、続きの取り込みでは呼ばない
pub const MIN_INTERVAL_MINUTES: i64 = 25;
/// 画面に並べる窓（長さ（分）, 名前）
pub const KNOWN_WINDOWS: [(i64, &str); 2] = [(300, "5 時間"), (10080, "7 日")];
const TIMEOUT: Duration = Duration::from_secs(60);

/// 問い合わせる機体か。台帳の "codex_limits" が true / false ならそれに従う。
/// 書いていなければ、AI の取り込みをする機体で、共用機でないもの
pub fn enabled(node: &Node) -> bool {
    match node.get("codex_limits") {
        Some(Value::Bool(b)) => *b,
        _ => ai_sessions::enabled(node) && !node.shared,
    }
}

/// 問い合わせの結果
#[derive(Clone, Debug, PartialEq)]
pub enum Limits {
    /// 読めた（窓が空なら empty）
    Ok {
        windows: Vec<LimitWindow>,
        empty: bool,
        version: Option<String>,
    },
    NoCodex,
    NoPython(String),
    Error(String),
}

impl Limits {
    /// 保存する状態の名前（ai_limits_status.state）
    pub fn state(&self) -> &'static str {
        match self {
            Limits::Ok { empty: true, .. } => "empty",
            Limits::Ok { .. } => "ok",
            Limits::NoCodex => "no_codex",
            Limits::NoPython(_) => "no_python",
            Limits::Error(_) => "error",
        }
    }
}

/// 出力を結果にする。窓は長さ・使用率・リセット時刻だけを取り出す（ほかの項目があっても使わない）
pub fn parse(r: &RunResult) -> Limits {
    if let Some(v) = last_json_line(&r.out).filter(|v| v.get("codex").is_some_and(Value::is_boolean)) {
        if v["codex"] == Value::Bool(false) {
            return Limits::NoCodex;
        }
        if let Some(e) = v.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()) {
            return Limits::Error(js::slice16(e, 300));
        }
        let windows: Vec<LimitWindow> = js::arr(v.get("windows"))
            .iter()
            .filter_map(|w| {
                let used = w.get("used_pct").and_then(Value::as_f64).filter(|x| x.is_finite())?;
                Some(LimitWindow {
                    mins: w.get("mins").and_then(Value::as_i64).filter(|m| *m > 0),
                    used_pct: used.clamp(0.0, 1000.0),
                    resets_at: w.get("resets_at").and_then(Value::as_i64).filter(|t| *t > 0),
                })
            })
            .collect();
        let empty = windows.is_empty();
        return Limits::Ok { windows, empty, version: v.get("version").and_then(Value::as_str).map(str::to_string) };
    }
    match ai_sessions::parse(r) {
        ai_sessions::Fetched::NoPython(m) => Limits::NoPython(m),
        ai_sessions::Fetched::Error(e) => Limits::Error(e),
        ai_sessions::Fetched::Ok(_) => Limits::Error("残り枠の結果が無い".into()),
    }
}

/// 1台に問い合わせる
pub async fn fetch(node: &Node) -> Limits {
    let r = ai_sessions::run_python(node, LIMITS_PROBE, TIMEOUT).await;
    parse(&r)
}

/// 調査スクリプトの版
pub fn probe_version() -> Option<String> {
    ai_sessions::script_const(LIMITS_PROBE, "KT_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_sessions::NO_PYTHON;
    use serde_json::json;

    fn out(s: &str) -> RunResult {
        RunResult { code: Some(0), out: s.into(), err: String::new() }
    }

    #[test]
    fn parses_windows_by_length_and_drops_other_fields() {
        let r = out(&json!({ "codex": true, "windows": [
            { "mins": 300, "used_pct": 12.5, "resets_at": 1_900_000_000, "plan": "x" },
            { "mins": null, "used_pct": 3, "resets_at": null },
            { "mins": 10080, "used_pct": "bad" }
        ], "error": null })
        .to_string());
        let Limits::Ok { windows, empty, .. } = parse(&r) else { panic!() };
        assert!(!empty);
        assert_eq!(
            windows,
            vec![LimitWindow { mins: Some(300), used_pct: 12.5, resets_at: Some(1_900_000_000) }, LimitWindow { mins: None, used_pct: 3.0, resets_at: None }]
        );
    }

    #[test]
    fn outcomes() {
        assert_eq!(parse(&out("{\"codex\":false,\"windows\":[]}")), Limits::NoCodex);
        assert_eq!(parse(&out("{\"codex\":true,\"windows\":[],\"empty\":true,\"error\":null}")).state(), "empty");
        assert!(
            matches!(parse(&out("{\"codex\":true,\"windows\":[],\"error\":\"chatgpt authentication required to read rate limits\"}")), Limits::Error(e) if e.contains("authentication"))
        );
        assert!(matches!(parse(&out(&format!("{NO_PYTHON}\n"))), Limits::NoPython(_)));
        let ssh = RunResult { code: Some(255), out: String::new(), err: "ssh: connect to host x port 22: Operation timed out".into() };
        assert!(matches!(parse(&ssh), Limits::Error(_)));
    }

    #[test]
    fn shared_nodes_are_not_asked() {
        let n = |v: Value| Node::from_value(&v, "other-host");
        assert!(enabled(&n(json!({ "id": "a", "alias": "a", "os": "macos" }))));
        // 共用機は AI の取り込みを明示しても、残り枠は呼ばない（呼ぶなら codex_limits: true）
        assert!(!enabled(&n(json!({ "id": "s", "alias": "s", "os": "windows", "shared": true, "ai_sessions": true }))));
        assert!(enabled(&n(json!({ "id": "s", "alias": "s", "os": "windows", "shared": true, "codex_limits": true }))));
        assert!(!enabled(&n(json!({ "id": "a", "alias": "a", "os": "macos", "ai_sessions": false }))));
        assert!(!enabled(&n(json!({ "id": "a", "alias": "a", "os": "macos", "codex_limits": false }))));
    }

    #[test]
    fn probe_has_a_version_and_reads_no_credentials() {
        assert!(probe_version().is_some());
        // 認証情報のファイルを開かない（Codex に任せる）
        for bad in ["auth.json", "cookie", "Cookie", "access_token", "refresh_token"] {
            assert!(!LIMITS_PROBE.lines().filter(|l| !l.trim_start().starts_with('#')).any(|l| l.contains(bad)), "{bad}");
        }
    }
}
