use fleet_studio::{Observation, recent};
use serde_json::json;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tune_core::{db::Store, nodes::Node};

fn node(value: serde_json::Value) -> Node {
    Node::from_value(&value, "fixture-host")
}

fn fixture_dir() -> PathBuf {
    let id = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("recent-fixtures").join(format!("{}-{id}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn eligibility_rejects_shared_inactive_unapproved_and_ambiguous_nodes() {
    let nodes = vec![
        node(json!({"id":"local","local_hostname":"fixture-host","shared":false})),
        node(json!({"id":"remote","ai_sessions":true,"shared":false})),
        node(json!({"id":"remote-default","shared":false})),
        node(json!({"id":"shared","shared":true,"ai_sessions":true})),
        node(json!({"id":"retired","status":"retired","ai_sessions":true,"shared":false})),
        node(json!({"id":"local-optout","local_hostname":"fixture-other","ai_sessions":false,"shared":false})),
        node(json!({"id":"duplicate","ai_sessions":true,"shared":false})),
        node(json!({"id":"duplicate","shared":true})),
        node(json!({"id":"","ai_sessions":true})),
    ];
    let chosen = recent::targets(&nodes);
    assert_eq!(chosen.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["local"]);
    assert!(recent::targets(&[node(json!({"id":"local-optout","local_hostname":"fixture-host","ai_sessions":false,"shared":false}))]).is_empty());
}

#[test]
fn multiple_local_hostname_matches_including_shared_are_not_opened_as_local() {
    let nodes = vec![
        node(json!({"id":"fixture-local","local_hostname":"fixture-host","shared":false})),
        node(json!({"id":"fixture-shared","local_hostname":"fixture-host","shared":true})),
        node(json!({"id":"fixture-remote","ai_sessions":true,"shared":false})),
    ];
    assert!(recent::local_ambiguous(&nodes));
    assert!(recent::targets(&nodes).is_empty());
    assert!(recent::selection_incomplete(&nodes));
}

#[test]
fn default_scope_is_disabled_and_explicit_scope_accepts_only_current_host() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    std::fs::write(
        &config,
        r#"{"nodes":[{"id":"fixture-local","local_hostname":"fixture-host","shared":false},{"id":"fixture-remote","ai_sessions":true,"shared":false}]}"#,
    )
    .unwrap();
    let disabled = recent::load_for_host(&config, &dir.join("missing"), 10_000, "fixture-host", false);
    assert!(disabled.nodes.is_empty());
    assert!(disabled.entries.is_empty());
    let enabled = recent::load_for_host(&config, &dir.join("missing"), 10_000, "fixture-host", true);
    assert_eq!(enabled.nodes.len(), 1);
    assert_eq!(enabled.nodes[0].node, "fixture-local");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-local","local_hostname":"fixture-host"}]}"#).unwrap();
    let missing_shared = recent::load_for_host(&config, &dir.join("missing"), 10_000, "fixture-host", true);
    assert_eq!(missing_shared.nodes.len(), 1);
    assert!(missing_shared.selection_incomplete);
}

#[test]
fn no_matching_local_ledger_row_is_unknown_without_opening_the_database() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    let data = dir.join("missing-data");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-remote","local_hostname":"fixture-other","shared":false}]}"#).unwrap();
    let result = recent::load_for_host(&config, &data, 10_000, "fixture-host", true);
    assert!(result.selection_incomplete);
    assert!(result.error.is_some_and(|message| message.contains("機体照合不明")));
    assert!(result.nodes.is_empty());
    assert!(result.entries.is_empty());
    assert!(!data.exists());
}

#[test]
fn hostname_local_suffix_is_normalized_before_deciding_local_ambiguity() {
    let nodes = vec![
        node(json!({"id":"fixture-a","local_hostname":"fixture-host","shared":false})),
        node(json!({"id":"fixture-b","local_hostname":"FIXTURE-HOST.local","shared":true})),
    ];
    assert!(recent::local_ambiguous_for_host(&nodes, "fixture-host"));
    assert!(recent::targets_for_host(&nodes, "fixture-host").is_empty());
}

