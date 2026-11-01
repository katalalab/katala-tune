use fleet_studio::audit::project;
use serde_json::json;

#[test]
fn audit_scope_incomplete_walk_keeps_current_and_previous_results() {
    use fleet_studio::audit::preserve_previous;
    let previous = project(&[json!({
        "node_id":"fixture-node", "at":100, "ok":true, "roots":[{"path":"fixture"}], "truncated":false, "skipped":0,
        "repos":[{"path":"fixture/previous","state":"ok","tracked":1,"untracked":0,"conflicted":0,"upstream":false,"detached":false}]
    })]);
    let mut next = project(&[json!({
        "node_id":"fixture-node", "at":200, "ok":true, "roots":[{"path":"fixture"}], "truncated":false, "skipped":0,
        "error":"walk_incomplete", "repos":[{"path":"fixture/current","state":"ok","tracked":2,"untracked":1,"conflicted":0,"upstream":false,"detached":false}]
    })]);
    assert_eq!(next.incomplete_nodes, vec!["fixture-node"]);
    assert_eq!(next.issues[0].reason, "探索先の一部を確認できません");
    assert_eq!(next.issues[0].at, Some(200));
    assert_eq!(next.repos[0].dirty, Some(3));
    assert_eq!(next.repos[0].at, Some(200));
    assert!(!next.repos[0].previous);
    preserve_previous(&previous, &mut next);
    assert_eq!(next.repos.len(), 2);
    assert_eq!(next.repos[1].path, "fixture/previous");
    assert_eq!(next.repos[1].at, Some(100));
    assert!(next.repos[1].previous);
}

#[test]
fn audit_scope_missing_or_invalid_completion_metadata_is_incomplete() {
    for field in ["truncated", "skipped"] {
        for value in [None, Some(json!(null)), Some(json!("unknown")), Some(json!(-1))] {
            let mut input = json!({"node_id":"fixture-node","at":123,"ok":true,"roots":[{"path":"fixture"}],"truncated":false,"skipped":0,"repos":[]});
            if let Some(value) = value {
                input[field] = value;
            } else {
                input.as_object_mut().unwrap().remove(field);
            }
            let audit = project(&[input]);
            assert_eq!(audit.incomplete_nodes, vec!["fixture-node"], "{field}");
            assert_eq!(audit.issues[0].reason, "探索の完了状態が不明");
            assert_eq!(audit.issues[0].at, Some(123));
        }
    }
}

#[test]
fn audit_scope_valid_completion_metadata_is_not_invented_as_missing() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"roots":[{"path":"fixture"}],"truncated":false,"skipped":0,"repos":[]})]);
    assert!(audit.incomplete_nodes.is_empty());
    assert!(audit.issues.is_empty());
}

#[test]
fn audit_scope_failed_node_never_replaces_previous_success_with_a_success_row() {
    use fleet_studio::audit::preserve_previous;
    let previous = project(&[json!({
        "node_id":"fixture-node", "at":100, "ok":true, "roots":[{"path":"fixture"}], "truncated":false, "skipped":0,
        "repos":[{"path":"fixture/repo","state":"ok","tracked":2,"untracked":1,"conflicted":0,"upstream":false,"detached":false}]
    })]);
    let mut failed = project(&[json!({
        "node_id":"fixture-node", "at":200, "ok":false, "error":"timeout_or_unreachable", "roots":[{"path":"fixture"}],
        "truncated":false, "skipped":0, "repos":[
            {"path":"fixture/repo","state":"ok","tracked":0,"untracked":0,"conflicted":0,"upstream":false,"detached":false},
            {"path":"fixture/other","state":"unobserved","reason":"git_failed"}
        ]
    })]);
    assert_eq!(failed.issues[0].reason, "タイムアウトまたは接続不可");
    assert_eq!(failed.repos[0].dirty, None);
    assert_eq!(failed.repos[0].error, Some("機体の監査に失敗したため未確認"));
    assert_eq!(failed.repos[1].error, Some("Git の読み取りに失敗"));
    preserve_previous(&previous, &mut failed);
    let old = failed.repos.iter().find(|row| row.path == "fixture/repo" && row.previous).unwrap();
    assert_eq!(old.dirty, Some(3));
    assert_eq!(old.at, Some(100));
}

