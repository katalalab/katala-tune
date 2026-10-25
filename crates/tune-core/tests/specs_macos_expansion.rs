use serde_json::{Value, json};
use tune_core::specs;

fn components(report: &Value, section: &str) -> Vec<Value> {
    report["sections"][section]["components"].as_array().unwrap().clone()
}

#[test]
fn newer_usb_host_provider_keeps_peripheral_fields_without_identifiers() {
    let r = specs::normalize(
        "macos",
        &json!({"SPUSBHostDataType":[{
            "_name":"USB Bus", "Driver":"ExampleUSBHost", "_items":[{
                "_name":"Example Camera", "USBDeviceKeyVendorName":"Example Vendor",
                "USBDeviceKeyVendorID":"0x1234", "USBDeviceKeyProductID":"0x5678",
                "USBDeviceKeyProductVersion":"0x0102", "USBDeviceKeyLinkSpeed":"5 Gb/s",
                "USBDeviceKeySerialNumber":"SYNTHETIC_PRIVATE", "USBKeyLocationID":"SYNTHETIC_PRIVATE"
            }]
        }]}),
    );
    let c = components(&r, "usb");
    assert_eq!(c.len(), 2);
    assert_eq!(c[1]["fields"]["manufacturer"], "Example Vendor");
    assert_eq!(c[1]["fields"]["vendor_id"], "0x1234");
    assert_eq!(c[1]["fields"]["product_id"], "0x5678");
    assert_eq!(c[1]["fields"]["version"], "0x0102");
    assert_eq!(c[1]["fields"]["link_speed"], "5 Gb/s");
    assert!(!r.to_string().contains("SYNTHETIC_PRIVATE"));
}

#[test]
fn wifi_firmware_whitespace_is_normalized_and_firmware_text_is_not_a_model() {
    let r = specs::normalize(
        "macos",
        &json!({"SPAirPortDataType":[{
            "spairport_airport_interfaces":[{
                "_name":"en0", "spairport_wireless_card_type":"chip id: 0x11 firmware [Rev 1.2.3]",
                "spairport_wireless_firmware_version":"Example firmware version 1.2.3\nDriverKit-4.5\tbuild",
                "spairport_wireless_mac_address":"SYNTHETIC_PRIVATE"
            },{
                "_name":"en1", "spairport_wireless_card_type":"AirPort Extreme (0x1234, 0x5678)",
                "spairport_wireless_firmware_version":"Example firmware 2.3\n"
            }]
        }]}),
    );
    let c = components(&r, "network");
    let first = c.iter().find(|c| c["fields"]["interface"] == "en0").unwrap();
    assert_eq!(first["fields"]["model"], "Wi-Fi adapter");
    assert_eq!(first["fields"]["firmware_version"], "Example firmware version 1.2.3 DriverKit-4.5 build");
    let second = c.iter().find(|c| c["fields"]["interface"] == "en1").unwrap();
    assert_eq!(second["fields"]["model"], "AirPort Extreme (0x1234, 0x5678)");
    assert!(!r.to_string().contains("SYNTHETIC_PRIVATE"));
}

#[test]
fn nvme_preserves_trim_smart_and_available_controller_link_fields() {
    let r = specs::normalize(
        "macos",
        &json!({"SPNVMeDataType":[{
            "_name":"NVMe Controller", "link_speed":"8.0 GT/s", "link_width":"x4", "_items":[{
                "_name":"Example SSD", "device_model":"Example NVMe 2000", "device_revision":"REV2",
                "spnvme_trim_support":"Yes", "smart_status":"Verified", "serial_num":"SYNTHETIC_PRIVATE"
            }]
        }]}),
    );
    let c = components(&r, "storage");
    assert_eq!(c[0]["fields"]["link_speed"], "8.0 GT/s");
    assert_eq!(c[0]["fields"]["pcie_width"], "x4");
    assert_eq!(c[1]["fields"]["trim_support"], "Yes");
    assert_eq!(c[1]["fields"]["smart_status"], "Verified");
    assert_eq!(c[1]["fields"]["firmware_version"], "REV2");
    assert!(!r.to_string().contains("SYNTHETIC_PRIVATE"));
}

