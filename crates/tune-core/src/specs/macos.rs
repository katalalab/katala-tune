use super::*;

#[cfg(target_os = "macos")]
pub async fn collect() -> Value {
    // One provider invocation per type isolates unsupported devices and timeouts.
    let mut raw = Map::new();
    let mut errors = Map::new();
    let mut jobs = tokio::task::JoinSet::new();
    for (kind, _) in TYPES {
        let kind = *kind;
        jobs.spawn(async move {
            (
                kind,
                provider("/usr/sbin/system_profiler", &["-json".into(), "-detailLevel".into(), "mini".into(), "-timeout".into(), "15".into(), kind.into()], 22)
                    .await,
            )
        });
    }
    while let Some(Ok((kind, result))) = jobs.join_next().await {
        match result {
            Ok(v) => {
                if let Some(items) = v.get(kind) {
                    raw.insert(kind.into(), items.clone());
                }
            }
            Err(e) => {
                errors.insert(kind.into(), json!(e));
            }
        }
    }
    // Query only a finite, non-identifying set. Never persist complete command output.
    for key in
        ["hw.physicalcpu", "hw.logicalcpu", "hw.memsize", "hw.perflevel0.physicalcpu", "hw.perflevel1.physicalcpu", "hw.perflevel0.name", "hw.perflevel1.name"]
    {
        let r = collect::run_bounded("/usr/sbin/sysctl", &["-n".into(), key.into()], &[], Duration::from_secs(2), 1024, 1024).await;
        if r.code == Some(0)
            && let Ok(n) = String::from_utf8_lossy(&r.stdout).trim().parse::<u64>()
        {
            raw.insert(key.into(), json!(n));
        } else if r.code == Some(0)
            && key.ends_with(".name")
            && let Some(v) = scalar(&json!(String::from_utf8_lossy(&r.stdout).trim()))
        {
            raw.insert(key.into(), v);
        }
    }
    let r = collect::run_bounded("/usr/bin/pmset", &["-g".into(), "custom".into()], &[], Duration::from_secs(3), 16384, 1024).await;
    if r.code == Some(0) {
        let mut modes = Vec::new();
        let mut mode = json!({"model":"Power settings"});
        for line in String::from_utf8_lossy(&r.stdout).lines() {
            if line.trim_end().ends_with("Power:") {
                if mode.as_object().is_some_and(|m| m.len() > 1) {
                    modes.push(mode);
                }
                mode = json!({"model":line.trim().trim_end_matches(':')});
            } else {
                let words: Vec<_> = line.split_whitespace().collect();
                if let [key, value, ..] = words.as_slice()
                    && ["lowpowermode", "sleep", "displaysleep", "disksleep", "hibernatemode"].contains(key)
                    && let Ok(n) = value.parse::<u64>()
                {
                    mode[*key] = json!(n);
                }
            }
        }
        if mode.as_object().is_some_and(|m| m.len() > 1) {
            modes.push(mode);
        }
        raw.insert("power_settings".into(), json!(modes));
    } else {
        errors.insert("settings".into(), json!("error"));
    }
    raw.insert("errors".into(), Value::Object(errors));
    Value::Object(raw)
}

const TYPES: &[(&str, &str)] = &[
    ("SPHardwareDataType", "system"),
    ("SPSoftwareDataType", "os"),
    ("SPMemoryDataType", "memory"),
    ("SPDisplaysDataType", "gpu"),
    ("SPNVMeDataType", "storage"),
    ("SPSerialATADataType", "storage"),
    ("SPEthernetDataType", "network"),
    ("SPAirPortDataType", "network"),
    ("SPUSBDataType", "usb"),
    ("SPThunderboltDataType", "thunderbolt"),
    ("SPPCIDataType", "pci"),
    ("SPAudioDataType", "audio"),
    ("SPPowerDataType", "settings"),
];

fn walk<'a>(items: &'a [Value], out: &mut Vec<&'a Value>, depth: usize) {
    if depth > 8 {
        return;
    }
    for v in items.iter().take(MAX_COMPONENTS) {
        if out.len() >= MAX_COMPONENTS {
            return;
        }
        out.push(v);
        if let Some(children) = v["_items"].as_array() {
            walk(children, out, depth + 1);
        }
        if let Some(children) = v["spairport_airport_interfaces"].as_array() {
            walk(children, out, depth + 1);
        }
    }
}

