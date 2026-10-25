use serde_json::{Value, json};
use tune_core::specs;

fn report(raw: Value) -> Value {
    specs::normalize("windows", &raw)
}
fn fields<'a>(r: &'a Value, section: &str, index: usize) -> &'a Value {
    &r["sections"][section]["components"][index]["fields"]
}

#[test]
fn modern_disks_join_by_unique_os_number_and_do_not_expose_identifiers() {
    let r = report(json!({"storage":[{"Index":3,"Model":"Example SSD","InterfaceType":"SCSI","SerialNumber":"PRIVATE"}],
        "modern_disks":[{"Number":1,"BusType":7},{"Number":3,"BusType":17,"LogicalSectorSize":512,"PhysicalSectorSize":4096,
            "PartitionStyle":2,"NumberOfPartitions":4,"IsReadOnly":false,"IsBoot":true,"IsSystem":true,"HealthStatus":0,"UniqueId":"PRIVATE","Path":"PRIVATE"}]}));
    let f = fields(&r, "storage", 0);
    assert_eq!(f["bus"], "NVMe");
    assert_eq!(f["logical_sector_bytes"], 512);
    assert_eq!(f["physical_sector_bytes"], 4096);
    assert_eq!(f["partition_style"], "GPT");
    assert_eq!(f["partition_count"], 4);
    assert_eq!(f["read_only"], false);
    assert_eq!(f["boot_disk"], true);
    assert_eq!(f["health_status"], "healthy");
    assert!(!r.to_string().contains("PRIVATE"));
    assert!(f.get("Index").is_none() && f.get("Number").is_none());
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert_eq!(fields(&safe, "storage", 0)["bus"], "NVMe");
    assert_eq!(fields(&safe, "storage", 0)["physical_sector_bytes"], 4096);
}

#[test]
fn ambiguous_modern_disk_numbers_do_not_guess_a_join_and_failures_remain_visible() {
    for raw in [
        json!({"storage":[{"Index":0,"Model":"Example"}],"modern_disks":[{"Number":0,"BusType":17},{"Number":0,"BusType":7}]}),
        json!({"storage":[{"Index":0,"Model":"Example"},{"Index":0,"Model":"Other"}],"modern_disks":[{"Number":0,"BusType":17}]}),
        json!({"storage":[{"Model":"Example"}],"modern_disks":[{"BusType":17}]}),
    ] {
        let r = report(raw);
        assert!(fields(&r, "storage", 0)["bus"].is_null());
    }
    let r = report(json!({"storage":[{"Index":0,"Model":"Example"}],"errors":{"modern_disks":"permission_denied"}}));
    assert_eq!(r["sections"]["storage"]["status"], "partial");
    assert!(r["sections"]["storage"]["providers"].as_array().unwrap().iter().any(|p| p["status"] == "permission_denied"));
}

#[test]
fn device_guard_arrays_become_scalar_states_and_missing_data_is_not_false() {
    let r = report(json!({"settings":[{"SecureBoot":"enabled"}],"device_guard":[{"VirtualizationBasedSecurityStatus":2,
        "SecurityServicesConfigured":[1,2],"SecurityServicesRunning":[2]}],"os":[{"Caption":"Example OS"}],
        "os_registry":{"DisplayVersion":"25H2","UBR":1234,"InstallationType":"Client"}}));
    let f = fields(&r, "settings", 0);
    assert_eq!(f["vbs_status"], "running");
    assert_eq!(f["credential_guard_configured"], true);
    assert_eq!(f["credential_guard_running"], false);
    assert_eq!(f["memory_integrity_running"], true);
    assert_eq!(fields(&r, "os", 0)["display_version"], "25H2");
    assert_eq!(fields(&r, "os", 0)["update_build_revision"], 1234);
    let missing = report(
        json!({"settings":[{"SecureBoot":"permission_denied"}],"device_guard":[{"VirtualizationBasedSecurityStatus":1}],"errors":{"os_registry":"permission_denied"},"os":[{"Caption":"Example"}]}),
    );
    assert!(fields(&missing, "settings", 0)["memory_integrity_running"].is_null());
    assert_eq!(fields(&missing, "settings", 0)["vbs_status"], "enabled_not_running");
    assert_eq!(missing["sections"]["os"]["status"], "partial");
    let denied = report(json!({"settings":[{"SecureBoot":"enabled"}],"errors":{"device_guard":"permission_denied"}}));
    assert_eq!(denied["sections"]["settings"]["status"], "partial");
    assert!(fields(&denied, "settings", 0)["vbs_status"].is_null());
    assert!(fields(&denied, "settings", 0)["credential_guard_running"].is_null());
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert_eq!(fields(&safe, "settings", 0)["memory_integrity_running"], true);
    assert_eq!(fields(&safe, "os", 0)["update_build_revision"], 1234);
}

