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
    assert_eq!(dashboard.nodes[0].local, Some(true));
    assert_eq!(dashboard.nodes[1].local, Some(false));
}

#[test]
fn machine_cards_do_not_claim_ambiguous_or_invalid_local_identity() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    std::fs::write(
        &config,
        json!({"nodes":[
            {"id":"fixture-a","local_hostname":"fixture-host"},
            {"id":"fixture-b","local_hostname":"FIXTURE-HOST.local","shared":true},
            {"id":"fixture-other","local_hostname":"fixture-other"},
            {"id":"fixture-unknown","local_hostname":null}
        ]})
        .to_string(),
    )
    .unwrap();
    let dashboard = source::load_for_host(&config, &dir.join("missing"), 10_000, "fixture-host");
    assert_eq!(dashboard.nodes.iter().map(|node| node.local).collect::<Vec<_>>(), [None, None, Some(false), None]);
    assert_eq!(dashboard.nodes[0].identity_text(), "同一性不明");
    assert!(source::load_for_host(&config, &dir.join("missing"), 10_000, "").nodes.iter().all(|node| node.local.is_none()));
    std::fs::write(&config, json!({"nodes":[{"id":"fixture-a","local_hostname":"FIXTURE-HOST.local"}]}).to_string()).unwrap();
    assert_eq!(source::load_for_host(&config, &dir.join("missing"), 10_000, "fixture-host").nodes[0].local, Some(true));
}

#[test]
fn unavailable_repository_coverage_is_not_reported_as_an_empty_result() {
    use source::Coverage;
    assert_eq!(source::empty_repository_text(Coverage::Unknown, false), "保存済みリポジトリの有無は未確認です。");
    assert_eq!(source::empty_repository_text(Coverage::Complete, true), "保存済みリポジトリの有無は未確認です。");
    assert_eq!(source::empty_repository_text(Coverage::Complete, false), "表示範囲に該当するリポジトリはありません。");
}

#[test]
fn failed_refresh_keeps_previous_data_but_changes_its_status_to_unknown() {
    let previous = source::Dashboard {
        nodes: vec![source::NodeCard {
            id: "fixture-node".into(),
            role: "test".into(),
            operating_state: "online".into(),
            local: Some(true),
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
    assert!(next.repositories.is_empty());
    assert_eq!(next.topology_coverage, source::Coverage::Unknown);
    assert!(next.database_error.is_some());
    let mut missing_config = source::Dashboard { nodes: Vec::new(), config_error: Some("unavailable".into()), ..next };
    source::preserve_previous(&previous, &mut missing_config);
    assert_eq!(missing_config.nodes[0].identity_text(), "同一性不明");
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
fn corrupted_saved_schedule_keeps_machine_freshness_unknown() {
    let dir = fixture_dir();
    let config = config(&dir);
    let data = dir.join("data");
    let db = Store::open(&data).unwrap();
    assert!(db.get_meta_strict("schedule").unwrap().is_none());
    db.add_snapshot("fixture-node", 9_000, None, None, &json!([]), &json!({}), &json!({})).unwrap();
    db.conn().execute_batch("INSERT INTO meta(k,v,updated_at) VALUES ('schedule','not-json',1)").unwrap();
    assert!(db.get_meta_strict("schedule").is_err());
    drop(db);
    let dashboard = source::load(&config, &data, 10_000);
    assert_eq!(dashboard.nodes[0].observation, Observation::Unknown);
}

#[test]
fn non_object_saved_schedule_does_not_become_a_default_interval() {
    let dir = fixture_dir();
    let config = config(&dir);
    let data = dir.join("data");
    let db = Store::open(&data).unwrap();
    db.add_snapshot("fixture-node", 9_000, None, None, &json!([]), &json!({}), &json!({})).unwrap();
    db.set_meta("schedule", &json!(null)).unwrap();
    drop(db);
    assert_eq!(source::load(&config, &data, 10_000).nodes[0].observation, Observation::Unknown);
}

#[test]
fn displayed_snapshot_becomes_stale_after_its_deadline() {
    let card = source::NodeCard {
        id: "fixture".into(),
        role: "test".into(),
        operating_state: "online".into(),
        local: Some(true),
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

#[test]
fn unscoped_topology_is_hidden_and_cannot_be_searched() {
    let dir = fixture_dir();
    let config = config(&dir);
    let data = dir.join("data");
    let db = Store::open(&data).unwrap();
    drop(db);
    let dashboard = source::load(&config, &data, 10_000);
    assert!(dashboard.repositories.is_empty());
    assert_eq!(dashboard.topology_coverage, source::Coverage::Unknown);
    assert_eq!(dashboard.topology_error.as_deref(), Some(source::TOPOLOGY_SCOPE_UNAVAILABLE));
    assert_eq!(source::search_topology(&data, "fixture", 10_000).unwrap_err(), source::TOPOLOGY_SCOPE_UNAVAILABLE);
}

#[test]
fn disabled_topology_does_not_restore_previously_cached_repository_names() {
    let mut previous = source::load(&fixture_dir().join("missing"), &fixture_dir().join("missing"), 10_000);
    previous.repositories = vec![source::Repository { name: "fixture-private".into(), origin: None, observed_at: Some(9_000), previous: false }];
    let mut next = previous.clone();
    next.repositories.clear();
    next.topology_error = Some(source::TOPOLOGY_SCOPE_UNAVAILABLE.into());
    source::preserve_previous(&previous, &mut next);
    assert!(next.repositories.is_empty());
    let mut config_failure = next.clone();
    config_failure.topology_error = None;
    config_failure.config_error = Some("fixture".into());
    source::preserve_previous(&previous, &mut config_failure);
    assert!(config_failure.repositories.is_empty());
}
