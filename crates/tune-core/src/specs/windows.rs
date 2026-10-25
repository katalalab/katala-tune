use super::*;

// OS built-in CIM provider, invoked by the Rust binary; no script installation.
#[cfg(windows)]
const SCRIPT: &str = r#"
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$r = @{}; $errors = @{}
function Q($key, $class, $props) {
  try { $r[$key] = @(Get-CimInstance -ClassName $class -OperationTimeoutSec 8 | Select-Object -Property $props) }
  catch { $errors[$key] = $(if ($_.Exception.HResult -eq -2147024891) { "permission_denied" } else { "error" }) }
}
Q "system" "Win32_ComputerSystem" @("Manufacturer","Model","SystemType","HypervisorPresent")
Q "cpu" "Win32_Processor" @("Name","Manufacturer","NumberOfCores","NumberOfLogicalProcessors","MaxClockSpeed","CurrentClockSpeed","L2CacheSize","L3CacheSize","VirtualizationFirmwareEnabled","SecondLevelAddressTranslationExtensions")
Q "memory" "Win32_PhysicalMemory" @("Manufacturer","PartNumber","DeviceLocator","Capacity","Speed","ConfiguredClockSpeed","SMBIOSMemoryType","FormFactor","DataWidth","ConfiguredVoltage")
Q "gpu" "Win32_VideoController" @("Name","AdapterCompatibility","DriverVersion","DriverDate","VideoProcessor","CurrentHorizontalResolution","CurrentVerticalResolution","CurrentRefreshRate","Status")
Q "storage" "Win32_DiskDrive" @("Index","Model","Manufacturer","FirmwareRevision","Size","InterfaceType","MediaType","Status")
try {
  $r.modern_disks = @(Get-CimInstance -Namespace root/Microsoft/Windows/Storage -ClassName MSFT_Disk -OperationTimeoutSec 8 | Select-Object Number,BusType,LogicalSectorSize,PhysicalSectorSize,PartitionStyle,NumberOfPartitions,IsReadOnly,IsBoot,IsSystem,HealthStatus)
} catch { $errors.modern_disks = $(if ($_.Exception.HResult -eq -2147024891) { "permission_denied" } else { "error" }) }
Q "board" "Win32_BaseBoard" @("Manufacturer","Product","Version")
Q "firmware" "Win32_BIOS" @("Manufacturer","SMBIOSBIOSVersion","ReleaseDate")
Q "os" "Win32_OperatingSystem" @("Caption","Version","BuildNumber","OSArchitecture")
try {
  $r.os_registry = Get-ItemProperty -LiteralPath "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion" | Select-Object DisplayVersion,UBR,InstallationType
} catch { $errors.os_registry = $(if ($_.Exception.HResult -eq -2147024891) { "permission_denied" } else { "error" }) }
Q "audio" "Win32_SoundDevice" @("Name","Manufacturer","Status")
try {
  $r.network = @(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetAdapter -OperationTimeoutSec 8 | Select-Object InterfaceDescription,DriverVersionString,DriverDate,TransmitLinkSpeed,ReceiveLinkSpeed,MediaConnectState,FullDuplex,MtuSize,Virtual,HardwareInterface,InterfaceAdminStatus)
} catch { $errors.network = $(if ($_.Exception.HResult -eq -2147024891) { "permission_denied" } else { "error" }) }
try {
  $pnp = @(Get-CimInstance Win32_PnPEntity -OperationTimeoutSec 8)
  foreach ($kind in @("usb","pci","thunderbolt")) {
    $r[$kind] = @($pnp | Where-Object { $(if ($kind -eq "thunderbolt") { $_.Name -match "Thunderbolt|USB4" } else { $_.PNPDeviceID -like ($kind + "\*") }) } | ForEach-Object {
      $vendor = $null; $product = $null
      if ($_.PNPDeviceID -match "(?:VEN|VID)_([0-9A-F]{4})") { $vendor = $Matches[1] }
      if ($_.PNPDeviceID -match "(?:DEV|PID)_([0-9A-F]{4})") { $product = $Matches[1] }
      [pscustomobject]@{ Name=$_.Name; Manufacturer=$_.Manufacturer; Class=$_.PNPClass; Status=$_.Status; Vendor=$vendor; Product=$product }
    })
  }
} catch { foreach ($kind in @("usb","pci","thunderbolt")) { $errors[$kind] = "error" } }
try {
  $r.drivers = @(Get-CimInstance Win32_PnPSignedDriver -OperationTimeoutSec 8 | Where-Object { $_.DeviceClass -in @("DISPLAY","NET","MEDIA","USB","SCSIADAPTER","HDC") } | Select-Object DeviceName,Manufacturer,DriverVersion,DriverDate,DeviceClass)
} catch { $errors.drivers = "error" }
try {
  $ids = @(Get-CimInstance -Namespace root/wmi -ClassName WmiMonitorID -OperationTimeoutSec 8)
  $r.display = @(Get-CimInstance -Namespace root/wmi -ClassName WmiMonitorBasicDisplayParams -OperationTimeoutSec 8 | ForEach-Object {
    $basic = $_; $identity = $ids | Where-Object { $_.InstanceName -eq $basic.InstanceName } | Select-Object -First 1
    $model = ($identity.UserFriendlyName | Where-Object { $_ -gt 0 } | ForEach-Object { [char]$_ }) -join ""
    $maker = ($identity.ManufacturerName | Where-Object { $_ -gt 0 } | ForEach-Object { [char]$_ }) -join ""
    [pscustomobject]@{ Name=$model; Manufacturer=$maker; MaxHorizontalImageSize=$basic.MaxHorizontalImageSize; MaxVerticalImageSize=$basic.MaxVerticalImageSize; VideoInputType=$basic.VideoInputType; Active=$basic.Active }
  })
} catch { $errors.display = $(if ($_.Exception.HResult -eq -2147024891) { "permission_denied" } else { "error" }) }
if ($errors.ContainsKey("display")) {
  $r.display_registry_skipped = 0
  $r.display_registry = @()
  try {
    Get-Item -Path "HKLM:\SYSTEM\CurrentControlSet\Enum\DISPLAY\*\*\Device Parameters" -ErrorAction Stop | Select-Object -First 64 | ForEach-Object {
      try {
        $edid = (Get-ItemProperty -LiteralPath $_.PSPath -Name EDID -ErrorAction Stop).EDID
        if ($null -ne $edid) { $r.display_registry += [pscustomobject]@{ EDID=@($edid | Select-Object -First 128) } }
        else { $r.display_registry_skipped++ }
      } catch {
        $r.display_registry_skipped++
        if ($_.Exception.HResult -eq -2147024891 -or $_.FullyQualifiedErrorId -match "Unauthorized|AccessDenied|Security") { $errors.display_registry = "permission_denied" }
        elseif ($_.FullyQualifiedErrorId -notmatch "PropertyNotFound|ItemNotFound" -and !$errors.ContainsKey("display_registry")) { $errors.display_registry = "error" }
      }
    }
  } catch { $errors.display_registry = $(if ($_.Exception.HResult -eq -2147024891) { "permission_denied" } else { "error" }) }
}
try {
  $r.device_guard = @(Get-CimInstance -Namespace root/Microsoft/Windows/DeviceGuard -ClassName Win32_DeviceGuard -OperationTimeoutSec 8 | Select-Object VirtualizationBasedSecurityStatus,SecurityServicesConfigured,SecurityServicesRunning)
} catch { $errors.device_guard = $(if ($_.Exception.HResult -eq -2147024891) { "permission_denied" } else { "unsupported" }) }
$secure = "unsupported"
try { $secure = $(if (Confirm-SecureBootUEFI) { "enabled" } else { "disabled" }) }
catch { $secure = $(if ($_.Exception.HResult -eq -2147024891 -or $_.FullyQualifiedErrorId -match "Privilege|Unauthorized|AccessDenied") { "permission_denied" } else { "unsupported" }) }
$plan = "not_collected"
try {
  $active = (& powercfg.exe -getactivescheme 2>$null) -join " "
  $plan = $(if ($active -match "381b4222-f694-41f0-9685-ff5bb260df2e") { "balanced" } elseif ($active -match "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c") { "high_performance" } elseif ($active -match "a1841308-3541-4fab-bc81-f71556f20b4a") { "power_saver" } else { "custom_or_unknown" })
} catch {}
$r.settings = @([pscustomobject]@{ Name="Boot / Power / Memory"; SecureBoot=$secure; PowerPlan=$plan; XmpExpo="not_exposed_by_standard_provider" })
$r.errors = $errors
$r | ConvertTo-Json -Depth 8 -Compress
"#;

