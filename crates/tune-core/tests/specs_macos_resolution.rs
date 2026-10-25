use serde_json::{Value, json};
use tune_core::specs;

#[test]
fn mac_platform_manufacturer_uses_only_the_native_public_property() {
    let report = specs::normalize(
        "macos",
        &json!({
            "SPHardwareDataType":[{"machine_model":"Example1,1", "chip_type":"Apple Test"}],
            "ioreg_public":{"manufacturer":"Apple Inc.","IOPlatformSerialNumber":"PRIVATE"}
        }),
    );
    let system = &report["sections"]["system"]["components"][0];
    assert_eq!(system["fields"]["manufacturer"], "Apple Inc.");
    assert!(system["source"].as_str().unwrap().contains("IOPlatformExpertDevice"));
    assert!(!report.to_string().contains("PRIVATE"));
    let no_property = specs::normalize("macos", &json!({"SPHardwareDataType":[{"chip_type":"Apple Test"}]}));
    assert!(no_property["sections"]["system"]["components"][0]["fields"]["manufacturer"].is_null());
}

fn gpu_report(bundles: Value, loaded: Value) -> Value {
    specs::normalize(
        "macos",
        &json!({
            "SPDisplaysDataType":[{"sppci_model":"Apple Test GPU","spdisplays_vendor":"sppci_vendor_Apple"},
                {"sppci_model":"Example discrete GPU","spdisplays_vendor":"Example"}],
            "ioreg_public":{"gpu_driver_bundles":bundles}, "gpu_loaded_drivers":loaded
        }),
    )
}

#[test]
fn mac_gpu_driver_requires_the_unique_active_registry_bundle_to_be_loaded() {
    let report = gpu_report(json!(["com.apple.AGXTest"]), json!({"com.apple.AGXTest":"123.4", "com.apple.Other":"999.0"}));
    let gpu = &report["sections"]["gpu"]["components"][0];
    assert_eq!(gpu["fields"]["driver_version"], "123.4");
    assert_eq!(gpu["fields"]["driver_name"], "com.apple.AGXTest");
    assert!(gpu["source"].as_str().unwrap().contains("kmutil showloaded"));
    assert!(report["sections"]["gpu"]["components"][1]["fields"]["driver_version"].is_null());
    for report in [
        gpu_report(json!(["com.apple.AGXTest"]), json!({"com.apple.Other":"999.0"})),
        gpu_report(json!(["com.apple.AGXTest", "com.apple.AGXOther"]), json!({"com.apple.AGXTest":"123.4"})),
        gpu_report(json!([]), json!({"com.apple.AGXTest":"123.4"})),
    ] {
        assert!(report["sections"]["gpu"]["components"][0]["fields"]["driver_version"].is_null());
    }
}

#[test]
fn mac_wifi_extracts_the_explicit_driver_component_without_copying_it_to_virtual_interfaces() {
    let report = specs::normalize(
        "macos",
        &json!({"SPAirPortDataType":[{"spairport_airport_interfaces":[
            {"_name":"en0","spairport_wireless_card_type":"Wi-Fi adapter", "spairport_wireless_firmware_version":
                "firmware 7.8 IO80211_driverkit-1234.5 \"IO80211_driverkit-1234.5\" Jan 1 2026"},
            {"_name":"awdl0","spairport_supported_channels":["1 (2GHz)"]},
            {"_name":"en1","spairport_wireless_card_type":"Example","spairport_wireless_firmware_version":"Firmware 9.8 CoreWLAN 99.0"}
        ]}]}),
    );
    let items = report["sections"]["network"]["components"].as_array().unwrap();
    let physical = items.iter().find(|c| c["fields"]["interface"] == "en0").unwrap();
    assert_eq!(physical["fields"]["driver_name"], "IO80211_driverkit");
    assert_eq!(physical["fields"]["driver_version"], "1234.5");
    assert!(physical["fields"]["firmware_version"].as_str().unwrap().contains("firmware 7.8"));
    for interface in ["awdl0", "en1"] {
        assert!(items.iter().find(|c| c["fields"]["interface"] == interface).unwrap()["fields"]["driver_version"].is_null());
    }
}

#[test]
fn mac_native_provider_failure_remains_visible_when_system_profiler_succeeds() {
    let report = specs::normalize(
        "macos",
        &json!({
            "SPHardwareDataType":[{"machine_model":"Example1,1"}],
            "SPDisplaysDataType":[{"sppci_model":"Apple Test GPU"}],
            "errors":{"ioreg_public":"timeout"}
        }),
    );
    for section in ["system", "gpu"] {
        assert_eq!(report["sections"][section]["status"], "partial");
        assert!(report["sections"][section]["providers"].as_array().unwrap().iter().any(|p| p["status"] == "timeout"));
    }
}
