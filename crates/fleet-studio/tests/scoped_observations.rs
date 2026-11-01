use fleet_studio::{
    Observation,
    source::{self, Coverage, Dashboard, NodeCard},
};

#[test]
fn missing_ledger_does_not_restore_saved_observations() {
    let previous = Dashboard {
        nodes: vec![NodeCard {
            id: "fixture-one".into(),
            fingerprint: None,
            permit: None,
            saved_state: source::SavedState::Unapproved,
            previous: false,
            role: "fixture-role".into(),
            operating_state: "online".into(),
            local: Some(false),
            last_at: Some(100),
            observation: Observation::Current,
            failure_reason: None,
            failure_at: None,
            max_age_ms: Some(1_000),
            score: Some(70),
            critical: Some(1),
            warnings: Some(2),
        }],
        config_error: None,
        database_error: None,
        repositories: vec![],
        topology_coverage: Coverage::Unknown,
        topology_error: None,
        topology_oldest_at: None,
        topology_limited: false,
    };
    let mut next = Dashboard { nodes: vec![], config_error: Some("unavailable".into()), ..previous.clone() };
    source::preserve_previous(&previous, &mut next);
    assert!(next.nodes.iter().all(|node| node.last_at.is_none() && node.score.is_none() && node.critical.is_none() && node.warnings.is_none()));
}

use fleet_studio::scope::Grants;
use fleet_studio::source::{MachineReading, ProbeFailure, SavedReadError, SavedReader, SavedState};
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tune_core::nodes::Node;

#[derive(Clone)]
struct FakeReader {
    reads: Rc<RefCell<Vec<String>>>,
    result: Result<MachineReading, SavedReadError>,
}

impl SavedReader for FakeReader {
    fn read_machine(&self, node_id: &str) -> Result<MachineReading, SavedReadError> {
        self.reads.borrow_mut().push(node_id.into());
        self.result.clone()
    }
}

fn fixture_nodes() -> Vec<Node> {
    [json!({"id":"fixture-one","role":"fixture-role","shared":true}), json!({"id":"fixture-two","role":"fixture-role","status":"retired"})]
        .iter()
        .map(|value| Node::from_value(value, "fixture-host"))
        .collect()
}

fn fixture_reading() -> MachineReading {
    MachineReading { last_at: Some(9_000), score: Some(70), critical: Some(1), warnings: Some(2), failure: ProbeFailure::None, schedule: Default::default() }
}

fn approve(nodes: &[Node], grants: &mut Grants, id: &str) {
    let fingerprint = source::fingerprint_for_node(nodes, "fixture-host", id).unwrap();
    grants.authorize(id, &fingerprint);
}

#[test]
fn no_grants_do_not_open_database_even_for_retired_nodes() {
    let nodes = fixture_nodes();
    let opened = Cell::new(0);
    let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &Grants::default(), || {
        opened.set(opened.get() + 1);
        Ok(FakeReader { reads: Rc::default(), result: Ok(fixture_reading()) })
    });
    assert_eq!(opened.get(), 0);
    assert_eq!(result.nodes.len(), 2);
    assert!(result.nodes.iter().all(|node| node.saved_state == SavedState::Unapproved && node.last_at.is_none()));
    assert!(result.database_error.is_none());
}

#[test]
fn only_explicitly_approved_id_is_read_even_when_shared() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    let reads = Rc::new(RefCell::new(Vec::new()));
    let reader = FakeReader { reads: reads.clone(), result: Ok(fixture_reading()) };
    let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
    assert_eq!(&*reads.borrow(), &["fixture-one"]);
    assert_eq!(result.nodes[0].last_at, Some(9_000));
    assert_eq!(result.nodes[1].saved_state, SavedState::Unapproved);
    assert!(result.nodes[1].last_at.is_none());
}

#[test]
fn duplicate_or_replaced_ledger_id_invalidates_the_prior_grant() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    let mut replaced = nodes.clone();
    replaced[0] = Node::from_value(&json!({"id":"fixture-one","role":"different-fixture"}), "fixture-host");
    let mut duplicated = nodes.clone();
    duplicated.push(nodes[0].clone());
    for current in [&replaced, &duplicated] {
        let opened = Cell::new(0);
        let result = source::project_nodes(current, None, "fixture-host", 10_000, &grants, || {
            opened.set(opened.get() + 1);
            Ok(FakeReader { reads: Rc::default(), result: Ok(fixture_reading()) })
        });
        assert_eq!(opened.get(), 0);
        assert!(result.nodes.iter().all(|node| node.last_at.is_none()));
    }
}

