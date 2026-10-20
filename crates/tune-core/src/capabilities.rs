//! Finite read-only firmware inventory; maintenance writes are deliberately unavailable.
use crate::{collect, nodes::Node};
use serde_json::{Value, json};
use std::time::Duration;
const MAC: &[u8] = include_bytes!("../../../probes/mac_capabilities.py");
const WIN: &str = include_str!("../../../probes/win_capabilities.ps1");
const TIMEOUT: Duration = Duration::from_secs(35);
const OUT_LIMIT: usize = 64 * 1024;
const ERR_LIMIT: usize = 8 * 1024;
static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn select(nodes: &[Node], ids: &[String]) -> Result<Vec<Node>, String> {
    if ids.iter().any(|id| id.starts_with('-') || !nodes.iter().any(|n| &n.id == id)) {
        return Err("未知の機体または未対応のオプション".into());
    }
    let mut selected: Vec<_> = nodes.iter().filter(|n| if ids.is_empty() { n.local } else { ids.contains(&n.id) }).cloned().collect();
    if selected.is_empty() {
        return Err("調査する機体を指定してください".into());
    }
    for n in &selected {
        if !n.local && (n.alias.is_empty() || n.alias.starts_with('-') || n.alias.chars().any(|c| c.is_whitespace() || c.is_control())) {
            return Err("無効な SSH 接続先".into());
        }
        if let Some(reason) = n.refusal() {
            return Err(reason);
        }
        if n.get("capabilities") == Some(&Value::Bool(false)) {
            return Err("台帳で firmware 調査を停止しています".into());
        }
        if !n.is_mac() && !n.is_windows() {
            return Err("firmware 調査は macOS / Windows のみ対応しています".into());
        }
    }
    selected.sort_by_key(|n| !n.local);
    Ok(selected)
}

/// Fixed script over stdin. No helper installation or remote file creation.
pub fn transport(node: &Node) -> (String, Vec<String>, Vec<u8>) {
    let (cmd, args, input, remote) = if node.is_mac() {
        (
            "/usr/bin/env".into(),
            vec!["python3".into(), "-I".into(), "-".into()],
            MAC.to_vec(),
            "if command -v python3 >/dev/null 2>&1; then exec python3 -I -; else exec /usr/bin/python3 -I -; fi".into(),
        )
    } else {
        let script = WIN.trim_start_matches('\u{feff}');
        let input = format!("{}\n", crate::logs::base64(script.as_bytes())).into_bytes();
        let loader = "& ([scriptblock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String([Console]::In.ReadLine()))))";
        let utf16: Vec<u8> = loader.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = crate::logs::base64(&utf16);
        let args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded].iter().map(|s| s.to_string()).collect();
        let remote = format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}");
        ("powershell.exe".into(), args, input, remote)
    };
    if node.local {
        (cmd, args, input)
    } else {
        let mut args = collect::ssh_args(&node.alias, &remote);
        args.insert(args.len() - 2, "--".into());
        args.splice(0..0, ["-o".into(), "StrictHostKeyChecking=yes".into()]);
        ("ssh".into(), args, input)
    }
}