#[test]
fn audit_scope_reported_incomplete_fields_are_not_healthy() {
    for marker in [json!(["repos"]), json!(["roots"]), json!(["counts"]), json!(["error"]), json!(["unknown"]), json!(null), json!("invalid")] {
        let audit = project(&[json!({
            "node_id":"fixture-node", "at":123, "ok":true, "roots":[{"path":"fixture"}],
            "truncated":false, "skipped":0, "repos":[], "incomplete_fields":marker
        })]);
        assert_eq!(audit.incomplete_nodes, vec!["fixture-node"]);
        assert_eq!(audit.issues[0].reason, "監査データの一部が不明");
        assert_eq!(audit.issues[0].at, Some(123));
    }
    let valid = project(&[json!({
        "node_id":"fixture-node", "ok":true, "roots":[{"path":"fixture"}], "truncated":false, "skipped":0, "repos":[], "incomplete_fields":[]
    })]);
    assert!(valid.incomplete_nodes.is_empty());
}

#[test]
fn dirty_and_unpushed_values_come_from_existing_hygiene_output() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"roots":[{"path":"fixture"}],"truncated":false,"skipped":0,"repos":[
        {"path":"fixture/project","state":"ok","branch":"feature","tracked":2,"untracked":1,"conflicted":0,"upstream":true,"detached":false,"ahead":3,"behind":4}
    ]})]);
    assert_eq!(audit.repos[0].dirty, Some(3));
    assert_eq!(audit.repos[0].unpushed, Some(3));
    assert_eq!(audit.repos[0].behind, Some(4));
}

#[test]
fn unseen_repositories_and_failed_nodes_are_not_clean() {
    let audit = project(&[
        json!({"node_id":"fixture-a","ok":true,"roots":[{"path":"fixture"}],"truncated":true,"skipped":0,"repos":[{"path":"fixture/missing","state":"unobserved"}]}),
        json!({"node_id":"fixture-b","ok":false,"error":"timeout_or_unreachable"}),
    ]);
    assert_eq!(audit.repos[0].dirty, None);
    assert_eq!(audit.repos[0].unpushed, None);
    assert_eq!(audit.incomplete_nodes, vec!["fixture-a", "fixture-b"]);
}

#[test]
fn missing_upstream_uses_only_confirmed_local_only_count() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"roots":[{"path":"fixture"}],"truncated":false,"skipped":0,"repos":[
        {"path":"fixture/project","state":"ok","tracked":0,"untracked":0,"conflicted":0,"upstream":false,"detached":false,"local_only":null}
    ]})]);
    assert_eq!(audit.repos[0].dirty, Some(0));
    assert_eq!(audit.repos[0].unpushed, None);
}

#[test]
fn malformed_skip_count_is_incomplete() {
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"roots":[{"path":"fixture"}],"truncated":false,"skipped":"unknown","repos":[]})]);
    assert_eq!(audit.incomplete_nodes, vec!["fixture-node"]);
}

#[test]
fn local_audit_never_selects_other_active_machines() {
    use fleet_studio::audit::{Scope, targets};
    use tune_core::nodes::Node;
    let nodes = vec![
        Node::from_value(&json!({"id":"fixture-local","alias":"fixture-local","os":"windows","shared":false,"local_hostname":"fixture-host"}), "fixture-host"),
        Node::from_value(&json!({"id":"fixture-remote","alias":"fixture-remote","os":"macos","shared":false,"local_hostname":"fixture-other"}), "fixture-host"),
        Node::from_value(
            &json!({"id":"fixture-opted-in","alias":"fixture-opted-in","os":"macos","shared":false,"repo_hygiene":true,"local_hostname":"fixture-other"}),
            "fixture-host",
        ),
        Node::from_value(
            &json!({"id":"fixture-shared","alias":"fixture-shared","os":"windows","shared":true,"repo_hygiene":true,"local_hostname":"fixture-other"}),
            "fixture-host",
        ),
        Node::from_value(
            &json!({"id":"fixture-retired","alias":"fixture-retired","os":"windows","shared":false,"status":"retired","local_hostname":"fixture-other"}),
            "fixture-host",
        ),
    ];
    assert_eq!(targets(&nodes, Scope::Local).iter().map(|node| node.id.as_str()).collect::<Vec<_>>(), vec!["fixture-local"]);
    assert!(targets(&nodes, Scope::Fleet).is_empty());
}

