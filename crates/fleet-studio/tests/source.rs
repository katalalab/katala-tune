use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fleet_studio::{Observation, source};
use serde_json::json;
use tune_core::db::{DB_FILE, Store};

fn fixture_dir() -> PathBuf {
    let id = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("fixtures").join(format!("{}-{id}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn config(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("nodes.json");
    std::fs::write(&path, r#"{"nodes":[{"id":"fixture-node","alias":"fixture-node","os":"windows","role":"research"}]}"#).unwrap();
    path
}

#[test]
fn missing_database_keeps_ledger_nodes_unknown_without_creating_a_database() {
    let dir = fixture_dir();
    let config = config(&dir);
    let data = dir.join("missing-data");
    let dashboard = source::load(&config, &data, 10_000);
    assert_eq!(dashboard.nodes.len(), 1);
    assert_eq!(dashboard.nodes[0].observation, Observation::Unknown);
    assert!(dashboard.database_error.is_some());
    assert!(!data.exists());
}

#[test]
fn newer_saved_failure_is_visible_but_retains_last_success_time() {
    let dir = fixture_dir();
    let config = config(&dir);
    let data = dir.join("data");
    let db = Store::open(&data).unwrap();
    db.add_snapshot("fixture-node", 9_000, None, None, &json!([]), &json!({}), &json!({})).unwrap();
    db.save_probe_failure("fixture-node", 9_500, "fixture failure").unwrap();
    drop(db);
    let before = std::fs::read(data.join(DB_FILE)).unwrap();
    let dashboard = source::load(&config, &data, 10_000);
    assert_eq!(dashboard.nodes[0].observation, Observation::Failed);
    assert_eq!(dashboard.nodes[0].last_at, Some(9_000));
    assert_eq!(dashboard.nodes[0].failure_reason, Some("詳細不明（Katala Tune で確認）"));
    assert_eq!(std::fs::read(data.join(DB_FILE)).unwrap(), before);
}

#[test]
fn missing_configuration_is_not_an_empty_healthy_fleet() {
    let dir = fixture_dir();
    let dashboard = source::load(&dir.join("missing.json"), &dir.join("missing-data"), 10_000);
    assert!(dashboard.config_error.is_some());
    assert!(dashboard.nodes.is_empty());
}

#[test]
fn excluded_machine_keeps_its_last_observation_without_becoming_online() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-node","alias":"fixture-node","os":"windows","status":"retired"}]}"#).unwrap();
    let data = dir.join("data");
    let db = Store::open(&data).unwrap();
    db.add_snapshot("fixture-node", 9_000, None, None, &json!([]), &json!({}), &json!({})).unwrap();
    drop(db);
    let dashboard = source::load(&config, &data, 10_000);
    assert_eq!(dashboard.nodes[0].observation, Observation::Excluded);
    assert_eq!(dashboard.nodes[0].last_at, Some(9_000));
}

#[test]
fn local_identity_comes_from_the_actual_hostname_not_the_role_label() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    let host = tune_core::nodes::local_host();
    let data = json!({"nodes":[
        {"id":"fixture-local","alias":"fixture-local","os":"windows","local_hostname":host},
        {"id":"fixture-other","alias":"fixture-other","os":"windows","role":"この機体","local_hostname":"different-fixture-host"}
    ]});
    std::fs::write(&config, data.to_string()).unwrap();
    let dashboard = source::load(&config, &dir.join("missing"), 10_000);
    assert!(dashboard.nodes[0].local);
    assert!(!dashboard.nodes[1].local);
}

#[test]
fn failed_refresh_keeps_previous_data_but_changes_its_status_to_unknown() {
    let previous = source::Dashboard {
        nodes: vec![source::NodeCard {
            id: "fixture-node".into(),
            role: "test".into(),
            operating_state: "online".into(),
            local: true,
            last_at: Some(100),
            observation: Observation::Current,
            failure_reason: None,
            max_age_ms: Some(1_000),
            score: None,
            critical: None,
            warnings: None,
        }],
        config_error: None,
        database_error: None,
        repositories: vec![source::Repository { name: "fixture-repo".into(), origin: None, observed_at: Some(100), previous: false }],
        topology_coverage: source::Coverage::Complete,
        topology_error: None,
        topology_oldest_at: Some(100),
        topology_limited: false,
    };
    let mut next = source::Dashboard {
        nodes: vec![source::NodeCard { last_at: None, observation: Observation::Unknown, ..previous.nodes[0].clone() }],
        config_error: None,
        database_error: Some("unavailable".into()),
        repositories: vec![],
        topology_coverage: source::Coverage::Unknown,
        topology_error: Some("unavailable".into()),
        topology_oldest_at: None,
        topology_limited: false,
    };
    source::preserve_previous(&previous, &mut next);
    assert_eq!(next.nodes[0].last_at, Some(100));
    assert_eq!(next.nodes[0].observation, Observation::Unknown);
    assert_eq!(next.repositories[0].name, "fixture-repo");
    assert!(next.repositories[0].previous);
    assert_eq!(next.topology_coverage, source::Coverage::Unknown);
    assert!(next.database_error.is_some());
}

#[test]
fn probe_errors_are_classified_without_exposing_raw_output() {
    assert_eq!(source::failure_reason("ssh: Permission denied (publickey)"), "SSH 認証・権限");
    assert_eq!(source::failure_reason("Host key verification failed"), "ホスト鍵");
    assert_eq!(source::failure_reason("arbitrary private detail"), "詳細不明（Katala Tune で確認）");
}

#[test]
fn freshness_uses_ledger_interval_then_database_override() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-node","alias":"fixture-node","os":"windows"}],"schedule":{"probe_minutes":5}}"#).unwrap();
    let data = dir.join("data");
    let db = Store::open(&data).unwrap();
    db.add_snapshot("fixture-node", 1_000_000, None, None, &json!([]), &json!({}), &json!({})).unwrap();
    drop(db);
    assert_eq!(source::load(&config, &data, 2_200_000).nodes[0].observation, Observation::Stale);
    let db = Store::open(&data).unwrap();
    db.set_meta("schedule", &json!({"probe_minutes":60})).unwrap();
    drop(db);
    assert_eq!(source::load(&config, &data, 2_200_000).nodes[0].observation, Observation::Current);
}