pub fn sanitize(v: &Value, node: &Node) -> Option<Value> {
    let platform = if node.is_mac() { "darwin" } else { "win32" };
    if v["schema"] != "katala_capabilities.v1" || v["platform"] != platform {
        return None;
    }
    let status = v["inventory_status"].as_str()?;
    if !["ok", "error"].contains(&status) {
        return None;
    }
    let state = v["secure_boot"]["state"].as_str()?;
    let valid = if node.is_mac() {
        state == "not_collected"
    } else {
        ["enabled", "disabled", "permission_denied", "unsupported", "tool_missing", "error"].contains(&state)
    };
    if !valid {
        return None;
    }
    let mut out = json!({"schema":"katala_capabilities.v1","platform":platform,"source":if node.is_mac(){"system_profiler"}else{"windows_cim_secureboot"},"secure_boot":{"state":state},"inventory_status":status});
    let mut missing = Vec::new();
    for key in ["manufacturer", "model", "baseboard_manufacturer", "baseboard_product", "bios_version", "bios_date"] {
        let value = v[key].as_str().filter(|s| !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control));
        out[key] = value.map_or(Value::Null, |s| json!(s));
        if value.is_none() {
            missing.push(key);
        }
    }
    if status == "ok" && (!missing.is_empty()) {
        out["inventory_status"] = json!("partial");
    }
    out["missing_fields"] = json!(missing);
    out["inventory_reason"] = if status == "error" {
        json!(v["inventory_reason"].as_str().filter(|s| ["permission_denied", "unsupported", "error"].contains(s)).unwrap_or("error"))
    } else {
        Value::Null
    };
    let present = ["manufacturer", "model", "baseboard_manufacturer", "baseboard_product", "bios_version", "bios_date"].iter().any(|k| !out[k].is_null());
    let support = if !present {
        "unavailable"
    } else if out["inventory_status"] == "ok" {
        "observed"
    } else {
        "partial"
    };
    out["capabilities"] = json!({
        "firmware_read": {"access":"read_only", "support":support},
        "secure_boot_read": {"access":"read_only", "state":state,"support":if ["enabled","disabled"].contains(&state){"observed"}else{"unavailable"},"privilege":if node.is_mac(){"recoveryos_policy_inspection_unverified"}else{"administrator_required"}},
        "firmware_write": {"support":"unknown", "implementation":"unavailable", "privilege":"privileged", "reboot":"operation_dependent_unverified"}
    });
    if node.is_mac() {
        out["secure_boot"]["reason"] = json!("not_evaluated_requires_recoveryos_policy_inspection");
    }
    Some(out)
}

type RunFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = collect::BoundedResult> + Send + 'a>>;
trait Executor {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], input: &'a [u8], timeout: Duration, out: usize, err: usize) -> RunFuture<'a>;
}
struct System;
impl Executor for System {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], input: &'a [u8], timeout: Duration, out: usize, err: usize) -> RunFuture<'a> {
        Box::pin(collect::run_bounded(cmd, args, input, timeout, out, err))
    }
}
async fn check_one(executor: &impl Executor, node: &Node) -> Value {
    let at = crate::db::now_ms();
    let (cmd, args, input) = transport(node);
    let result = executor.run(&cmd, &args, &input, TIMEOUT, OUT_LIMIT, ERR_LIMIT).await;
    let error = if result.timed_out {
        Some("timeout")
    } else if result.output_limited {
        Some("output_limit")
    } else if result.spawn_failed {
        Some("spawn_failed")
    } else if result.code != Some(0) {
        Some("transport_failed")
    } else {
        None
    };
    let data = if error.is_none() { serde_json::from_slice::<Value>(&result.stdout).ok().and_then(|v| sanitize(&v, node)) } else { None };
    let error = error.or(if data.is_none() { Some("invalid_report") } else { None });
    json!({"node_id":node.id,"at":at,"ok":error.is_none(),"data":data,"error":error})
}
pub async fn check(nodes: &[Node]) -> Result<Value, String> {
    let _guard = RUNNING.try_lock().map_err(|_| "firmware 調査が実行中です")?;
    // Validate again so callers cannot bypass the ledger opt-out with direct use.
    let ids = nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
    let selected = select(nodes, &ids)?;
    let mut rows = Vec::new();
    for node in &selected {
        rows.push(check_one(&System, node).await);
    }
    Ok(json!({"schema":"katala_capabilities_fleet.v1","read_only":true,"nodes":rows}))
}
pub fn maintenance_plan(evidence: &Value) -> Value {
    let requirements = [
        "write_implementation_unavailable",
        "fresh_evidence_required",
        "exact_model_firmware_support_unverified",
        "oem_recovery_procedure_unverified",
        "physical_access_unconfirmed",
        "stable_power_unconfirmed",
        "backup_restore_unverified",
        "encryption_recovery_key_possession_unconfirmed_no_key_collection",
        "maintenance_window_unconfirmed",
        "execution_time_ledger_reread_required",
        "application_confirmation_required",
    ];
    let specs: [(&str, &[&str]); 7] = [
        ("firmware_update", &["oem_recovery_media", "signed_image_exact_model", "stable_power", "rollback_availability_unverified"]),
        ("bios_setting", &["export_current_values", "oem_setting_recovery_method", "physical_console"]),
        ("secure_boot", &["trusted_boot_chain", "recoveryos_or_oem_recovery", "encryption_recovery_key_possession"]),
        ("tpm", &["encrypted_data_recovery", "no_automatic_reset", "recovery_key_possession"]),
        ("bitlocker", &["recovery_key_custody", "no_key_collection", "encrypted_volume_recovery"]),
        ("boot_order", &["known_boot_entry", "physical_console", "boot_recovery_media"]),
        ("reboot", &["active_work_and_sessions_hold", "maintenance_window", "boot_recovery"]),
    ];
    let rows = evidence["nodes"].as_array().cloned().unwrap_or_default();
    let refs: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "node_id":row["node_id"],"at":row["at"],"evidence_ok":row["ok"],
                "source":row["data"]["source"],"model":row["data"]["model"],"bios_version":row["data"]["bios_version"]
            })
        })
        .collect();
    let actions: Vec<Value>=specs.iter().map(|(name,recovery)| {
        let per_node: Vec<Value>=rows.iter().map(|row| {
            let platform=row["data"]["platform"].as_str().unwrap_or("unknown");
            let inapplicable=platform=="darwin" && ["bios_setting","tpm","bitlocker"].contains(name);
            let privilege=if platform=="darwin" {"administrator_or_recoveryos_unverified"} else {"administrator_and_firmware_authorization_unverified"};
            json!({"node_id":row["node_id"],"support":if inapplicable{"unsupported"}else{"unknown"},"reason":if inapplicable{"platform_not_applicable"}else{"exact_oem_operation_support_unverified"},"access":"write","privilege":privilege,"executable":false})
        }).collect();
        let support=if !per_node.is_empty() && per_node.iter().all(|n|n["support"]=="unsupported") {"unsupported"}else{"unknown"};
        json!({"action":name,"executable":false,"implementation":"unavailable","support":support,"access":"write","privilege":"privileged_operation_authorization_required","reboot":if *name=="reboot"{"yes"}else{"vendor_or_setting_dependent_unverified"},"recovery_conditions":recovery,"evidence_refs":refs,"nodes":per_node,"missing_requirements":requirements})
    }).collect();
    json!({"schema":"katala_maintenance_plan.v1","executable":false,"read_only_refresh_available":true,"evidence":evidence,"actions":actions})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Fake(Mutex<Option<collect::BoundedResult>>);
    impl Executor for Fake {
        fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], input: &'a [u8], timeout: Duration, out: usize, err: usize) -> RunFuture<'a> {
            assert_eq!(timeout, Duration::from_secs(35));
            assert_eq!((out, err), (65536, 8192));
            assert_eq!(cmd, "ssh");
            assert!(args.contains(&"StrictHostKeyChecking=yes".to_string()));
            assert!(!input.is_empty());
            Box::pin(async { self.0.lock().unwrap().take().unwrap() })
        }
    }
    #[tokio::test]
    async fn bounded_failures_discard_raw_or_partial_output() {
        let node = Node::from_value(&json!({"id":"sample","alias":"sample","os":"windows"}), "local");
        let results = [
            (collect::BoundedResult { timed_out: true, ..Default::default() }, "timeout"),
            (collect::BoundedResult { output_limited: true, ..Default::default() }, "output_limit"),
            (collect::BoundedResult { spawn_failed: true, ..Default::default() }, "spawn_failed"),
            (collect::BoundedResult { code: Some(1), ..Default::default() }, "transport_failed"),
            (collect::BoundedResult { code: Some(0), stdout: b"{\"schema\":".to_vec(), ..Default::default() }, "invalid_report"),
            (collect::BoundedResult { code: Some(0), stdout: b"null".to_vec(), ..Default::default() }, "invalid_report"),
        ];
        for (mut result, error) in results {
            result.stderr = b"PRIVATE".to_vec();
            let answer = check_one(&Fake(Mutex::new(Some(result))), &node).await;
            assert_eq!(answer["ok"], false);
            assert_eq!(answer["error"], error);
            assert_eq!(answer["data"], Value::Null);
            assert!(!answer.to_string().contains("PRIVATE"));
        }
    }
    #[tokio::test]
    async fn successful_transport_returns_whitelisted_evidence() {
        let node = Node::from_value(&json!({"id":"sample","alias":"sample","os":"windows"}), "local");
        let stdout=json!({"schema":"katala_capabilities.v1","platform":"win32","inventory_status":"ok","manufacturer":"Example","model":"Model","baseboard_manufacturer":"Example","baseboard_product":"Board","bios_version":"1","bios_date":"2026-01-01","secure_boot":{"state":"disabled"},"serial":"PRIVATE"}).to_string().into_bytes();
        let answer = check_one(&Fake(Mutex::new(Some(collect::BoundedResult { code: Some(0), stdout, ..Default::default() }))), &node).await;
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["data"]["capabilities"]["firmware_read"]["support"], "observed");
        assert_eq!(answer["data"]["capabilities"]["firmware_write"]["support"], "unknown");
        assert!(!answer.to_string().contains("PRIVATE"));
    }
    #[test]
    fn missing_fields_and_inventory_errors_are_explicit() {
        let n = Node::from_value(&json!({"id":"sample","os":"macos"}), "local");
        let mut v = json!({"schema":"katala_capabilities.v1","platform":"darwin","inventory_status":"ok","secure_boot":{"state":"not_collected"},"manufacturer":"Apple","model":"Sample","bios_version":"1"});
        let clean = sanitize(&v, &n).unwrap();
        assert_eq!(clean["inventory_status"], "partial");
        assert!(clean["missing_fields"].as_array().unwrap().contains(&json!("bios_date")));
        v["inventory_status"] = json!("error");
        assert_eq!(sanitize(&v, &n).unwrap()["inventory_status"], "error");
        v["model"] = json!("bad\nvalue");
        assert_eq!(sanitize(&v, &n).unwrap()["model"], Value::Null);
    }
}

