use serde_json::{Value, json};
use tune_core::specs;

fn identity(section: &str, vendor: &str, product: &str) -> Value {
    let raw = json!({section:[{"Name":"Generic OS device","PNPDeviceID":format!("USB\\VID_{vendor}&PID_{product}\\PRIVATE")} ]});
    let report = specs::normalize("windows", &raw);
    let validated = specs::validate_report(&report, "windows").unwrap();
    assert!(!validated.to_string().contains("PRIVATE"));
    validated["sections"][section]["components"][0]["official"].clone()
}

#[test]
fn manufacturer_receiver_ids_identify_families_without_guessing_the_paired_peripheral() {
    for (product, family) in [
        ("C52B", "Unifying receiver family"),
        ("C548", "Logitech C548 receiver / input family"),
        ("C53F", "LIGHTSPEED receiver family"),
        ("C547", "LIGHTSPEED receiver family"),
    ] {
        let official = identity("usb", "046D", product);
        assert_eq!(official["status"], "identified", "{product}");
        assert_eq!(official["match_kind"], "hardware_id");
        assert_eq!(official["specifications"]["controller_function"], family);
        for field in ["dpi", "polling_rate_hz", "paired_device", "firmware_version"] {
            assert!(official["specifications"][field].is_null(), "{product}: {field}");
        }
    }
}

#[test]
fn manufacturer_driver_ids_identify_audio_reader_and_input_families_without_sku_specs() {
    for (section, vendor, product, family) in [
        ("usb", "0B05", "1AF1", "Realtek USB Audio driver family"),
        ("audio", "0B05", "1AF1", "Realtek USB Audio driver family"),
        ("usb", "05E3", "0749", "Genesys Logic USB3.0 Card Reader driver family"),
        ("usb", "1532", "0091", "Razer Viper 8KHz USB input family"),
    ] {
        let official = identity(section, vendor, product);
        assert_eq!(official["status"], "identified", "{section} {vendor}:{product}");
        assert_eq!(official["specifications"]["controller_function"], family);
        assert_eq!(official["source_evidence"]["coverage"], "full_archive");
        assert_eq!(official["source_evidence"]["sha256"].as_str().unwrap().len(), 64);
        for field in ["codec", "board_model", "sample_rate_hz", "current_speed_mbps", "capacity_bytes", "dpi"] {
            assert!(official["specifications"][field].is_null(), "{section} {vendor}:{product}: {field}");
        }
    }
}

#[test]
fn new_usb_family_ids_reject_wrong_vendor_section_and_generic_names() {
    for (vendor, product) in [("046D", "C52B"), ("046D", "C548"), ("046D", "C53F"), ("046D", "C547"), ("0B05", "1AF1"), ("05E3", "0749"), ("1532", "0091")] {
        assert_eq!(identity("usb", "FFFF", product)["status"], "unverified");
        assert_eq!(identity("network", vendor, product)["status"], "unverified");
    }
    for name in ["USB Input Device", "USB Composite Device", "USB Audio Device", "LIGHTSPEED Receiver", "Generic USB Hub"] {
        assert_eq!(specs::official(name)["status"], "unverified", "{name}");
    }
    assert_eq!(identity("audio", "046D", "C52B")["status"], "unverified");
    assert_eq!(identity("audio", "05E3", "0749")["status"], "unverified");
}
