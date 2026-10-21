use serde_json::{Value, json};
use tune_core::{
    db::Store,
    topology::{
        Edge, EdgeKind, Entity, EntityKind, ScopeStatus, SourceCacheEntry, SourceCacheKey, SourceFacts, TopologySnapshot,
        export::{ExportView, public_json, public_mermaid},
    },
};

fn snapshot(scope: &str, status: ScopeStatus, package: &str) -> TopologySnapshot {
    let package = Entity::new(EntityKind::Package, package);
    TopologySnapshot {
        scope_id: scope.into(),
        status,
        entities: vec![package.clone()],
        edges: vec![Edge::new(EdgeKind::DeclaresDependency, &package.id, "registry:serde")],
        failures: vec![],
        counters: Default::default(),
        observed_at_ms: 1,
    }
}

fn cache_entry(source: &str, sha: &str, fetched_at_ms: i64) -> SourceCacheEntry {
    SourceCacheEntry {
        key: SourceCacheKey { source_id: source.into(), content_sha256: sha.into(), parser_version: "parser-v1".into(), kind: "node".into() },
        revision: Some("oid-safe".into()),
        fetched_at_ms,
        facts: SourceFacts::Package { name: "demo".into(), dependencies: vec![], workspace_dependencies: vec![] },
    }
}

#[test]
fn receipts_keep_success_after_partial_and_bound_history_by_time() {
    let db = Store::open_in_memory().unwrap();
    db.record_topology("r3", &snapshot("machine-a", ScopeStatus::Complete, "alpha"), 30).unwrap();
    db.record_topology("r1", &snapshot("machine-a", ScopeStatus::Partial, "missing"), 10).unwrap();
    db.record_topology("r2", &snapshot("machine-a", ScopeStatus::Failed, "missing"), 20).unwrap();
    assert_eq!(db.topology_latest_success("machine-a").unwrap().unwrap()["run_id"], "r3");
    assert_eq!(db.topology_latest_failure("machine-a").unwrap().unwrap()["run_id"], "r2");
    assert!(db.topology_entities("machine-a").unwrap().iter().any(|row| row["label"] == "alpha"));
    db.prune_topology_history(2).unwrap();
    assert_eq!(db.topology_receipts("machine-a").unwrap().len(), 2, "prune uses receipt time, never a run hash");

    let db = Store::open_in_memory().unwrap();
    db.record_topology("success", &snapshot("machine-b", ScopeStatus::Complete, "stable"), 1).unwrap();
    db.record_topology("failure-one", &snapshot("machine-b", ScopeStatus::Failed, "missing"), 2).unwrap();
    db.record_topology("failure-two", &snapshot("machine-b", ScopeStatus::Failed, "missing"), 3).unwrap();
    db.prune_topology_history(2).unwrap();
    assert_eq!(db.topology_latest_success("machine-b").unwrap().unwrap()["run_id"], "success");
    assert_eq!(db.topology_latest_failure("machine-b").unwrap().unwrap()["run_id"], "failure-two");
}