#[cfg(all(test, windows))]
mod windows_script_tests {
    use super::*;
    /// Execute the actual shipped probe with mock native commands, never hardware writes.
    #[tokio::test]
    async fn native_script_classifies_privileges_and_partial_cim_without_localized_messages() {
        let mock_cim = r#"
function Get-CimInstance {
    param($ClassName,$Property,$OperationTimeoutSec)
    if ($ClassName -eq "Win32_ComputerSystem") { return @{ Manufacturer = "Example"; Model = "Model" } }
    if ($ClassName -eq "Win32_BaseBoard") { return @{ Manufacturer = "Example"; Product = "Board" } }
    return @{ SMBIOSBIOSVersion = "1"; ReleaseDate = [datetime]::new(2026,1,1) }
}
"#;
        let cases = [
            ("return $true", "enabled"),
            ("return $false", "disabled"),
            ("throw [System.UnauthorizedAccessException]::new(\"localized\")", "permission_denied"),
            ("throw [System.ComponentModel.Win32Exception]::new(1314)", "permission_denied"),
            ("throw [System.ComponentModel.Win32Exception]::new(5)", "permission_denied"),
            ("throw [System.PlatformNotSupportedException]::new(\"localized\")", "unsupported"),
            ("throw [System.Exception]::new(\"localized\")", "error"),
        ];
        let n = Node::from_value(&json!({"id":"sample","os":"windows","local_hostname":"local"}), "local");
        let (cmd, args, _) = transport(&n);
        for (body, state) in cases {
            let script = format!("{mock_cim}\nfunction Confirm-SecureBootUEFI {{ {body} }}\n{}", WIN.trim_start_matches('\u{feff}'));
            let input = format!("{}\n", crate::logs::base64(script.as_bytes()));
            let result = collect::run_bounded(&cmd, &args, input.as_bytes(), TIMEOUT, OUT_LIMIT, ERR_LIMIT).await;
            assert_eq!(result.code, Some(0));
            let value: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(value["secure_boot"]["state"], state);
            assert_eq!(value["model"], "Model");
            assert!(!String::from_utf8_lossy(&result.stdout).contains("localized"));
        }
        let script = format!(
            "{mock_cim}\nfunction Get-CimInstance {{ param($ClassName,$Property,$OperationTimeoutSec) if ($ClassName -eq \"Win32_BaseBoard\") {{ throw [System.UnauthorizedAccessException]::new() }} return @{{ Manufacturer=\"Example\";Model=\"Model\";SMBIOSBIOSVersion=\"1\" }} }}\nfunction Confirm-SecureBootUEFI {{ return $false }}\n{}",
            WIN.trim_start_matches('\u{feff}')
        );
        let input = format!("{}\n", crate::logs::base64(script.as_bytes()));
        let result = collect::run_bounded(&cmd, &args, input.as_bytes(), TIMEOUT, OUT_LIMIT, ERR_LIMIT).await;
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["model"], "Model");
        assert_eq!(value["bios_version"], "1");
        assert_eq!(value["inventory_reason"], "permission_denied");
        assert_eq!(value["secure_boot"]["state"], "disabled");
    }
}