#[cfg(windows)]
pub async fn collect() -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(100);
    let utf16: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &crate::logs::base64(&utf16)].map(str::to_string);
    let mut raw = match provider("powershell.exe", &args, 100).await {
        Ok(raw) => raw,
        Err(e) => json!({"errors":SECTIONS.iter().map(|k| ((*k).to_string(),json!(e))).collect::<Map<String,Value>>()}),
    };
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return raw;
    }
    let r = collect::run_bounded(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total,vbios_version,power.limit,pcie.link.gen.max,pcie.link.width.max,driver_version".into(),
            "--format=csv,noheader,nounits".into(),
        ],
        &[],
        remaining.min(Duration::from_secs(8)),
        65536,
        2048,
    )
    .await;
    if r.code == Some(0) && !r.timed_out && !r.output_limited {
        let mut gpus = Vec::new();
        for line in String::from_utf8_lossy(&r.stdout).lines().take(32) {
            let values: Vec<_> = line.split(',').map(str::trim).collect();
            if values.len() != 7 {
                continue;
            }
            let mut gpu = json!({"model":values[0]});
            for (index, field) in [(1, "dedicated_vram_bytes"), (3, "power_limit_w"), (4, "pcie_generation"), (5, "pcie_width")] {
                if let Ok(n) = values[index].parse::<f64>()
                    && n.is_finite()
                    && n >= 0.0
                {
                    gpu[field] = json!(if index == 1 { n * 1048576.0 } else { n });
                }
            }
            for (index, field) in [(2, "vbios_version"), (6, "driver_version")] {
                if !values[index].contains("N/A")
                    && let Some(v) = scalar(&json!(values[index]))
                {
                    gpu[field] = v;
                }
            }
            gpus.push(gpu);
        }
        raw["nvidia"] = json!(gpus);
    }
    raw
}

