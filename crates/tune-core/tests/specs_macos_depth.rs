use serde_json::{Value, json};
use tune_core::specs;

fn items(report: &Value, section: &str) -> Vec<Value> {
    report["sections"][section]["components"].as_array().unwrap().clone()
}

#[test]
fn unified_memory_summary_is_a_group_and_total_has_actual_type_and_vendor() {
    let r = specs::normalize(
        "macos",
        &json!({
            "SPHardwareDataType":[{"chip_type":"Apple Example", "physical_memory":"32 GB"}],
            "SPMemoryDataType":[{"dimm_type":"LPDDR5", "dimm_manufacturer":"Example Vendor"}],
            "hw.memsize":34359738368_u64
        }),
    );
    let c = items(&r, "memory");
    assert_eq!(c.len(), 2);
    assert_eq!(c[0]["fields"]["record_kind"], "aggregate");
    assert_eq!(c[0]["fields"]["memory_type"], "LPDDR5");
    assert_eq!(c[0]["fields"]["manufacturer"], "Example Vendor");
    assert_eq!(c[1]["fields"]["record_kind"], "group");
    assert_eq!(c[1]["fields"]["class"], "memory_summary");
    assert_eq!(c[0]["fields"]["capacity_bytes"], 34359738368_u64);
}

#[test]
fn nvme_controller_and_disk_are_distinct_records() {
    let r = specs::normalize(
        "macos",
        &json!({"SPNVMeDataType":[{
            "_name":"Apple SSD Controller", "_items":[{"_name":"Example SSD", "device_model":"Example SSD 1TB",
            "device_revision":"REV1", "size_in_bytes":1000000000000_u64}]
        }]}),
    );
    let c = items(&r, "storage");
    assert_eq!(c[0]["fields"]["class"], "storage_controller");
    assert_eq!(c[0]["fields"]["record_kind"], "device");
    assert!(c[0]["fields"]["capacity_bytes"].is_null());
    assert_eq!(c[1]["fields"]["record_kind"], "device");
    assert_eq!(c[1]["fields"]["capacity_bytes"], 1000000000000_u64);
}

#[test]
fn audio_group_and_explicit_virtual_transport_are_not_physical_devices() {
    let r = specs::normalize(
        "macos",
        &json!({"SPAudioDataType":[{"_name":"coreaudio_device", "_items":[
            {"_name":"Example Microphone", "coreaudio_device_transport":"coreaudio_device_type_usb", "coreaudio_device_input":2},
            {"_name":"Example Virtual Audio", "coreaudio_device_transport":"coreaudio_device_type_virtual", "coreaudio_device_output":2}
        ]}]}),
    );
    let c = items(&r, "audio");
    assert_eq!(c[0]["fields"]["record_kind"], "group");
    assert_eq!(c[1]["fields"]["record_kind"], "device");
    assert_eq!(c[2]["fields"]["record_kind"], "software");
    assert_eq!(c[2]["fields"]["output_channels"], 2);
}

#[test]
fn wifi_public_ids_and_capabilities_do_not_include_connection_identifiers() {
    let r = specs::normalize(
        "macos",
        &json!({"SPAirPortDataType":[{"spairport_airport_interfaces":[
            {"_name":"en0", "spairport_wireless_card_type":"spairport_wireless_card_type_wifi (0x14E4, 0x4388)",
             "spairport_supported_phymodes":"802.11 a/b/g/n/ac/ax", "spairport_status_information":"spairport_status_connected",
             "spairport_supported_channels":["1 (2GHz)","36 (5GHz)"],
             "spairport_current_network_information":{"_name":"SYNTHETIC_PRIVATE", "spairport_network_bssid":"SYNTHETIC_PRIVATE"},
             "spairport_wireless_mac_address":"SYNTHETIC_PRIVATE"},
            {"_name":"awdl0", "spairport_supported_channels":["1 (2GHz)"]}
        ]}]}),
    );
    let c = items(&r, "network");
    let physical = c.iter().find(|c| c["fields"]["interface"] == "en0").unwrap();
    assert_eq!(physical["fields"]["vendor_id"], "0x14E4");
    assert_eq!(physical["fields"]["device_id"], "0x4388");
    assert_eq!(physical["fields"]["connection_state"], "connected");
    assert_eq!(physical["fields"]["supported_standards"], "802.11 a/b/g/n/ac/ax");
    assert_eq!(physical["fields"]["supported_channels"], "2GHz: 1; 5GHz: 36");
    let virtual_device = c.iter().find(|c| c["fields"]["model"] == "awdl0").unwrap();
    assert_eq!(virtual_device["fields"]["record_kind"], "software");
    assert!(!r.to_string().contains("SYNTHETIC_PRIVATE"));
}

#[test]
fn display_public_panel_ids_and_refresh_are_retained_without_unique_identifiers() {
    let r = specs::normalize(
        "macos",
        &json!({"SPDisplaysDataType":[{"sppci_model":"Example GPU", "spdisplays_ndrvs":[{
            "_name":"Built-in display", "_spdisplays_display-vendor-id":"610", "_spdisplays_display-product-id":"a001",
            "_spdisplays_resolution":"1512 x 982 @ 120.00Hz", "_spdisplays_displayID":"SYNTHETIC_PRIVATE",
            "_spdisplays_display-serial-number":"SYNTHETIC_PRIVATE"
        }]}]}),
    );
    let c = items(&r, "display");
    assert_eq!(c[0]["fields"]["vendor_id"], "610");
    assert_eq!(c[0]["fields"]["product_id"], "a001");
    assert_eq!(c[0]["fields"]["refresh_hz"], 120.0);
    assert!(!r.to_string().contains("SYNTHETIC_PRIVATE"));
}

#[test]
fn os_build_is_separate_from_version_and_apple_cpu_vendor_uses_chip_evidence() {
    let r = specs::normalize(
        "macos",
        &json!({
            "SPSoftwareDataType":[{"os_version":"macOS 26.6.2 (25G83)","kernel_version":"Darwin 25.6.0"}],
            "SPHardwareDataType":[{"chip_type":"Apple Example"}]
        }),
    );
    let os = items(&r, "os");
    assert_eq!(os[0]["fields"]["version"], "macOS 26.6.2");
    assert_eq!(os[0]["fields"]["build"], "25G83");
    assert_eq!(items(&r, "cpu")[0]["fields"]["manufacturer"], "Apple");
    let other = specs::normalize("macos", &json!({"SPHardwareDataType":[{"cpu_type":"Example CPU"}]}));
    assert!(items(&other, "cpu")[0]["fields"]["manufacturer"].is_null());
}

#[test]
fn os_architecture_comes_from_finite_hw_machine_without_model_inference() {
    for architecture in ["arm64", "x86_64"] {
        let r = specs::normalize("macos", &json!({"SPSoftwareDataType":[{"os_version":"macOS 26.6.2 (25G83)"}], "hw.machine":architecture}));
        let os = items(&r, "os");
        assert_eq!(os[0]["fields"]["architecture"], architecture);
        assert!(os[0]["source"].as_str().unwrap().contains("sysctl hw.machine"));
    }
    let missing = specs::normalize(
        "macos",
        &json!({
            "SPSoftwareDataType":[{"os_version":"macOS 26.6.2 (25G83)"}],
            "SPHardwareDataType":[{"machine_model":"MacExample1,1", "chip_type":"Apple Example"}],
            "hw.model":"MacExample1,1"
        }),
    );
    assert!(items(&missing, "os")[0]["fields"]["architecture"].is_null());
}