#[test]
fn revocation_discards_old_result_even_after_same_id_is_reapproved() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    let old_grants = grants.clone();
    let reader = FakeReader { reads: Rc::default(), result: Ok(fixture_reading()) };
    let mut old_result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &old_grants, || Ok(reader));
    assert_eq!(old_result.nodes[0].last_at, Some(9_000));
    grants.revoke("fixture-one");
    approve(&nodes, &mut grants, "fixture-one");
    source::discard_revoked(&mut old_result, &grants);
    assert_eq!(old_result.nodes[0].saved_state, SavedState::Unapproved);
    assert!(old_result.nodes[0].last_at.is_none());
    assert!(old_result.nodes[0].score.is_none());
}

#[test]
fn malformed_or_future_failure_never_appears_current() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    for failure in [ProbeFailure::Invalid, ProbeFailure::At(11_000)] {
        let mut reading = fixture_reading();
        reading.failure = failure;
        let reader = FakeReader { reads: Rc::default(), result: Ok(reading) };
        let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
        assert_eq!(result.nodes[0].observation, Observation::Unknown);
        assert_eq!(result.nodes[0].saved_state, SavedState::Invalid);
    }
}

#[test]
fn future_snapshot_does_not_expose_its_counts() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    let mut reading = fixture_reading();
    reading.last_at = Some(11_000);
    let reader = FakeReader { reads: Rc::default(), result: Ok(reading) };
    let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
    assert_eq!(result.nodes[0].observation, Observation::Unknown);
    assert_eq!(result.nodes[0].saved_state, SavedState::Invalid);
    assert!(result.nodes[0].score.is_none());
    assert!(result.nodes[0].critical.is_none());
    assert!(result.nodes[0].warnings.is_none());
}

#[test]
fn previous_values_require_same_active_grant_and_failed_read() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    let before =
        source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(FakeReader { reads: Rc::default(), result: Ok(fixture_reading()) }));
    let mut failed =
        source::project_nodes(&nodes, None, "fixture-host", 10_100, &grants, || Ok(FakeReader { reads: Rc::default(), result: Err(SavedReadError) }));
    source::preserve_previous(&before, &mut failed);
    assert_eq!(failed.nodes[0].last_at, Some(9_000));
    assert!(failed.nodes[0].previous);
    assert_eq!(failed.nodes[0].observation, Observation::Unknown);
    grants.revoke("fixture-one");
    source::discard_revoked(&mut failed, &grants);
    assert!(failed.nodes[0].last_at.is_none());
    assert!(failed.nodes[0].score.is_none());
}

#[test]
fn later_recorded_failure_keeps_the_prior_summary_and_both_times() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    let mut reading = fixture_reading();
    reading.failure = ProbeFailure::At(9_500);
    let reader = FakeReader { reads: Rc::default(), result: Ok(reading) };
    let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
    assert_eq!(result.nodes[0].observation, Observation::Failed);
    assert_eq!(result.nodes[0].last_at, Some(9_000));
    assert_eq!(result.nodes[0].failure_at, Some(9_500));
    assert_eq!(result.nodes[0].score, Some(70));
}

#[test]
fn cancellation_during_open_prevents_the_following_summary_query() {
    let nodes = fixture_nodes();
    let grants = Rc::new(RefCell::new(Grants::default()));
    approve(&nodes, &mut grants.borrow_mut(), "fixture-one");
    let snapshot = grants.borrow().clone();
    let reads = Rc::new(RefCell::new(Vec::new()));
    let reader = FakeReader { reads: reads.clone(), result: Ok(fixture_reading()) };
    let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &snapshot, || {
        grants.borrow_mut().revoke("fixture-one");
        Ok(reader)
    });
    assert!(reads.borrow().is_empty());
    assert_eq!(result.nodes[0].saved_state, SavedState::Unapproved);
    assert!(result.nodes[0].last_at.is_none());
}

