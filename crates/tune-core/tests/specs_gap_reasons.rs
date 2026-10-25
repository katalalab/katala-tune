use serde_json::{Value, json};
use tune_core::specs;

fn reasons<'a>(r: &'a Value, section: &str, index: usize) -> &'a Value {
    &r["sections"][section]["components"][index]["gap_reasons"]
}

#[test]
fn every_unverified_identity_and_missing_field_has_one_primary_reason() {
    let r = specs::normalize(
        "windows",
        &json!({
            "usb":[{"Name":"USB Input Device","Vendor":"1234","Product":"5678"}],
            "board":[{"Manufacturer":"Example","Product":"Example Board"}],
            "network":[{"InterfaceDescription":"Hyper-V Virtual Ethernet Adapter","Virtual":true,"HardwareInterface":false}]
        }),
    );
    let all: Vec<_> = r["sections"].as_object().unwrap().values().flat_map(|s| s["components"].as_array().unwrap()).collect();
    let gaps: Vec<_> = all.iter().flat_map(|c| c["gap_reasons"].as_array().unwrap()).collect();
    assert_eq!(gaps.iter().filter(|g| g["kind"] == "identity").count(), all.iter().filter(|c| c["official"]["status"] == "unverified").count());
    assert_eq!(gaps.iter().filter(|g| g["kind"] == "missing_field").count(), all.iter().map(|c| c["missing_fields"].as_array().unwrap().len()).sum::<usize>());
    assert_eq!(reasons(&r, "board", 0).as_array().unwrap().iter().find(|g| g["field"] == "version").unwrap()["code"], "firmware_not_reported");
    assert_eq!(reasons(&r, "network", 0)[0]["code"], "logical_interface_no_driver_record");
    assert_eq!(r["gap_summary"]["identity"], 2);
}

#[test]
fn ambiguity_internal_names_and_public_ids_have_distinct_explanations() {
    let r = specs::normalize(
        "windows",
        &json!({"display":[{"Name":"VG252Q"}],"memory":[{"PartNumber":"F4-3600C19-16GSXWB"}],"pci":[{"Name":"Example Function","PNPDeviceID":"PCI\\VEN_1234&DEV_5678\\PRIVATE"}]}),
    );
    assert_eq!(reasons(&r, "display", 0)[0]["code"], "variant_ambiguous");
    assert_eq!(reasons(&r, "memory", 0)[0]["code"], "variant_ambiguous");
    assert_eq!(reasons(&r, "pci", 0)[0]["code"], "public_id_not_catalogued");
    let m = specs::normalize("macos", &json!({"SPNVMeDataType":[{"device_model":"APPLE SSD AP0000Z","size_in_bytes":1000}]}));
    assert_eq!(reasons(&m, "storage", 0)[0]["code"], "manufacturer_internal_identifier");
}

#[test]
fn remote_reason_claims_are_rebuilt_and_driver_name_remains_public_metadata() {
    let mut r = specs::normalize("windows", &json!({"cpu":[{"Name":"Example CPU"}]}));
    let c = &mut r["sections"]["cpu"]["components"][0];
    c["gap_reasons"] = json!([{"code":"PRIVATE","detail":"PRIVATE","url":"https://evil.example"}]);
    c["fields"]["driver_name"] = json!("ExampleDriver");
    r["gap_summary"] = json!({"identity":0,"PRIVATE":999});
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert!(!safe.to_string().contains("PRIVATE"));
    assert!(!safe.to_string().contains("evil.example"));
    assert_eq!(safe["gap_summary"]["identity"], 1);
    assert_eq!(safe["sections"]["cpu"]["components"][0]["fields"]["driver_name"], "ExampleDriver");
}

#[test]
fn container_interfaces_and_indexed_document_bodies_keep_the_actual_limitation() {
    let r = specs::normalize(
        "windows",
        &json!({"network":[{"InterfaceDescription":"Hyper-V Virtual Ethernet Container Adapter","Virtual":true,"HardwareInterface":false}]}),
    );
    assert_eq!(reasons(&r, "network", 0)[0]["code"], "logical_interface_no_driver_record");
    for access in ["manufacturer_document_indexed_excerpt", "manufacturer_page_indexed_excerpt"] {
        let c = json!({"official":{"status":"matched","source_access":access},"missing_fields":[]});
        assert_eq!(specs::explain_gaps("windows", "usb", &c, &json!([]))[0]["code"], "body_access_limited");
    }
}
