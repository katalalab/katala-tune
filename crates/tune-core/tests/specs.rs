use serde_json::json;
use tune_core::{nodes::Node, specs};

#[test]
fn windows_modules_keep_configured_speed_separate_and_drop_identifiers() {
    let report = specs::normalize(
        "windows",
        &json!({"memory":[{
            "Manufacturer":"Example", "PartNumber":"DIMM-1", "Capacity":17179869184_u64,
            "Speed":6000,"ConfiguredClockSpeed":4800,"SerialNumber":"PRIVATE",
            "DeviceLocator":"DIMM A1","MACAddress":"PRIVATE"
        }]}),
    );
    let m = &report["sections"]["memory"]["components"][0];
    assert_eq!(m["fields"]["capacity_bytes"], 17179869184_u64);
    assert_eq!(m["fields"]["rated_speed_mts"], 6000);
    assert_eq!(m["fields"]["configured_speed_mts"], 4800);
    assert!(!report.to_string().contains("PRIVATE"));
    assert_eq!(report["sections"]["gpu"]["status"], "not_collected");
}

#[test]
fn mac_unified_memory_is_not_dedicated_vram_and_nested_devices_are_collected() {
    let r = specs::normalize(
        "macos",
        &json!({
            "SPHardwareDataType":[{"machine_model":"MacTest1,1","chip_type":"Apple Test","physical_memory":"32 GB","serial_number":"PRIVATE"}],
            "SPDisplaysDataType":[{"sppci_model":"Apple Test GPU","spdisplays_vram":"32 GB"}],
            "SPUSBDataType":[{"_name":"USB Bus", "_items":[{"_name":"Example Camera","vendor_id":"0x1234","product_id":"0x5678","serial_num":"PRIVATE"}]}]
        }),
    );
    assert_eq!(r["sections"]["gpu"]["components"][0]["fields"]["memory_kind"], "unified");
    assert!(r["sections"]["gpu"]["components"][0]["fields"]["dedicated_vram_bytes"].is_null());
    assert_eq!(r["sections"]["usb"]["components"].as_array().unwrap().len(), 2);
    assert!(!r.to_string().contains("PRIVATE"));
}

#[test]
fn failed_sources_and_empty_devices_are_distinct() {
    let r = specs::normalize("windows", &json!({"usb":[], "errors":{"memory":"permission_denied"}}));
    assert_eq!(r["sections"]["usb"]["status"], "empty");
    assert_eq!(r["sections"]["memory"]["status"], "permission_denied");
    assert_eq!(r["sections"]["storage"]["status"], "not_collected");
}

#[test]
fn official_catalog_requires_an_exact_model_match() {
    let matched = specs::official("NVIDIA GeForce RTX 4090");
    assert_eq!(matched["status"], "matched");
    assert_eq!(matched["specifications"]["dedicated_vram_gib"], 24);
    assert_eq!(specs::official("NVIDIA GeForce RTX 4090 D")["status"], "unverified");
    assert_eq!(specs::official("Example device")["status"], "unverified");
}

#[test]
fn remote_contract_is_validated_and_rebuilt_from_whitelisted_fields() {
    assert!(specs::validate_report(&json!({"schema":"wrong"}), "windows").is_err());
    let mut r = specs::normalize("windows", &json!({"cpu":[{"Name":"Example CPU", "NumberOfCores":8}]}));
    r["secret"] = json!("PRIVATE");
    r["sections"]["cpu"]["components"][0]["fields"]["serial"] = json!("PRIVATE");
    r["sections"]["cpu"]["components"][0]["official"] = json!({"url":"javascript:bad"});
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert!(!safe.to_string().contains("PRIVATE"));
    assert!(!safe.to_string().contains("javascript:"));
    assert!(specs::validate_report(&r, "macos").is_err());
}

#[test]
fn paused_shared_unknown_or_injected_targets_are_rejected() {
    let n = |v| Node::from_value(&v, "local-test");
    let nodes = vec![
        n(json!({"id":"a","alias":"example","os":"windows"})),
        n(json!({"id":"b","alias":"example","os":"macos","status":"paused"})),
        n(json!({"id":"c","alias":"example","os":"macos","shared":true})),
        n(json!({"id":"d","alias":"host; echo bad","os":"macos"})),
    ];
    assert_eq!(specs::select(&nodes, &["a".into()]).unwrap().len(), 1);
    for id in ["b", "c", "d", "missing"] {
        assert!(specs::select(&nodes, &[id.into()]).is_err(), "{id}");
    }
}

#[test]
fn failed_refresh_retains_last_success_and_tracks_attempt_separately() {
    use tune_core::{db::Store, engine_specs};
    let db = Store::open_in_memory().unwrap();
    let node = Node::from_value(&json!({"id":"a","alias":"example","os":"windows"}), "test");
    let report = specs::normalize("windows", &json!({"cpu":[{"Name":"Example CPU"}]}));
    engine_specs::save_attempt(&db, &node, &json!({"ok":true,"at":report["at"],"report":report})).unwrap();
    engine_specs::save_attempt(&db, &node, &json!({"ok":false,"at":report["at"],"error":"timeout"})).unwrap();
    let row = engine_specs::load_node(&db, &node).unwrap();
    assert_eq!(row["report"]["sections"]["cpu"]["components"][0]["fields"]["model"], "Example CPU");
    assert_eq!(row["last_attempt"]["ok"], false);
    assert_eq!(row["last_attempt"]["error"], "timeout");
    assert_eq!(row["stale"], true);
}

#[test]
fn mac_displays_are_not_gpus_and_super_cores_are_not_efficiency_cores() {
    let r = specs::normalize(
        "macos",
        &json!({
            "SPHardwareDataType":[{"chip_type":"Apple M5 Max","physical_memory":"64 GB"}],
            "hw.memsize":68719476736_u64,"hw.physicalcpu":18,"hw.logicalcpu":18,
            "hw.perflevel0.physicalcpu":6,"hw.perflevel1.physicalcpu":12,
            "hw.perflevel0.name":"Super","hw.perflevel1.name":"Performance",
            "SPDisplaysDataType":[{"sppci_model":"Apple M5 Max","spdisplays_ndrvs":[{"_name":"Color LCD","_spdisplays_resolution":"3456 x 2234"}]}]
        }),
    );
    assert_eq!(r["sections"]["gpu"]["components"].as_array().unwrap().len(), 1);
    assert_eq!(r["sections"]["display"]["components"][0]["fields"]["resolution"], "3456 x 2234");
    assert!(r["sections"]["cpu"]["components"][0]["fields"]["e_cores"].is_null());
    assert_eq!(r["sections"]["cpu"]["components"][0]["fields"]["cluster0_name"], "Super");
    assert_eq!(r["sections"]["memory"]["components"][0]["fields"]["capacity_bytes"], 68719476736_u64);
}

#[test]
fn nvidia_smi_provides_large_vram_without_cim_adapterram_guessing() {
    let r = specs::normalize(
        "windows",
        &json!({"gpu":[{"Name":"NVIDIA GeForce RTX 4090","AdapterRAM":4294967295_u64}],"nvidia":[{"model":"NVIDIA GeForce RTX 4090","dedicated_vram_bytes":25757220864_u64,"vbios_version":"EXAMPLE"}]}),
    );
    let gpu = &r["sections"]["gpu"]["components"][0];
    assert_eq!(gpu["fields"]["memory_kind"], "dedicated");
    assert_eq!(gpu["fields"]["dedicated_vram_bytes"], 25757220864_u64);
    assert!(gpu["source"].as_str().unwrap().contains("nvidia-smi"));
}
