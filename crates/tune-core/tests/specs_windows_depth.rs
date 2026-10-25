use serde_json::{Value, json};
use tune_core::specs;

fn fields<'a>(report: &'a Value, section: &str, index: usize) -> &'a Value {
    &report["sections"][section]["components"][index]["fields"]
}

#[test]
fn duplicate_device_names_join_only_by_unique_private_instance_ids() {
    let r = specs::normalize(
        "windows",
        &json!({
            "usb":[
                {"Name":"Same hub","PNPDeviceID":"USB\\VID_1234&PID_5678\\PRIVATE-A"},
                {"Name":"Same hub","PNPDeviceID":"USB\\VID_1234&PID_5678\\PRIVATE-B"}],
            "drivers":[
                {"DeviceName":"Same hub","DeviceID":"usb\\vid_1234&pid_5678\\private-a","DriverVersion":"1","DriverProviderName":"Example","InfName":"oem42.inf","IsSigned":true},
                {"DeviceName":"Same hub","DeviceID":"USB\\VID_1234&PID_5678\\PRIVATE-B","DriverVersion":"2","IsSigned":false}]
        }),
    );
    let a = fields(&r, "usb", 0);
    assert_eq!(a["driver_version"], "1");
    assert_eq!(a["driver_provider"], "Example");
    assert_eq!(a["driver_inf"], "oem42.inf");
    assert_eq!(a["driver_signed"], true);
    assert_eq!(fields(&r, "usb", 1)["driver_version"], "2");
    assert_eq!(fields(&r, "usb", 1)["driver_signed"], false);
    assert!(!r.to_string().to_ascii_uppercase().contains("PRIVATE"));
    let safe = specs::validate_report(&r, "windows").unwrap();
    assert_eq!(fields(&safe, "usb", 0)["driver_inf"], "oem42.inf");
}

#[test]
fn conflicting_missing_or_duplicate_id_joins_never_fall_back_to_names() {
    for raw in [
        json!({"audio":[{"Name":"Same","PNPDeviceID":"PRIVATE-A"}],"drivers":[{"DeviceName":"Same","DeviceID":"PRIVATE-B","DriverVersion":"wrong"}]}),
        json!({"audio":[{"Name":"Same","PNPDeviceID":"PRIVATE-A"}],"drivers":[{"DeviceID":"PRIVATE-A","DriverVersion":"a"},{"DeviceName":"Same","DeviceID":"private-a","DriverVersion":"b"}]}),
        json!({"audio":[{"Name":"Same","PNPDeviceID":"PRIVATE-A"},{"Name":"Other","PNPDeviceID":"private-a"}],"drivers":[{"DeviceName":"Same","DeviceID":"PRIVATE-A","DriverVersion":"wrong"}]}),
    ] {
        let r = specs::normalize("windows", &raw);
        assert!(fields(&r, "audio", 0)["driver_version"].is_null());
        assert!(!r.to_string().contains("PRIVATE"));
    }
}

#[test]
fn native_component_classes_share_exact_pnp_and_driver_identity_without_overwriting_fields() {
    for (section, model_key) in [("gpu", "Name"), ("storage", "Model"), ("audio", "Name"), ("network", "InterfaceDescription")] {
        let mut row = json!({"PNPDeviceID":"PCI\\VEN_1234&DEV_5678&SUBSYS_ABCD4321&REV_0F\\PRIVATE","DriverVersion":"native","DriverVersionString":"native","DriverProvider":"native provider","Status":"OK"});
        row[model_key] = json!("Example device");
        let mut raw = json!({"pnp":[{"PNPDeviceID":row["PNPDeviceID"],"Present":false,"ConfigManagerErrorCode":22,"Status":"Degraded"}],
            "drivers":[{"DeviceID":row["PNPDeviceID"],"DeviceName":"Different label","DriverVersion":"supplemental","DriverProviderName":"supplemental provider","DriverDate":"2026-01-01","InfName":"native.inf"}]});
        raw[section] = json!([row]);
        let r = specs::normalize("windows", &raw);
        let f = fields(&r, section, 0);
        assert_eq!(f["driver_inf"], "native.inf", "{section}");
        if matches!(section, "gpu" | "network") {
            assert_eq!(f["driver_version"], "native");
        }
        if section == "network" {
            assert_eq!(f["driver_provider"], "native provider");
        }
        assert_eq!(f["present"], false);
        assert_eq!(f["problem_code"], 22);
        if section != "network" {
            assert_eq!(f["status"], "OK");
        }
        assert_eq!(f["vendor_id"], "1234");
        assert_eq!(f["product_id"], "5678");
        assert_eq!(f["device_id"], "5678");
        assert_eq!(f["subsystem_device_id"], "ABCD");
        assert_eq!(f["subsystem_vendor_id"], "4321");
        assert_eq!(f["revision_id"], "0F");
        assert!(!r.to_string().contains("PRIVATE"));
    }
}

