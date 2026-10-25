use serde_json::json;
use tune_core::specs;

#[test]
fn unified_gpu_keeps_exact_product_verification() {
    let r = specs::normalize(
        "macos",
        &json!({
            "SPHardwareDataType":[{"chip_type":"Apple M2 Pro"}],
            "SPDisplaysDataType":[{"sppci_model":"Apple M2 Pro"}]
        }),
    );
    assert_eq!(r["sections"]["gpu"]["components"][0]["official"]["status"], "matched");
    assert_eq!(r["sections"]["memory"]["components"][0]["official"]["status"], "not_applicable");
}

#[test]
fn missing_fields_are_category_specific_and_documentation_is_not_a_product_match() {
    let r = specs::normalize(
        "windows",
        &json!({
            "cpu":[{"Name":"Example CPU","Manufacturer":"Example","NumberOfCores":8,"NumberOfLogicalProcessors":16}],
            "firmware":[{"SMBIOSBIOSVersion":"1.2"}],
            "os":[{"Caption":"Example OS","Version":"1","BuildNumber":"2","OSArchitecture":"x64"}],
            "settings":[{"SecureBoot":"permission_denied","PowerPlan":"balanced","XmpExpo":"not_exposed_by_standard_provider"}]
        }),
    );
    assert_eq!(r["sections"]["cpu"]["components"][0]["missing_fields"], json!([]));
    for id in ["os", "firmware", "settings"] {
        let s = &r["sections"][id];
        assert_eq!(s["components"][0]["official"]["status"], "not_applicable");
        assert!(!s["references"].as_array().unwrap().is_empty());
    }
    assert_eq!(r["sections"]["settings"]["components"][0]["fields"]["secure_boot"], "permission_denied");
}

#[test]
fn trusted_coverage_and_documentation_are_rebuilt_on_remote_validation() {
    let mut r = specs::normalize("macos", &json!({"SPHardwareDataType":[{"chip_type":"Apple Test"}]}));
    r["sections"]["settings"]["references"] = json!([{"url":"https://evil.example/"}]);
    r["sections"]["cpu"]["components"][0]["missing_fields"] = json!([]);
    let safe = specs::validate_report(&r, "macos").unwrap();
    assert!(!safe.to_string().contains("evil.example"));
    assert!(safe["catalog_version"].as_u64().is_some());
    assert!(safe["sections"]["cpu"]["components"][0]["missing_fields"].as_array().unwrap().contains(&json!("threads")));
}

#[test]
fn extended_safe_fields_survive_remote_validation_but_device_identifiers_do_not() {
    let mut r = specs::normalize("windows", &json!({"storage":[{"Model":"Example SSD"}]}));
    let f = &mut r["sections"]["storage"]["components"][0]["fields"];
    for (k, v) in [("trim_support", json!("Yes")), ("smart_status", json!("Verified")), ("sample_rate_hz", json!(48000)), ("filevault", json!("enabled"))] {
        f[k] = v;
    }
    f["serial_number"] = json!("PRIVATE");
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert_eq!(safe["sections"]["storage"]["components"][0]["fields"]["trim_support"], "Yes");
    assert_eq!(safe["sections"]["storage"]["components"][0]["fields"]["sample_rate_hz"], 48000);
    assert!(!safe.to_string().contains("PRIVATE"));
}
