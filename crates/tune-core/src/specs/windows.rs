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
Q "storage" "Win32_DiskDrive" @("Model","Manufacturer","FirmwareRevision","Size","InterfaceType","MediaType","Status")
Q "board" "Win32_BaseBoard" @("Manufacturer","Product","Version")
Q "firmware" "Win32_BIOS" @("Manufacturer","SMBIOSBIOSVersion","ReleaseDate")
Q "os" "Win32_OperatingSystem" @("Caption","Version","BuildNumber","OSArchitecture")
Q "audio" "Win32_SoundDevice" @("Name","Manufacturer","Status")
try {
  $r.network = @(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetAdapter -OperationTimeoutSec 8 | Select-Object InterfaceDescription,DriverVersionString,DriverDate,LinkSpeed,MediaConnectState,FullDuplex,Virtual)
} catch { $errors.network = "error" }
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
  $r.display = @(Get-CimInstance -Namespace root/wmi -ClassName WmiMonitorBasicDisplayParams -OperationTimeoutSec 8 | Select-Object MaxHorizontalImageSize,MaxVerticalImageSize,VideoInputType,Active)
} catch { $errors.display = "error" }
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
    let utf16: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &crate::logs::base64(&utf16)].map(str::to_string);
    match provider("powershell.exe", &args, 100).await {
        Ok(raw) => raw,
        Err(e) => json!({"errors":SECTIONS.iter().map(|k| ((*k).to_string(),json!(e))).collect::<Map<String,Value>>()}),
    }
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
            ("LinkSpeed", "speed_bps"),
            ("MediaConnectState", "status"),
            ("FullDuplex", "duplex"),
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
    ("display", "WmiMonitorBasicDisplayParams", &[("Active", "enabled"), ("VideoInputType", "interface")]),
    ("settings", "Confirm-SecureBootUEFI / powercfg", &[("SecureBoot", "secure_boot"), ("PowerPlan", "power_plan"), ("XmpExpo", "xmp_expo")]),
];

pub fn normalize(raw: &Value, sections: &mut Map<String, Value>) {
    for (id, source, mapping) in QUERIES {
        if let Some(error) = raw["errors"][*id].as_str() {
            let error = if ["permission_denied", "timeout", "tool_missing", "output_limit"].contains(&error) { error } else { "error" };
            sections.insert((*id).into(), section(vec![], error, source));
            continue;
        }
        let Some(items) = raw[*id].as_array() else { continue };
        let mut components = Vec::new();
        for (index, v) in items.iter().take(MAX_COMPONENTS).enumerate() {
            let mut fields = mapped(v, mapping);
            if *id == "gpu" {
                // CIM AdapterRAM is a 32-bit value and is not authoritative VRAM.
                fields.insert("memory_kind".into(), json!("unknown"));
                if let (Some(w), Some(h)) = (v["CurrentHorizontalResolution"].as_u64(), v["CurrentVerticalResolution"].as_u64()) {
                    fields.insert("resolution".into(), json!(format!("{w} × {h}")));
                }
            }
            if let Some(model) = fields.get("model").and_then(Value::as_str)
                && let Some(driver) = raw["drivers"].as_array().into_iter().flatten().find(|d| d["DeviceName"].as_str() == Some(model))
            {
                fields.extend(mapped(driver, &[("DriverVersion", "driver_version"), ("DriverDate", "driver_date")]));
            }
            let name = fields.get("model").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("{id} {}", index + 1));
            components.push(component(&name, source, fields));
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
}
