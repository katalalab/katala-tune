use serde_json::{Value, json};
use tune_core::specs;

fn official(section: &str, fields: Value) -> Value {
    let vendor = fields["vendor_id"].as_str().unwrap();
    let id = if let Some(device) = fields["device_id"].as_str() {
        format!("PCI\\VEN_{vendor}&DEV_{device}\\PRIVATE")
    } else {
        format!("USB\\VID_{vendor}&PID_{}\\PRIVATE", fields["product_id"].as_str().unwrap())
    };
    let report = specs::normalize("windows", &json!({section:[{"Name":"Generic OS device","InterfaceDescription":"Generic OS device","PNPDeviceID":id}]}));
    let report = specs::validate_report(&report, "windows").unwrap();
    assert!(!report.to_string().contains("PRIVATE"));
    report["sections"][section]["components"][0]["official"].clone()
}

#[test]
fn processor_functions_are_identified_from_exact_public_ids_without_promoting_a_retail_model() {
    for (device, function) in [
        ("A700", "Host bridge / DRAM registers"),
        ("A77D", "Crash Log & Telemetry"),
        ("7D35", "Host bridge / DRAM registers"),
        ("AD03", "Dynamic Tuning Technology (DTT)"),
        ("AE0D", "Direct eSPI Controller"),
        ("7A50", "High Definition Audio"),
    ] {
        let result = official("pci", json!({"vendor_id":"8086","device_id":device}));
        assert_eq!(result["status"], "identified", "{device}");
        assert_eq!(result["match_kind"], "hardware_id");
        assert_eq!(result["specifications"]["controller_function"], function);
        assert!(result["specifications"]["cores"].is_null());
        assert!(result["specifications"]["revision_id"].is_null());
        assert_eq!(specs::official(&format!("PCI standard host CPU bridge {device}"))["status"], "unverified");
    }
}

#[test]
fn usb_and_pci_driver_family_ids_do_not_identify_unrelated_vendors_or_sections() {
    for (section, fields) in [
        ("network", json!({"vendor_id":"10EC","device_id":"8125"})),
        ("pci", json!({"vendor_id":"10EC","device_id":"8125"})),
        ("network", json!({"vendor_id":"2357","product_id":"0106"})),
        ("usb", json!({"vendor_id":"2357","product_id":"0106"})),
        ("network", json!({"vendor_id":"2357","product_id":"0138"})),
        ("usb", json!({"vendor_id":"2357","product_id":"0138"})),
    ] {
        let result = official(section, fields.clone());
        assert_eq!(result["status"], "identified", "{section} {fields}");
        assert!(result["scope"].as_str().unwrap().contains("family"));
        let mut other = fields;
        other["vendor_id"] = json!("FFFF");
        assert_eq!(official(section, other)["status"], "unverified");
    }
    assert_eq!(official("usb", json!({"vendor_id":"8086","product_id":"A700"}))["status"], "unverified");
    assert_eq!(official("pci", json!({"vendor_id":"2357","device_id":"0106"}))["status"], "unverified");
}

#[test]
fn streamcam_identity_covers_composite_and_audio_functions_without_inventing_endpoint_specs() {
    for section in ["usb", "audio"] {
        let result = official(section, json!({"vendor_id":"046D","product_id":"0893"}));
        assert_eq!(result["status"], "identified", "{section}");
        assert_eq!(result["specifications"]["controller_function"], "StreamCam USB composite / audio function");
        assert_eq!(result["source_access"], "manufacturer_page_full_body");
        assert!(result["scope"].as_str().unwrap().contains("endpoint"));
        assert!(result["specifications"]["resolution"].is_null());
        assert!(result["specifications"]["sample_rate_hz"].is_null());
        assert_eq!(official(section, json!({"vendor_id":"FFFF","product_id":"0893"}))["status"], "unverified");
    }
    assert_eq!(official("network", json!({"vendor_id":"046D","product_id":"0893"}))["status"], "unverified");
}

#[test]
fn logical_network_interfaces_and_unreported_baseboard_versions_keep_real_missing_fields() {
    let report = specs::normalize(
        "windows",
        &json!({
            "network":[{"InterfaceDescription":"Hyper-V Virtual Ethernet Adapter","Virtual":true,"HardwareInterface":false,"PNPDeviceID":null}],
            "board":[{"Manufacturer":"Example","Product":"Example board","Version":null}],
            "drivers":[{"DeviceName":"Hyper-V Virtual Ethernet Adapter","DeviceID":"ROOT\\UNRELATED\\PRIVATE","DriverVersion":"10.0.1"}]
        }),
    );
    let network = &report["sections"]["network"]["components"][0];
    assert!(network["fields"]["driver_version"].is_null());
    assert!(network["missing_fields"].as_array().unwrap().contains(&json!("driver_version")));
    assert!(report["sections"]["board"]["components"][0]["missing_fields"].as_array().unwrap().contains(&json!("version")));
    assert!(!report.to_string().contains("PRIVATE"));
}
