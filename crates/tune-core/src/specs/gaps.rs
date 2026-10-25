use serde_json::{Map, Value, json};

fn identity(section: &str, c: &Value) -> (&'static str, &'static str) {
    let f = &c["fields"];
    let name = f["model"].as_str().or_else(|| c["name"].as_str()).unwrap_or("");
    if ["VG252Q", "F4-3600C19-16GSXWB", "3000 C16 Series", "OWC USB-C Dock", "iLok", "LIGHTSPEED Receiver", "Mi TV"].contains(&name) {
        return ("variant_ambiguous", "取得名だけでは製品の世代・派生型番を一意に特定できません。正確なラベルや追加の公開IDが必要です。");
    }
    if f["record_kind"] == "software"
        || name.to_ascii_lowercase().contains("virtual")
        || name.starts_with("Steam Streaming")
        || matches!(name, "Parsec VDA" | "ParsecVDA")
    {
        return ("software_endpoint", "ソフトウェアが公開する機能・端点です。この表示名から物理製品の型番は確定できません。");
    }
    if section == "storage" && name.starts_with("APPLE SSD AP") {
        return ("manufacturer_internal_identifier", "OSが公開するメーカー内部の識別名です。公開資料で販売型番との対応を確認できていません。");
    }
    if f["class"] == "storage_controller"
        || name.to_ascii_lowercase().contains("root hub")
        || name.starts_with("thunderboltusb4_bus_")
        || (name.starts_with("USB") && name.ends_with("Bus"))
    {
        return ("infrastructure_function", "バス・ハブ・コントローラーの機能単位です。完成品型番との対応には追加の公式識別資料が必要です。");
    }
    if name == "Color LCD" || name.starts_with("MacBook Pro") || ["NVIDIA High Definition Audio", "Realtek High Definition Audio"].contains(&name) {
        return ("integrated_component", "組み込み機能の表示名です。搭載部品の製造元・詳細型番との公式な対応は確認できていません。");
    }
    if f["vendor_id"].is_string() && (f["device_id"].is_string() || f["product_id"].is_string()) {
        return ("public_id_not_catalogued", "公開ハードウェアIDは取得済みですが、現在のカタログに該当する公式資料との対応がありません。");
    }
    if [
        "Display",
        "WiFi adapter",
        "Wi-Fi adapter",
        "System Product Name",
        "Generic MassStorageClass USB Device",
        "Blackmagic Design",
        "KURO Disk0 SCSI Disk Device",
        "KURO Disk1 SCSI Disk Device",
    ]
    .contains(&name)
    {
        return ("generic_name", "取得名が汎用名またはブランド名のため、正確な型番を特定できません。");
    }
    ("model_not_catalogued", "取得した型番の完全一致を、現在の公式資料カタログで確認できていません。類似名による推定は行いません。")
}

fn missing(platform: &str, section: &str, c: &Value, field: &str, providers: &Value) -> (&'static str, &'static str) {
    if section == "network" && field == "driver_version" && c["fields"]["record_kind"] == "software" {
        if platform == "windows" && c["name"].as_str().is_some_and(|n| n.starts_with("Hyper-V Virtual Ethernet ")) {
            return (
                "logical_interface_no_driver_record",
                "論理インターフェースです。調査したOSのネットワーク・PnP・ドライバー記録に、この端点と一意に対応する版の情報がありません。",
            );
        }
        return (
            "shared_interface_driver_unresolved",
            "論理インターフェースに一意に対応するドライバー版を取得できていません。物理インターフェースの版は転用しません。",
        );
    }
    if section == "board" && field == "version" {
        return ("firmware_not_reported", "ファームウェアの基板情報に版が報告されていません。公式製品仕様から搭載基板のPCB版は補完できません。");
    }
    if platform == "macos" && section == "display" && field == "manufacturer" {
        return ("panel_vendor_not_exposed", "取得できた表示情報にはパネル製造元がありません。製品ブランドや表示用IDからパネル製造元を推定しません。");
    }
    if providers.as_array().into_iter().flatten().any(|p| p["status"] == "permission_denied") {
        return ("provider_permission_denied", "関連する取得元に権限不足があります。この項目が取得できるかは未確定です。");
    }
    if providers
        .as_array()
        .into_iter()
        .flatten()
        .any(|p| matches!(p["status"].as_str(), Some("partial" | "error" | "timeout" | "tool_missing" | "output_limit")))
    {
        return ("provider_incomplete", "関連する取得元に未完了・取得制限があります。この項目が公開されているかは未確定です。");
    }
    ("provider_not_exposed", "使用した取得元の応答に、この項目の値がありません。未対応・非公開・機器側未報告の区別は未確定です。")
}

pub(super) fn explain(platform: &str, section: &str, c: &Value, providers: &Value) -> Value {
    let mut result = Vec::new();
    if c["official"]["status"] == "unverified" {
        let (code, detail) = identity(section, c);
        result.push(json!({"kind":"identity","code":code,"detail":detail}));
    }
    for field in c["missing_fields"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|f| super::FIELDS.contains(f)) {
        let (code, detail) = missing(platform, section, c, field, providers);
        result.push(json!({"kind":"missing_field","field":field,"code":code,"detail":detail}));
    }
    if matches!(c["official"]["status"].as_str(), Some("matched" | "identified"))
        && !c["official"]["source_access"].as_str().is_some_and(|s| s.ends_with("_full_body"))
    {
        let (code, detail) = if c["official"]["source_access"].as_str().is_some_and(|s| s.ends_with("indexed_excerpt")) {
            ("body_access_limited", "公式資料の検索抜粋での確認です。本文取得に制限があり、全文の照合には至っていません。")
        } else {
            ("body_not_recorded", "この対応には本文取得の証跡が登録されていません。本文の取得失敗を意味するものではありません。")
        };
        result.push(json!({"kind":"source_body","code":code,"detail":detail}));
    }
    json!(result)
}

pub(super) fn summary(sections: &Map<String, Value>) -> Value {
    let mut counts = Map::new();
    for kind in ["identity", "missing_field", "source_body"] {
        let count = sections
            .values()
            .flat_map(|s| s["components"].as_array().into_iter().flatten())
            .flat_map(|c| c["gap_reasons"].as_array().into_iter().flatten())
            .filter(|r| r["kind"] == kind)
            .count();
        counts.insert(kind.into(), json!(count));
    }
    json!(counts)
}
