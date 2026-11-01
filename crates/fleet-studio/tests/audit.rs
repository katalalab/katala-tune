use fleet_studio::audit::project;
use serde_json::json;

#[test]
fn dirty_and_unpushed_values_come_from_existing_hygiene_output() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"repos":[
        {"path":"fixture/project","state":"ok","branch":"feature","tracked":2,"untracked":1,"upstream":true,"ahead":3,"behind":4}
    ]})]);
    assert_eq!(audit.repos[0].dirty, Some(3));
    assert_eq!(audit.repos[0].unpushed, Some(3));
    assert_eq!(audit.repos[0].behind, Some(4));
}

#[test]
fn unseen_repositories_and_failed_nodes_are_not_clean() {
    let audit = project(&[
        json!({"node_id":"fixture-a","ok":true,"truncated":true,"repos":[{"path":"fixture/missing","state":"unobserved"}]}),
        json!({"node_id":"fixture-b","ok":false,"error":"timeout_or_unreachable"}),
    ]);
    assert_eq!(audit.repos[0].dirty, None);
    assert_eq!(audit.repos[0].unpushed, None);
    assert_eq!(audit.incomplete_nodes, vec!["fixture-a", "fixture-b"]);
}

#[test]
fn missing_upstream_uses_only_confirmed_local_only_count() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"repos":[
        {"path":"fixture/project","state":"ok","tracked":0,"untracked":0,"upstream":false,"local_only":null}
    ]})]);
    assert_eq!(audit.repos[0].dirty, Some(0));
    assert_eq!(audit.repos[0].unpushed, None);
}

#[test]
fn malformed_skip_count_is_incomplete() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"skipped":"unknown","repos":[]})]);
    assert_eq!(audit.incomplete_nodes, vec!["fixture-node"]);
}

#[test]
fn local_audit_never_selects_other_active_machines() {
    use fleet_studio::audit::{Scope, targets};
    use tune_core::nodes::Node;
    let nodes = vec![
        Node::from_value(&json!({"id":"fixture-local","os":"windows","local_hostname":"fixture-host"}), "fixture-host"),
        Node::from_value(&json!({"id":"fixture-remote","os":"macos","local_hostname":"fixture-other"}), "fixture-host"),
        Node::from_value(&json!({"id":"fixture-opted-in","os":"macos","repo_hygiene":true,"local_hostname":"fixture-other"}), "fixture-host"),
        Node::from_value(&json!({"id":"fixture-shared","os":"windows","shared":true,"repo_hygiene":true,"local_hostname":"fixture-other"}), "fixture-host"),
        Node::from_value(&json!({"id":"fixture-retired","os":"windows","status":"retired","local_hostname":"fixture-host"}), "fixture-host"),
    ];
    assert_eq!(targets(&nodes, Scope::Local).iter().map(|node| node.id.as_str()).collect::<Vec<_>>(), vec!["fixture-local"]);
    assert_eq!(targets(&nodes, Scope::Fleet).iter().map(|node| node.id.as_str()).collect::<Vec<_>>(), vec!["fixture-local", "fixture-opted-in"]);
}

#[test]
fn failure_reports_a_safe_reason_and_observation_time() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":false,"at":12345,"error":"timeout_or_unreachable"})]);
    assert_eq!(audit.issues[0].node, "fixture-node");
    assert_eq!(audit.issues[0].at, Some(12345));
    assert_eq!(audit.issues[0].reason, "タイムアウトまたは接続不可");
}

#[test]
fn missing_roots_are_not_a_healthy_empty_audit() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"roots":[],"repos":[]})]);
    assert_eq!(audit.incomplete_nodes, vec!["fixture-node"]);
}

#[test]
fn previous_values_survive_a_failed_refresh_without_becoming_current() {
    use fleet_studio::audit::preserve_previous;
    let previous = project(&[json!({"node_id":"fixture-node","at":100,"ok":true,"roots":[{"path":"fixture"}],"repos":[
        {"path":"fixture/repo","state":"ok","tracked":1,"untracked":0,"upstream":true,"ahead":2}
    ]})]);
    let mut next = project(&[json!({"node_id":"fixture-node","at":200,"ok":false,"error":"timeout_or_unreachable"})]);
    preserve_previous(&previous, &mut next);
    assert_eq!(next.repos[0].dirty, Some(1));
    assert_eq!(next.repos[0].at, Some(100));
    assert!(next.repos[0].previous);
}

#[test]
fn unrequested_failed_machine_keeps_its_previous_failure() {
    use fleet_studio::audit::preserve_previous;
    let previous = project(&[json!({"node_id":"fixture-remote","at":100,"ok":false,"error":"timeout_or_unreachable"})]);
    let mut next = project(&[json!({"node_id":"fixture-local","at":200,"ok":true,"roots":[{"path":"fixture"}],"repos":[]})]);
    preserve_previous(&previous, &mut next);
    assert_eq!(next.issues[0].node, "fixture-remote");
    assert_eq!(next.issues[0].at, Some(100));
    assert!(next.issues[0].previous);
}

#[test]
fn explicit_missing_git_is_not_misreported_as_missing_roots() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"error":"git_missing","roots":[],"repos":[]})]);
    assert_eq!(audit.issues[0].reason, "Git を利用できません");
}
