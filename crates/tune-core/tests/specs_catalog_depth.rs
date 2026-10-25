use serde_json::Value;
use std::collections::HashSet;

fn catalog() -> Value {
    serde_json::from_str(include_str!("../../../data/specs-catalog.json")).unwrap()
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[test]
fn full_body_evidence_is_auditable_and_distinct_from_indexed_excerpts() {
    let catalog = catalog();
    let products = catalog["products"].as_array().unwrap();
    for product in products {
        assert!(product["url"].as_str().unwrap().starts_with("https://"));
        assert!(!product["scope"].as_str().unwrap().is_empty());
        let access = product["source_access"].as_str().unwrap_or("");
        if access.ends_with("_full_body") {
            let evidence = &product["source_evidence"];
            assert_eq!(evidence["url"], product["url"]);
            let hash = evidence["sha256"].as_str().unwrap();
            assert!(hex(hash, 64));
            assert_eq!(hash, hash.to_ascii_lowercase());
            let date = evidence["verified_at"].as_str().unwrap();
            assert_eq!(date.len(), 10);
            assert!(date <= catalog["verified_at"].as_str().unwrap());
            assert!(!evidence["notes"].as_str().unwrap().is_empty());
            match access {
                "manufacturer_document_full_body" => {
                    assert_eq!(evidence["coverage"], "full_document");
                    let locations = if evidence["material"] == "text_document" { &evidence["lines"] } else { &evidence["pages"] };
                    let locations = locations.as_array().unwrap();
                    assert!(!locations.is_empty());
                    assert!(locations.iter().all(|position| position.as_u64().unwrap() > 0));
                }
                "manufacturer_page_full_body" => {
                    assert_eq!(evidence["coverage"], "full_page");
                }
                "manufacturer_driver_package_full_body" => {
                    assert_eq!(evidence["coverage"], "full_archive");
                    assert!(evidence["notes"].as_str().unwrap().contains("without"));
                    assert!(evidence["notes"].as_str().unwrap().contains("SHA256"));
                }
                "os_driver_source_full_body" => {
                    assert_eq!(evidence["coverage"], "full_source_file");
                    assert_eq!(evidence["material"], "source_code");
                    let lines = evidence["lines"].as_array().unwrap();
                    assert!(!lines.is_empty());
                    assert!(lines.iter().all(|line| line.as_u64().unwrap() > 0));
                    assert!(evidence["url"].as_str().unwrap().starts_with("https://raw.githubusercontent.com/torvalds/linux/v"));
                }
                other => panic!("unrecognized full-body access: {other}"),
            }
        }
    }
    let eizo = products.iter().find(|p| p["name"] == "EIZO FlexScan EV2750 monitor").unwrap();
    assert_eq!(eizo["source_access"], "manufacturer_page_full_body");
    assert_eq!(eizo["specifications"]["native_resolution"], "2560 × 1440");
}

#[test]
fn hardware_id_records_have_exact_unique_section_scoped_keys() {
    let catalog = catalog();
    let mut keys = HashSet::new();
    let mut hardware_records = 0;
    for product in catalog["products"].as_array().unwrap() {
        let Some(ids) = product.get("hardware_ids") else { continue };
        hardware_records += 1;
        assert!(product["models"].as_array().unwrap().is_empty(), "ID mappings must not match generic model names");
        assert!(!ids.as_array().unwrap().is_empty());
        for id in ids.as_array().unwrap() {
            let section = id["section"].as_str().unwrap();
            assert!(["pci", "usb", "network", "audio"].contains(&section));
            let vendor = id["vendor_id"].as_str().unwrap();
            let device = id.get("device_id").or_else(|| id.get("product_id")).unwrap().as_str().unwrap();
            assert!(hex(vendor, 4) && hex(device, 4));
            assert_eq!(vendor, vendor.to_ascii_uppercase());
            assert_eq!(device, device.to_ascii_uppercase());
            assert!(keys.insert((section.to_string(), vendor.to_string(), device.to_string())));
        }
        let specs = product["specifications"].as_object().unwrap();
        assert!(
            specs.keys().all(|key| ["silicon_family", "controller_function", "pci_device_function"].contains(&key.as_str())),
            "ID records describe family/function only"
        );
        for unsupported in ["stepping", "board_revision", "motherboard_sku", "negotiated_link_gbps"] {
            assert!(!specs.contains_key(unsupported), "function identity cannot prove {unsupported}");
        }
    }
    assert!(hardware_records > 0, "expected verified manufacturer ID mappings");
}

#[test]
fn ambiguous_product_labels_and_kit_inference_remain_unmatched() {
    let catalog = catalog();
    for product in catalog["products"].as_array().unwrap() {
        for alias in product["models"].as_array().unwrap() {
            assert!(!["VG252Q", "F4-3600C19-16GSXWB", "3000 C16 Series", "USB Input Device", "PCI Express Root Port"].contains(&alias.as_str().unwrap()));
        }
    }
}