#[test]
fn missing_snapshot_and_invalid_schedule_remain_unknown() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    let empty = MachineReading::default();
    let reader = FakeReader { reads: Rc::default(), result: Ok(empty) };
    let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
    assert_eq!(result.nodes[0].saved_state, SavedState::NoData);
    assert_eq!(result.nodes[0].observation, Observation::Unknown);

    let mut invalid = fixture_reading();
    invalid.schedule = tune_core::db::StoredSchedule::Invalid;
    let reader = FakeReader { reads: Rc::default(), result: Ok(invalid) };
    let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
    assert_eq!(result.nodes[0].observation, Observation::Unknown);
}

#[test]
fn old_summary_and_intentionally_retired_state_are_separate() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-two");
    let mut reading = fixture_reading();
    reading.schedule = tune_core::db::StoredSchedule::Minutes(5);
    let reader = FakeReader { reads: Rc::default(), result: Ok(reading) };
    let result = source::project_nodes(&nodes, None, "fixture-host", 1_000_000, &grants, || Ok(reader));
    assert_eq!(result.nodes[1].operating_state, "retired");
    assert_eq!(result.nodes[1].observation, Observation::Stale);
    assert_eq!(result.nodes[1].last_at, Some(9_000));
}

#[test]
fn ambiguous_local_hostname_blocks_both_rows() {
    let nodes: Vec<Node> = [json!({"id":"fixture-one","local_hostname":"fixture-host"}), json!({"id":"fixture-two","local_hostname":"FIXTURE-HOST.local"})]
        .iter()
        .map(|value| Node::from_value(value, "fixture-host"))
        .collect();
    assert!(source::fingerprint_for_node(&nodes, "fixture-host", "fixture-one").is_none());
    assert!(source::fingerprint_for_node(&nodes, "fixture-host", "fixture-two").is_none());
}

#[test]
fn in_flight_permit_cannot_start_another_read_after_revocation() {
    use std::sync::mpsc;
    use std::time::Duration;

    let mut grants = Grants::default();
    grants.authorize("fixture-one", "synthetic-fingerprint");
    let permit = grants.matching("fixture-one", "synthetic-fingerprint").unwrap();
    let old = permit.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        permit.read(|| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).ok();
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    grants.revoke("fixture-one");
    assert!(!old.is_active());
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    let invoked = Cell::new(false);
    assert!(old.read(|| invoked.set(true)).is_none());
    assert!(!invoked.get());
}

#[test]
fn out_of_range_score_does_not_look_like_a_valid_summary() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    for score in [-1, 101] {
        let mut reading = fixture_reading();
        reading.score = Some(score);
        let reader = FakeReader { reads: Rc::default(), result: Ok(reading) };
        let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
        assert_eq!(result.nodes[0].saved_state, SavedState::Invalid);
        assert_eq!(result.nodes[0].observation, Observation::Unknown);
        assert!(result.nodes[0].score.is_none());
    }
}

#[test]
fn manual_refresh_during_a_read_discards_the_old_result_and_restarts() {
    let mut gate = fleet_studio::scope::RefreshGate::default();
    let first = gate.request().unwrap();
    gate.invalidate();
    assert!(gate.request().is_none());
    let done = gate.finish(first).unwrap();
    assert!(!done.accept);
    assert!(done.restart);
    let second = gate.request().unwrap();
    let done = gate.finish(second).unwrap();
    assert!(done.accept);
    assert!(!done.restart);
}

#[test]
fn old_confirmation_cannot_authorize_or_cancel_a_new_confirmation() {
    let mut confirmations = fleet_studio::scope::Confirmations::default();
    let first = confirmations.begin("fixture-one", "fingerprint").unwrap();
    assert!(confirmations.cancel_if(&first));
    let second = confirmations.begin("fixture-one", "fingerprint").unwrap();
    assert!(!confirmations.take_if(&first));
    assert!(!confirmations.cancel_if(&first));
    assert_eq!(confirmations.current(), Some(&second));
    confirmations.clear();
    let third = confirmations.begin("fixture-one", "fingerprint").unwrap();
    assert!(!confirmations.take_if(&second));
    assert!(confirmations.take_if(&third));
}

