use super::*;

#[cfg(target_os = "macos")]
pub async fn collect() -> Value {
    // One provider invocation per type isolates unsupported devices and timeouts.
    let mut raw = Map::new();
    let mut errors = Map::new();
    let mut jobs = tokio::task::JoinSet::new();
    let types = collect::run_bounded("/usr/sbin/system_profiler", &["-listDataTypes".into()], &[], Duration::from_secs(3), 32768, 1024).await;
    let host_usb = types.code == Some(0) && String::from_utf8_lossy(&types.stdout).split_whitespace().any(|t| t == "SPUSBHostDataType");
    for (kind, _) in TYPES {
        let kind = *kind;
        if (kind == "SPUSBHostDataType" && !host_usb) || (kind == "SPUSBDataType" && host_usb) {
            continue;
        }
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
    for key in [
        "hw.physicalcpu",
        "hw.logicalcpu",
        "hw.memsize",
        "hw.machine",
        "hw.perflevel0.physicalcpu",
        "hw.perflevel1.physicalcpu",
        "hw.perflevel0.name",
        "hw.perflevel1.name",
    ] {
        let r = collect::run_bounded("/usr/sbin/sysctl", &["-n".into(), key.into()], &[], Duration::from_secs(2), 1024, 1024).await;
        if r.code == Some(0)
            && let Ok(n) = String::from_utf8_lossy(&r.stdout).trim().parse::<u64>()
        {
            raw.insert(key.into(), json!(n));
        } else if r.code == Some(0)
            && (key.ends_with(".name") || key == "hw.machine")
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
    let mut security = Map::new();
    for (tool, field) in [("csrutil", "system_integrity_protection"), ("fdesetup", "filevault")] {
        let result = collect::run_bounded(&format!("/usr/bin/{tool}"), &["status".into()], &[], Duration::from_secs(3), 4096, 1024).await;
        if result.code == Some(0) {
            let text = String::from_utf8_lossy(&result.stdout).to_lowercase();
            let state = match tool {
                "csrutil" if text.starts_with("system integrity protection status: enabled.") => Some("enabled"),
                "csrutil" if text.starts_with("system integrity protection status: disabled.") => Some("disabled"),
                "csrutil" if text.starts_with("system integrity protection status: unknown (custom configuration)") => Some("custom"),
                "fdesetup" if text.starts_with("filevault is on.") => Some("enabled"),
                "fdesetup" if text.starts_with("filevault is off.") => Some("disabled"),
                _ => None,
            };
            if let Some(state) = state {
                security.insert(field.into(), json!(state));
            } else {
                errors.insert(tool.into(), json!("unsupported"));
            }
        } else {
            let stderr = String::from_utf8_lossy(&result.stderr).to_lowercase();
            let state = if result.timed_out {
                "timeout"
            } else if stderr.contains("permission") || stderr.contains("not permitted") {
                "permission_denied"
            } else {
                "error"
            };
            errors.insert(tool.into(), json!(state));
        }
    }
    raw.insert("security_settings".into(), Value::Object(security));
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
    ("SPUSBHostDataType", "usb"),
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
        if let Some(fields) = v.as_object() {
            for (_, port) in fields.iter().filter(|(key, _)| key.starts_with("receptacle_") && key.ends_with("_tag")).take(64) {
                walk(std::slice::from_ref(port), out, depth + 1);
            }
        }
    }
}

fn device_model(name: &str) -> &str {
    // RME exposes an eight-digit unit serial in these device names. Keep unrelated
    // numeric model/SKU suffixes intact, including other manufacturers' products.
    for model in ["Babyface Pro", "Babyface Pro FS"] {
        if let Some(serial) = name.strip_prefix(model).and_then(|s| s.strip_prefix(" (")).and_then(|s| s.strip_suffix(')'))
            && serial.len() == 8
            && serial.bytes().all(|b| b.is_ascii_digit())
        {
            return model;
        }
    }
    name
}

fn wifi_channels(value: &Value) -> Option<Value> {
    // Group public channel numbers by band to stay inside the scalar report bound.
    // Never traverse the nearby current-network dictionary (SSID / BSSID).
    let mut bands = std::collections::BTreeMap::<&str, Vec<u16>>::new();
    for channel in value.as_array()?.iter().take(256).filter_map(Value::as_str) {
        let Some((number, band)) = channel.split_once(" (") else { continue };
        let Some(band) = band.strip_suffix(')').filter(|b| ["2GHz", "5GHz", "6GHz"].contains(b)) else { continue };
        let Some(number) = number.parse::<u16>().ok().filter(|n| (1..=233).contains(n)) else { continue };
        let channels = bands.entry(band).or_default();
        if !channels.contains(&number) {
            channels.push(number);
        }
    }
    let text = bands
        .into_iter()
        .map(|(band, channels)| format!("{band}: {}", channels.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")))
        .collect::<Vec<_>>()
        .join("; ");
    scalar(&json!(text))
}

fn public_wifi_ids(card: &str) -> Option<(String, String)> {
    let (_, pair) = card.rsplit_once(" (")?;
    let (vendor, device) = pair.strip_suffix(')')?.split_once(", ")?;
    let valid = |id: &str| id.strip_prefix("0x").is_some_and(|s| s.len() == 4 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    (valid(vendor) && valid(device)).then(|| (vendor.into(), device.into()))
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
                    ("spairport_supported_phymodes", "supported_standards"),
                    ("USBDeviceKeyVendorName", "manufacturer"),
                    ("USBDeviceKeyVendorID", "vendor_id"),
                    ("USBDeviceKeyProductID", "product_id"),
                    ("USBDeviceKeyProductVersion", "version"),
                    ("USBDeviceKeyLinkSpeed", "link_speed"),
                    ("spnvme_trim_support", "trim_support"),
                    ("smart_status", "smart_status"),
                    ("vendor_name_key", "manufacturer"),
                    ("current_speed_key", "link_speed"),
                    ("receptacle_status_key", "status"),
                    ("receptacle_id_key", "port"),
                    ("coreaudio_device_manufacturer", "manufacturer"),
                    ("coreaudio_device_transport", "interface"),
                    ("coreaudio_device_srate", "sample_rate_hz"),
                    ("coreaudio_device_input", "input_channels"),
                    ("coreaudio_device_output", "output_channels"),
                ],
            );
            if let Some(model) = fields.get("model").and_then(Value::as_str) {
                fields.insert("model".into(), json!(device_model(model)));
            }
            if *kind == "SPAirPortDataType" && (v.get("spairport_wireless_card_type").is_some() || v.get("spairport_supported_channels").is_some()) {
                if let Some(interface) = scalar(&v["_name"]) {
                    fields.insert("interface".into(), interface);
                }
                let card = v["spairport_wireless_card_type"].as_str().unwrap_or("");
                if let Some((vendor, device)) = public_wifi_ids(card) {
                    fields.insert("vendor_id".into(), json!(vendor));
                    fields.insert("device_id".into(), json!(device));
                }
                if let Some(channels) = wifi_channels(&v["spairport_supported_channels"]) {
                    fields.insert("supported_channels".into(), channels);
                }
                if let Some(state) = v["spairport_status_information"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("spairport_status_"))
                    .filter(|s| ["connected", "disconnected", "off", "on"].contains(s))
                {
                    fields.insert("connection_state".into(), json!(state));
                }
                let lower = card.to_lowercase();
                if lower.contains("firmware") || lower.contains("fwid") || lower.contains("chip id:") {
                    fields.insert("model".into(), json!("Wi-Fi adapter"));
                }
                if let Some(firmware) = v["spairport_wireless_firmware_version"].as_str()
                    && let Some(firmware) = scalar(&json!(firmware.split_whitespace().collect::<Vec<_>>().join(" ")))
                {
                    fields.insert("firmware_version".into(), firmware);
                }
            }
            if *id == "thunderbolt" && fields.contains_key("port") {
                let physical = fields["port"].as_str().is_some_and(|p| p.len() <= 3 && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
                if !physical {
                    fields.remove("port");
                }
            }
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
            let model = fields.get("model").and_then(Value::as_str).unwrap_or("");
            let record_kind = match *kind {
                "SPMemoryDataType"
                    if unified && (model.is_empty() || model == "memory") && v.get("dimm_size").is_none() && v.get("dimm_part_number").is_none() =>
                {
                    fields.insert("class".into(), json!("memory_summary"));
                    "group"
                }
                "SPNVMeDataType" | "SPSerialATADataType" if v.get("_items").is_some() && model.to_ascii_lowercase().contains("controller") => {
                    fields.insert("class".into(), json!("storage_controller"));
                    "device"
                }
                "SPAudioDataType" if v["coreaudio_device_transport"] == "coreaudio_device_type_virtual" => "software",
                "SPAudioDataType" if model == "coreaudio_device" && v.get("coreaudio_device_transport").is_none() => "group",
                "SPAirPortDataType" if model == "awdl0" || model == "llw0" => "software",
                "SPAirPortDataType" if v.get("spairport_airport_interfaces").is_some() => "group",
                "SPThunderboltDataType" if fields.contains_key("port") => "port",
                _ => "device",
            };
            fields.insert("record_kind".into(), json!(record_kind));
            if *kind == "SPSoftwareDataType"
                && let Some(version) = fields.get("version").and_then(Value::as_str)
                && let Some((version, build)) = version.rsplit_once(" (")
                && let Some(build) = build.strip_suffix(')').filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
            {
                let (version, build) = (version.to_string(), build.to_string());
                fields.insert("version".into(), json!(version));
                fields.insert("build".into(), json!(build));
            }
            if *kind == "SPSoftwareDataType"
                && let Some(architecture) = scalar(&raw["hw.machine"])
            {
                fields.insert("architecture".into(), architecture);
            }
            let name = if let Some(port) = fields.get("port").and_then(Value::as_str) {
                format!("Thunderbolt port {port}")
            } else {
                fields.get("model").and_then(Value::as_str).unwrap_or(id).to_string()
            };
            let component_source =
                if *kind == "SPSoftwareDataType" && fields.contains_key("architecture") { format!("{source} / sysctl hw.machine") } else { source.clone() };
            components.push(component(&name, &component_source, fields));
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
        if hw["chip_type"].as_str().is_some_and(|s| s.starts_with("Apple ")) {
            cpu.insert("manufacturer".into(), json!("Apple"));
        }
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
            f.insert("record_kind".into(), json!("aggregate"));
            let mut memory_source = "system_profiler / sysctl hw.memsize";
            if unified && let Some(memory) = raw["SPMemoryDataType"].as_array().and_then(|a| a.first()) {
                let details = mapped(memory, &[("dimm_type", "memory_type"), ("dimm_manufacturer", "manufacturer")]);
                if !details.is_empty() {
                    memory_source = "system_profiler SPHardwareDataType / SPMemoryDataType / sysctl hw.memsize";
                    f.extend(details);
                }
            }
            let entry = sections.get_mut("memory").expect("memory");
            entry["components"]
                .as_array_mut()
                .expect("components")
                .insert(0, component(if unified { "Unified memory" } else { "System memory total" }, memory_source, f));
            entry["status"] = json!("ok");
        }
    }
    if let Some(gpus) = raw["SPDisplaysDataType"].as_array() {
        let mut displays = Vec::new();
        for gpu in gpus {
            for display in gpu["spdisplays_ndrvs"].as_array().into_iter().flatten() {
                let mut fields = mapped(
                    display,
                    &[
                        ("_name", "model"),
                        ("_spdisplays_resolution", "resolution"),
                        ("spdisplays_resolution", "resolution"),
                        ("spdisplays_pixelresolution", "resolution"),
                        ("spdisplays_connection_type", "interface"),
                        ("spdisplays_refreshRate", "refresh_hz"),
                        ("spdisplays_online", "status"),
                        ("_spdisplays_display-vendor-id", "vendor_id"),
                        ("_spdisplays_display-product-id", "product_id"),
                    ],
                );
                if !fields.contains_key("refresh_hz")
                    && let Some(resolution) = display["_spdisplays_resolution"].as_str()
                    && let Some((_, hz)) = resolution.rsplit_once(" @ ")
                    && let Some(hz) = hz.strip_suffix("Hz").and_then(|n| n.parse::<f64>().ok()).filter(|n| n.is_finite() && *n > 0.0 && *n <= 1000.0)
                {
                    fields.insert("refresh_hz".into(), json!(hz));
                }
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
    for (tool, field) in [("csrutil", "system_integrity_protection"), ("fdesetup", "filevault")] {
        let source = format!("{tool} status");
        if let Some(state) = raw["security_settings"][field].as_str().filter(|s| ["enabled", "disabled", "custom"].contains(s)) {
            merge(sections, "settings", vec![component(field, &source, mapped(&json!({field:state}), &[(field, field)]))], "ok", &source);
        } else if let Some(error) = raw["errors"][tool].as_str() {
            let state = if ["timeout", "tool_missing", "permission_denied", "unsupported"].contains(&error) { error } else { "error" };
            merge(sections, "settings", vec![], state, &source);
        }
    }
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
