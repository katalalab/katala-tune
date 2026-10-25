//! Finite read-only machine inventory. Rust owns collection and the public report contract.
mod macos;
mod windows;

use crate::{collect, db::now_ms, nodes::Node};
use serde_json::{Map, Value, json};
use std::time::Duration;

pub const SCHEMA: &str = "katala_specs.v1";
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_COMPONENTS: usize = 256;
pub const SECTIONS: &[&str] =
    &["system", "cpu", "memory", "gpu", "storage", "board", "firmware", "os", "display", "network", "usb", "thunderbolt", "pci", "audio", "settings"];

/// No fuzzy model matching: suffixes and variants are materially different hardware.
pub fn official(model: &str) -> Value {
    let catalog: Value = serde_json::from_str(include_str!("../../../../data/specs-catalog.json")).expect("bundled specifications catalog");
    for p in catalog["products"].as_array().into_iter().flatten() {
        if p["models"].as_array().into_iter().flatten().any(|m| m.as_str().is_some_and(|m| m.eq_ignore_ascii_case(model.trim()))) {
            return json!({"status":"matched", "name":p["name"],"url":p["url"],"scope":p["scope"],"specifications":p["specifications"],"verified_at":catalog["verified_at"],"catalog_version":catalog["version"]});
        }
    }
    json!({"status":"unverified", "reason":"exact_model_not_in_catalog"})
}

pub(super) fn scalar(v: &Value) -> Option<Value> {
    match v {
        Value::String(s) if !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control) => Some(json!(s.trim())),
        Value::Number(n) if n.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0) => Some(v.clone()),
        Value::Bool(_) => Some(v.clone()),
        _ => None,
    }
}

/// Only these named fields can cross the remote boundary or enter the private DB.
pub const FIELDS: &[&str] = &[
    "cluster0_name",
    "cluster0_cores",
    "cluster1_name",
    "cluster1_cores",
    "model",
    "manufacturer",
    "part_number",
    "slot",
    "cores",
    "threads",
    "p_cores",
    "e_cores",
    "architecture",
    "max_clock_mhz",
    "current_clock_mhz",
    "l2_cache_kib",
    "l3_cache_kib",
    "capacity_bytes",
    "capacity",
    "rated_speed_mts",
    "configured_speed_mts",
    "memory_type",
    "form_factor",
    "data_width_bits",
    "voltage_mv",
    "memory_kind",
    "dedicated_vram_bytes",
    "vram",
    "driver_version",
    "driver_date",
    "firmware_version",
    "bios_date",
    "interface",
    "media_type",
    "bus",
    "link_speed",
    "status",
    "resolution",
    "refresh_hz",
    "vendor_id",
    "product_id",
    "device_id",
    "class",
    "version",
    "build",
    "kernel",
    "metal",
    "cuda_version",
    "vbios_version",
    "power_limit_w",
    "pcie_generation",
    "pcie_width",
    "secure_boot",
    "virtualization_firmware",
    "hypervisor_present",
    "slat",
    "power_plan",
    "low_power_mode",
    "sleep_minutes",
    "display_sleep_minutes",
    "disk_sleep_minutes",
    "hibernate_mode",
    "battery_cycles",
    "battery_condition",
    "battery_max_capacity_pct",
    "xmp_expo",
    "boot_mode",
    "setting",
    "value",
    "duplex",
    "speed_bps",
    "enabled",
    "dhcp_enabled",
    "mtu_bytes",
];

pub(super) fn component(name: &str, source: &str, fields: Map<String, Value>) -> Value {
    let model = fields.get("model").and_then(Value::as_str).unwrap_or(name);
    let missing: Vec<&str> =
        ["manufacturer", "model", "part_number", "firmware_version", "driver_version"].into_iter().filter(|k| !fields.contains_key(*k)).collect();
    json!({"name":name, "source":source,"fields":fields,"official":official(model),"missing_fields":missing})
}

