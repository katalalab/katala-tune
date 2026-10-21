use tune_core::db::Store;

#[test]
fn lifecycle_schema_is_separate_from_logs_and_metrics() {
    let db = Store::open_in_memory().unwrap();
    let exists: bool = db.conn().prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='monitor_events'").unwrap().exists([]).unwrap();
    assert!(exists, "dedicated lifecycle history is missing");
    for table in ["logs", "metrics_minute", "source_span"] {
        assert!(db.conn().prepare("SELECT 1 FROM sqlite_master WHERE name=?").unwrap().exists([table]).unwrap());
    }
}

#[test]
fn lifecycle_query_accepts_only_typed_correlation_and_kind_filters() {
    let f = serde_json::from_value::<tune_core::monitor_journal::Filter>(serde_json::json!({
        "kind":"end", "collector_id":"fixture", "generation":2, "attempt":3, "limit":500
    }));
    assert!(f.is_ok(), "typed lifecycle correlation filter missing");
    for bad in [
        serde_json::json!({"kind":"free text"}),
        serde_json::json!({"attempt":true}),
        serde_json::json!({"limit":2.5}),
        serde_json::json!({"argv":"dummy-private-canary"}),
    ] {
        assert!(serde_json::from_value::<tune_core::monitor_journal::Filter>(bad).is_err());
    }
}