#[test]
fn malformed_shared_values_never_authorize_access() {
    let nodes = vec![
        node(json!({"id":"fixture-null","ai_sessions":true,"shared":null})),
        node(json!({"id":"fixture-zero","ai_sessions":true,"shared":0})),
        node(json!({"id":"fixture-string","ai_sessions":true,"shared":"false"})),
        node(json!({"id":"fixture-missing","ai_sessions":true})),
    ];
    assert!(recent::targets_for_host(&nodes, "fixture-host").is_empty());
    assert!(recent::selection_incomplete_for_host(&nodes, "fixture-host"));
    for value in [json!(null), json!(0), json!("false")] {
        let nodes = vec![node(json!({"id":"fixture-local","local_hostname":"fixture-host","shared":value}))];
        assert!(recent::targets_for_host(&nodes, "fixture-host").is_empty());
        assert!(recent::selection_incomplete_for_host(&nodes, "fixture-host"));
    }
}

#[test]
fn unknown_local_hostname_never_reclassifies_a_node_as_remote() {
    let nodes = vec![node(json!({"id":"fixture","ai_sessions":true,"shared":false}))];
    assert!(recent::targets_for_host(&nodes, "").is_empty());
    assert!(recent::targets_for_host(&nodes, ".local").is_empty());
    assert!(recent::targets_for_host(&nodes, "fixture/host").is_empty());
    assert!(recent::selection_incomplete_for_host(&nodes, ""));
    assert!(recent::selection_incomplete_for_host(&nodes, "fixture/host"));
}

#[test]
fn home_expansion_never_creates_a_network_file_url() {
    let unc = if cfg!(windows) { r"\\fixture-server\share" } else { "//fixture-server/share" };
    assert!(recent::local_cwd_url_with_home(true, "~/project", std::path::Path::new(unc)).is_none());
    assert!(recent::local_cwd_url_with_home(true, "file://fixture-server/share", std::path::Path::new(unc)).is_none());
}

#[test]
fn capped_partial_errors_are_never_presented_as_an_exact_total() {
    assert_eq!(recent::file_error_count_text(None), "不明");
    assert_eq!(recent::file_error_count_text(Some(4)), "4件");
    assert_eq!(recent::file_error_count_text(Some(5)), "5件以上");
}

#[test]
fn malformed_ai_session_permission_is_reported_as_incomplete() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-remote","ai_sessions":"true","shared":false}]}"#).unwrap();
    let work = recent::load_for_host(&config, &dir.join("missing-data"), 10_000, "fixture-host", true);
    assert!(work.entries.is_empty());
    assert!(work.nodes.is_empty());
    assert!(work.selection_incomplete);
}

#[test]
fn cursor_projection_separates_failure_partial_pending_and_freshness() {
    let current = json!({"updated_at":9000,"last_ok_at":8000,"has_error":1,"no_python":0,"truncated":1,"file_error_count":2});
    let state = recent::project_cursor("fixture", Some(&current), 10_000, 3_000);
    assert_eq!(state.attempt_at, Some(9_000));
    assert_eq!(state.last_ok_at, Some(8_000));
    assert_eq!(state.failure, Some(true));
    assert_eq!(state.pending, Some(true));
    assert_eq!(state.file_errors, Some(2));
    assert_eq!(state.collection_state(), recent::CollectionState::Failed);
    assert_eq!(state.freshness(10_000, 3_000), Observation::Current);
    assert_eq!(state.freshness(11_001, 3_000), Observation::Stale);
    let missing = recent::project_cursor("fixture", None, 10_000, 3_000);
    assert_eq!((missing.failure, missing.pending, missing.file_errors), (None, None, None));
    assert_eq!(missing.collection_state(), recent::CollectionState::Unknown);
    assert_eq!(missing.freshness(10_000, 3_000), Observation::Unknown);
    let future = json!({"updated_at":10001,"last_ok_at":10001,"has_error":0,"no_python":0,"truncated":0,"file_error_count":0});
    let future = recent::project_cursor("fixture", Some(&future), 10_000, 3_000);
    assert_eq!((future.attempt_at, future.last_ok_at), (None, None));
    assert_eq!(future.collection_state(), recent::CollectionState::Unknown);
    assert_eq!(future.freshness(10_000, 3_000), Observation::Unknown);
    let success = json!({"updated_at":9000,"last_ok_at":9000,"has_error":0,"no_python":0,"truncated":0,"file_error_count":0});
    assert_eq!(recent::project_cursor("fixture", Some(&success), 10_000, 3_000).collection_state(), recent::CollectionState::Succeeded);
}