pub(super) fn mapped(v: &Value, mapping: &[(&str, &str)]) -> Map<String, Value> {
    mapping.iter().filter_map(|(from, to)| scalar(&v[*from]).map(|v| ((*to).into(), v))).collect()
}

pub(super) fn section(components: Vec<Value>, status: &str, source: &str) -> Value {
    let providers: Vec<Value> = if source.is_empty() { vec![] } else { vec![json!({"source":source,"status":status})] };
    json!({"status":status,"source":source,"components":components,"providers":providers})
}

pub fn normalize(platform: &str, raw: &Value) -> Value {
    let mut sections = Map::new();
    for id in SECTIONS {
        sections.insert((*id).into(), section(vec![], "not_collected", ""));
    }
    match platform {
        "windows" => windows::normalize(raw, &mut sections),
        "macos" => macos::normalize(raw, &mut sections),
        _ => {}
    }
    let collected =
        sections.values().any(|s| matches!(s["status"].as_str(), Some("ok" | "partial")) && s["components"].as_array().is_some_and(|c| !c.is_empty()));
    json!({"schema":SCHEMA,"platform":platform,"at":now_ms(),"ok":collected,"collector_version":env!("CARGO_PKG_VERSION"),"sections":sections})
}

pub fn validate_report(v: &Value, platform: &str) -> Result<Value, String> {
    if v["schema"] != SCHEMA || v["platform"] != platform || !matches!(platform, "windows" | "macos") {
        return Err("収集形式が未対応です。対象機の tune CLI を更新してください".into());
    }
    let at = v["at"].as_i64().filter(|at| *at > 0 && *at <= now_ms() + 300_000).ok_or("invalid collection time")?;
    let mut sections = Map::new();
    for id in SECTIONS {
        let s = &v["sections"][*id];
        let status = s["status"]
            .as_str()
            .filter(|s| {
                ["ok", "partial", "empty", "not_collected", "permission_denied", "unsupported", "error", "timeout", "tool_missing", "output_limit"].contains(s)
            })
            .ok_or("invalid section status")?;
        let source = scalar(&s["source"]).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        let items = s["components"].as_array().ok_or("invalid components")?;
        if items.len() > MAX_COMPONENTS {
            return Err("too many components".into());
        }
        let mut components = Vec::new();
        for c in items {
            let fields = c["fields"].as_object().ok_or("invalid fields")?;
            let fields: Map<String, Value> =
                fields.iter().filter(|(k, _)| FIELDS.contains(&k.as_str())).filter_map(|(k, v)| scalar(v).map(|v| (k.clone(), v))).collect();
            let name = scalar(&c["name"]).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| (*id).into());
            let component_source = scalar(&c["source"]).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| source.clone());
            components.push(component(&name, &component_source, fields));
        }
        let mut safe_section = section(components, status, &source);
        if let Some(providers) = s["providers"].as_array() {
            safe_section["providers"] = json!(
                providers
                    .iter()
                    .take(8)
                    .filter_map(|p| {
                        let source = scalar(&p["source"])?;
                        let state = p["status"].as_str().filter(|s| {
                            ["ok", "partial", "empty", "not_collected", "permission_denied", "unsupported", "error", "timeout", "tool_missing", "output_limit"]
                                .contains(s)
                        })?;
                        Some(json!({"source":source,"status":state}))
                    })
                    .collect::<Vec<_>>()
            );
        }
        sections.insert((*id).into(), safe_section);
    }
    let ok = sections.values().any(|s| matches!(s["status"].as_str(), Some("ok" | "partial")) && s["components"].as_array().is_some_and(|c| !c.is_empty()));
    Ok(json!({"schema":SCHEMA,"platform":platform,"at":at,"ok":ok,"collector_version":scalar(&v["collector_version"]),"sections":sections}))
}

/// A shared node needs an explicit specs-only read opt-in; other shared policies remain intact.
pub fn enabled(node: &Node) -> bool {
    node.get("specs") != Some(&Value::Bool(false)) && (!node.shared || node.get("specs") == Some(&Value::Bool(true)))
}

