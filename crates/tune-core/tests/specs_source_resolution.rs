use serde_json::{Value, json};

fn catalog() -> Value {
    serde_json::from_str(include_str!("../../../data/specs-catalog.json")).unwrap()
}

#[test]
fn crucial_exact_skus_have_complete_pdf_evidence_instead_of_indexed_access() {
    let catalog = catalog();
    let products = catalog["products"].as_array().unwrap();
    for sku in ["CT2000T500SSD8", "CT1000MX500SSD1", "CT4000MX500SSD1", "CT2000BX500SSD1", "CT1000P3PSSD8"] {
        let product = products.iter().find(|p| p["models"] == json!([sku])).unwrap();
        assert_eq!(product["source_access"], "manufacturer_document_full_body", "{sku}");
        let evidence = &product["source_evidence"];
        assert_eq!(evidence["url"], product["url"]);
        assert!(evidence["url"].as_str().unwrap().starts_with("https://assets.micron.com/adobe/assets/"));
        assert_eq!(evidence["coverage"], "full_document");
        assert_eq!(evidence["material"], "pdf_document");
        assert_eq!(evidence["pages"], json!([2]));
        assert_eq!(evidence["sha256"].as_str().unwrap().len(), 64);
        assert!(!product["scope"].as_str().unwrap().contains("fetch was"));
    }
}

#[test]
fn rendered_razer_evidence_is_labelled_and_manual_claims_have_explicit_scope() {
    let catalog = catalog();
    let product = catalog["products"].as_array().unwrap().iter().find(|p| p["models"] == json!(["Razer Viper 8KHz"])).unwrap();
    assert_eq!(product["source_access"], "manufacturer_page_full_body");
    let evidence = &product["source_evidence"];
    assert_eq!(evidence["coverage"], "full_page");
    assert_eq!(evidence["material"], "rendered_dom_text");
    assert!(evidence["notes"].as_str().unwrap().contains("not the raw HTTP HTML"));
    let manual = &product["supporting_source_evidence"][0];
    assert_eq!(manual["coverage"], "full_document");
    assert_eq!(manual["material"], "pdf_document");
    assert_eq!(manual["pages"], json!([7]));
    assert_eq!(manual["claim_scope"].as_array().unwrap().len(), 4);
    for claim in manual["claim_scope"].as_array().unwrap() {
        assert!(product["specifications"].get(claim.as_str().unwrap()).is_some());
    }
    for item in [evidence, manual] {
        assert!(item["url"].as_str().unwrap().starts_with("https://"));
        assert_eq!(item["sha256"].as_str().unwrap().len(), 64);
        assert!(!item.to_string().contains("localhost"));
        assert!(!item.to_string().contains("file:"));
    }
}
