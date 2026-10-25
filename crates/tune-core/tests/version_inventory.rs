use serde_json::json;
use tune_core::{nodes::Node, version_inventory as versions};

#[test]
fn local_version_has_schema_platform_and_real_binary_hash() {
    let v = versions::local();
    assert_eq!(v["schema"], "katala_cli_version.v1");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["platform"], std::env::consts::OS);
    assert_eq!(v["architecture"], std::env::consts::ARCH);
    assert_eq!(v["specs_schema"], "katala_specs.v1");
    let hash = v["executable_sha256"].as_str().unwrap();
    assert_eq!(hash.len(), 64);
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn remote_version_requires_matching_platform_schema_and_a_complete_hash() {
    let v = json!({"schema":"katala_cli_version.v1","version":"0.6.5-specs.2","platform":"macos","architecture":"aarch64","specs_schema":"katala_specs.v1","executable_sha256":"a".repeat(64),"private":"PRIVATE"});
    assert!(versions::validate(&v, "windows").is_err());
    let clean = versions::validate(&v, "macos").unwrap();
    assert!(!clean.to_string().contains("PRIVATE"));
    let mut broken = v.clone();
    broken["executable_sha256"] = json!("abc");
    assert!(versions::validate(&broken, "macos").is_err());
    broken = v.clone();
    broken["schema"] = json!("wrong");
    assert!(versions::validate(&broken, "macos").is_err());
}

#[tokio::test]
async fn excluded_machine_is_refused_before_any_ssh_program_is_run() {
    let node = Node::from_value(&json!({"id":"excluded","alias":"invalid; command","os":"windows","status":"retired"}), "none");
    let report = versions::audit(vec![node]).await;
    assert_eq!(report["nodes"][0]["ok"], false);
    assert_eq!(report["nodes"][0]["refused"], true);
    assert_eq!(report["counts"]["failed"], 1);
}
