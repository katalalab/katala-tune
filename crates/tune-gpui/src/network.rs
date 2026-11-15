//! Session-only projection of the core's sanitized connectivity report.
use crate::panels::{Panel, Row, Tone};
use serde_json::Value;
use tune_core::nodes::Node;
const MAX_ROWS: usize = 200;
pub const EXPLANATION: &str = "設定確認は経路・IP・DNS設定の有無だけを取得し、HTTPS未実行です。外部到達診断は Apple と Cloudflare へ各1回の短いHTTPS要求を送ります（接続2秒・要求4秒、リダイレクトなし）。VPN・proxyの影響を含み、帯域測定ではありません。IP・SSID・DNS名・通信本文は表示・保存しません。結果はこのセッションだけ保持します。";

pub fn initial(nodes: &[Node]) -> Panel {
    Panel {
        facts: vec![("実行状態".into(), "未実行（画面表示・再読込では収集しません）".into()), ("台帳の機体".into(), nodes.len().to_string())],
        rows: nodes
            .iter()
            .take(MAX_ROWS)
            .map(|node| Row {
                title: node.id.clone(),
                badge: "未実行".into(),
                tone: Tone::Muted,
                detail: "明示ボタンで全対象を順番に診断します。実行直前の台帳とconnectivity許可を確認します。sharedやnetwork:falseだけでは拒否しません。"
                    .into(),
            })
            .collect(),
        note: "未実行です。結果はセッション内だけ保持し、別画面の収集・再訪・再読込では消えません。明示操作の直前に台帳の許可を再確認します。".into(),
    }
}
fn flags(data: &Value) -> Vec<(&'static str, &'static str)> {
    let mut keys = vec![("default_route_present", "既定経路"), ("ip_address_present", "IP設定あり"), ("dns_configured", "DNS設定あり")];
    if data["platform"] == "win32" {
        keys.push(("gateway_configured", "Gateway設定あり"));
    } else {
        keys.extend([("interface_up", "経路NIC稼働"), ("link_active", "Link稼働")]);
    }
    keys
}
pub fn warnings(row: &Value) -> bool {
    if row["ok"] != true {
        return false;
    }
    let data = &row["data"];
    flags(data).iter().any(|(key, _)| data[key].as_bool() != Some(true))
        || (data["platform"] == "win32" && data["interfaces_up"].as_u64().is_none_or(|n| n == 0))
        || (data["active_probes"] == true
            && ["named", "fixed_ip"].iter().any(|key| {
                let probe = &data["https"][key];
                probe["state"] != "reachable"
                    || ["dns_ms", "connect_ms", "tls_ms", "first_byte_ms", "total_ms"]
                        .iter()
                        .any(|timing| probe["timings"][timing].as_f64().is_none_or(|n| !n.is_finite() || !(0.0..=60000.0).contains(&n)))
            }))
}
fn error_label(value: &Value) -> &'static str {
    match value.as_str() {
        Some("timeout") => "機体からの取得がタイムアウト",
        Some("auth") => "機体への認証失敗",
        Some("unreachable") => "機体へ接続できません",
        Some("invalid_report") => "診断reportを検証できません",
        Some("disabled_or_unsupported" | "policy_refused") => "現在の台帳・connectivity許可・対応OSによる対象外",
        _ => "診断要求を完了できません（台帳・許可・実行状態を確認してください）",
    }
}
fn https(value: &Value) -> String {
    match value["state"].as_str() {
        Some("reachable") => {
            let Some(status) = value["http_status"].as_u64().filter(|s| (100..=599).contains(s)) else {
                return "未取得".into();
            };
            let timings = [("dns_ms", "DNS"), ("connect_ms", "接続"), ("tls_ms", "TLS"), ("first_byte_ms", "最初のbyte"), ("total_ms", "合計")]
                .iter()
                .map(|(key, label)| {
                    let ms = value["timings"][key]
                        .as_f64()
                        .filter(|n| n.is_finite() && (0.0..=60000.0).contains(n))
                        .map_or_else(|| "未取得".into(), |n| format!("{n:.1}ms"));
                    format!("{label} {ms}")
                })
                .collect::<Vec<_>>()
                .join(" / ");
            format!("到達 HTTP {status} · {timings}（開始からの累積時間）")
        }
        Some("dns_failed") => "DNS解決失敗".into(),
        Some("connect_failed") => "接続失敗".into(),
        Some("timed_out") => "HTTPS要求タイムアウト".into(),
        Some("tls_failed") => "TLS接続失敗".into(),
        Some("tls_certificate_failed") => "TLS証明書検証失敗".into(),
        Some("tool_missing") => "診断ツールなし".into(),
        _ => "未取得".into(),
    }
}
pub fn row(value: &Value, active: bool) -> Row {
    let ok = value["ok"] == true;
    let skipped = value["skipped"] == true;
    let warning = warnings(value);
    let mut detail = vec![format!("モード: {} · 取得開始: {}", if active { "外部到達診断" } else { "設定確認" }, crate::panels::time(&value["at"]))];
    if let Some(wall) = value["wall_s"].as_f64().filter(|n| n.is_finite() && *n >= 0.) {
        detail.push(format!("機体からの取得経過: {wall:.1}秒"));
    }
    if ok {
        let data = &value["data"];
        detail.push(
            flags(data)
                .iter()
                .map(|(key, label)| {
                    format!(
                        "{label}: {}",
                        match data[key].as_bool() {
                            Some(true) => "はい",
                            Some(false) => "いいえ",
                            None => "未取得",
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" · "),
        );
        if data["platform"] == "win32" {
            detail.push(format!("稼働NIC: {}", data["interfaces_up"].as_u64().filter(|n| *n <= 10000).map_or_else(|| "未取得".into(), |n| format!("{n}本"))));
        }
        if active {
            detail.push(format!("Apple: {}", https(&data["https"]["named"])));
            detail.push(format!("Cloudflare: {}", https(&data["https"]["fixed_ip"])));
            detail.push("HTTP 3xx・4xx・5xxも到達です。通信速度・アプリ速度の検査ではありません。".into());
        } else {
            detail.push("HTTPS未実行（設定の有無から外部到達を推定しません）".into());
        }
    } else {
        detail.push(error_label(&value["error"]).into());
    }
    Row {
        title: value["node_id"].as_str().unwrap_or("診断全体").chars().take(128).collect(),
        badge: (if skipped {
            "対象外"
        } else if !ok {
            "取得失敗・診断未取得"
        } else if warning {
            "診断完了・注意/欠測あり"
        } else {
            "診断完了"
        })
        .into(),
        detail: detail.join("\n"),
        tone: if skipped {
            Tone::Muted
        } else if !ok {
            Tone::Error
        } else if warning {
            Tone::Warning
        } else {
            Tone::Good
        },
    }
}
pub fn panel(rows: &[Value], active: bool, total: usize, running: bool) -> Panel {
    let complete = rows.iter().filter(|r| r["ok"] == true).count();
    let skipped = rows.iter().filter(|r| r["skipped"] == true).count();
    let warning = rows.iter().filter(|r| warnings(r)).count();
    Panel {
        facts: vec![
            ("モード".into(), if active { "外部到達診断" } else { "設定確認" }.into()),
            ("進捗".into(), format!("{} / {total} 台 · {}", rows.len(), if running { "実行中" } else { "終了" })),
            ("report取得".into(), format!("成功 {complete} · 失敗 {} · 対象外 {skipped}", rows.len() - complete - skipped)),
            ("診断所見".into(), format!("注意・欠測 {warning}（report取得成功の内数）")),
        ],
        rows: rows.iter().take(MAX_ROWS).map(|r| row(r, active)).collect(),
        note: format!(
            "外部到達診断の対象は Apple / Cloudflare。先頭{}台を表示（{}台省略）。診断結果は取得当時の状態で、現在の接続を保証しません。結果はこのセッションだけ保持し、IP・SSID・DNS名・本文を表示・保存しません。",
            rows.len().min(MAX_ROWS),
            rows.len().saturating_sub(MAX_ROWS)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tune_core::collect::{RunFuture, RunResult, Runner};
    struct Fake {
        report: Value,
        calls: Arc<Mutex<Vec<String>>>,
    }
    impl Runner for Fake {
        fn run<'a>(&'a self, cmd: &'a str, _args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
            self.calls.lock().unwrap().push(cmd.into());
            Box::pin(async move { RunResult { code: Some(0), out: self.report.to_string(), err: "192.0.2.33 fixture-private-body".into() } })
        }
    }
    fn report(active: bool) -> Value {
        json!({"schema":"katala_network_check.v1","platform":"darwin","active_probes":active,
            "default_route_present":true,"ip_address_present":true,"dns_configured":true,"interface_up":true,"link_active":true,
            "ip":"192.0.2.33","ssid":"fixture-private-ssid","resolver":"resolver.fixture.invalid","body":"fixture-private-body",
            "https":{"named":{"state":"reachable","http_status":403,"timings":{"total_ms":12.5},"body":"fixture-private-body"},"fixed_ip":{"state":"tls_failed"}}})
    }
    fn node() -> Node {
        Node::from_value(&json!({"id":"example","os":"macos","shared":true,"network":false,"local_hostname":"synthetic"}), "synthetic")
    }
    #[tokio::test]
    async fn sanitized_active_report_distinguishes_operation_success_and_connectivity_warning() {
        let calls = Arc::new(Mutex::new(vec![]));
        let value = tune_core::network::check_one(&Fake { report: report(true), calls: calls.clone() }, &node(), true).await;
        assert_eq!(value["ok"], true);
        assert!(warnings(&value));
        let result = row(&value, true);
        assert!(result.badge.contains("診断完了") && result.badge.contains("注意"));
        assert!(result.detail.contains("HTTP 403"));
        assert!(result.detail.contains("12.5"));
        assert!(result.detail.contains("TLS"));
        for private in ["192.0.2.33", "fixture-private-ssid", "resolver.fixture.invalid", "fixture-private-body"] {
            assert!(!result.detail.contains(private));
        }
        assert_eq!(*calls.lock().unwrap(), ["/usr/bin/python3"]);
    }
    #[test]
    fn null_flags_and_windows_zero_interfaces_are_not_filled_with_success() {
        let zero = json!({"node_id":"zero","ok":true,"data":{"platform":"win32","active_probes":false,"interfaces_up":0,"default_route_present":false,"ip_address_present":true,"dns_configured":null}});
        let unknown = json!({"node_id":"unknown","ok":true,"data":{"platform":"win32","active_probes":false,"interfaces_up":null}});
        assert!(warnings(&zero) && warnings(&unknown));
        assert!(row(&zero, false).detail.contains("0本"));
        assert!(row(&unknown, false).detail.contains("未取得"));
    }
    #[test]
    fn passive_has_no_https_success_and_transport_errors_never_expose_raw_output() {
        let passive = json!({"node_id":"example","ok":true,"data":report(false)});
        let text = row(&passive, false).detail;
        assert!(text.contains("HTTPS未実行"));
        assert!(!text.contains("HTTP 403"));
        let failed = json!({"node_id":"example","ok":false,"error":"192.0.2.33 raw ssh output","data":report(true)});
        let text = row(&failed, true).detail;
        assert!(!text.contains("192.0.2.33") && !text.contains("HTTP 403") && !text.contains("fixture-private"));
    }
    #[test]
    fn panel_retains_mode_scope_and_bounds_rows_without_dropping_counts() {
        let rows: Vec<_> = (0..300).map(|i| json!({"node_id":format!("example-{i}"),"ok":false,"error":"timeout"})).collect();
        let result = panel(&rows, true, 300, false);
        assert_eq!(result.rows.len(), 200);
        assert!(result.facts.iter().any(|(_, v)| v.contains("300")));
        assert!(result.note.contains("100"));
        assert!(result.note.contains("Apple") && result.note.contains("Cloudflare"));
    }
    #[test]
    fn reachable_http_403_is_not_a_failure_but_missing_timings_stay_unknown() {
        let mut data = report(true);
        let reachable = json!({"state":"reachable","http_status":403,"timings":{"dns_ms":1.,"connect_ms":2.,"tls_ms":3.,"first_byte_ms":4.,"total_ms":5.}});
        data["https"] = json!({"named":reachable,"fixed_ip":reachable});
        let mut value = json!({"node_id":"example","ok":true,"data":data});
        assert!(!warnings(&value));
        value["data"]["https"]["named"]["timings"]["dns_ms"] = Value::Null;
        assert!(warnings(&value));
        assert!(row(&value, true).detail.contains("DNS 未取得"));
    }
}