#[test]
fn current_scopes_are_unbounded_while_history_stays_bounded_and_exports_keep_receipt_state() {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let directory = std::env::temp_dir().join(format!("tune-topology-current-scopes-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let db = Store::open(&directory).unwrap();

    let mut oldest = snapshot("scope-000", ScopeStatus::Complete, "oldest");
    oldest.observed_at_ms = 4_242;
    let oldest_id = oldest.entities[0].id.clone();
    db.record_topology("run-000", &oldest, 1).unwrap();
    for index in 1..=200 {
        let scope = format!("scope-{index:03}");
        db.record_topology(&format!("run-{index:03}"), &snapshot(&scope, ScopeStatus::Complete, &format!("package-{index}")), index as i64 + 1).unwrap();
    }

    let mut retained = snapshot("scope-retained", ScopeStatus::Complete, "retained");
    retained.observed_at_ms = 1_000;
    let retained_id = retained.entities[0].id.clone();
    db.record_topology("run-retained-complete", &retained, 1_000).unwrap();
    let mut partial = snapshot("scope-retained", ScopeStatus::Partial, "ignored");
    partial.observed_at_ms = 1_001;
    partial.entities.clear();
    partial.edges.clear();
    partial.failures = vec![tune_core::topology::Failure::new(tune_core::topology::FailureCode::LimitReached, "limit reached")];
    db.record_topology("run-retained-partial", &partial, 1_001).unwrap();

    let mut failed = snapshot("scope-no-fact-failure", ScopeStatus::Failed, "ignored");
    failed.entities.clear();
    failed.edges.clear();
    failed.failures = vec![tune_core::topology::Failure::new(tune_core::topology::FailureCode::PermissionDenied, "denied")];
    db.record_topology("run-no-fact-failure", &failed, 1_002).unwrap();
    drop(db);

    let reopened = Store::open(&directory).unwrap();
    let view = reopened.topology_view().unwrap();
    let scopes = view["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 203, "every current scope is needed for currentness, not only the newest 200");
    assert!(scopes.iter().any(|scope| scope["scope_id"] == "scope-000" && scope["status"] == "complete"));
    assert!(scopes.iter().any(|scope| scope["scope_id"] == "scope-retained" && scope["status"] == "partial"));
    assert!(scopes.iter().any(|scope| scope["scope_id"] == "scope-no-fact-failure" && scope["status"] == "failed"));
    assert_eq!(view["history"].as_array().unwrap().len(), 200, "historical rows remain presentation-bounded");
    assert!(view["entities"].as_array().unwrap().iter().any(|entity| entity["id"] == retained_id), "partial receipt preserves the last complete graph");

    let public: Value = serde_json::from_str(&public_json(&view, ExportView::All)).unwrap();
    assert!(view["entities"].as_array().unwrap().iter().any(|entity| entity["id"] == oldest_id));
    assert!(public["entities"].as_array().unwrap().iter().any(|entity| entity["currentness"] == "complete" && entity["observed_at_ms"] == 4_242));
    assert!(
        public["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entity| entity["currentness"] == "partial" && entity["observed_at_ms"] == 1_000 && entity["last_receipt_observed_at_ms"] == 1_001)
    );
    assert!(public_mermaid(&view, ExportView::All).contains("(partial)"));
    drop(reopened);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn scope_prefix_lookup_keeps_complete_graphs_addressable_for_unobserved_receipts() {
    let db = Store::open_in_memory().unwrap();
    db.record_topology("first", &snapshot("node:native:checkout:one", ScopeStatus::Complete, "one"), 1).unwrap();
    db.record_topology("first", &snapshot("node:native:checkout:two", ScopeStatus::Complete, "two"), 1).unwrap();
    db.record_topology("first", &snapshot("other:native:checkout:three", ScopeStatus::Complete, "three"), 1).unwrap();
    assert_eq!(
        db.topology_scope_ids_with_prefix("node:native:checkout:").unwrap(),
        vec!["node:native:checkout:one".to_string(), "node:native:checkout:two".to_string()]
    );
    let mut unobserved = snapshot("node:native:checkout:two", ScopeStatus::Partial, "ignored");
    unobserved.failures =
        vec![tune_core::topology::Failure::new(tune_core::topology::FailureCode::Unsupported, "checkout was not observed after a complete root-set scan")];
    db.record_topology("second", &unobserved, 2).unwrap();
    assert_eq!(db.topology_entities("node:native:checkout:two").unwrap()[0]["label"], "two");
    assert_eq!(db.topology_latest_failure("node:native:checkout:two").unwrap().unwrap()["status"], "partial");
}

#[test]
fn fenced_typed_cache_keeps_only_normalized_facts_and_original_fetch_time() {
    let db = Store::open_in_memory().unwrap();
    let old = db.acquire_topology_lease_fenced("old", 1, 10).unwrap().unwrap();
    let entry = cache_entry("checkout:one:package.json", "hash-a", 7);
    db.save_source_cache_fenced(&old, &entry, 2).unwrap();
    let hit = db.source_cache(&entry.key).unwrap().unwrap();
    assert_eq!(hit.fetched_at_ms, 7);
    assert_eq!(serde_json::to_value(&hit).unwrap()["facts"]["name"], "demo");
    assert!(!serde_json::to_string(&hit).unwrap().contains("raw manifest body"));
    let new = db.acquire_topology_lease_fenced("new", 11, 10).unwrap().unwrap();
    assert!(db.save_source_cache_fenced(&old, &cache_entry("checkout:one:package.json", "hash-b", 12), 12).is_err());
    db.save_source_cache_fenced(&new, &entry, 12).unwrap();
    assert_eq!(db.source_cache(&entry.key).unwrap().unwrap().fetched_at_ms, 7, "cache hit preserves original fetch receipt");
    let newer = cache_entry("checkout:two:package.json", "hash-c", 20);
    let newest = cache_entry("checkout:three:package.json", "hash-d", 30);
    db.save_source_cache_fenced(&new, &newer, 13).unwrap();
    db.save_source_cache_fenced(&new, &newest, 14).unwrap();
    db.prune_source_cache(2).unwrap();
    assert!(db.source_cache(&entry.key).unwrap().is_none());
    assert!(db.source_cache(&newest.key).unwrap().is_some());
    db.save_topology_progress_fenced(&new, "machine-a", &json!({"relative_checkout":"repo-a","position":7}), 15).unwrap();
    assert_eq!(db.topology_progress("machine-a").unwrap().unwrap()["position"], 7);
    assert!(db.save_topology_progress_fenced(&old, "machine-a", &json!({"raw":"manifest body"}), 15).is_err());
}

#[test]
fn scopes_are_independent_and_edge_delta_is_semantic() {
    let db = Store::open_in_memory().unwrap();
    let mut first = snapshot("checkout-one", ScopeStatus::Complete, "demo");
    first.entities[0].attributes = json!({"nested":{"observed_at_ms":1}});
    assert_eq!(db.record_topology("run-1", &first, 1).unwrap().len(), 2);
    let mut only_time_changed = first.clone();
    only_time_changed.observed_at_ms = 2;
    only_time_changed.entities[0].attributes = json!({"nested":{"observed_at_ms":2}});
    assert!(db.record_topology("run-2", &only_time_changed, 2).unwrap().is_empty());
    let mut edge_changed = only_time_changed.clone();
    edge_changed.edges = vec![Edge::new(EdgeKind::DeclaresDependency, &edge_changed.entities[0].id, "registry:tokio")];
    let changes = db.record_topology("run-3", &edge_changed, 3).unwrap();
    assert!(changes.iter().any(|change| change["kind"] == "edge" && change["change"] == "add"));
    assert!(changes.iter().any(|change| change["kind"] == "edge" && change["change"] == "remove"));
    let mut clone = edge_changed;
    clone.scope_id = "checkout-two".into();
    db.record_topology("run-4", &clone, 4).unwrap();
    assert_eq!(db.topology_entities("checkout-one").unwrap().len(), db.topology_entities("checkout-two").unwrap().len());
}

#[test]
fn public_json_and_mermaid_are_stable_and_strip_secret_nested_attributes() {
    let db = Store::open_in_memory().unwrap();
    let mut graph = snapshot("machine-a", ScopeStatus::Complete, "CANARY_SECRET_package");
    graph.entities[0].attributes = json!({
        "safe": "kept only internally",
        "nested": {"credential": "CANARY_SECRET_nested", "path": "/Users/username/private"},
        "observed_at_ms": 1
    });
    graph.edges[0].attributes = json!({"if_condition":"CANARY_SECRET_if", "safe":true});
    db.record_topology("run-a", &graph, 1).unwrap();
    let view = db.topology_view().unwrap();
    let first = public_json(&view, ExportView::All);
    let second = public_json(&view, ExportView::All);
    assert_eq!(first, second);
    assert!(!first.contains("CANARY_SECRET"));
    assert!(!first.contains("/Users/username/private"));
    assert!(!first.contains("if_condition"));
    let mermaid = public_mermaid(&view, ExportView::Dependencies);
    assert!(!mermaid.contains("CANARY_SECRET"));
    assert!(!mermaid.contains("/Users/username/private"));
    assert!(public_json(&view, ExportView::Hosts).contains("entities"));
    assert!(public_json(&view, ExportView::Cicd).contains("entities"));
    let package = tune_core::topology::canonical_entity_id(EntityKind::Package, "fixture-package");
    let host = tune_core::topology::canonical_entity_id(EntityKind::Host, "fixture-host.example.com");
    let repository = tune_core::topology::canonical_entity_id(EntityKind::Repository, "fixture-repository");
    let edge = tune_core::topology::canonical_edge_id(EdgeKind::DeclaresDependency, &package, &repository);
    let injected = json!({
        "entities":[
            {"id":package,"kind":"package","label":"fixture-package","attributes":{"nested":{"token":"CANARY_SECRET"},"path":"/Users/username/alice"}},
            {"id":host,"kind":"host","label":"unknown-freeform <script>","attributes":{"query":"token=CANARY"}},
            {"id":repository,"kind":"repository","label":"fixture-repository","attributes":{}},
            {"id":"CANARY injected id","kind":"unknown","label":"CANARY unknown"}
        ],
        "edges":[
            {"id":edge,"kind":"declares_dependency","from_id":package,"to_id":repository,"attributes":{"body":"CANARY"}},
            {"id":"injected edge","kind":"unknown","from_id":"CANARY","to_id":package}
        ]
    });
    let public = public_json(&injected, ExportView::All);
    assert!(public.contains("package-1") && public.contains("host-1") && public.contains("repository-1"));
    for private in ["fixture-host", "alice", "CANARY", "/Users", "script", "injected"] {
        assert!(!public.contains(private), "public output leaked {private}");
    }
    for canonical_id in [&package, &host, &repository, &edge] {
        assert!(!public.contains(canonical_id), "public JSON leaked internal canonical ID");
    }
    let exported: Value = serde_json::from_str(&public).unwrap();
    let entity_ids = exported["entities"].as_array().unwrap().iter().map(|entity| entity["id"].as_str().unwrap()).collect::<std::collections::BTreeSet<_>>();
    assert!(
        exported["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|edge| entity_ids.contains(edge["from"].as_str().unwrap()) && entity_ids.contains(edge["to"].as_str().unwrap()))
    );
    let mermaid = public_mermaid(&injected, ExportView::All);
    assert!(mermaid.contains("-->|declares_dependency ("));
    assert!(!mermaid.contains("unknown-freeform"));
    for canonical_id in [&package, &host, &repository, &edge] {
        assert!(!mermaid.contains(canonical_id), "Mermaid leaked internal canonical ID");
    }
}

#[test]
fn view_projects_shared_entities_and_edges_once_with_scope_provenance() {
    let directory = std::env::temp_dir().join(format!("tune-topology-view-{}", tune_core::db::now_ms()));
    let _ = std::fs::remove_dir_all(&directory);
    let db = Store::open(&directory).unwrap();
    let host = Entity::with_key(EntityKind::Host, "private host", "host-key");
    let repository = Entity::with_key(EntityKind::Repository, "private repo", "repository-key");
    let mut first = Entity::with_key(EntityKind::Checkout, "first", "checkout-first");
    let mut second = Entity::with_key(EntityKind::Checkout, "second", "checkout-second");
    first.attributes = json!({"checkout":1});
    second.attributes = json!({"checkout":2});
    for (run, scope, checkout) in [("one", "scope-one", first), ("two", "scope-two", second)] {
        db.record_topology(
            run,
            &TopologySnapshot {
                scope_id: scope.into(),
                status: ScopeStatus::Complete,
                entities: vec![host.clone(), repository.clone(), checkout.clone()],
                edges: vec![Edge::new(EdgeKind::CheckoutOf, &checkout.id, &repository.id)],
                failures: vec![],
                counters: Default::default(),
                observed_at_ms: 1,
            },
            if run == "one" { 1 } else { 2 },
        )
        .unwrap();
    }
    drop(db);
    let reopened = Store::open(&directory).unwrap();
    let view = reopened.topology_view().unwrap();
    assert_eq!(view["entities"].as_array().unwrap().iter().filter(|entity| entity["id"] == host.id).count(), 1);
    assert_eq!(view["entities"].as_array().unwrap().iter().filter(|entity| entity["id"] == repository.id).count(), 1);
    assert_eq!(view["entities"].as_array().unwrap().iter().filter(|entity| entity["kind"] == "checkout").count(), 2);
    let host_view = view["entities"].as_array().unwrap().iter().find(|entity| entity["id"] == host.id).unwrap();
    assert_eq!(host_view["scope_ids"].as_array().unwrap().len(), 2);
    let public = public_json(&view, ExportView::All);
    assert_eq!(serde_json::from_str::<Value>(&public).unwrap()["entities"].as_array().unwrap().iter().filter(|entity| entity["kind"] == "host").count(), 1);
    let mermaid = public_mermaid(&view, ExportView::All);
    assert_eq!(mermaid.matches("host-1").count(), 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn accepted_facts_bind_to_applied_scope_and_survive_later_partial_history() {
    let db = Store::open_in_memory().unwrap();
    let mut complete = snapshot("scope-a", ScopeStatus::Complete, "shared");
    complete.observed_at_ms = 100;
    db.record_topology("complete", &complete, 900).unwrap();
    for i in 0..=200 {
        let mut partial = complete.clone();
        partial.status = ScopeStatus::Partial;
        partial.observed_at_ms = 1000 + i;
        db.record_topology(&format!("partial-{i}"), &partial, 1000 + i).unwrap();
    }
    let view = db.topology_view().unwrap();
    assert_eq!(view["entities"][0]["facts_observed_at_ms"], 100);
    assert_eq!(view["edges"][0]["facts_observed_at_ms"], 100);
    let public: Value = serde_json::from_str(&public_json(&view, ExportView::All)).unwrap();
    assert_eq!(public["entities"][0]["observed_at_ms"], 100);
    assert!(public["entities"][0]["last_attempt_at_ms"].is_null());
    assert_eq!(public["entities"][0]["last_receipt_observed_at_ms"], 1200);
    db.conn().execute("DELETE FROM topology_runs WHERE run_id='complete'", []).unwrap();
    assert_eq!(db.topology_view().unwrap()["entities"][0]["facts_observed_at_ms"], 100, "accepted binding does not cascade from run cleanup");
    // Applying older facts later must bind to the newly adopted row, not max receipt time.
    complete.observed_at_ms = 50;
    complete.entities[0].attributes = json!({"revision":"older-facts-applied-later"});
    db.record_topology("later-applied", &complete, 2000).unwrap();
    let view = db.topology_view().unwrap();
    assert_eq!(view["entities"][0]["facts_observed_at_ms"], 50);
    assert_eq!(view["entities"][0]["accepted_run_id"], "later-applied");
    assert_eq!(view["entities"][0]["attributes"]["revision"], "older-facts-applied-later");
    let lease = db.acquire_topology_lease_fenced("fixture", 0, 10000).unwrap().unwrap();
    db.save_source_cache_fenced(&lease, &cache_entry("fixture", "digest", 7), 3000).unwrap();
    let mut cached = complete;
    cached.observed_at_ms = 50;
    db.record_topology("cached", &cached, 3000).unwrap();
    assert_eq!(db.topology_view().unwrap()["entities"][0]["facts_observed_at_ms"], 50);
    assert_eq!(db.source_cache(&cache_entry("fixture", "digest", 7).key).unwrap().unwrap().fetched_at_ms, 7);
}

#[test]
fn shared_projection_facts_follow_attribute_owner_not_another_scope_or_receipt_maximum() {
    let db = Store::open_in_memory().unwrap();
    let mut a = snapshot("a-scope", ScopeStatus::Complete, "shared");
    a.observed_at_ms = 10;
    a.entities[0].attributes = json!({"revision":"owner-a"});
    let mut b = a.clone();
    b.scope_id = "b-scope".into();
    b.observed_at_ms = 900;
    b.entities[0].attributes = json!({"revision":"owner-b"});
    db.record_topology("b", &b, 1).unwrap();
    db.record_topology("a", &a, 2).unwrap();
    let view = db.topology_view().unwrap();
    assert_eq!(view["entities"][0]["attributes"]["revision"], "owner-a");
    assert_eq!(view["entities"][0]["facts_observed_at_ms"], 10);
    assert_eq!(view["entities"][0]["accepted_run_id"], "a");
    assert_eq!(view["entities"][0]["scope_ids"].as_array().unwrap().len(), 2);
    assert_eq!(view["edges"][0]["facts_observed_at_ms"], 10);
    db.conn().execute("DELETE FROM topology_accepted_facts WHERE scope_id='a-scope'", []).unwrap();
    let unknown = db.topology_view().unwrap();
    assert!(unknown["entities"][0]["facts_observed_at_ms"].is_null());
    assert_eq!(unknown["entities"][0]["attributes"]["revision"], "owner-a");
}

#[test]
fn accepted_graph_and_fact_binding_rollback_together() {
    let db = Store::open_in_memory().unwrap();
    let mut old = snapshot("scope", ScopeStatus::Complete, "stable");
    old.observed_at_ms = 10;
    db.record_topology("old", &old, 100).unwrap();
    let before = db.topology_view().unwrap();
    db.conn()
        .execute_batch("CREATE TRIGGER reject_fact_update BEFORE UPDATE ON topology_accepted_facts BEGIN SELECT RAISE(ABORT,'fixture atomic rollback'); END;")
        .unwrap();
    let mut next = old;
    next.entities[0].attributes = json!({"revision":"new"});
    next.observed_at_ms = 20;
    assert!(db.record_topology("new", &next, 200).is_err());
    assert_eq!(db.topology_view().unwrap(), before);
    db.conn().execute_batch("DROP TRIGGER reject_fact_update;CREATE TRIGGER reject_change_insert BEFORE INSERT ON topology_changes WHEN NEW.run_id='new' BEGIN SELECT RAISE(ABORT,'fixture rollback after facts'); END;").unwrap();
    assert!(db.record_topology("new", &next, 200).is_err());
    assert_eq!(db.topology_view().unwrap(), before);
}

#[test]
fn unknown_or_corrupt_fact_metadata_does_not_hide_accepted_or_write_on_view() {
    for update in
        ["version=2", "version='unknown'", "facts_observed_at_ms=-1", "facts_observed_at_ms='unknown'", "facts_observed_at_ms=x'00'", "accepted_run_id=x'00'"]
    {
        let db = Store::open_in_memory().unwrap();
        db.record_topology("accepted", &snapshot("scope", ScopeStatus::Complete, "stable"), 100).unwrap();
        db.conn().execute(&format!("UPDATE topology_accepted_facts SET {update}"), []).unwrap();
        let changes = db.conn().total_changes();
        let view = db.topology_view().unwrap();
        assert_eq!(view["entities"][0]["label"], "stable");
        assert!(view["entities"][0]["facts_observed_at_ms"].is_null(), "{update}");
        assert!(view["edges"][0]["facts_observed_at_ms"].is_null());
        assert_eq!(db.conn().total_changes(), changes);
    }
    let db = Store::open_in_memory().unwrap();
    db.record_topology("accepted", &snapshot("scope", ScopeStatus::Complete, "stable"), 100).unwrap();
    db.conn().execute_batch("ALTER TABLE topology_accepted_facts RENAME COLUMN version TO unknown_version;").unwrap();
    assert!(db.topology_view().unwrap()["entities"][0]["facts_observed_at_ms"].is_null());
}

#[test]
fn legacy_fact_metadata_is_not_bootstrapped_from_receipts_on_read_only_or_reopen() {
    let path = std::env::temp_dir().join(format!(
        "tune-facts-legacy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let db = Store::open(&path).unwrap();
    let mut s = snapshot("scope", ScopeStatus::Complete, "stable");
    s.observed_at_ms = 999;
    db.record_topology("old-complete", &s, 100).unwrap();
    db.conn().execute_batch("DROP TABLE topology_accepted_facts;PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(db);
    let bytes = std::fs::read(path.join(tune_core::db::DB_FILE)).unwrap();
    let db = Store::open_read_only(&path).unwrap();
    let changes = db.conn().total_changes();
    let view = db.topology_view().unwrap();
    assert_eq!(view["entities"][0]["label"], "stable");
    assert!(view["entities"][0]["facts_observed_at_ms"].is_null());
    assert_eq!(db.conn().total_changes(), changes);
    drop(db);
    assert_eq!(std::fs::read(path.join(tune_core::db::DB_FILE)).unwrap(), bytes);
    let db = Store::open(&path).unwrap();
    assert!(db.topology_view().unwrap()["entities"][0]["facts_observed_at_ms"].is_null());
    s.observed_at_ms = 10;
    db.record_topology("new-complete", &s, 200).unwrap();
    assert_eq!(db.topology_view().unwrap()["entities"][0]["facts_observed_at_ms"], 10);
    drop(db);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn public_facts_and_attempt_times_never_export_candidates_or_metadata_identities() {
    let id = Entity::new(EntityKind::Host, "private-canary").id;
    for facts in [Value::Null, json!(-1), json!("private-canary"), json!(0), json!(42)] {
        let graph = json!({"entities":[{"id":id,"kind":"host","label":"private-canary","scope_ids":["private-scope"],"facts_observed_at_ms":facts,"accepted_run_id":"private-run"}],"edges":[],"scopes":[{"scope_id":"private-scope","status":"partial","observed_at":99}],"candidates":[{"scope_id":"private-candidate-scope","attempt_id":"private-attempt","source_generation":"private-source","digest":"private-digest","entities":[{"kind":"docker_container","label":"private-candidate"}],"edges":[]}]});
        let exported = public_json(&graph, ExportView::All);
        let public: Value = serde_json::from_str(&exported).unwrap();
        assert_eq!(public["entities"][0]["observed_at_ms"], facts.as_i64().filter(|at| *at >= 0).map_or(Value::Null, |at| json!(at)));
        assert!(public["entities"][0]["last_attempt_at_ms"].is_null());
        assert_eq!(public["entities"][0]["last_receipt_observed_at_ms"], 99);
        for text in [exported, public_mermaid(&graph, ExportView::All)] {
            for private in ["private-", "accepted_run_id", "candidates", "source_generation", "digest"] {
                assert!(!text.contains(private));
            }
        }
    }
}

#[test]
fn concurrent_complete_writes_never_mix_accepted_attributes_and_facts() {
    let path = std::env::temp_dir().join(format!(
        "tune-facts-concurrent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let reader = Store::open(&path).unwrap();
    let writer = Store::open(&path).unwrap();
    let mut initial = snapshot("scope", ScopeStatus::Complete, "shared");
    initial.observed_at_ms = 0;
    initial.entities[0].attributes = json!({"fact_version":0,"memory_bytes":0});
    writer.record_topology("initial", &initial, 0).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let started = barrier.clone();
    let thread = std::thread::spawn(move || {
        started.wait();
        for i in 1..=50 {
            let mut s = initial.clone();
            s.observed_at_ms = i;
            s.entities[0].attributes = json!({"fact_version":i,"memory_bytes":i});
            writer.record_topology(&format!("write-{i}"), &s, i).unwrap();
        }
    });
    barrier.wait();
    for index in 0..100 {
        let view = match index % 3 {
            0 => reader.topology_view(),
            1 => reader.topology_accepted_view(),
            _ => reader.topology_ui_view(&Default::default()),
        }
        .unwrap();
        assert_eq!(view["entities"][0]["attributes"]["memory_bytes"], view["entities"][0]["facts_observed_at_ms"]);
    }
    thread.join().unwrap();
    drop(reader);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn accepted_only_view_never_reads_candidate_schema_or_payload_and_preserves_export() {
    let db = Store::open_in_memory().unwrap();
    let mut s = snapshot("scope", ScopeStatus::Complete, "stable");
    s.observed_at_ms = 42;
    db.record_topology("accepted", &s, 100).unwrap();
    let full = db.topology_view().unwrap();
    // Inspecting this optional candidate view, even via PRAGMA, raises a real
    // SQL error. Accepted-only reads must not reach it or mask full API errors.
    db.conn().execute_batch("DROP TABLE docker_candidates;CREATE VIEW docker_candidates AS SELECT candidate_probe_must_not_run() AS scope_id;DROP TABLE docker_ui_rows;CREATE VIEW docker_ui_rows AS SELECT projection_probe_must_not_run() AS scope_id;").unwrap();
    assert!(db.topology_view().is_err());
    let changes = db.conn().total_changes();
    let accepted = db.topology_accepted_view().unwrap();
    let mut expected = full.clone();
    expected.as_object_mut().unwrap().remove("candidates");
    assert_eq!(accepted, expected);
    assert_eq!(accepted["entities"][0]["facts_observed_at_ms"], 42);
    assert!(accepted.get("candidates").is_none());
    assert_eq!(public_json(&accepted, ExportView::All), public_json(&full, ExportView::All));
    assert_eq!(public_mermaid(&accepted, ExportView::All), public_mermaid(&full, ExportView::All));
    assert_eq!(db.conn().total_changes(), changes);
}

fn reopen_unknown_ui_schema(ddl: &str) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "tune-optional-ui-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let db = Store::open(&path).unwrap();
    let mut s = snapshot("accepted-scope", ScopeStatus::Complete, "stable");
    s.observed_at_ms = 42;
    db.record_topology("accepted", &s, 100).unwrap();
    let before = db.topology_accepted_view().unwrap();
    db.conn().execute_batch(&format!("DROP TABLE docker_ui_rows;{ddl}")).unwrap();
    let schema: String = db.conn().query_row("SELECT sql FROM sqlite_schema WHERE name='docker_ui_rows'", [], |r| r.get(0)).unwrap();
    drop(db);
    let reopened = Store::open(&path);
    assert!(reopened.is_ok(), "unknown optional UI schema must not block accepted startup: {:?}", reopened.as_ref().err());
    let db = reopened.unwrap();
    assert_eq!(db.topology_accepted_view().unwrap(), before);
    assert_eq!(db.conn().query_row("SELECT sql FROM sqlite_schema WHERE name='docker_ui_rows'", [], |r| r.get::<_, String>(0)).unwrap(), schema);
    assert_eq!(db.conn().query_row("SELECT old FROM docker_ui_rows", [], |r| r.get::<_, String>(0)).unwrap(), "preserved");
    assert_eq!(db.conn().query_row("SELECT count(*) FROM sqlite_schema WHERE name='docker_ui_rows_selection'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    let writes = db.conn().total_changes();
    let ui = db.topology_ui_view(&Default::default()).unwrap();
    assert!(ui["candidates"].as_array().unwrap().is_empty());
    assert!(!ui["entities"].as_array().unwrap().is_empty());
    assert_eq!(ui["window"]["candidate_unavailable_reasons"], json!(["unknown_projection_schema"]));
    assert_eq!(db.conn().total_changes(), writes);
    drop(db);
    let readonly = Store::open_read_only(&path).unwrap();
    let writes = readonly.conn().total_changes();
    assert!(readonly.topology_ui_view(&Default::default()).unwrap()["candidates"].as_array().unwrap().is_empty());
    assert_eq!(readonly.topology_accepted_view().unwrap(), before);
    assert_eq!(readonly.conn().total_changes(), writes);
    drop(readonly);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn optional_ui_unknown_table_does_not_block_accepted_reopen() {
    reopen_unknown_ui_schema("CREATE TABLE docker_ui_rows(old TEXT);INSERT INTO docker_ui_rows VALUES ('preserved');");
}
#[test]
fn optional_ui_future_table_does_not_block_accepted_reopen() {
    reopen_unknown_ui_schema("CREATE TABLE docker_ui_rows(old TEXT,future_version INTEGER);INSERT INTO docker_ui_rows VALUES ('preserved',2);");
}
#[test]
fn optional_ui_unknown_view_does_not_block_accepted_reopen() {
    reopen_unknown_ui_schema("CREATE VIEW docker_ui_rows AS SELECT 'preserved' AS old;");
}
#[test]
fn optional_ui_known_index_is_initialized_only_on_writable_open() {
    let path = std::env::temp_dir().join(format!(
        "tune-optional-index-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let columns = |db: &Store| -> Vec<String> {
        let mut s = db.conn().prepare("PRAGMA index_info(docker_ui_rows_selection)").unwrap();
        s.query_map([], |r| r.get(2)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    let db = Store::open(&path).unwrap();
    assert_eq!(columns(&db), vec!["row_type", "kind", "scope_id", "id"]);
    db.conn().execute_batch("DROP INDEX docker_ui_rows_selection").unwrap();
    drop(db);
    let readonly = Store::open_read_only(&path).unwrap();
    let writes = readonly.conn().total_changes();
    readonly.topology_ui_view(&Default::default()).unwrap();
    assert!(columns(&readonly).is_empty());
    assert_eq!(readonly.conn().total_changes(), writes);
    drop(readonly);
    let db = Store::open(&path).unwrap();
    assert_eq!(columns(&db), vec!["row_type", "kind", "scope_id", "id"]);
    let writes = db.conn().total_changes();
    db.topology_ui_view(&Default::default()).unwrap();
    assert_eq!(db.conn().total_changes(), writes);
    drop(db);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(columns(&reopened), vec!["row_type", "kind", "scope_id", "id"]);
    drop(reopened);
    std::fs::remove_dir_all(path).unwrap();
}
