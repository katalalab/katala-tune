//! Finite read-only machine inventory. Rust owns collection and the public report contract.
mod gaps;
mod macos;
mod windows;

use crate::{collect, db::now_ms, nodes::Node};
use serde_json::{Map, Value, json};
use std::{sync::LazyLock, time::Duration};

static CATALOG: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../../../../data/specs-catalog.json")).expect("bundled specifications catalog"));

pub const SCHEMA: &str = "katala_specs.v1";
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_COMPONENTS: usize = 256;
pub const SECTIONS: &[&str] =
    &["system", "cpu", "memory", "gpu", "storage", "board", "firmware", "os", "display", "network", "usb", "thunderbolt", "pci", "audio", "settings"];

/// No fuzzy model matching: suffixes and variants are materially different hardware.
pub fn official(model: &str) -> Value {
    let catalog = &*CATALOG;
    for p in catalog["products"].as_array().into_iter().flatten() {
        if p["models"].as_array().into_iter().flatten().any(|m| m.as_str().is_some_and(|m| m.eq_ignore_ascii_case(model.trim()))) {
            return official_product(p, "matched", "model");
        }
    }
    json!({"status":"unverified", "reason":"exact_model_not_in_catalog"})
}

fn official_product(p: &Value, status: &str, match_kind: &str) -> Value {
    json!({"status":status,"match_kind":match_kind,"name":p["name"],"url":p["url"],"scope":p["scope"],
        "source_access":p["source_access"],"source_evidence":p["source_evidence"],"supporting_source_evidence":p["supporting_source_evidence"],"specifications":p["specifications"],
        "verified_at":p.get("verified_at").unwrap_or(&CATALOG["verified_at"]),"catalog_version":CATALOG["version"]})
}

fn public_id(v: &Value, length: usize) -> Option<String> {
    let value = v.as_str()?.trim();
    let value = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")).unwrap_or(value);
    (value.len() == length && value.bytes().all(|b| b.is_ascii_hexdigit())).then(|| value.to_ascii_uppercase())
}

fn official_for(section: &str, c: &Value) -> Value {
    let fields = &c["fields"];
    let model = fields["model"].as_str().or_else(|| c["name"].as_str()).unwrap_or("");
    let exact = official(model);
    if exact["status"] == "matched" {
        return exact;
    }
    let matches: Vec<_> = CATALOG["products"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| {
            p["hardware_ids"].as_array().into_iter().flatten().any(|identity| {
                let Some(identity) = identity.as_object() else {
                    return false;
                };
                if identity.get("section").and_then(Value::as_str) != Some(section)
                    || !identity.contains_key("vendor_id")
                    || (!identity.contains_key("device_id") && !identity.contains_key("product_id"))
                {
                    return false;
                }
                identity.iter().all(|(key, expected)| {
                    if key == "section" {
                        return true;
                    }
                    if key == "hardware_id_bus" {
                        return matches!(expected.as_str(), Some("PCI" | "USB" | "HDAUDIO")) && *expected == fields[key];
                    }
                    let length = match key.as_str() {
                        "vendor_id" | "device_id" | "product_id" | "subsystem_vendor_id" | "subsystem_device_id" => 4,
                        "revision_id" => {
                            if section == "usb" {
                                4
                            } else {
                                2
                            }
                        }
                        _ => return false,
                    };
                    let observed = if section == "pci" && key == "device_id" && fields[key].is_null() { &fields["product_id"] } else { &fields[key] };
                    public_id(expected, length).is_some_and(|id| public_id(observed, length).as_ref() == Some(&id))
                })
            })
        })
        .collect();
    match matches.as_slice() {
        [p] => official_product(p, "identified", "hardware_id"),
        [] if section == "usb" => {
            let mut reported = fields["reported_model"].as_str().map(official).unwrap_or(Value::Null);
            if reported["status"] == "matched" {
                reported["match_kind"] = json!("bus_reported_model");
                reported
            } else {
                exact
            }
        }
        [] => exact,
        _ => json!({"status":"unverified","reason":"ambiguous_hardware_id_catalog"}),
    }
}

pub(super) fn scalar(v: &Value) -> Option<Value> {
    match v {
        Value::String(s) if !s.trim().is_empty() && s.len() <= 512 && !s.chars().any(char::is_control) => Some(json!(s.trim())),
        Value::Number(n) if n.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0) => Some(v.clone()),
        Value::Bool(_) => Some(v.clone()),
        _ => None,
    }
}