#[test]
fn github_pr_links_are_strictly_canonical_and_never_include_secrets() {
    let good = "https://github.com/fixture-owner/fixture-repo/pull/42";
    assert_eq!(recent::github_pr_url(good).as_deref(), Some(good));
    for bad in [
        "http://github.com/fixture/repo/pull/1",
        concat!("https://fixture", "@github.com/fixture/repo/pull/1"),
        "https://github.com/fixture/repo/pull/1?token=fixture",
        "https://github.com/fixture/repo/pull/1#fragment",
        "https://github.com.evil.invalid/fixture/repo/pull/1",
        "https://github.com/fixture/repo/pull/0",
        "https://github.com/fixture/repo/pull/1/files",
        "https://github.com/fixture/repo/pull/%31",
        "https://github.com/fixture//pull/1",
    ] {
        assert!(recent::github_pr_url(bad).is_none());
    }
}

#[test]
fn pr_absence_unknown_and_truncated_lists_remain_distinct() {
    assert_eq!(recent::project_prs(None).1, recent::PrState::Unknown);
    assert_eq!(recent::project_prs(Some("not-json")).1, recent::PrState::Unknown);
    assert_eq!(recent::project_prs(Some("{}")).1, recent::PrState::Unknown);
    assert_eq!(recent::project_prs(Some("[]")).1, recent::PrState::Empty);
    let links = r#"["https://github.com/fixture/repo/pull/1","https://github.com/fixture/repo/pull/2","https://github.com/fixture/repo/pull/3","https://github.com/fixture/repo/pull/4"]"#;
    let (urls, state) = recent::project_prs(Some(links));
    assert_eq!(urls.len(), 3);
    assert_eq!(state, recent::PrState::Partial);
    assert_eq!(recent::project_prs(Some(r#"["https://evil.invalid/"]"#)).1, recent::PrState::Partial);
}

#[test]
fn remote_and_network_share_working_directories_never_become_local_links() {
    let local = if cfg!(windows) { "C:\\fixture\\repo" } else { "/fixture/repo" };
    assert!(recent::local_cwd_url(true, local).is_some());
    assert!(recent::local_cwd_url(false, local).is_none());
    assert!(recent::local_cwd_url(true, "\\\\fixture-server\\share\\repo").is_none());
    assert!(recent::local_cwd_url(true, "relative/path").is_none());
}

#[test]
fn bounded_cursor_query_selects_only_approved_ids_and_returns_no_error_text() {
    let db = Store::open_in_memory().unwrap();
    db.conn().execute_batch("INSERT INTO ai_cursors(node_id, updated_at, last_ok_at, last_error, file_errors, truncated, no_python) VALUES ('fixture-a', 200, 100, 'fixture-private-error', '[\"fixture-private-file\"]', 1, 0), ('fixture-b', 300, NULL, 'fixture-private-error', NULL, 0, 1);").unwrap();
    let rows = db.ai_cursor_summaries(&["fixture-a".into()]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["node_id"], "fixture-a");
    assert_eq!(rows[0]["has_error"], 1);
    assert_eq!(rows[0]["file_error_count"], 1);
    assert!(rows[0].get("last_error").is_none());
    assert!(rows[0].get("file_errors").is_none());
    assert!(db.ai_cursor_summaries(&vec!["fixture".into(); 33]).is_err());
    assert!(db.ai_cursor_summaries(&[]).unwrap().is_empty());
}

#[test]
fn invalid_partial_error_metadata_is_unknown_not_zero() {
    let db = Store::open_in_memory().unwrap();
    db.conn().execute_batch("INSERT INTO ai_cursors(node_id, updated_at, last_ok_at, file_errors) VALUES ('fixture', 200, 100, NULL);").unwrap();
    let rows = db.ai_cursor_summaries(&["fixture".into()]).unwrap();
    assert!(rows[0]["file_error_count"].is_null());
    db.conn().execute_batch("UPDATE ai_cursors SET file_errors='[]' WHERE node_id='fixture'").unwrap();
    let rows = db.ai_cursor_summaries(&["fixture".into()]).unwrap();
    assert_eq!(rows[0]["file_error_count"], 0);
    db.conn().execute_batch("UPDATE ai_cursors SET file_errors='invalid' WHERE node_id='fixture'").unwrap();
    let rows = db.ai_cursor_summaries(&["fixture".into()]).unwrap();
    assert!(rows[0]["file_error_count"].is_null());
    let oversized = format!("[\"{}\"]", "x".repeat(20_000));
    db.conn().execute_batch(&format!("UPDATE ai_cursors SET file_errors='{oversized}' WHERE node_id='fixture'")).unwrap();
    let rows = db.ai_cursor_summaries(&["fixture".into()]).unwrap();
    assert!(rows[0]["file_error_count"].is_null());
}

#[test]
fn saved_metadata_uses_byte_not_character_limits() {
    let db = Store::open_in_memory().unwrap();
    let byte_len: i64 = db.conn().query_row("SELECT octet_length('あ')", [], |row| row.get(0)).unwrap();
    assert_eq!(byte_len, 3);
    let good_tool = "あ".repeat(21);
    let bad_tool = "あ".repeat(22);
    let good_cwd = "あ".repeat(682);
    let bad_cwd = "あ".repeat(683);
    let good_prs = format!("[\"{}\"]", "あ".repeat(2729));
    let bad_prs = format!("[\"{}\"]", "あ".repeat(2730));
    db.conn().execute_batch(&format!(
        "INSERT INTO ai_sessions(node_id,file,tool,cwd,prs,last_ts,updated_at) VALUES ('fixture','good','{good_tool}','{good_cwd}','{good_prs}',2,2), ('fixture','bad','{bad_tool}','{bad_cwd}','{bad_prs}',1,1)"
    )).unwrap();
    let rows = db.ai_recent_work_list("fixture", 21).unwrap();
    assert_eq!(rows[0]["tool"], good_tool);
    assert_eq!(rows[0]["cwd"], good_cwd);
    assert_eq!(rows[0]["prs"], good_prs);
    assert!(rows[1]["tool"].is_null());
    assert!(rows[1]["cwd"].is_null());
    assert!(rows[1]["prs"].is_null());
    let oversized_errors = format!("[\"{}\"]", "あ".repeat(5461));
    db.conn().execute_batch(&format!("INSERT INTO ai_cursors(node_id,updated_at,file_errors) VALUES ('fixture',2,'{oversized_errors}')")).unwrap();
    let cursors = db.ai_cursor_summaries(&["fixture".into()]).unwrap();
    assert!(cursors[0]["file_error_count"].is_null());
}

#[test]
fn recent_session_query_limits_fields_and_never_returns_extra_metadata() {
    let db = Store::open_in_memory().unwrap();
    db.conn().execute_batch("INSERT INTO ai_sessions(node_id,file,tool,session_id,cwd,last_ts,prs,updated_at) VALUES ('fixture','fixture-file','codex','fixture-id','/fixture/repo',9000,'[]',9000); INSERT INTO ai_sessions(node_id,file,tool,parent_id,last_ts,updated_at) VALUES ('fixture','fixture-child','codex','fixture-parent',9999,9999);").unwrap();
    let large = "x".repeat(9000);
    db.conn()
        .execute_batch(&format!(
            "INSERT INTO ai_sessions(node_id,file,tool,cwd,prs,last_ts,updated_at) VALUES ('fixture','fixture-large','codex','{large}','{large}',8000,8000)"
        ))
        .unwrap();
    let rows = db.ai_recent_work_list("fixture", 21).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].get("file").is_none());
    assert!(rows[0].get("session_id").is_none());
    assert!(rows[0].get("tool_error_counts").is_none());
    assert!(rows[1]["cwd"].is_null());
    assert!(rows[1]["prs"].is_null());
    assert!(db.ai_recent_work_list("fixture", 22).is_err());
    assert!(db.ai_recent_work_list("", 1).is_err());
}

#[test]
fn invalid_utf8_in_saved_metadata_is_not_displayed_as_a_modified_path() {
    let db = Store::open_in_memory().unwrap();
    db.conn()
        .execute_batch(
            "INSERT INTO ai_sessions(node_id,file,tool,cwd,last_ts,updated_at) VALUES ('fixture','fixture-file','codex',CAST(x'ff' AS TEXT),9000,9000)",
        )
        .unwrap();
    assert!(db.ai_recent_work_list("fixture", 21).is_err());
}

#[test]
fn invalid_saved_schedule_never_falls_back_to_a_fresh_default() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    let data = dir.join("data");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture","local_hostname":"fixture-host","ai_sessions":true,"shared":false}]}"#).unwrap();
    let db = Store::open(&data).unwrap();
    db.conn().execute_batch("INSERT INTO meta(k,v,updated_at) VALUES ('schedule','not-json',1); INSERT INTO ai_cursors(node_id,updated_at,last_ok_at) VALUES ('fixture',9000,9000);").unwrap();
    drop(db);
    let work = recent::load_for_host(&config, &data, 10_000, "fixture-host", true);
    assert_eq!(work.nodes.len(), 1);
    assert!(work.max_age_ms.is_none());
    assert_eq!(work.nodes[0].freshness(10_000, work.max_age_ms.unwrap_or(-1)), Observation::Unknown);
}

