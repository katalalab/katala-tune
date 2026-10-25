use serde_json::{Value, json};
use tune_core::specs;

fn observed(section: &str, bus: &str, vendor: &str, device: &str) -> Value {
    let id = if bus == "PCI" { format!("PCI\\VEN_{vendor}&DEV_{device}\\PRIVATE") } else { format!("USB\\VID_{vendor}&PID_{device}\\PRIVATE") };
    let report = specs::normalize("windows", &json!({section:[{"Name":"Generic controller","InterfaceDescription":"Generic controller","PNPDeviceID":id}]}));
    let safe = specs::validate_report(&report, "windows").unwrap();
    assert!(!safe.to_string().contains("PRIVATE"));
    safe["sections"][section]["components"][0]["official"].clone()
}

#[test]
fn additional_pci_ids_identify_functions_without_claiming_installed_retail_skus() {
    for (vendor, device, function) in
        [("8086", "7F2F", "Error Aggregation Handler (EAH)"), ("10DE", "1ADB", "USB Type-C port policy controller"), ("8086", "F1A6", "NVMe controller")]
    {
        let result = observed("pci", "PCI", vendor, device);
        assert_eq!(result["status"], "identified", "{vendor}:{device}");
        assert_eq!(result["match_kind"], "hardware_id");
        assert_eq!(result["specifications"]["controller_function"], function);
        for unsupported in ["capacity_bytes", "dedicated_vram_bytes", "firmware_version", "board_revision", "cores"] {
            assert!(result["specifications"][unsupported].is_null(), "{unsupported}");
        }
        assert_eq!(observed("pci", "PCI", "FFFF", device)["status"], "unverified");
        assert_eq!(observed("usb", "USB", vendor, device)["status"], "unverified");
    }
    let os_evidence = observed("pci", "PCI", "8086", "F1A6");
    assert_eq!(os_evidence["source_access"], "os_driver_source_full_body");
    assert_eq!(os_evidence["source_evidence"]["material"], "source_code");
    assert_eq!(os_evidence["source_evidence"]["coverage"], "full_source_file");
}

#[test]
fn oem_usb_ethernet_evidence_covers_public_ids_without_assigning_the_oem_adapter_model() {
    for section in ["network", "usb"] {
        let result = observed(section, "USB", "0BDA", "8153");
        assert_eq!(result["status"], "identified", "{section}");
        assert_eq!(result["specifications"]["silicon_family"], "Realtek RTL8153 controller family");
        assert_eq!(result["specifications"]["controller_function"], "USB Ethernet controller");
        assert_eq!(observed(section, "USB", "FFFF", "8153")["status"], "unverified");
    }
    assert_eq!(observed("pci", "PCI", "0BDA", "8153")["status"], "unverified");
    for name in [
        "Realtek USB GbE Family Controller",
        "Error Aggregation Handler (EAH) - 7F2F",
        "NVIDIA USB Type-C Port Policy Controller",
        "Standard NVM Express Controller",
    ] {
        assert_eq!(specs::official(name)["status"], "unverified", "{name}");
    }
}

#[test]
fn auxiliary_ids_absent_from_obtained_sources_remain_unverified() {
    for (vendor, device) in [("10DE", "10F9"), ("10DE", "228B"), ("10DE", "22BA"), ("10DE", "22EB"), ("10DE", "1ADA"), ("C0A9", "5415"), ("C0A9", "5426")] {
        assert_eq!(observed("pci", "PCI", vendor, device)["status"], "unverified", "{vendor}:{device}");
    }
}

#[test]
fn hdaudio_codec_ids_are_public_but_instance_and_implementation_ids_are_not_pci_subsystems() {
    for device in ["0093", "009E", "00A3", "00AD"] {
        let report = specs::normalize(
            "windows",
            &json!({"audio":[{"Name":"NVIDIA High Definition Audio","PNPDeviceID":format!("HDAUDIO\\FUNC_01&VEN_10DE&DEV_{device}&SUBSYS_12345678&REV_1001\\PRIVATE")}]}),
        );
        let safe = specs::validate_report(&report, "windows").unwrap();
        let component = &safe["sections"]["audio"]["components"][0];
        assert_eq!(component["fields"]["vendor_id"], "10DE", "{device}");
        assert_eq!(component["fields"]["device_id"], device);
        assert_eq!(component["fields"]["revision_id"], "1001");
        assert!(component["fields"]["subsystem_vendor_id"].is_null());
        assert!(component["fields"]["subsystem_device_id"].is_null());
        assert_eq!(component["official"]["status"], "identified");
        assert_eq!(component["official"]["specifications"]["controller_function"], "High Definition Audio codec function");
        assert!(component["official"]["specifications"]["output_channels"].is_null());
        assert!(!safe.to_string().contains("PRIVATE"));
        assert!(!safe.to_string().contains("12345678"));
        assert_eq!(observed("pci", "PCI", "10DE", device)["status"], "unverified");
    }
    let no_revision = specs::normalize("windows", &json!({"audio":[{"Name":"Generic audio","PNPDeviceID":"hdaudio\\func_01&ven_10de&dev_00a3\\PRIVATE"}]}));
    let component = &no_revision["sections"]["audio"]["components"][0];
    assert_eq!(component["fields"]["device_id"], "00A3");
    assert!(component["fields"]["revision_id"].is_null());
}