fn report_field(key: &str, v: &Value) -> Option<Value> {
    match key {
        "reported_model" => public_reported_model(v),
        "hardware_id_bus" => v.as_str().filter(|bus| ["PCI", "USB", "HDAUDIO"].contains(bus)).map(|bus| json!(bus)),
        "driver_inf" => {
            let name = v.as_str()?;
            (name.len() > 4
                && name.len() <= 128
                && name.to_ascii_lowercase().ends_with(".inf")
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
            .then(|| v.clone())
        }
        "driver_signed" | "present" => v.is_boolean().then(|| v.clone()),
        "problem_code" => v.as_u64().filter(|n| *n <= u32::MAX.into()).map(|n| json!(n)),
        "subsystem_vendor_id" | "subsystem_device_id" => public_id(v, 4).map(|s| json!(s)),
        "revision_id" => public_id(v, 2).or_else(|| public_id(v, 4)).map(|s| json!(s)),
        _ => scalar(v),
    }
}

// USB product strings may contain serials or custom text. Only public catalog
// aliases can be retained as an additional observation, never arbitrary text.
pub(super) fn public_reported_model(v: &Value) -> Option<Value> {
    let value = scalar(v)?;
    (official(value.as_str()?)["status"] == "matched").then_some(value)
}

/// Only these named fields can cross the remote boundary or enter the private DB.
pub const FIELDS: &[&str] = &[
    "cluster0_name",
    "cluster0_cores",
    "cluster1_name",
    "cluster1_cores",
    "model",
    "reported_model",
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
    "driver_name",
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
    "hardware_id_bus",
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
    "transmit_speed_bps",
    "receive_speed_bps",
    "enabled",
    "dhcp_enabled",
    "mtu_bytes",
    "trim_support",
    "smart_status",
    "system_integrity_protection",
    "filevault",
    "port",
    "sample_rate_hz",
    "input_channels",
    "output_channels",
    "logical_sector_bytes",
    "physical_sector_bytes",
    "partition_style",
    "partition_count",
    "read_only",
    "boot_disk",
    "system_disk",
    "health_status",
    "vbs_status",
    "credential_guard_configured",
    "credential_guard_running",
    "memory_integrity_configured",
    "memory_integrity_running",
    "display_version",
    "update_build_revision",
    "installation_type",
    "virtual_adapter",
    "hardware_interface",
    "admin_status",
    "physical_width_cm",
    "physical_height_cm",
    "display_origin",
    "connection_status",
    "record_kind",
    "supported_standards",
    "supported_channels",
    "connection_state",
    "subsystem_vendor_id",
    "subsystem_device_id",
    "revision_id",
    "driver_provider",
    "driver_inf",
    "driver_signed",
    "present",
    "problem_code",
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

fn record_kind(platform: &str, id: &str, c: &Value) -> (&'static str, Option<&'static str>) {
    let f = &c["fields"];
    let model = f["model"].as_str().or_else(|| c["name"].as_str()).unwrap_or("");
    let source = c["source"].as_str().unwrap_or("");
    if id == "memory" && matches!(f["memory_kind"].as_str(), Some("unified" | "system_total")) {
        return ("aggregate", None);
    }
    if f["virtual_adapter"] == true {
        return ("software", None);
    }
    if platform == "macos" {
        if id == "memory"
            && source == "system_profiler SPMemoryDataType"
            && (f["model"].is_null() || model == "memory")
            && ["part_number", "capacity_bytes", "capacity", "slot"].iter().all(|k| f[*k].is_null())
        {
            return ("group", Some("memory_summary"));
        }
        if id == "storage"
            && ["system_profiler SPNVMeDataType", "system_profiler SPSerialATADataType"].contains(&source)
            && model.to_ascii_lowercase().contains("controller")
            && f["capacity_bytes"].is_null()
        {
            return ("device", Some("storage_controller"));
        }
        if id == "audio" && source == "system_profiler SPAudioDataType" {
            if model == "coreaudio_device" && ["interface", "sample_rate_hz", "input_channels", "output_channels"].iter().all(|k| f[*k].is_null()) {
                return ("group", None);
            }
            if f["interface"] == "coreaudio_device_type_virtual" {
                return ("software", None);
            }
        }
        if id == "network" && source == "system_profiler SPAirPortDataType" {
            for prefix in ["awdl", "llw"] {
                if model.strip_prefix(prefix).is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())) {
                    return ("software", None);
                }
            }
        }
        if id == "thunderbolt" && source == "system_profiler SPThunderboltDataType" && f["port"].is_string() {
            return ("port", None);
        }
    }
    ("device", None)
}

