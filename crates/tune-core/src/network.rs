//! 手動の軽量接続診断。宛先・SSID・通信本文・ログ・サービスは集めない。DBと設定を変更しない。
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::collect::{self, Runner, System};
use crate::db::now_ms;
use crate::nodes::Node;

const MAC: &str = include_str!("../../../probes/mac_network.py");
const WIN: &[u8] = include_bytes!("../../../probes/win_network.ps1");
const TIMEOUT: Duration = Duration::from_secs(35);
static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 空ならこの機体だけ。明示した未知の ID と無効な機体は、実行前に拒否する。
pub fn select(nodes: &[Node], ids: &[String]) -> Result<Vec<Node>, String> {
    if ids.iter().any(|id| !nodes.iter().any(|n| &n.id == id)) {
        return Err("台帳に無い機体".into());
    }
    let selected: Vec<_> = nodes.iter().filter(|n| if ids.is_empty() { n.local } else { ids.contains(&n.id) }).cloned().collect();
    if selected.is_empty() {
        return Err("診断する機体を指定してください".into());
    }
    if selected.iter().any(|n| n.get("connectivity") == Some(&Value::Bool(false))) {
        return Err("台帳で接続診断を止めている機体".into());
    }
    if selected.iter().any(|n| !n.is_mac() && !n.is_windows()) {
        return Err("接続診断は macOS / Windows に対応しています".into());
    }
    Ok(selected)
}

/// 1 アプリからは同時に走らせず、選んだ機体を順番に調べる。自動スキャンからは呼ばない。
pub async fn check(nodes: &[Node], active: bool) -> Result<Value, String> {
    let _guard = RUNNING.try_lock().map_err(|_| "接続診断が実行中です")?;
    let mut rows = Vec::new();
    for n in nodes {
        rows.push(check_one(&System, n, active).await);
    }
    Ok(json!({ "schema": "katala_network_fleet.v1", "nodes": rows }))
}

pub async fn check_one(runner: &dyn Runner, node: &Node, active: bool) -> Value {
    let at = now_ms();
    let started = Instant::now();
    if node.get("connectivity") == Some(&Value::Bool(false)) || (!node.is_mac() && !node.is_windows()) {
        return json!({ "node_id": node.id, "at": at, "ok": false, "error": "disabled_or_unsupported" });
    }
    let result = if node.is_mac() {
        let suffix = if active { " --probe" } else { "" };
        if node.local {
            let mut args = vec!["-".to_string()];
            if active {
                args.push("--probe".into());
            }
            runner.run("/usr/bin/python3", &args, Some(MAC.as_bytes()), TIMEOUT).await
        } else {
            runner.run("ssh", &collect::ssh_args(&node.alias, &format!("exec /usr/bin/python3 -{suffix}")), Some(MAC.as_bytes()), TIMEOUT).await
        }
    } else {
        let script = String::from_utf8_lossy(WIN).trim_start_matches('\u{feff}').to_string();
        let text = format!("& {{ {script}\n }} {}", if active { "-Probe" } else { "" });
        // Windows sshd の既定シェル経由では長いコマンド行が切られる。
        // 固定の短い loader を呼び、埋め込み probe は ASCII の stdin で渡す。
        // Base64 は単一行なので LF を付け、EOF の転送を待たずに読み終える。
        // Runner の stdin 閉鎖と全体の TIMEOUT も維持する。
        let payload = format!("{}\n", crate::logs::base64(text.as_bytes()));
        let loader = "& ([scriptblock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String([Console]::In.ReadLine()))))";
        let utf16: Vec<u8> = loader.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = crate::logs::base64(&utf16);
        let command = format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}");
        if node.local {
            let args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded].iter().map(|s| s.to_string()).collect::<Vec<_>>();
            runner.run("powershell.exe", &args, Some(payload.as_bytes()), TIMEOUT).await
        } else {
            runner.run("ssh", &collect::ssh_args(&node.alias, &command), Some(payload.as_bytes()), TIMEOUT).await
        }
    };
    let data = collect::last_json_line(&result.out).and_then(|v| sanitize(&v, node, active));
    let error = if result.code != Some(0) {
        Some(collect::classify_failure(&result))
    } else if data.is_none() {
        Some("invalid_report")
    } else {
        None
    };
    // 生の stdout/stderr は宛先や OS のパスを含む可能性があるため返さない。
    json!({ "node_id": node.id, "at": at, "ok": error.is_none(), "wall_s": started.elapsed().as_secs_f64(), "data": data, "error": error })
}