#[test]
fn hdaudio_requires_the_documented_audio_function_and_valid_codec_identifiers() {
    for id in [
        "HDAUDIO\\FUNC_02&VEN_10DE&DEV_00A3\\PRIVATE",
        "HDAUDIO\\VEN_10DE&DEV_00A3\\PRIVATE",
        "HDAUDIO\\FUNC_01&VEN_10DG&DEV_00A3\\PRIVATE",
        "HDAUDIO\\FUNC_01&VEN_10DE&DEV_00AG\\PRIVATE",
        "HDAUDIO\\FUNC_01&VEN_0000&DEV_00A3\\PRIVATE",
        "HDAUDIO\\FUNC_01&VEN_FFFF&DEV_00A3\\PRIVATE",
        "HDAUDIO\\FUNC_01&VEN_10DE&DEV_00A3&DEV_0093\\PRIVATE",
        "HDAUDIO\\FUNC_01&VEN_10DE&DEV_00A3&VEN_1234\\PRIVATE",
        "HDAUDIO\\FUNC_01&VEN_10DE&DEV_00A3&SUBSYS_123\\PRIVATE",
        "HDAUDIO\\FUNC_01&VEN_10DE&DEV_00A3&REV_1001&REV_1002\\PRIVATE",
    ] {
        let report = specs::normalize("windows", &json!({"audio":[{"Name":"NVIDIA High Definition Audio","PNPDeviceID":id}]}));
        let component = &report["sections"]["audio"]["components"][0];
        assert!(component["fields"]["vendor_id"].is_null(), "{id}");
        assert!(component["fields"]["device_id"].is_null(), "{id}");
        assert_eq!(component["official"]["status"], "unverified");
        assert!(!report.to_string().contains("PRIVATE"));
    }
    let unknown = specs::normalize("windows", &json!({"audio":[{"Name":"Generic audio","PNPDeviceID":"HDAUDIO\\FUNC_01&VEN_1234&DEV_00A3\\PRIVATE"}]}));
    let component = &unknown["sections"]["audio"]["components"][0];
    assert_eq!(component["fields"]["vendor_id"], "1234");
    assert_eq!(component["fields"]["device_id"], "00A3");
    assert_eq!(component["official"]["status"], "unverified");
}

#[test]
fn audio_codec_catalog_cannot_match_pci_ids_in_the_audio_section() {
    for (vendor, device) in [("10DE", "00A3"), ("10EC", "0897")] {
        let r = specs::normalize("windows", &json!({"audio":[{"Name":"Generic audio","PNPDeviceID":format!("PCI\\VEN_{vendor}&DEV_{device}\\PRIVATE")}]}));
        let c = &r["sections"]["audio"]["components"][0];
        assert_eq!(c["official"]["status"], "unverified");
        assert_eq!(c["fields"]["hardware_id_bus"], "PCI");
    }
    let mut r = specs::normalize("windows", &json!({"audio":[{"Name":"Generic audio","PNPDeviceID":"HDAUDIO\\FUNC_01&VEN_10EC&DEV_0897\\PRIVATE"}]}));
    r["sections"]["audio"]["components"][0]["fields"]["hardware_id_bus"] = json!("PRIVATE");
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert!(!safe.to_string().contains("PRIVATE"));
    assert_eq!(safe["sections"]["audio"]["components"][0]["official"]["status"], "unverified");
}

#[test]
fn realtek_codec_mapping_uses_the_complete_vendor_and_codec_id_without_board_claims() {
    for (vendor, expected) in [("10EC", "identified"), ("1234", "unverified")] {
        let report =
            specs::normalize("windows", &json!({"audio":[{"Name":"Generic audio","PNPDeviceID":format!("HDAUDIO\\FUNC_01&VEN_{vendor}&DEV_0897\\PRIVATE")}]}));
        let safe = specs::validate_report(&report, "windows").unwrap();
        let official = &safe["sections"]["audio"]["components"][0]["official"];
        assert_eq!(official["status"], expected);
        if expected == "identified" {
            assert_eq!(official["source_access"], "os_driver_source_full_body");
            assert_eq!(official["specifications"]["silicon_family"], "Realtek ALC897 codec family");
            for unsupported in ["output_channels", "sample_rate_hz", "board_revision", "motherboard_sku"] {
                assert!(official["specifications"][unsupported].is_null());
            }
        }
        assert!(!safe.to_string().contains("PRIVATE"));
    }
    assert_eq!(observed("pci", "PCI", "10EC", "0897")["status"], "unverified");
    assert_eq!(specs::official("Realtek(R) Audio")["status"], "unverified");
}