// These are documentation references, never evidence of the installed product or settings.
// Both local and remote reports derive them here; remote URLs and gap claims are discarded.
fn finalize_sections(platform: &str, sections: &mut Map<String, Value>) {
    for (id, s) in sections {
        let references = match (platform, id.as_str()) {
            ("windows", "settings") => {
                json!([{"name":"Microsoft: VBS / メモリ整合性の状態","url":"https://learn.microsoft.com/en-us/windows/security/hardware-security/enable-virtualization-based-protection-of-code-integrity"}])
            }
            ("windows", "firmware") => {
                json!([{"name":"Microsoft: Win32_BIOS の公開情報","url":"https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-bios"}])
            }
            ("windows", "os") => {
                json!([{"name":"Microsoft: OS の公開情報","url":"https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-operatingsystem"}])
            }
            ("macos", "settings") => json!([
                {"name":"Apple: System Integrity Protection","url":"https://support.apple.com/en-us/102149"},
                {"name":"Apple: FileVault","url":"https://support.apple.com/guide/mac-help/protect-data-on-your-mac-with-filevault-mh11785/mac"}
            ]),
            ("macos", "os" | "firmware") => {
                json!([{"name":"Apple: システム情報の確認方法","url":"https://support.apple.com/guide/system-information/welcome/mac"}])
            }
            _ => json!([]),
        };
        s["references"] = references;
        let providers = s["providers"].clone();
        for c in s["components"].as_array_mut().into_iter().flatten() {
            let (kind, class) = record_kind(platform, id, c);
            c["fields"]["record_kind"] = json!(kind);
            if let Some(class) = class {
                c["fields"]["class"] = json!(class);
            }
            let f = &c["fields"];
            let aggregate_memory = id == "memory" && matches!(f["memory_kind"].as_str(), Some("unified" | "system_total"));
            let expected: &[&str] = match (kind, class, id.as_str()) {
                ("group" | "port", _, _) => &[],
                (_, Some("storage_controller"), _) => &["model"],
                _ => match id.as_str() {
                    "cpu" => &["model", "manufacturer", "cores", "threads"],
                    "memory" if aggregate_memory => &["capacity_bytes"],
                    "memory" => &["part_number", "manufacturer", "capacity_bytes", "configured_speed_mts"],
                    "gpu" => &["model", "driver_version"],
                    "storage" => &["model", "firmware_version", "capacity_bytes"],
                    "board" => &["model", "manufacturer", "version"],
                    "firmware" => &["firmware_version"],
                    "os" => &["version", "build", "architecture"],
                    "settings" => &[],
                    "display" => &["model", "manufacturer"],
                    "network" => &["model", "driver_version"],
                    "system" => &["model", "manufacturer"],
                    _ => &["model"],
                },
            };
            let missing: Vec<_> = expected.iter().filter(|k| f.get(**k).is_none()).copied().collect();
            let not_applicable = matches!(id.as_str(), "os" | "firmware" | "settings") || matches!(kind, "group" | "aggregate" | "software" | "port");
            c["missing_fields"] = json!(missing);
            if not_applicable {
                c["official"] = json!({"status":"not_applicable","reason":"documentation_or_aggregate_not_exact_product"});
            } else {
                c["official"] = official_for(id, c);
            }
            c["gap_reasons"] = gaps::explain(platform, id, c, &providers);
        }
    }
}

/// Derive explanations for a normalized observation without changing its identity status.
pub fn explain_gaps(platform: &str, section: &str, component: &Value, providers: &Value) -> Value {
    gaps::explain(platform, section, component, providers)
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
    finalize_sections(platform, &mut sections);
    let collected =
        sections.values().any(|s| matches!(s["status"].as_str(), Some("ok" | "partial")) && s["components"].as_array().is_some_and(|c| !c.is_empty()));
    json!({"schema":SCHEMA,"platform":platform,"at":now_ms(),"ok":collected,"collector_version":env!("CARGO_PKG_VERSION"),"catalog_version":CATALOG["version"],"gap_summary":gaps::summary(&sections),"sections":sections})
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
                fields.iter().filter(|(k, _)| FIELDS.contains(&k.as_str())).filter_map(|(k, v)| report_field(k, v).map(|v| (k.clone(), v))).collect();
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
    finalize_sections(platform, &mut sections);
    let ok = sections.values().any(|s| matches!(s["status"].as_str(), Some("ok" | "partial")) && s["components"].as_array().is_some_and(|c| !c.is_empty()));
    Ok(
        json!({"schema":SCHEMA,"platform":platform,"at":at,"ok":ok,"collector_version":scalar(&v["collector_version"]),"catalog_version":CATALOG["version"],"gap_summary":gaps::summary(&sections),"sections":sections}),
    )
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