#[test]
fn saved_sessions_are_filtered_and_removed_when_permission_changes() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    let data = dir.join("data");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-allowed","local_hostname":"fixture-host","ai_sessions":true,"shared":false},{"id":"fixture-shared","shared":true,"ai_sessions":true},{"id":"fixture-remote","ai_sessions":true,"shared":false}]}"#).unwrap();
    let db = Store::open(&data).unwrap();
    db.conn().execute_batch("INSERT INTO ai_sessions(node_id,file,tool,cwd,last_ts,prs,updated_at) VALUES ('fixture-allowed','fixture-file-a','codex','/fixture/path',9000,'[\"https://github.com/fixture/repo/pull/4\"]',9000), ('fixture-shared','fixture-file-b','codex','/fixture/hidden',9000,'[]',9000); INSERT INTO ai_cursors(node_id,updated_at,last_ok_at) VALUES ('fixture-allowed',9500,9500),('fixture-shared',9500,9500);").unwrap();
    db.conn().execute_batch("INSERT INTO ai_sessions(node_id,file,tool,parent_id,cwd,last_ts,updated_at) VALUES ('fixture-allowed','fixture-child','codex','fixture-parent','/fixture/child',9999,9999);").unwrap();
    db.conn().execute_batch("INSERT INTO ai_sessions(node_id,file,tool,cwd,last_ts,updated_at) VALUES ('fixture-remote','fixture-remote-file','codex','/fixture/remote',9999,9999);").unwrap();
    drop(db);
    let before = std::fs::read(data.join(tune_core::db::DB_FILE)).unwrap();
    let first = recent::load_for_host(&config, &data, 10_000, "fixture-host", true);
    assert_eq!(std::fs::read(data.join(tune_core::db::DB_FILE)).unwrap(), before);
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].node, "fixture-allowed");
    assert!(first.entries[0].directory_url.is_none());
    assert_eq!(first.entries[0].prs.len(), 1);
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-allowed","local_hostname":"fixture-host","ai_sessions":false,"shared":false}]}"#).unwrap();
    let revoked = recent::load_for_host(&config, &data, 10_000, "fixture-host", true);
    assert!(revoked.entries.is_empty());
    assert!(revoked.nodes.is_empty());
    let missing = recent::load_for_host(&dir.join("missing.json"), &data, 10_000, "fixture-host", true);
    assert!(missing.entries.is_empty());
    assert!(missing.error.is_some());
}