#[test]
fn old_revoke_event_cannot_cancel_a_reapproved_permit() {
    let mut grants = Grants::default();
    assert!(grants.authorize("fixture-one", "fingerprint"));
    let first = grants.matching("fixture-one", "fingerprint").unwrap();
    assert!(grants.revoke_if_current("fixture-one", &first));
    assert!(grants.authorize("fixture-one", "fingerprint"));
    let second = grants.matching("fixture-one", "fingerprint").unwrap();
    assert_ne!(first.serial(), second.serial());
    assert!(!grants.revoke_if_current("fixture-one", &first));
    assert!(grants.contains("fixture-one", &second));
}

#[test]
fn changed_or_unreadable_ledger_revokes_previously_loaded_values() {
    let nodes = fixture_nodes();
    let mut changed = nodes.clone();
    changed[0] = Node::from_value(&json!({"id":"fixture-one","role":"different-fixture"}), "fixture-host");
    for current in [Some(changed.as_slice()), None] {
        let mut grants = Grants::default();
        approve(&nodes, &mut grants, "fixture-one");
        let fingerprint = source::fingerprint_for_node(&nodes, "fixture-host", "fixture-one").unwrap();
        let reader = FakeReader { reads: Rc::default(), result: Ok(fixture_reading()) };
        let mut result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
        assert!(result.nodes[0].last_at.is_some());
        source::revalidate_dashboard(&mut result, current, None, None, "fixture-host");
        assert!(result.nodes[0].last_at.is_none());
        assert!(result.nodes[0].score.is_none());
        assert!(grants.matching("fixture-one", &fingerprint).is_none());
    }
}

#[test]
fn machine_keys_do_not_activate_actions_while_editing_or_modified() {
    use fleet_studio::scope::{MachineKey, machine_key};
    assert_eq!(machine_key("m", false, false, false), Some(MachineKey::Next));
    assert_eq!(machine_key("down", true, false, false), Some(MachineKey::Next));
    assert_eq!(machine_key("up", true, false, false), Some(MachineKey::Previous));
    assert_eq!(machine_key("enter", true, false, false), Some(MachineKey::Activate));
    assert_eq!(machine_key("escape", true, false, false), Some(MachineKey::Leave));
    assert_eq!(machine_key("enter", false, false, false), None);
    assert_eq!(machine_key("enter", true, true, false), None);
    assert_eq!(machine_key("enter", true, false, true), Some(MachineKey::Consume));
}

#[test]
fn saved_values_are_hidden_until_the_ledger_recheck_finishes() {
    let mut grants = Grants::default();
    assert!(grants.authorize("fixture-one", "fingerprint"));
    let permit = grants.matching("fixture-one", "fingerprint").unwrap();
    assert!(fleet_studio::scope::may_show_saved(&grants, "fixture-one", Some(&permit), false));
    assert!(!fleet_studio::scope::may_show_saved(&grants, "fixture-one", Some(&permit), true));
    grants.revoke("fixture-one");
    assert!(!fleet_studio::scope::may_show_saved(&grants, "fixture-one", Some(&permit), false));
}

#[test]
fn hidden_saved_state_never_reveals_whether_a_record_exists() {
    let nodes = fixture_nodes();
    let mut grants = Grants::default();
    approve(&nodes, &mut grants, "fixture-one");
    for reading in [fixture_reading(), MachineReading::default()] {
        let reader = FakeReader { reads: Rc::default(), result: Ok(reading) };
        let result = source::project_nodes(&nodes, None, "fixture-host", 10_000, &grants, || Ok(reader));
        assert_eq!(result.nodes[0].visible_status(10_000, false, false), "保存情報: 未確認／表示対象外");
        assert_eq!(result.nodes[0].visible_status(10_000, false, true), "台帳を再照合中：保存要約は非表示");
    }
}

#[test]
fn machine_confirmation_discloses_refresh_and_revocation() {
    for text in [fleet_studio::scope::MACHINE_CONFIRMATION, fleet_studio::scope::MACHINE_CONFIRMATION_KEYBOARD] {
        assert!(text.contains("更新"));
        assert!(text.contains("再読取"));
        assert!(text.contains("取消"));
    }
}