type Mapping = &'static [(&'static str, &'static str)];
const QUERIES: &[(&str, &str, Mapping)] = &[
    (
        "system",
        "Win32_ComputerSystem",
        &[("Manufacturer", "manufacturer"), ("Model", "model"), ("SystemType", "architecture"), ("HypervisorPresent", "hypervisor_present")],
    ),
    (
        "cpu",
        "Win32_Processor",
        &[
            ("Name", "model"),
            ("Manufacturer", "manufacturer"),
            ("NumberOfCores", "cores"),
            ("NumberOfLogicalProcessors", "threads"),
            ("MaxClockSpeed", "max_clock_mhz"),
            ("CurrentClockSpeed", "current_clock_mhz"),
            ("L2CacheSize", "l2_cache_kib"),
            ("L3CacheSize", "l3_cache_kib"),
            ("VirtualizationFirmwareEnabled", "virtualization_firmware"),
            ("SecondLevelAddressTranslationExtensions", "slat"),
        ],
    ),
    (
        "memory",
        "Win32_PhysicalMemory",
        &[
            ("Manufacturer", "manufacturer"),
            ("PartNumber", "part_number"),
            ("PartNumber", "model"),
            ("DeviceLocator", "slot"),
            ("Capacity", "capacity_bytes"),
            ("Speed", "rated_speed_mts"),
            ("ConfiguredClockSpeed", "configured_speed_mts"),
            ("SMBIOSMemoryType", "memory_type"),
            ("FormFactor", "form_factor"),
            ("DataWidth", "data_width_bits"),
            ("ConfiguredVoltage", "voltage_mv"),
        ],
    ),
    (
        "gpu",
        "Win32_VideoController",
        &[
            ("Name", "model"),
            ("AdapterCompatibility", "manufacturer"),
            ("DriverVersion", "driver_version"),
            ("DriverDate", "driver_date"),
            ("CurrentRefreshRate", "refresh_hz"),
            ("Status", "status"),
        ],
    ),
    (
        "storage",
        "Win32_DiskDrive",
        &[
            ("Model", "model"),
            ("Manufacturer", "manufacturer"),
            ("FirmwareRevision", "firmware_version"),
            ("Size", "capacity_bytes"),
            ("InterfaceType", "interface"),
            ("MediaType", "media_type"),
            ("Status", "status"),
        ],
    ),
    ("board", "Win32_BaseBoard", &[("Manufacturer", "manufacturer"), ("Product", "model"), ("Version", "version")]),
    ("firmware", "Win32_BIOS", &[("Manufacturer", "manufacturer"), ("SMBIOSBIOSVersion", "firmware_version"), ("ReleaseDate", "bios_date")]),
    ("os", "Win32_OperatingSystem", &[("Caption", "model"), ("Version", "version"), ("BuildNumber", "build"), ("OSArchitecture", "architecture")]),
    (
        "network",
        "MSFT_NetAdapter",
        &[
            ("InterfaceDescription", "model"),
            ("DriverVersionString", "driver_version"),
            ("DriverDate", "driver_date"),
            ("TransmitLinkSpeed", "transmit_speed_bps"),
            ("ReceiveLinkSpeed", "receive_speed_bps"),
            ("MediaConnectState", "status"),
            ("FullDuplex", "duplex"),
            ("MtuSize", "mtu_bytes"),
            ("Virtual", "virtual_adapter"),
            ("HardwareInterface", "hardware_interface"),
        ],
    ),
    ("audio", "Win32_SoundDevice", &[("Name", "model"), ("Manufacturer", "manufacturer"), ("Status", "status")]),
    (
        "usb",
        "Win32_PnPEntity USB",
        &[("Name", "model"), ("Manufacturer", "manufacturer"), ("Class", "class"), ("Status", "status"), ("Vendor", "vendor_id"), ("Product", "product_id")],
    ),
    (
        "pci",
        "Win32_PnPEntity PCI",
        &[("Name", "model"), ("Manufacturer", "manufacturer"), ("Class", "class"), ("Status", "status"), ("Vendor", "vendor_id"), ("Product", "product_id")],
    ),
    (
        "thunderbolt",
        "Win32_PnPEntity Thunderbolt/USB4",
        &[("Name", "model"), ("Manufacturer", "manufacturer"), ("Class", "class"), ("Status", "status"), ("Vendor", "vendor_id"), ("Product", "product_id")],
    ),
    (
        "display",
        "WmiMonitorBasicDisplayParams / WmiMonitorID",
        &[("Name", "model"), ("Manufacturer", "manufacturer"), ("Active", "enabled"), ("VideoInputType", "interface")],
    ),
    ("settings", "Confirm-SecureBootUEFI / powercfg", &[("SecureBoot", "secure_boot"), ("PowerPlan", "power_plan"), ("XmpExpo", "xmp_expo")]),
];