#[test]
fn thunderbolt_physical_ports_keep_status_and_advertised_speed_without_unique_ids() {
    let r = specs::normalize(
        "macos",
        &json!({"SPThunderboltDataType":[{
            "_name":"Thunderbolt/USB4 Bus", "vendor_name_key":"Example Vendor",
            "domain_uuid_key":"SYNTHETIC_PRIVATE", "route_string_key":"SYNTHETIC_PRIVATE", "switch_uid_key":"SYNTHETIC_PRIVATE",
            "receptacle_1_tag":{"receptacle_id_key":"3", "current_speed_key":"Up to 120 Gb/s", "receptacle_status_key":"receptacle_no_devices_connected", "link_status_key":"0x100"}
        }]}),
    );
    let c = components(&r, "thunderbolt");
    assert_eq!(c.len(), 2);
    assert_eq!(c[0]["fields"]["manufacturer"], "Example Vendor");
    assert_eq!(c[1]["name"], "Thunderbolt port 3");
    assert_eq!(c[1]["fields"]["port"], "3");
    assert_eq!(c[1]["fields"]["status"], "receptacle_no_devices_connected");
    assert_eq!(c[1]["fields"]["link_speed"], "Up to 120 Gb/s");
    assert!(!r.to_string().contains("SYNTHETIC_PRIVATE"));
}

#[test]
fn known_audio_serial_suffix_is_removed_in_audio_and_usb_but_skus_are_preserved() {
    let r = specs::normalize(
        "macos",
        &json!({
            "SPAudioDataType":[{"_name":"Devices","_items":[{
                "_name":"Babyface Pro (87654321)", "coreaudio_device_manufacturer":"RME",
                "coreaudio_device_transport":"USB", "coreaudio_device_srate":48000,
                "coreaudio_device_input":12,"coreaudio_device_output":12
            },{"_name":"Example Model 12345678"}]}],
            "SPUSBHostDataType":[{"_name":"Babyface Pro FS (87654321)","USBDeviceKeyVendorName":"RME"},
                {"_name":"Example SKU-12345678"}]
        }),
    );
    let audio = components(&r, "audio");
    assert_eq!(audio[1]["name"], "Babyface Pro");
    assert_eq!(audio[1]["fields"]["model"], "Babyface Pro");
    assert_eq!(audio[1]["fields"]["manufacturer"], "RME");
    assert_eq!(audio[1]["fields"]["interface"], "USB");
    assert_eq!(audio[1]["fields"]["sample_rate_hz"], 48000);
    assert_eq!(audio[1]["fields"]["input_channels"], 12);
    assert_eq!(audio[1]["fields"]["output_channels"], 12);
    assert_eq!(audio[2]["name"], "Example Model 12345678");
    let usb = components(&r, "usb");
    assert_eq!(usb[0]["name"], "Babyface Pro FS");
    assert_eq!(usb[1]["name"], "Example SKU-12345678");
    assert!(!r.to_string().contains("87654321"));
}

#[test]
fn readonly_security_states_are_reported_separately_from_uninspected_recovery_policy() {
    let r = specs::normalize(
        "macos",
        &json!({
            "power_settings":[{"model":"AC Power","sleep":10}],
            "security_settings":{"system_integrity_protection":"enabled","filevault":"disabled"}
        }),
    );
    let c = components(&r, "settings");
    assert!(c.iter().any(|c| c["fields"]["system_integrity_protection"] == "enabled"));
    assert!(c.iter().any(|c| c["fields"]["filevault"] == "disabled"));
    assert!(c.iter().any(|c| c["fields"]["secure_boot"] == "not_collected_requires_recovery_policy_inspection"));
    assert_eq!(r["sections"]["settings"]["status"], "ok");
    let failed = specs::normalize("macos", &json!({"errors":{"csrutil":"permission_denied","fdesetup":"timeout"}}));
    assert!(
        failed["sections"]["settings"]["providers"].as_array().unwrap().iter().any(|p| p["source"] == "csrutil status" && p["status"] == "permission_denied")
    );
    assert!(!failed.to_string().contains("\"filevault\":\"disabled\""));
}