#[test]
fn network_keeps_adapter_settings_and_driver_completion_requires_unique_names() {
    let r = report(
        json!({"network":[{"InterfaceDescription":"Example NIC","DriverVersionString":"authoritative","MtuSize":1500,"Virtual":false,"HardwareInterface":true,"AdminStatus":1}],
        "drivers":[{"DeviceName":"Example NIC","DriverVersion":"supplemental"}]}),
    );
    let f = fields(&r, "network", 0);
    assert_eq!(f["mtu_bytes"], 1500);
    assert_eq!(f["virtual_adapter"], false);
    assert_eq!(f["hardware_interface"], true);
    assert_eq!(f["admin_status"], "up");
    assert_eq!(f["driver_version"], "authoritative");
    for raw in [
        json!({"audio":[{"Name":"Same"}],"drivers":[{"DeviceName":"Same","DriverVersion":"a"},{"DeviceName":"Same","DriverVersion":"b"}]}),
        json!({"audio":[{"Name":"Same"},{"Name":"Same"}],"drivers":[{"DeviceName":"Same","DriverVersion":"a"}]}),
    ] {
        assert!(fields(&report(raw), "audio", 0)["driver_version"].is_null());
    }
}

fn edid() -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
    bytes[8] = 0x10;
    bytes[9] = 0xac; // DEL
    bytes[10] = 0x34;
    bytes[11] = 0x12;
    bytes[21] = 60;
    bytes[22] = 34;
    bytes[54..59].copy_from_slice(&[0, 0, 0, 0xfc, 0]);
    bytes[59..72].copy_from_slice(b"Example Panel");
    bytes[72..77].copy_from_slice(&[0, 0, 0, 0xff, 0]);
    bytes[77..90].copy_from_slice(b"PRIVATESECRET");
    bytes[127] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
    bytes
}

#[test]
fn registry_edid_fallback_is_validated_cached_and_preserves_wmi_failure() {
    let r = report(json!({"errors":{"display":"permission_denied"},"display_registry":[{"EDID":edid(),"Path":"PRIVATE"}]}));
    assert_eq!(r["sections"]["display"]["status"], "partial");
    let f = fields(&r, "display", 0);
    assert_eq!(f["model"], "Example Panel");
    assert_eq!(f["manufacturer"], "DEL");
    assert_eq!(f["product_id"], "1234");
    assert_eq!(f["display_origin"], "registry_cached");
    assert_eq!(f["connection_status"], "unconfirmed");
    assert_eq!(f["physical_width_cm"], 60);
    assert!(f["enabled"].is_null());
    assert!(!r.to_string().contains("PRIVATE"));
    assert!(r["sections"]["display"]["providers"].as_array().unwrap().iter().any(|p| p["status"] == "permission_denied"));
    let mut invalid = edid();
    invalid[127] ^= 1;
    let rejected = report(json!({"errors":{"display":"error"},"display_registry":[{"EDID":invalid}]}));
    assert!(rejected["sections"]["display"]["components"].as_array().unwrap().is_empty());
    let normal = report(json!({"display":[{"Name":"Live"}],"display_registry":[{"EDID":edid()}]}));
    assert_eq!(fields(&normal, "display", 0)["model"], "Live");
    assert_eq!(normal["sections"]["display"]["components"].as_array().unwrap().len(), 1);
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert_eq!(fields(&safe, "display", 0)["display_origin"], "registry_cached");
    assert!(safe["sections"]["display"]["providers"].as_array().unwrap().iter().any(|p| p["status"] == "permission_denied"));
}

#[test]
fn incomplete_registry_enumeration_retains_valid_displays_and_partial_reason() {
    let r = report(json!({"errors":{"display":"error"},"display_registry":[{"EDID":edid()}],"display_registry_skipped":1}));
    assert_eq!(fields(&r, "display", 0)["model"], "Example Panel");
    assert!(r["sections"]["display"]["providers"].as_array().unwrap().iter().any(|p| p["source"] == "Registry EDID (cached)" && p["status"] == "partial"));
    assert!(r["sections"]["display"].get("display_registry_skipped").is_none());
}
