//! Opt-in, one-shot, local-only comparison probe. Not the Fleet ingestion schema.
//! No node identity, process enumeration, network requests, files, or GPU setters.
use serde::Serialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Observed,
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Reading<T: Serialize> {
    value: Option<T>,
    status: Status,
    reason: Option<&'static str>,
}

impl<T: Serialize> Reading<T> {
    fn observed(value: T) -> Self {
        Self { value: Some(value), status: Status::Observed, reason: None }
    }
    fn unknown(reason: &'static str) -> Self {
        Self { value: None, status: Status::Unknown, reason: Some(reason) }
    }
}

fn percentage(value: f32) -> Reading<f32> {
    if value.is_finite() && (0.0..=100.0).contains(&value) { Reading::observed(value) } else { Reading::unknown("invalid_value") }
}

fn memory(used: u64, total: u64) -> Reading<serde_json::Value> {
    if total == 0 || used > total {
        Reading::unknown("unavailable_or_invalid")
    } else {
        Reading::observed(serde_json::json!({"usedBytes": used, "totalBytes": total}))
    }
}

fn cpu(system: &System) -> Reading<f32> {
    if !sysinfo::IS_SUPPORTED_SYSTEM || system.cpus().is_empty() {
        Reading::unknown("unavailable_or_unsupported")
    } else {
        percentage(system.global_cpu_usage())
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn nvml_reason(error: nvml_wrapper::error::NvmlError) -> &'static str {
    use nvml_wrapper::error::NvmlError;
    match error {
        NvmlError::NoPermission => "permission_denied",
        NvmlError::NotSupported => "unsupported",
        NvmlError::LibloadingError(_) => "library_unavailable",
        _ => "collection_error",
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn gpu() -> Reading<Vec<serde_json::Value>> {
    use nvml_wrapper::{Nvml, enum_wrappers::device::TemperatureSensor};
    let nvml = match Nvml::init() {
        Ok(nvml) => nvml,
        Err(error) => return Reading::unknown(nvml_reason(error)),
    };
    let count = match nvml.device_count() {
        Ok(0) => return Reading::unknown("no_devices"),
        Ok(count) if count <= 16 => count,
        Ok(_) => return Reading::unknown("device_limit_exceeded"),
        Err(error) => return Reading::unknown(nvml_reason(error)),
    };
    let mut devices = Vec::new();
    for index in 0..count {
        let device = match nvml.device_by_index(index) {
            Ok(device) => device,
            // Do not silently turn an inaccessible device into an empty success.
            Err(error) => return Reading::unknown(nvml_reason(error)),
        };
        let utilization = match device.utilization_rates() {
            Ok(rates) => percentage(rates.gpu as f32),
            Err(error) => Reading::unknown(nvml_reason(error)),
        };
        let memory = match device.memory_info() {
            Ok(info) => memory(info.used, info.total),
            Err(error) => Reading::unknown(nvml_reason(error)),
        };
        let temperature = match device.temperature(TemperatureSensor::Gpu) {
            Ok(value) if value <= 150 => Reading::observed(value),
            Ok(_) => Reading::unknown("invalid_value"),
            Err(error) => Reading::unknown(nvml_reason(error)),
        };
        devices.push(serde_json::json!({"index":index,"utilizationPercent":utilization,"memory":memory,"temperatureC":temperature}));
    }
    Reading::observed(devices)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn gpu() -> Reading<Vec<serde_json::Value>> {
    Reading::unknown("unsupported_platform")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Reject extra arguments instead of accidentally offering a service or remote mode.
    if std::env::args_os().len() != 1 {
        return Err("This local one-shot probe accepts no arguments".into());
    }
    let mut system = System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_cpu_usage()));
    // CPU utilization is a delta, never interpret its unprimed first read as 0%.
    let window = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(250));
    std::thread::sleep(window);
    system.refresh_cpu_usage();
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let captured = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let output = serde_json::json!({
        "schemaVersion":1,"kind":"hybrid-canary","observedAtUnixMs":captured,
        "cpuWindowMs":window.as_millis(),"cpuPercent":cpu(&system),
        "memory":memory(system.used_memory(),system.total_memory()),"gpus":gpu(),
        // Do not claim missing WSL/container/Apple sensors are zero degrees.
        "hostTemperatureC":Reading::<f32>::unknown("not_collected"),
        "sources":{"host":"sysinfo-0.37.2","gpu":"nvml-wrapper-0.11.0"}
    });
    serde_json::to_writer(std::io::stdout().lock(), &output)?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_zero_cpu_is_not_missing() {
        assert_eq!(percentage(0.0).value, Some(0.0));
        assert_eq!(percentage(100.0).value, Some(100.0));
    }
    #[test]
    fn invalid_cpu_stays_unknown() {
        for value in [f32::NAN, f32::INFINITY, -1.0, 100.1] {
            let reading = percentage(value);
            assert_eq!(reading.status, Status::Unknown);
            assert!(reading.value.is_none());
        }
    }
    #[test]
    fn unavailable_memory_is_not_zero() {
        assert_eq!(memory(0, 0).status, Status::Unknown);
        assert_eq!(memory(2, 1).status, Status::Unknown);
        assert_eq!(memory(0, 1024).status, Status::Observed);
    }
    #[test]
    fn unknown_serializes_explicit_null_and_reason() {
        let reading = serde_json::to_value(Reading::<f32>::unknown("permission_denied")).unwrap();
        assert!(reading["value"].is_null());
        assert_eq!(reading["status"], "unknown");
        assert_eq!(reading["reason"], "permission_denied");
    }
    #[test]
    fn no_identifying_fields_in_reading() {
        let text = serde_json::to_string(&memory(1, 2)).unwrap();
        for forbidden in ["hostname", "serial", "process", "username", "address"] {
            assert!(!text.contains(forbidden));
        }
    }
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn nvml_errors_are_sanitized() {
        use nvml_wrapper::error::NvmlError;
        assert_eq!(nvml_reason(NvmlError::NoPermission), "permission_denied");
        assert_eq!(nvml_reason(NvmlError::NotSupported), "unsupported");
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    #[test]
    fn unsupported_gpu_is_unknown_not_empty_success() {
        assert_eq!(gpu().status, Status::Unknown);
        assert!(gpu().value.is_none());
    }
}