#[test]
fn displayed_snapshot_becomes_stale_after_its_deadline() {
    let card = source::NodeCard {
        id: "fixture".into(),
        role: "test".into(),
        operating_state: "online".into(),
        local: true,
        last_at: Some(9_000),
        observation: Observation::Current,
        failure_reason: None,
        max_age_ms: Some(1_000),
        score: None,
        critical: None,
        warnings: None,
    };
    assert_eq!(card.current_observation(10_000), Observation::Current);
    assert_eq!(card.current_observation(10_001), Observation::Stale);
}

#[test]
fn saved_health_counts_match_the_success_snapshot() {
    let dir = fixture_dir();
    let config = config(&dir);
    let data = dir.join("data");
    let db = Store::open(&data).unwrap();
    db.add_snapshot("fixture-node", 9_000, None, Some(72), &json!([]), &json!({}), &json!({"critical":2,"warn":3})).unwrap();
    drop(db);
    let dashboard = source::load(&config, &data, 10_000);
    assert_eq!((dashboard.nodes[0].score, dashboard.nodes[0].critical, dashboard.nodes[0].warnings), (Some(72), Some(2), Some(3)));
}

#[test]
fn topology_search_does_not_create_missing_database_or_accept_unbounded_input() {
    let dir = fixture_dir();
    let data = dir.join("missing-data");
    assert!(source::search_topology(&data, "fixture", 10_000).is_err());
    assert!(!data.exists());
    assert!(source::search_topology(&data, &"x".repeat(257), 10_000).is_err());
}