fn provider_error(error: &str) -> &str {
    if ["permission_denied", "timeout", "tool_missing", "output_limit", "unsupported"].contains(&error) { error } else { "error" }
}

fn enum_name(value: &Value, names: &[(u64, &str)]) -> Option<Value> {
    let number = value.as_u64()?;
    names.iter().find(|(n, _)| *n == number).map(|(_, name)| json!(name))
}

fn modern_disk(raw: &Value, drive: &Value, items: &[Value]) -> Option<Map<String, Value>> {
    let number = drive["Index"].as_u64()?;
    if items.iter().filter(|item| item["Index"].as_u64() == Some(number)).count() != 1 {
        return None;
    }
    let matches: Vec<_> = raw["modern_disks"].as_array()?.iter().filter(|item| item["Number"].as_u64() == Some(number)).collect();
    if matches.len() != 1 || raw["errors"]["modern_disks"].is_string() {
        return None;
    }
    let disk = matches[0];
    let mut fields = mapped(
        disk,
        &[
            ("LogicalSectorSize", "logical_sector_bytes"),
            ("PhysicalSectorSize", "physical_sector_bytes"),
            ("NumberOfPartitions", "partition_count"),
            ("IsReadOnly", "read_only"),
            ("IsBoot", "boot_disk"),
            ("IsSystem", "system_disk"),
        ],
    );
    for (key, value) in [
        (
            "bus",
            enum_name(
                &disk["BusType"],
                &[
                    (0, "unknown"),
                    (1, "SCSI"),
                    (2, "ATAPI"),
                    (3, "ATA"),
                    (4, "IEEE1394"),
                    (5, "SSA"),
                    (6, "FibreChannel"),
                    (7, "USB"),
                    (8, "RAID"),
                    (9, "iSCSI"),
                    (10, "SAS"),
                    (11, "SATA"),
                    (12, "SD"),
                    (13, "MMC"),
                    (14, "Virtual"),
                    (15, "FileBackedVirtual"),
                    (16, "StorageSpaces"),
                    (17, "NVMe"),
                    (18, "SCM"),
                    (19, "UFS"),
                ],
            ),
        ),
        ("partition_style", enum_name(&disk["PartitionStyle"], &[(0, "RAW"), (1, "MBR"), (2, "GPT")])),
        ("health_status", enum_name(&disk["HealthStatus"], &[(0, "healthy"), (1, "warning"), (2, "unhealthy"), (5, "unknown")])),
    ] {
        if let Some(value) = value {
            fields.insert(key.into(), value);
        }
    }
    Some(fields)
}