fn sanitize(v: &Value, node: &Node, active: bool) -> Option<Value> {
    let platform = if node.is_mac() { "darwin" } else { "win32" };
    if v["schema"] != "katala_network_check.v1" || v["platform"] != platform || v["active_probes"] != active {
        return None;
    }
    let mut out = json!({ "schema": "katala_network_check.v1", "platform": platform, "active_probes": active });
    for key in ["default_route_present", "ip_address_present", "dns_configured", "gateway_configured", "interface_up", "link_active"] {
        out[key] = v[key].as_bool().map_or(Value::Null, Value::Bool);
    }
    out["interfaces_up"] = v["interfaces_up"].as_u64().filter(|n| *n <= 10000).map_or(Value::Null, |n| json!(n));
    if active {
        for key in ["named", "fixed_ip"] {
            let r = &v["https"][key];
            let state = r["state"].as_str()?;
            if !["reachable", "dns_failed", "connect_failed", "timed_out", "tls_failed", "tls_certificate_failed", "tool_missing", "unavailable"]
                .contains(&state)
            {
                return None;
            }
            let mut item = json!({ "state": state });
            if state == "reachable" {
                let status = r["http_status"].as_u64().filter(|s| (100..=599).contains(s))?;
                item["http_status"] = json!(status);
                for timing in ["dns_ms", "connect_ms", "tls_ms", "first_byte_ms", "total_ms"] {
                    item["timings"][timing] =
                        r["timings"][timing].as_f64().filter(|n| n.is_finite() && (0.0..=60000.0).contains(n)).map_or(Value::Null, |n| json!(n));
                }
            }
            out["https"][key] = item;
        }
    }
    Some(out)
}

impl crate::engine::Engine {
    pub async fn network_check(&self, ids: &[String], active: bool) -> Result<Value, String> {
        let cfg = crate::nodes::load_config(self.config_path())?;
        check(&select(&cfg.nodes, ids)?, active).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{RunFuture, RunResult};
    use std::sync::Mutex;

    struct Fake(Mutex<Vec<String>>);
    impl Runner for Fake {
        fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], input: Option<&'a [u8]>, timeout: Duration) -> RunFuture<'a> {
            assert_eq!(timeout, TIMEOUT);
            let platform = if cmd == "powershell.exe" || args.iter().any(|a| a.contains("powershell.exe")) { "win32" } else { "darwin" };
            if platform == "win32" {
                assert!(args.iter().map(String::len).sum::<usize>() < 1000, "Windows transport must fit the sshd shell command limit");
                assert!(input.is_some_and(|s| s.len() > 6000 && s.is_ascii()), "The complete probe must travel through stdin");
                assert!(
                    input.is_some_and(|s| s.ends_with(b"\n") && s.iter().filter(|c| **c == b'\n').count() == 1),
                    "One complete Base64 line must terminate without waiting for EOF"
                );
                assert!(
                    input.is_some_and(|s| s[..s.len() - 1].iter().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))),
                    "Payload before LF must contain only standard Base64"
                );
            }
            assert!(!input.is_some_and(|s| s.windows(8).any(|w| w == b"netsec()")));
            self.0.lock().unwrap().push(cmd.into());
            Box::pin(async move {
                RunResult { code: Some(0), err: "192.0.2.1 raw error".into(), out: json!({"schema":"katala_network_check.v1", "platform":platform,"active_probes":false,"ip_address_present":true,"private_ip":"192.0.2.1"}).to_string() }
            })
        }
    }
    fn node(os: &str, local: bool) -> Node {
        Node::from_value(
            &json!({"id":"test-node","alias":"test-alias","os":os,"shared":true,"network":false,"local_hostname":if local {"host"} else {"other"}}),
            "host",
        )
    }
    #[tokio::test]
    async fn all_local_and_remote_routes_work_with_shared_and_netsec_disabled() {
        let fake = Fake(Mutex::new(Vec::new()));
        for os in ["macos", "windows"] {
            for local in [true, false] {
                let r = check_one(&fake, &node(os, local), false).await;
                assert_eq!(r["ok"], true);
                assert_eq!(r["data"]["ip_address_present"], true);
                assert!(!r.to_string().contains("192.0.2.1"));
            }
        }
        assert_eq!(*fake.0.lock().unwrap(), ["/usr/bin/python3", "ssh", "powershell.exe", "ssh"]);
    }
    #[test]
    fn selection_defaults_to_local_and_rejects_unknown_and_disabled() {
        let nodes = vec![node("macos", true)];
        assert_eq!(select(&nodes, &[]).unwrap().len(), 1);
        assert!(select(&nodes, &["absent".into()]).is_err());
        let mut disabled = nodes[0].clone();
        disabled.raw["connectivity"] = json!(false);
        assert!(select(&[disabled], &[]).is_err());
        assert!(select(&[node("macos", false)], &[]).is_err());
    }
    #[test]
    fn active_results_are_whitelisted_and_incomplete_reports_fail_closed() {
        let n = node("windows", true);
        let mut v = json!({"schema":"katala_network_check.v1", "platform":"win32", "active_probes":true,"https":{"named":{"state":"reachable","http_status":403,"timings":{"total_ms":12.5},"body":"private"},"fixed_ip":{"state":"tls_failed","error":"secret"}}});
        let s = sanitize(&v, &n, true).unwrap();
        assert_eq!(s["https"]["named"]["timings"]["total_ms"], 12.5);
        assert!(!s.to_string().contains("private"));
        assert!(!s.to_string().contains("secret"));
        v["https"]["named"]["http_status"] = json!(0);
        assert!(sanitize(&v, &n, true).is_none());
        assert!(sanitize(&v, &n, false).is_none());
    }
}