#[test]
fn audit_rejects_ambiguous_local_and_unknown_or_invalid_remote_targets() {
    use fleet_studio::audit::{Scope, targets_for_host};
    use tune_core::nodes::Node;
    let n = |v| Node::from_value(&v, "fixture-host");
    let nodes = vec![
        n(json!({"id":"local","alias":"fixture-local","os":"windows","shared":false,"local_hostname":"fixture-host"})),
        n(json!({"id":"shared-local","alias":"fixture-shared","os":"windows","shared":true,"local_hostname":"FIXTURE-HOST.local"})),
        n(json!({"id":"missing-shared","alias":"fixture-missing","os":"macos","repo_hygiene":true,"local_hostname":"fixture-other"})),
        n(json!({"id":"null-shared","alias":"fixture-null","os":"macos","shared":null,"repo_hygiene":true,"local_hostname":"fixture-other"})),
        n(json!({"id":"bad-alias","alias":"-fixture","os":"macos","shared":false,"repo_hygiene":true,"local_hostname":"fixture-other"})),
        n(json!({"id":"good-remote","alias":"fixture-remote","os":"macos","shared":false,"repo_hygiene":true,"local_hostname":"fixture-other"})),
    ];
    assert!(targets_for_host(&nodes, Scope::Local, "fixture-host").is_empty());
    assert!(targets_for_host(&nodes, Scope::Fleet, "fixture-host").is_empty());
    assert!(targets_for_host(&nodes, Scope::Fleet, "").is_empty());
}

#[test]
fn remote_audit_is_refused_before_any_ledger_or_transport_access() {
    let result = fleet_studio::audit::collect(std::path::Path::new("fixture-nonexistent-ledger"), fleet_studio::audit::Scope::Fleet);
    assert_eq!(result.unwrap_err(), "遠隔監査は機体ごとの権限と接続先が未確認のため未対応です");
}

#[test]
fn explicitly_requested_local_audit_allows_missing_shared_but_rejects_invalid_shared() {
    use fleet_studio::audit::{Scope, targets_for_host};
    use tune_core::nodes::Node;
    let n = |v| Node::from_value(&v, "fixture-host");
    let missing = vec![n(json!({"id":"fixture-local","os":"windows","local_hostname":"fixture-host"}))];
    assert_eq!(targets_for_host(&missing, Scope::Local, "fixture-host").len(), 1);
    let warnings = fleet_studio::audit::selection_warnings(&missing, &missing, 100);
    assert!(warnings.iter().any(|warning| warning.reason.contains("未記入")));
    for value in [json!(null), json!(0), json!("false"), json!(true)] {
        let invalid = vec![n(json!({"id":"fixture-local","os":"windows","local_hostname":"fixture-host","shared":value}))];
        assert!(targets_for_host(&invalid, Scope::Local, "fixture-host").is_empty());
    }
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
    let audit = project(&[json!({"node_id":"fixture-node","ok":true,"roots":[],"truncated":false,"skipped":0,"repos":[]})]);
    assert_eq!(audit.incomplete_nodes, vec!["fixture-node"]);
}

#[test]
fn previous_values_survive_a_failed_refresh_without_becoming_current() {
    use fleet_studio::audit::preserve_previous;
    let previous = project(&[json!({"node_id":"fixture-node","at":100,"ok":true,"roots":[{"path":"fixture"}],"truncated":false,"skipped":0,"repos":[
        {"path":"fixture/repo","state":"ok","tracked":1,"untracked":0,"conflicted":0,"upstream":true,"detached":false,"ahead":2}
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
    let mut next = project(&[json!({"node_id":"fixture-local","at":200,"ok":true,"roots":[{"path":"fixture"}],"truncated":false,"skipped":0,"repos":[]})]);
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
