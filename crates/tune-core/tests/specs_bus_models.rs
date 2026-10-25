use serde_json::{Value, json};
use tune_core::specs;

const ID: &str = "USB\\VID_F00D&PID_0001\\PRIVATE";

fn raw() -> Value {
    json!({"usb":[{"Name":"Generic USB Device","PNPDeviceID":ID}],
        "bus_models":[{"PNPDeviceID":ID,"BusReportedModel":"Razer Viper 8KHz"}]})
}

#[test]
fn exact_public_bus_model_preserves_os_name_and_is_independent_of_driver_queries() {
    let mut raw = raw();
    raw["errors"] = json!({"drivers":"error"});
    let report = specs::normalize("windows", &raw);
    let c = &report["sections"]["usb"]["components"][0];
    assert_eq!(c["fields"]["model"], "Generic USB Device");
    assert_eq!(c["fields"]["reported_model"], "Razer Viper 8KHz");
    assert_eq!(c["official"]["status"], "matched");
    assert_eq!(c["official"]["match_kind"], "bus_reported_model");
    assert!(c["source"].as_str().unwrap().contains("DEVPKEY_Device_BusReportedDeviceDesc"));
    assert!(!report.to_string().contains("PRIVATE"));
    let safe = specs::validate_report(&report, "windows").unwrap();
    assert_eq!(safe["sections"]["usb"]["components"][0]["official"], c["official"]);
}

#[test]
fn bus_models_require_unique_instance_association_and_an_exact_public_catalog_alias() {
    let mut wrong = raw();
    wrong["bus_models"][0]["PNPDeviceID"] = json!("USB\\VID_F00D&PID_0002\\PRIVATE");
    let mut duplicate = raw();
    duplicate["bus_models"].as_array_mut().unwrap().push(json!({"PNPDeviceID":ID,"BusReportedModel":"Razer Viper 8KHz"}));
    let mut repeated_device = raw();
    repeated_device["usb"].as_array_mut().unwrap().push(json!({"Name":"Generic USB Device","PNPDeviceID":ID}));
    let mut unknown = raw();
    unknown["bus_models"][0]["BusReportedModel"] = json!("PRIVATE unidentified serial-like product");
    for raw in [wrong, duplicate, repeated_device, unknown] {
        let report = specs::normalize("windows", &raw);
        for c in report["sections"]["usb"]["components"].as_array().unwrap() {
            assert!(c["fields"]["reported_model"].is_null());
            assert_eq!(c["official"]["status"], "unverified");
        }
        assert!(!report.to_string().contains("PRIVATE"));
    }
}

#[test]
fn internal_disks_do_not_inherit_a_usb_enclosure_model() {
    let mut raw = raw();
    raw["storage"] = json!([{"Model":"KURO Disk0 SCSI Disk Device","PNPDeviceID":ID,"Size":1000}]);
    let report = specs::normalize("windows", &raw);
    let disk = &report["sections"]["storage"]["components"][0];
    assert_eq!(disk["fields"]["model"], "KURO Disk0 SCSI Disk Device");
    assert!(disk["fields"]["reported_model"].is_null());
    assert_eq!(disk["official"]["status"], "unverified");
}

#[test]
fn remote_bus_model_values_are_public_catalog_literals_only() {
    let mut report = specs::normalize("windows", &json!({"usb":[{"Name":"Generic USB Device"}]}));
    report["sections"]["usb"]["components"][0]["fields"]["reported_model"] = json!("Razer Viper 8KHz");
    let safe = specs::validate_report(&report, "windows").unwrap();
    assert_eq!(safe["sections"]["usb"]["components"][0]["fields"]["reported_model"], "Razer Viper 8KHz");
    report["sections"]["usb"]["components"][0]["fields"]["reported_model"] = json!("PRIVATE unknown product");
    assert!(!specs::validate_report(&report, "windows").unwrap().to_string().contains("PRIVATE"));
}

#[test]
fn bus_provider_timeout_remains_visible_and_cannot_supply_a_model() {
    let mut raw = raw();
    raw["errors"] = json!({"bus_models":"timeout"});
    let report = specs::normalize("windows", &raw);
    let section = &report["sections"]["usb"];
    assert_eq!(section["status"], "partial");
    assert!(
        section["providers"].as_array().unwrap().iter().any(|p| p["status"] == "timeout" && p["source"].as_str().unwrap().contains("BusReportedDeviceDesc"))
    );
    assert!(section["components"][0]["fields"]["reported_model"].is_null());
}

#[test]
fn mac_usb_provider_records_a_usb_id_namespace_without_guessing_coreaudio_codec_ids() {
    for kind in ["SPUSBDataType", "SPUSBHostDataType"] {
        let r = specs::normalize(
            "macos",
            &json!({kind:[{"_name":"Example USB","vendor_id":"0x1234","product_id":"0x5678"}],"SPAudioDataType":[{"_name":"Example Audio","coreaudio_device_transport":"USB"}]}),
        );
        let safe = specs::validate_report(&r, "macos").unwrap();
        assert_eq!(safe["sections"]["usb"]["components"][0]["fields"]["hardware_id_bus"], "USB");
        assert!(safe["sections"]["audio"]["components"][0]["fields"]["hardware_id_bus"].is_null());
    }
}