#[test]
fn local_scope_excludes_many_explicit_remote_nodes() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    let mut nodes: Vec<_> = (0..33).map(|n| json!({"id": format!("fixture-{n}"), "ai_sessions": true, "shared": false})).collect();
    nodes.push(json!({"id":"fixture-local","local_hostname":"fixture-host","shared":false}));
    std::fs::write(&config, json!({"nodes":nodes}).to_string()).unwrap();
    let work = recent::load_for_host(&config, &dir.join("missing-db"), 10_000, "fixture-host", true);
    assert!(!work.node_limited);
    assert_eq!(work.nodes.len(), 1);
    assert!(work.error.is_some());
}

#[test]
fn changing_permission_or_local_identity_during_read_invalidates_the_snapshot() {
    let original = vec![node(json!({"id":"fixture","ai_sessions":true,"shared":false}))];
    assert!(recent::same_selection(&original, &original));
    let revoked = vec![node(json!({"id":"fixture","ai_sessions":false,"shared":false}))];
    assert!(!recent::same_selection(&original, &revoked));
    let became_local = vec![node(json!({"id":"fixture","ai_sessions":true,"local_hostname":"fixture-host","shared":false}))];
    assert!(!recent::same_selection(&original, &became_local));
    let before = vec![node(json!({"id":"fixture","ai_sessions":true,"shared":false})), node(json!({"id":"other","status":"retired","shared":false}))];
    let invalid = vec![
        node(json!({"id":"fixture","ai_sessions":true,"shared":false})),
        node(json!({"id":"other","status":"retired","ai_sessions":"invalid","shared":false})),
    ];
    assert!(!recent::same_selection(&before, &invalid));
    let changed_alias = vec![node(json!({"id":"fixture","ai_sessions":true,"shared":false,"alias":"fixture-other"}))];
    assert!(!recent::same_selection(&original, &changed_alias));
    let changed_os = vec![node(json!({"id":"fixture","ai_sessions":true,"shared":false,"os":"windows"}))];
    assert!(!recent::same_selection(&original, &changed_os));
}