pub fn select(nodes: &[Node], ids: &[String]) -> Result<Vec<Node>, String> {
    if ids.len() > 64 || ids.iter().any(|id| !nodes.iter().any(|n| &n.id == id)) {
        return Err("未知の機体です".into());
    }
    let selected: Vec<_> = nodes.iter().filter(|n| ids.contains(&n.id)).cloned().collect();
    if selected.is_empty() {
        return Err("収集する機体を選択してください".into());
    }
    for n in &selected {
        if let Some(reason) = n.refusal() {
            return Err(reason);
        }
        if !enabled(n) {
            return Err("共用機の詳細仕様には台帳の specs: true が必要です。specs: false の機体は収集しません".into());
        }
        if !n.is_mac() && !n.is_windows() {
            return Err("macOS / Windows のみ対応しています".into());
        }
        if !n.local && (n.alias.is_empty() || n.alias.starts_with('-') || !n.alias.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))) {
            return Err("無効な SSH alias".into());
        }
    }
    Ok(selected)
}

pub(super) async fn provider(cmd: &str, args: &[String], timeout: u64) -> Result<Value, String> {
    let r = collect::run_bounded(cmd, args, &[], Duration::from_secs(timeout), MAX_BYTES, 8192).await;
    if r.timed_out {
        return Err("timeout".into());
    }
    if r.output_limited {
        return Err("output_limit".into());
    }
    if r.spawn_failed {
        return Err("tool_missing".into());
    }
    if r.code != Some(0) {
        return Err("error".into());
    }
    serde_json::from_slice(&r.stdout).map_err(|_| "error".into())
}

pub async fn collect_local() -> Value {
    #[cfg(windows)]
    let (platform, raw) = ("windows", windows::collect().await);
    #[cfg(target_os = "macos")]
    let (platform, raw) = ("macos", macos::collect().await);
    #[cfg(not(any(windows, target_os = "macos")))]
    let (platform, raw) = ("unsupported", json!({}));
    normalize(platform, &raw)
}

pub async fn collect_node(node: &Node) -> Value {
    let at = now_ms();
    if let Err(e) = select(std::slice::from_ref(node), std::slice::from_ref(&node.id)) {
        return json!({"node_id":node.id,"ok":false,"at":at,"error":e});
    }
    let expected = if node.is_mac() { "macos" } else { "windows" };
    let result = if node.local {
        let r = collect_local().await;
        validate_report(&r, expected)
    } else {
        // Fixed command. No ledger strings are interpolated into a shell command.
        let remote = if node.is_mac() {
            "if [ -x \"$HOME/.katala-tune/bin/tune\" ]; then exec \"$HOME/.katala-tune/bin/tune\" specs-local; else exec tune specs-local; fi"
        } else {
            "if [ -x \"$HOME/.katala-tune/bin/tune.exe\" ]; then exec \"$HOME/.katala-tune/bin/tune.exe\" specs-local; else exec tune.exe specs-local; fi"
        };
        let mut args = collect::ssh_args(&node.alias, remote);
        args.splice(
            0..0,
            ["-o", "StrictHostKeyChecking=yes", "-o", "UpdateHostKeys=no", "-o", "ClearAllForwardings=yes", "-o", "PermitLocalCommand=no"].map(str::to_string),
        );
        match provider("ssh", &args, 120).await {
            Ok(v) => validate_report(&v, expected),
            Err(e) => Err(format!("{e}: 対象機に specs-local 対応の Rust CLI が必要です。接続と CLI の版を確認してください")),
        }
    };
    match result {
        Ok(report) if report["ok"] == true => json!({"node_id":node.id,"ok":true,"at":at,"report":report}),
        Ok(report) => json!({"node_id":node.id,"ok":false,"at":at,"error":"ハードウェア情報を取得できませんでした","report":report}),
        Err(e) => json!({"node_id":node.id,"ok":false,"at":at,"error":e}),
    }
}
