use fleet_studio::pointer_id::PointerIds;

#[test]
fn reordered_or_filtered_rows_do_not_inherit_another_target_click() {
    let mut ids = PointerIds::default();
    let pressed = ids.get("repository", &["sample-a"]);
    let other = ids.get("repository", &["sample-b"]);
    ids.finish_frame();
    // A redraw after filtering moves B into the first visible row.
    ids.begin_frame();
    assert_ne!(pressed, ids.get("repository", &["sample-b"]));
    ids.finish_frame();
    ids.begin_frame();
    assert_eq!(other, ids.get("repository", &["sample-b"]));
    assert_ne!(pressed, ids.get("repository", &["sample-a"]));
    assert_ne!(pressed, ids.get("audit", &["sample-a"]));
}

#[test]
fn duplicate_targets_get_distinct_ids_without_exposing_the_target() {
    let mut ids = PointerIds::default();
    let first = ids.get("repository", &["sample-a"]);
    let second = ids.get("repository", &["sample-a"]);
    assert_ne!(first, second);
    ids.finish_frame();
    ids.begin_frame();
    assert_eq!(first, ids.get("repository", &["sample-a"]));
    assert_eq!(second, ids.get("repository", &["sample-a"]));
}

#[test]
fn retired_and_explicitly_invalidated_targets_get_fresh_ids() {
    let mut ids = PointerIds::default();
    let removed = ids.get("recent", &["sample-dir"]);
    ids.finish_frame();
    ids.begin_frame();
    ids.get("recent", &["another-sample"]);
    ids.finish_frame();
    ids.begin_frame();
    assert_ne!(removed, ids.get("recent", &["sample-dir"]));
    let current = ids.get("recent", &["another-sample"]);
    ids.clear();
    ids.begin_frame();
    assert_ne!(current, ids.get("recent", &["another-sample"]));
}

#[test]
fn identity_cache_cap_fails_closed_on_redraw() {
    let mut ids = PointerIds::default();
    for number in 0..4096 {
        ids.get("repository", &[&number.to_string()]);
    }
    let pressed = ids.get("repository", &["overflow"]);
    ids.begin_frame();
    assert_ne!(pressed, ids.get("repository", &["overflow"]));
}

#[test]
fn authorization_context_and_action_are_part_of_recent_work_identity() {
    let mut ids = PointerIds::default();
    let first = ids.get("recent-pr", &["sample-node-a", "sample-dir", "sample-pr"]);
    assert_ne!(first, ids.get("recent-pr", &["sample-node-b", "sample-dir", "sample-pr"]));
    assert_ne!(first, ids.get("recent-dir", &["sample-node-a", "sample-dir", "sample-pr"]));
}