#[test]
fn recent_work_limits_the_current_node_after_sorting() {
    let dir = fixture_dir();
    let config = dir.join("nodes.json");
    let data = dir.join("data");
    std::fs::write(&config, r#"{"nodes":[{"id":"fixture-a","local_hostname":"fixture-host","ai_sessions":true,"shared":false}]}"#).unwrap();
    let db = Store::open(&data).unwrap();
    for node in ["fixture-a"] {
        for number in 0..25 {
            let sql = format!("INSERT INTO ai_sessions(node_id,file,tool,last_ts,updated_at) VALUES ('{node}','fixture-{number}','codex',{},1)", 1000 + number);
            db.conn().execute_batch(&sql).unwrap();
        }
    }
    drop(db);
    let recent = recent::load_for_host(&config, &data, 10_000, "fixture-host", true);
    assert_eq!(recent.entries.len(), 20);
    assert!(recent.entries_limited);
    assert!(recent.entries.windows(2).all(|pair| pair[0].activity_at >= pair[1].activity_at));
    assert!(recent.entries.iter().all(|entry| entry.activity_at.is_some_and(|at| at >= 1005)));
    assert!(recent.entries.iter().all(|entry| entry.node == "fixture-a"));
}

#[test]
fn refresh_clears_private_rows_and_discards_an_outdated_inflight_read() {
    let mut slot = recent::RecentSlot::default();
    let first = slot.request().unwrap();
    let old = recent::RecentWork { error: Some("fixture-old"), ..Default::default() };
    assert_eq!(slot.complete(first, old), None);
    assert!(slot.data.is_some());
    let second = slot.request().unwrap();
    assert!(slot.data.is_none());
    assert_eq!(slot.request(), None);
    assert!(slot.data.is_none());
    let next = slot.complete(second, recent::RecentWork { error: Some("fixture-invalidated"), ..Default::default() }).unwrap();
    assert!(slot.data.is_none());
    assert_eq!(slot.complete(next, recent::RecentWork { error: Some("fixture-new"), ..Default::default() }), None);
    assert_eq!(slot.data.as_ref().unwrap().error, Some("fixture-new"));
}

#[test]
fn timeout_is_reported_only_for_the_current_unfinished_read() {
    let mut slot = recent::RecentSlot::default();
    let first = slot.request().unwrap();
    slot.timeout(first);
    assert!(slot.timed_out);
    assert!(slot.data.is_none());
    assert_eq!(slot.request(), None);
    assert!(slot.timed_out);
    slot.timeout(first);
    assert!(slot.timed_out);
    let next = slot.complete(first, recent::RecentWork::default()).unwrap();
    assert!(!slot.timed_out);
    slot.timeout(next);
    assert!(slot.timed_out);
    slot.complete(next, recent::RecentWork::default());
    assert!(!slot.timed_out);

    let running = slot.request().unwrap();
    assert_eq!(slot.request(), None);
    slot.timeout(running);
    assert!(slot.timed_out, "a queued refresh must not hide the still-running read's timeout");
}