fn guard_fields(raw: &Value) -> Map<String, Value> {
    let mut fields = Map::new();
    if raw["errors"]["device_guard"].is_string() {
        return fields;
    }
    let Some(guards) = raw["device_guard"].as_array().filter(|items| items.len() == 1) else {
        return fields;
    };
    let guard = &guards[0];
    if let Some(value) = enum_name(&guard["VirtualizationBasedSecurityStatus"], &[(0, "disabled"), (1, "enabled_not_running"), (2, "running")]) {
        fields.insert("vbs_status".into(), value);
    }
    for (raw_key, credential_key, integrity_key) in [
        ("SecurityServicesConfigured", "credential_guard_configured", "memory_integrity_configured"),
        ("SecurityServicesRunning", "credential_guard_running", "memory_integrity_running"),
    ] {
        if let Some(values) = guard[raw_key].as_array().filter(|values| values.iter().all(|v| v.as_u64().is_some())) {
            fields.insert(credential_key.into(), json!(values.iter().any(|v| v.as_u64() == Some(1))));
            fields.insert(integrity_key.into(), json!(values.iter().any(|v| v.as_u64() == Some(2))));
        }
    }
    fields
}

fn add_provider(sections: &mut Map<String, Value>, id: &str, source: &str, status: &str) {
    let Some(section) = sections.get_mut(id) else {
        return;
    };
    if let Some(providers) = section["providers"].as_array_mut() {
        providers.push(json!({"source":source,"status":status}));
    }
    let failed = !matches!(status, "ok" | "empty" | "not_collected");
    if failed {
        section["status"] = if section["components"].as_array().is_some_and(|items| !items.is_empty()) {
            json!("partial")
        } else if section["status"] == "not_collected" {
            json!(status)
        } else {
            section["status"].clone()
        };
    }
}

