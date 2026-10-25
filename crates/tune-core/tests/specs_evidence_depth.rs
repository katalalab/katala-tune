use serde_json::json;
use tune_core::specs;

#[test]
fn sata_controller_applicability_is_rebuilt_for_remote_reports() {
    let r = specs::normalize(
        "macos",
        &json!({"SPSerialATADataType":[{"_name":"Example SATA Controller","_items":[{"device_model":"Example Disk","device_revision":"1","size_in_bytes":1000000000}]}]}),
    );
    let safe = specs::validate_report(&r, "macos").unwrap();
    let controller = &safe["sections"]["storage"]["components"][0];
    assert_eq!(controller["fields"]["class"], "storage_controller");
    assert_eq!(controller["missing_fields"], json!([]));
}

#[test]
fn public_id_identity_is_section_scoped_and_does_not_become_a_retail_model_match() {
    let r = specs::normalize("windows", &json!({"pci":[{"Name":"SMBus Controller","PNPDeviceID":"PCI\\VEN_8086&DEV_7A23\\PRIVATE"}]}));
    let c = &r["sections"]["pci"]["components"][0];
    assert_eq!(c["official"]["status"], "identified");
    assert_eq!(c["official"]["match_kind"], "hardware_id");
    assert!(c["official"]["source_access"].as_str().unwrap().ends_with("_full_body"));
    assert!(c["official"]["source_evidence"]["sha256"].as_str().is_some_and(|s| s.len() == 64));
    assert!(!r.to_string().contains("PRIVATE"));
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert_eq!(safe["sections"]["pci"]["components"][0]["official"], c["official"]);
    for (section, field, value) in [("usb", "product_id", "7A23"), ("pci", "device_id", "7A230")] {
        let mut other = specs::normalize("windows", &json!({"usb":[{"Name":"SMBus Controller"}],"pci":[{"Name":"SMBus Controller"}]}));
        let f = &mut other["sections"][section]["components"][0]["fields"];
        f["vendor_id"] = json!("8086");
        f[field] = json!(value);
        let safe = specs::validate_report(&other, "windows").unwrap();
        assert_eq!(safe["sections"][section]["components"][0]["official"]["status"], "unverified");
    }
}

#[test]
fn provider_groups_and_controllers_do_not_invent_missing_disk_or_dimm_properties() {
    let r = specs::normalize(
        "macos",
        &json!({
            "SPHardwareDataType":[{"chip_type":"Apple Test"}],"hw.memsize":17179869184_u64,
            "SPMemoryDataType":[{"dimm_type":"LPDDR5","dimm_manufacturer":"Example"}],
            "SPNVMeDataType":[{"_name":"Apple SSD Controller","_items":[{"device_model":"Example SSD","device_revision":"1","size_in_bytes":1000000000}]}],
            "SPAudioDataType":[{"_name":"coreaudio_device","_items":[{"_name":"Example Audio","coreaudio_device_transport":"coreaudio_device_type_virtual"}]}]
        }),
    );
    let safe = specs::validate_report(&r, "macos").unwrap();
    let memory = &safe["sections"]["memory"]["components"];
    let group = memory.as_array().unwrap().iter().find(|c| c["fields"]["class"] == "memory_summary").unwrap();
    assert_eq!(group["fields"]["record_kind"], "group");
    assert_eq!(group["missing_fields"], json!([]));
    assert_eq!(group["official"]["status"], "not_applicable");
    let controller = &safe["sections"]["storage"]["components"][0];
    assert_eq!(controller["fields"]["class"], "storage_controller");
    assert_eq!(controller["fields"]["record_kind"], "device");
    assert_eq!(controller["missing_fields"], json!([]));
    assert_eq!(controller["official"]["status"], "unverified");
    assert_eq!(safe["sections"]["audio"]["components"][0]["fields"]["record_kind"], "group");
    assert_eq!(safe["sections"]["audio"]["components"][1]["official"]["status"], "not_applicable");
}

#[test]
fn remote_record_kind_is_rederived_instead_of_hiding_a_physical_product_gap() {
    let mut r = specs::normalize("windows", &json!({"cpu":[{"Name":"Example CPU"}]}));
    let c = &mut r["sections"]["cpu"]["components"][0];
    c["fields"]["record_kind"] = json!("software");
    c["official"] = json!({"status":"identified","url":"https://evil.example"});
    c["missing_fields"] = json!([]);
    let safe = specs::validate_report(&r, "windows").unwrap();
    let c = &safe["sections"]["cpu"]["components"][0];
    assert_eq!(c["fields"]["record_kind"], "device");
    assert_eq!(c["official"]["status"], "unverified");
    assert!(!c["missing_fields"].as_array().unwrap().is_empty());
    assert!(!safe.to_string().contains("evil.example"));
}

#[test]
fn safe_driver_and_native_properties_survive_validation_without_private_instance_identifiers() {
    let mut r = specs::normalize("windows", &json!({"usb":[{"Name":"Example USB"}]}));
    let f = &mut r["sections"]["usb"]["components"][0]["fields"];
    for (k, v) in [
        ("driver_inf", json!("example.inf")),
        ("driver_provider", json!("Example")),
        ("driver_signed", json!(true)),
        ("present", json!(false)),
        ("problem_code", json!(45)),
        ("revision_id", json!("03")),
        ("subsystem_vendor_id", json!("1234")),
        ("subsystem_device_id", json!("5678")),
        ("supported_standards", json!("802.11ax")),
        ("connection_state", json!("disconnected")),
    ] {
        f[k] = v;
    }
    f["instance_id"] = json!("PRIVATE");
    let safe = specs::validate_report(&r, "windows").unwrap();
    let f = &safe["sections"]["usb"]["components"][0]["fields"];
    assert_eq!(f["driver_inf"], "example.inf");
    assert_eq!(f["present"], false);
    assert_eq!(f["subsystem_device_id"], "5678");
    assert_eq!(f["supported_standards"], "802.11ax");
    assert!(!safe.to_string().contains("PRIVATE"));
    let mut malformed = r.clone();
    let f = &mut malformed["sections"]["usb"]["components"][0]["fields"];
    f["driver_inf"] = json!("C:\\PRIVATE\\oem.inf");
    f["driver_signed"] = json!("PRIVATE");
    f["present"] = json!(1);
    f["subsystem_vendor_id"] = json!("PRIVATE");
    let safe = specs::validate_report(&malformed, "windows").unwrap();
    assert!(!safe.to_string().contains("PRIVATE"));
    assert!(safe["sections"]["usb"]["components"][0]["fields"]["present"].is_null());
}