pub fn normalize(raw: &Value, sections: &mut Map<String, Value>) {
    let unified = raw["SPHardwareDataType"].as_array().into_iter().flatten().any(|v| v["chip_type"].as_str().is_some_and(|s| s.starts_with("Apple")));
    for (kind, id) in TYPES {
        let source = format!("system_profiler {kind}");
        if let Some(error) = raw["errors"][*kind].as_str() {
            let error = if ["timeout", "tool_missing", "output_limit", "permission_denied"].contains(&error) { error } else { "error" };
            merge(sections, id, vec![], error, &source);
            continue;
        }
        let Some(items) = raw[*kind].as_array() else { continue };
        let mut devices = Vec::new();
        walk(items, &mut devices, 0);
        let mut components = Vec::new();
        for v in devices {
            let mut fields = mapped(
                v,
                &[
                    ("_name", "model"),
                    ("manufacturer", "manufacturer"),
                    ("dimm_manufacturer", "manufacturer"),
                    ("dimm_part_number", "part_number"),
                    ("machine_model", "model"),
                    ("machine_name", "class"),
                    ("model_number", "part_number"),
                    ("chip_type", "architecture"),
                    ("boot_rom_version", "firmware_version"),
                    ("os_loader_version", "version"),
                    ("physical_memory", "capacity"),
                    ("dimm_size", "capacity"),
                    ("dimm_speed", "link_speed"),
                    ("dimm_type", "memory_type"),
                    ("dimm_status", "status"),
                    ("sppci_model", "model"),
                    ("spdisplays_vendor", "manufacturer"),
                    ("spdisplays_metal", "metal"),
                    ("spdisplays_mtlgpufamilysupport", "metal"),
                    ("sppci_cores", "cores"),
                    ("spdisplays_vram", "vram"),
                    ("spdisplays_resolution", "resolution"),
                    ("spdisplays_refreshRate", "refresh_hz"),
                    ("device_model", "model"),
                    ("device_revision", "firmware_version"),
                    ("revision", "firmware_version"),
                    ("size_in_bytes", "capacity_bytes"),
                    ("spsata_medium_type", "media_type"),
                    ("link_speed", "link_speed"),
                    ("link_width", "pcie_width"),
                    ("vendor_id", "vendor_id"),
                    ("product_id", "product_id"),
                    ("device_id", "device_id"),
                    ("device_version", "version"),
                    ("driver_version", "driver_version"),
                    ("os_version", "version"),
                    ("kernel_version", "kernel"),
                    ("boot_mode", "boot_mode"),
                    ("spairport_wireless_card_type", "model"),
                    ("spairport_wireless_firmware_version", "firmware_version"),
                ],
            );
            if *id == "gpu" {
                fields.insert("memory_kind".into(), json!(if unified { "unified" } else { "unknown" }));
                if unified {
                    fields.remove("vram");
                }
            }
            // Power data is nested in dictionaries; read only known health fields.
            if *kind == "SPPowerDataType" {
                fields = mapped(
                    &v["sppower_battery_health_info"],
                    &[
                        ("sppower_battery_cycle_count", "battery_cycles"),
                        ("sppower_battery_health", "battery_condition"),
                        ("sppower_battery_maximum_capacity", "battery_max_capacity_pct"),
                    ],
                );
            }
            if fields.is_empty() {
                continue;
            }
            let name = fields.get("model").and_then(Value::as_str).unwrap_or(id).to_string();
            components.push(component(&name, &source, fields));
        }
        let status = if components.is_empty() {
            "empty"
        } else if components.len() >= MAX_COMPONENTS {
            "partial"
        } else {
            "ok"
        };
        merge(sections, id, components, status, &source);
    }
    if let Some(hw) = raw["SPHardwareDataType"].as_array().and_then(|a| a.first()) {
        let mut cpu =
            mapped(hw, &[("chip_type", "model"), ("cpu_type", "model"), ("number_processors", "cores"), ("current_processor_speed", "current_clock_mhz")]);
        for (key, field) in [
            ("hw.physicalcpu", "cores"),
            ("hw.logicalcpu", "threads"),
            ("hw.perflevel0.physicalcpu", "cluster0_cores"),
            ("hw.perflevel1.physicalcpu", "cluster1_cores"),
            ("hw.perflevel0.name", "cluster0_name"),
            ("hw.perflevel1.name", "cluster1_name"),
        ] {
            if let Some(v) = scalar(&raw[key]) {
                cpu.insert(field.into(), v);
            }
        }
        sections.insert("cpu".into(), section(vec![component("CPU", "system_profiler / sysctl", cpu)], "ok", "system_profiler / sysctl"));
        let firmware = mapped(hw, &[("boot_rom_version", "firmware_version"), ("os_loader_version", "version")]);
        sections.insert(
            "firmware".into(),
            section(vec![component("System firmware", "system_profiler SPHardwareDataType", firmware)], "ok", "system_profiler SPHardwareDataType"),
        );
        if unified || raw["hw.memsize"].is_u64() {
            let mut memory = mapped(hw, &[("physical_memory", "capacity")]);
            if let Some(v) = scalar(&raw["hw.memsize"]) {
                memory.insert("capacity_bytes".into(), v);
            }
            let mut f = memory;
            f.insert("memory_kind".into(), json!(if unified { "unified" } else { "system_total" }));
            let entry = sections.get_mut("memory").expect("memory");
            entry["components"]
                .as_array_mut()
                .expect("components")
                .insert(0, component(if unified { "Unified memory" } else { "System memory total" }, "system_profiler / sysctl hw.memsize", f));
            entry["status"] = json!("ok");
        }
    }
    if let Some(gpus) = raw["SPDisplaysDataType"].as_array() {
        let mut displays = Vec::new();
        for gpu in gpus {
            for display in gpu["spdisplays_ndrvs"].as_array().into_iter().flatten() {
                let fields = mapped(
                    display,
                    &[
                        ("_name", "model"),
                        ("_spdisplays_resolution", "resolution"),
                        ("spdisplays_resolution", "resolution"),
                        ("spdisplays_pixelresolution", "resolution"),
                        ("spdisplays_connection_type", "interface"),
                        ("spdisplays_refreshRate", "refresh_hz"),
                        ("spdisplays_online", "status"),
                    ],
                );
                let name = fields.get("model").and_then(Value::as_str).unwrap_or("Display");
                displays.push(component(name, "system_profiler SPDisplaysDataType", fields.clone()));
            }
        }
        let status = if displays.is_empty() { "empty" } else { "ok" };
        displays.truncate(MAX_COMPONENTS);
        sections.insert("display".into(), section(displays, status, "system_profiler SPDisplaysDataType"));
    }
    sections.insert("board".into(), section(vec![], "unsupported", "system_profiler: board product identifier not exposed"));
    let mut settings = vec![component(
        "Unavailable firmware settings",
        "OS provider coverage",
        mapped(
            &json!({"secure_boot":"not_collected_requires_recovery_policy_inspection","xmp_expo":"not_applicable"}),
            &[("secure_boot", "secure_boot"), ("xmp_expo", "xmp_expo")],
        ),
    )];
    if let Some(modes) = raw["power_settings"].as_array() {
        for mode in modes {
            let fields = mapped(
                mode,
                &[
                    ("model", "model"),
                    ("lowpowermode", "low_power_mode"),
                    ("sleep", "sleep_minutes"),
                    ("displaysleep", "display_sleep_minutes"),
                    ("disksleep", "disk_sleep_minutes"),
                    ("hibernatemode", "hibernate_mode"),
                ],
            );
            settings.push(component("Power settings", "pmset -g custom", fields));
        }
    }
    let entry = sections.get_mut("settings").expect("settings");
    let actual_components = entry["components"].as_array().map_or(0, Vec::len);
    entry["components"].as_array_mut().expect("components").extend(settings);
    if raw["power_settings"].as_array().is_some_and(|a| !a.is_empty()) {
        let old = entry["status"].as_str().unwrap_or("not_collected");
        entry["status"] = json!(if ["ok", "empty", "not_collected"].contains(&old) { "ok" } else { "partial" });
    } else if actual_components > 0 {
        entry["status"] = json!("partial");
    } else if entry["status"] == "empty" {
        entry["status"] = json!("not_collected");
    }
    let pmset_state = if raw.get("power_settings").is_some() {
        "ok"
    } else {
        raw["errors"]["settings"].as_str().filter(|s| ["error", "timeout", "permission_denied"].contains(s)).unwrap_or("not_collected")
    };
    entry["providers"].as_array_mut().expect("providers").push(json!({"source":"pmset -g custom","status":pmset_state}));
}

fn merge(sections: &mut Map<String, Value>, id: &str, components: Vec<Value>, status: &str, source: &str) {
    let entry = sections.get_mut(id).expect("known section");
    let items = entry["components"].as_array_mut().expect("components");
    items.extend(components);
    items.truncate(MAX_COMPONENTS);
    entry["providers"].as_array_mut().expect("providers").push(json!({"source":source,"status":status}));
    let has_data = entry["components"].as_array().is_some_and(|c| !c.is_empty());
    let failure = entry["providers"].as_array().into_iter().flatten().any(|p| !matches!(p["status"].as_str(), Some("ok" | "empty")));
    entry["status"] = json!(if has_data && failure {
        "partial"
    } else if has_data {
        "ok"
    } else if failure {
        "error"
    } else {
        status
    });
    let sources: Vec<_> = entry["providers"].as_array().into_iter().flatten().filter_map(|p| p["source"].as_str()).collect();
    entry["source"] = json!(sources.join(" / "));
}