fn registry_display(value: &Value) -> Option<Map<String, Value>> {
    let bytes: Vec<u8> = value["EDID"].as_array()?.iter().take(128).map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok())).collect::<Option<_>>()?;
    if bytes.len() != 128 || bytes[..8] != [0, 255, 255, 255, 255, 255, 255, 0] || bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0 {
        return None;
    }
    let manufacturer = u16::from_be_bytes([bytes[8], bytes[9]]);
    let letters = [(manufacturer >> 10) & 31, (manufacturer >> 5) & 31, manufacturer & 31];
    if letters.iter().any(|n| !(1..=26).contains(n)) {
        return None;
    }
    let maker: String = letters.iter().map(|n| (b'A' + *n as u8 - 1) as char).collect();
    let mut fields = Map::new();
    fields.insert("manufacturer".into(), json!(maker));
    fields.insert("product_id".into(), json!(format!("{:04X}", u16::from_le_bytes([bytes[10], bytes[11]]))));
    fields.insert("display_origin".into(), json!("registry_cached"));
    fields.insert("connection_status".into(), json!("unconfirmed"));
    for (byte, key) in [(21, "physical_width_cm"), (22, "physical_height_cm")] {
        if bytes[byte] > 0 {
            fields.insert(key.into(), json!(bytes[byte]));
        }
    }
    // Only the monitor-name descriptor is allowed. Serial descriptors and binary serial bytes never cross the boundary.
    for descriptor in bytes[54..126].as_chunks::<18>().0 {
        if descriptor[..5] == [0, 0, 0, 0xfc, 0] {
            let name: String = descriptor[5..].iter().take_while(|b| !matches!(**b, 0 | 10 | 13)).map(|b| *b as char).collect();
            if name.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
                && let Some(name) = scalar(&json!(name))
            {
                fields.insert("model".into(), name);
            }
        }
    }
    Some(fields)
}