#[test]
fn refresh_during_busy_dashboard_revokes_audit_result_and_guarded_open() {
    let mut dashboard = fleet_studio::scope::RefreshGate::default();
    let in_flight = dashboard.request().unwrap();
    let mut audit_gate = fleet_studio::scope::AuditGate::default();
    let old = audit_gate.ticket();
    let mut displayed = Some("synthetic audit");
    assert!(old.is_current());

    audit_gate.invalidate_and_clear(&mut displayed);
    dashboard.invalidate();
    assert!(dashboard.request().is_none());
    assert!(displayed.is_none());
    assert!(!old.is_current());
    let finish = dashboard.finish(in_flight).unwrap();
    assert!(!finish.accept);
    assert!(finish.restart);

    let new = audit_gate.ticket();
    assert!(new.is_current());
}

#[test]
fn audit_open_guard_detects_revocation_between_two_checks() {
    let mut gate = fleet_studio::scope::AuditGate::default();
    let ticket = gate.ticket();
    assert!(ticket.is_current());
    let mut saved = Some("synthetic");
    gate.invalidate_and_clear(&mut saved);
    assert!(!ticket.is_current());
    assert!(saved.is_none());
    let invoked = Cell::new(false);
    let result = fleet_studio::navigation::finish_if_authorized(
        || ticket.is_current(),
        || {
            invoked.set(true);
            true
        },
    );
    assert_eq!(result, fleet_studio::navigation::OpenResult::SelectionChanged);
    assert!(!invoked.get());
}

fn audit_fixture_nodes() -> Vec<Node> {
    vec![Node::from_value(&json!({"id":"fixture-one","alias":"fixture","os":"windows","local_hostname":"fixture-host","repo_hygiene":true}), "fixture-host")]
}

#[test]
fn audit_identity_rejects_changed_duplicate_or_unreadable_ledger() {
    let nodes = audit_fixture_nodes();
    let identity = fleet_studio::audit::identity_for_host(&nodes, "fixture-host").unwrap();
    assert!(fleet_studio::audit::identity_matches(Some(&nodes), "fixture-host", &identity));

    let mut changed = nodes.clone();
    changed[0] = Node::from_value(
        &json!({"id":"fixture-one","alias":"fixture","os":"windows","local_hostname":"fixture-host","repo_hygiene":true,"role":"changed"}),
        "fixture-host",
    );
    assert!(!fleet_studio::audit::identity_matches(Some(&changed), "fixture-host", &identity));
    let mut duplicate = nodes.clone();
    duplicate.push(Node::from_value(&json!({"id":"fixture-one","os":"windows","local_hostname":"other-fixture"}), "fixture-host"));
    assert!(!fleet_studio::audit::identity_matches(Some(&duplicate), "fixture-host", &identity));
    assert!(!fleet_studio::audit::identity_matches(None, "fixture-host", &identity));
}

#[test]
fn audit_identity_preserves_existing_shared_and_local_boundaries() {
    let nodes = audit_fixture_nodes();
    assert!(fleet_studio::audit::identity_for_host(&nodes, "fixture-host").is_some());
    for shared in [json!(true), json!("invalid"), json!(null), json!(0)] {
        let changed = vec![Node::from_value(
            &json!({"id":"fixture-one","os":"windows","local_hostname":"fixture-host","repo_hygiene":true,"shared":shared}),
            "fixture-host",
        )];
        assert!(fleet_studio::audit::identity_for_host(&changed, "fixture-host").is_none());
    }
    let ambiguous = vec![nodes[0].clone(), Node::from_value(&json!({"id":"fixture-two","os":"windows","local_hostname":"FIXTURE-HOST.local"}), "fixture-host")];
    assert!(fleet_studio::audit::identity_for_host(&ambiguous, "fixture-host").is_none());
}

#[test]
fn audit_open_guard_rejects_a_changed_ledger_before_dispatch() {
    let nodes = audit_fixture_nodes();
    let identity = fleet_studio::audit::identity_for_host(&nodes, "fixture-host").unwrap();
    assert!(fleet_studio::audit::identity_matches(Some(&nodes), "fixture-host", &identity));
    let changed = vec![Node::from_value(&json!({"id":"fixture-one","os":"windows","local_hostname":"fixture-host","shared":true}), "fixture-host")];
    let invoked = Cell::new(false);
    let result = fleet_studio::navigation::finish_if_authorized(
        || fleet_studio::audit::identity_matches(Some(&changed), "fixture-host", &identity),
        || {
            invoked.set(true);
            true
        },
    );
    assert_eq!(result, fleet_studio::navigation::OpenResult::SelectionChanged);
    assert!(!invoked.get());
}
