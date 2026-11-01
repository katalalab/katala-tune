use fleet_studio::source::{Coverage, repositories};
use serde_json::json;

#[test]
fn partial_coverage_preserves_previously_accepted_repositories() {
    let view = json!({
        "entities": [{"kind":"repository", "label":"fixture-repo", "attributes":{"origin":"https://example.org/fixture/repo"}}],
        "scopes": [{"status":"partial", "observed_at":9_000}]
    });
    let (repos, coverage) = repositories(&view, 10_000, 2_000).unwrap();
    assert_eq!(repos[0].name, "fixture-repo");
    assert_eq!(coverage, Coverage::Partial);
}

#[test]
fn old_or_absent_receipts_are_never_current() {
    let old = json!({"entities":[], "scopes":[{"status":"complete", "observed_at":1_000}]});
    assert_eq!(repositories(&old, 10_000, 2_000).unwrap().1, Coverage::Stale);
    let none = json!({"entities":[], "scopes":[]});
    assert_eq!(repositories(&none, 10_000, 2_000).unwrap().1, Coverage::Unknown);
    assert!(repositories(&json!({}), 10_000, 2_000).is_err());
}

#[test]
fn only_repository_entities_are_projected() {
    let view = json!({"entities":[{"kind":"host","label":"fixture-host"},{"kind":"repository","label":"fixture-repo","attributes":{}}], "scopes":[]});
    let (repos, _) = repositories(&view, 10_000, 2_000).unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].name, "fixture-repo");
    assert_eq!(repos[0].origin, None);
}

#[test]
fn recent_receipt_cannot_make_old_or_missing_accepted_facts_fresh() {
    let make = |at| json!({"entities":[{"kind":"repository","label":"fixture","attributes":{},"facts_observed_at_ms":at}], "scopes":[{"status":"complete","observed_at":9_999}]});
    assert_eq!(repositories(&make(json!(1_000)), 10_000, 2_000).unwrap().1, Coverage::Stale);
    assert_eq!(repositories(&make(serde_json::Value::Null), 10_000, 2_000).unwrap().1, Coverage::Unknown);
}

#[test]
fn edge_truncation_does_not_imply_missing_repositories_or_complete_fleet_coverage() {
    let view = json!({"schema":"katala_topology_ui.v1", "entities":[{"kind":"repository","label":"fixture","scope_ids":["scope-a"],"facts_observed_at_ms":9_999}],
        "scopes":[{"scope_id":"scope-a","status":"complete","observed_at":9_999}],
        "window":{"truncated":true,"filtered_stored_totals":{"accepted_entities":1}}});
    assert_eq!(repositories(&view, 10_000, 2_000).unwrap().1, Coverage::Selected);
    let mut missing = view.clone();
    missing["window"]["filtered_stored_totals"]["accepted_entities"] = json!(2);
    assert_eq!(repositories(&missing, 10_000, 2_000).unwrap().1, Coverage::Partial);
    let mut missing_scope = view.clone();
    missing_scope["scopes"] = json!([]);
    assert_eq!(repositories(&missing_scope, 10_000, 2_000).unwrap().1, Coverage::Unknown);
}