pub fn normalize(raw: &Value, sections: &mut Map<String, Value>) {
    for (id, source, mapping) in QUERIES {
        if let Some(error) = raw["errors"][*id].as_str() {
            let error = provider_error(error);
            sections.insert((*id).into(), section(vec![], error, source));
            continue;
        }
        let Some(items) = raw[*id].as_array() else { continue };
        let mut components = Vec::new();
        for (index, v) in items.iter().take(MAX_COMPONENTS).enumerate() {
            let mut fields = mapped(v, mapping);
            let mut used_source = (*source).to_string();
            if *id == "storage"
                && let Some(extra) = modern_disk(raw, v, items)
            {
                fields.extend(extra);
                used_source.push_str(" / MSFT_Disk");
            }
            if *id == "settings" {
                fields.extend(guard_fields(raw));
            }
            if *id == "os" && !raw["errors"]["os_registry"].is_string() {
                fields.extend(mapped(
                    &raw["os_registry"],
                    &[("DisplayVersion", "display_version"), ("UBR", "update_build_revision"), ("InstallationType", "installation_type")],
                ));
            }
            if *id == "network" {
                if let Some(status) = enum_name(&v["InterfaceAdminStatus"], &[(1, "up"), (2, "down"), (3, "testing")]) {
                    fields.insert("admin_status".into(), status);
                }
                if v["MtuSize"].as_u64() == Some(0) {
                    fields.remove("mtu_bytes");
                }
                // Preserve asymmetric rates; a single common rate is valid only when both agree.
                if let (Some(tx), Some(rx)) = (v["TransmitLinkSpeed"].as_u64(), v["ReceiveLinkSpeed"].as_u64())
                    && tx == rx
                {
                    fields.insert("speed_bps".into(), json!(tx));
                }
            }
            if *id == "display" {
                fields.extend(mapped(v, &[("MaxHorizontalImageSize", "physical_width_cm"), ("MaxVerticalImageSize", "physical_height_cm")]));
            }
            if *id == "gpu" {
                // CIM AdapterRAM is a 32-bit value and is not authoritative VRAM.
                fields.insert("memory_kind".into(), json!("unknown"));
                if let (Some(w), Some(h)) = (v["CurrentHorizontalResolution"].as_u64(), v["CurrentVerticalResolution"].as_u64()) {
                    fields.insert("resolution".into(), json!(format!("{w} × {h}")));
                }
            }
            if let Some(model) = fields.get("model").and_then(Value::as_str) {
                let drivers: Vec<_> = raw["drivers"].as_array().into_iter().flatten().filter(|d| d["DeviceName"].as_str() == Some(model)).collect();
                let model_key = mapping.iter().find(|(_, target)| *target == "model").map(|(from, _)| *from);
                let unique_device = model_key.is_some_and(|key| items.iter().filter(|item| item[key].as_str().map(str::trim) == Some(model)).count() == 1);
                if drivers.len() == 1 && unique_device {
                    for (key, value) in mapped(drivers[0], &[("DriverVersion", "driver_version"), ("DriverDate", "driver_date")]) {
                        fields.entry(key).or_insert(value);
                    }
                }
            }
            let name = fields.get("model").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("{id} {}", index + 1));
            if *id == "gpu" {
                let matches: Vec<_> = raw["nvidia"].as_array().into_iter().flatten().filter(|g| g["model"].as_str() == Some(name.as_str())).collect();
                if matches.len() == 1 && items.iter().filter(|g| g["Name"].as_str() == Some(name.as_str())).count() == 1 {
                    fields.extend(mapped(
                        matches[0],
                        &[
                            ("dedicated_vram_bytes", "dedicated_vram_bytes"),
                            ("vbios_version", "vbios_version"),
                            ("driver_version", "driver_version"),
                            ("power_limit_w", "power_limit_w"),
                            ("pcie_generation", "pcie_generation"),
                            ("pcie_width", "pcie_width"),
                        ],
                    ));
                    if fields.contains_key("dedicated_vram_bytes") {
                        fields.insert("memory_kind".into(), json!("dedicated"));
                    }
                    used_source.push_str(" / nvidia-smi");
                }
            }
            components.push(component(&name, &used_source, fields));
        }
        let status = if items.len() > MAX_COMPONENTS {
            "partial"
        } else if components.is_empty() {
            "empty"
        } else {
            "ok"
        };
        sections.insert((*id).into(), section(components, status, source));
    }
    for (key, id, source) in
        [("modern_disks", "storage", "MSFT_Disk"), ("device_guard", "settings", "Win32_DeviceGuard"), ("os_registry", "os", "Windows CurrentVersion registry")]
    {
        if let Some(error) = raw["errors"][key].as_str() {
            add_provider(sections, id, source, provider_error(error));
        } else if !raw[key].is_null() {
            add_provider(sections, id, source, "ok");
        }
    }
    if raw["errors"]["display"].is_string() {
        if let Some(items) = raw["display_registry"].as_array() {
            let components: Vec<_> = items
                .iter()
                .take(64)
                .filter_map(registry_display)
                .map(|fields| {
                    let name = fields.get("model").and_then(Value::as_str).unwrap_or("Cached display").to_string();
                    component(&name, "Registry EDID (cached; current connection unconfirmed)", fields)
                })
                .collect();
            if !components.is_empty()
                && let Some(display) = sections.get_mut("display")
            {
                display["components"] = json!(components);
                display["status"] = json!("partial");
            }
            let status = if raw["display_registry_skipped"].as_u64().is_some_and(|count| count > 0) {
                "partial"
            } else if items.is_empty() {
                "empty"
            } else if items.len() > 64 || items.iter().take(64).any(|item| registry_display(item).is_none()) {
                "partial"
            } else {
                "ok"
            };
            add_provider(sections, "display", "Registry EDID (cached)", status);
        }
        if let Some(error) = raw["errors"]["display_registry"].as_str() {
            add_provider(sections, "display", "Registry EDID (cached)", provider_error(error));
        }
    }
}