#[test]
fn public_ids_are_strict_bus_hex_tokens_and_usb_revision_uses_compatible_hardware_id() {
    let r = specs::normalize(
        "windows",
        &json!({"usb":[{"Name":"Example","PNPDeviceID":"USB\\VID_1234&PID_5678\\PRIVATE"}],
        "pnp":[{"PNPDeviceID":"USB\\VID_1234&PID_5678\\PRIVATE","HardwareID":["USB\\VID_1234&PID_5678&REV_0102","USB\\VID_1234&PID_5678"]}]}),
    );
    assert_eq!(fields(&r, "usb", 0)["revision_id"], "0102");
    assert!(fields(&r, "usb", 0)["device_id"].is_null());
    for id in ["ROOT\\VID_1234&PID_5678\\PRIVATE", "USB\\VID_12345&PID_5678\\PRIVATE", "USB\\VID_1234&PID_56789\\PRIVATE", "PCI\\VEN_1234&DEV_G678\\PRIVATE"] {
        let r = specs::normalize("windows", &json!({"usb":[{"Name":"Example","PNPDeviceID":id}]}));
        assert!(fields(&r, "usb", 0)["vendor_id"].is_null(), "{id}");
        assert!(fields(&r, "usb", 0)["product_id"].is_null());
    }
    let conflicting = specs::normalize(
        "windows",
        &json!({"usb":[{"Name":"Example","PNPDeviceID":"USB\\VID_1234&PID_5678\\PRIVATE"}],
        "pnp":[{"PNPDeviceID":"USB\\VID_1234&PID_5678\\PRIVATE","HardwareID":["USB\\VID_9999&PID_5678&REV_0102"]}]}),
    );
    assert!(fields(&conflicting, "usb", 0)["revision_id"].is_null());
}

#[test]
fn driver_metadata_and_pnp_scalars_reject_paths_types_and_failed_providers() {
    let r = specs::normalize(
        "windows",
        &json!({"audio":[{"Name":"Example","PNPDeviceID":"PRIVATE"}],
        "pnp":[{"PNPDeviceID":"PRIVATE","Present":"false","ConfigManagerErrorCode":-1}],
        "drivers":[{"DeviceID":"PRIVATE","InfName":"C:\\PRIVATE\\oem1.inf","IsSigned":"true","DriverProviderName":"Example"}]}),
    );
    let f = fields(&r, "audio", 0);
    assert!(f["driver_inf"].is_null());
    assert!(f["driver_signed"].is_null());
    assert!(f["present"].is_null());
    assert!(f["problem_code"].is_null());
    assert!(!r.to_string().contains("PRIVATE"));
    let denied = specs::normalize(
        "windows",
        &json!({"audio":[{"Name":"Example","PNPDeviceID":"PRIVATE"}],
        "drivers":[{"DeviceID":"PRIVATE","DriverVersion":"invalid"}],"errors":{"drivers":"permission_denied"}}),
    );
    assert!(fields(&denied, "audio", 0)["driver_version"].is_null());
    assert_eq!(denied["sections"]["audio"]["status"], "partial");
    assert!(
        denied["sections"]["audio"]["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["source"] == "Win32_PnPSignedDriver" && p["status"] == "permission_denied")
    );
}
