use std::ffi::OsString;

use fleet_studio::scope::Grants;
use fleet_studio::startup_selection::{Candidate, PendingSelection, PlanError, StartupRequest, initialize, initialize_at, parse_args, plan};

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn only_explicit_summary_ids_are_selected_in_order() {
    let request = parse_args(args(&["fleet-studio", "--sample", "--saved-summary=one", "--saved-summary=two", "--saved-summary=one"]));
    assert_eq!(request, StartupRequest::Requested(vec!["one".into(), "two".into()]));
    let candidates = [
        Candidate { id: "two", fingerprint: Some("fingerprint-two") },
        Candidate { id: "other", fingerprint: Some("fingerprint-other") },
        Candidate { id: "one", fingerprint: Some("fingerprint-one") },
    ];
    assert_eq!(plan(&request, &candidates), Ok(vec![("one".into(), "fingerprint-one".into()), ("two".into(), "fingerprint-two".into())]));
    assert_eq!(parse_args(args(&["fleet-studio", "--sample"])), StartupRequest::None);
}

#[test]
fn invalid_arguments_never_select_a_partial_set() {
    for argument in ["--saved-summary", "--saved-summary=", "--saved-summary= one", "--saved-summary=one\n"] {
        let request = parse_args(args(&["fleet-studio", "--saved-summary=one", argument]));
        assert_eq!(request, StartupRequest::Invalid);
        assert_eq!(plan(&request, &[Candidate { id: "one", fingerprint: Some("fp") }]), Err(PlanError::InvalidArgs));
    }
    let long = format!("--saved-summary={}", "a".repeat(257));
    assert_eq!(parse_args(vec![OsString::from(long)]), StartupRequest::Invalid);
    let many = (0..65).map(|n| OsString::from(format!("--saved-summary=id{n}")));
    assert_eq!(parse_args(many), StartupRequest::Invalid);
}

#[test]
fn missing_ambiguous_or_unidentifiable_targets_fail_closed() {
    let request = parse_args(args(&["--saved-summary=one", "--saved-summary=two"]));
    let missing = [Candidate { id: "one", fingerprint: Some("fp") }];
    let duplicate =
        [Candidate { id: "one", fingerprint: Some("fp") }, Candidate { id: "two", fingerprint: Some("fp") }, Candidate { id: "two", fingerprint: Some("fp2") }];
    let no_fingerprint = [Candidate { id: "one", fingerprint: Some("fp") }, Candidate { id: "two", fingerprint: None }];
    for cards in [missing.as_slice(), duplicate.as_slice(), no_fingerprint.as_slice()] {
        assert_eq!(plan(&request, cards), Err(PlanError::Unavailable));
    }
    assert_eq!(plan(&StartupRequest::None, &missing), Ok(Vec::new()));
}

#[test]
fn startup_selection_is_one_shot_even_after_resolution_failure() {
    let candidates = [Candidate { id: "one", fingerprint: Some("fp") }];
    let mut selected = PendingSelection::new(parse_args(args(&["--saved-summary=one"])));
    assert_eq!(selected.consume(&candidates), Ok(vec![("one".into(), "fp".into())]));
    assert_eq!(selected.consume(&candidates), Ok(Vec::new()));
    let mut missing = PendingSelection::new(parse_args(args(&["--saved-summary=missing"])));
    assert_eq!(missing.consume(&candidates), Err(PlanError::Unavailable));
    assert_eq!(missing.consume(&[Candidate { id: "missing", fingerprint: Some("new-fp") }]), Ok(Vec::new()));
}

#[test]
fn timeout_or_cancellation_prevents_late_authorization() {
    let mut pending = PendingSelection::new(parse_args(args(&["--saved-summary=one"])));
    assert!(pending.cancel());
    assert!(!pending.cancel());
    assert_eq!(pending.consume(&[Candidate { id: "one", fingerprint: Some("fp") }]), Ok(Vec::new()));
}

#[test]
fn completion_after_deadline_cannot_authorize_even_before_timer_callback() {
    let now = std::time::Instant::now();
    let request = parse_args(args(&["--saved-summary=one"]));
    let mut pending = PendingSelection::new_at(request, now);
    let mut grants = Grants::default();
    let cards = [Candidate { id: "one", fingerprint: Some("fp") }];
    assert_eq!(pending.apply_at(&cards, &mut grants, now + std::time::Duration::from_secs(5)), Err(PlanError::Expired));
    assert!(grants.matching("one", "fp").is_none());
    assert_eq!(pending.apply_at(&cards, &mut grants, now), Ok(0));
    assert!(grants.matching("one", "fp").is_none());
}

#[test]
fn deadline_includes_time_before_window_creation() {
    let process_start = std::time::Instant::now();
    let request = parse_args(args(&["--saved-summary=one"]));
    let (mut pending, _) = initialize_at(false, request, process_start);
    let mut grants = Grants::default();
    let cards = [Candidate { id: "one", fingerprint: Some("fp") }];
    assert_eq!(pending.apply_at(&cards, &mut grants, process_start + std::time::Duration::from_secs(7)), Err(PlanError::Expired));
    assert!(grants.matching("one", "fp").is_none());
}

#[test]
fn deadline_crossed_during_selection_rolls_back_new_grants() {
    let now = std::time::Instant::now();
    let request = parse_args(args(&["--saved-summary=one", "--saved-summary=two"]));
    let mut pending = PendingSelection::new_at(request, now);
    let mut grants = Grants::default();
    let cards = [Candidate { id: "one", fingerprint: Some("fp-one") }, Candidate { id: "two", fingerprint: Some("fp-two") }];
    let mut ticks = [now + std::time::Duration::from_secs(4), now + std::time::Duration::from_secs(4), now + std::time::Duration::from_secs(5)].into_iter();
    assert_eq!(pending.apply_with_clock(&cards, &mut grants, || ticks.next().unwrap()), Err(PlanError::Expired));
    assert!(grants.matching("one", "fp-one").is_none());
    assert!(grants.matching("two", "fp-two").is_none());
    assert_eq!(pending.apply_at(&cards, &mut grants, now), Ok(0));
}

#[test]
fn only_resolved_requested_ids_receive_session_grants() {
    let cards = [Candidate { id: "one", fingerprint: Some("fp-one") }, Candidate { id: "other", fingerprint: Some("fp-other") }];
    let mut grants = Grants::default();
    let mut selected = PendingSelection::new(parse_args(args(&["--saved-summary=one"])));
    assert_eq!(selected.apply(&cards, &mut grants), Ok(1));
    let permit = grants.matching("one", "fp-one").unwrap();
    assert!(permit.is_active());
    assert!(grants.matching("other", "fp-other").is_none());
    assert_eq!(selected.apply(&cards, &mut grants), Ok(0));
    grants.revoke("one");
    assert!(!permit.is_active());
    assert_eq!(selected.apply(&cards, &mut grants), Ok(0));
}

#[test]
fn conflicting_or_missing_targets_leave_existing_grants_unchanged() {
    let mut grants = Grants::default();
    assert!(grants.authorize("one", "old-fp"));
    let old = grants.matching("one", "old-fp").unwrap();
    let cards = [Candidate { id: "one", fingerprint: Some("new-fp") }, Candidate { id: "two", fingerprint: Some("fp-two") }];
    let mut selected = PendingSelection::new(parse_args(args(&["--saved-summary=one", "--saved-summary=two"])));
    assert_eq!(selected.apply(&cards, &mut grants), Err(PlanError::ConflictingGrant));
    assert!(old.is_active());
    assert!(grants.matching("two", "fp-two").is_none());
    let mut missing = PendingSelection::new(parse_args(args(&["--saved-summary=two", "--saved-summary=missing"])));
    assert_eq!(missing.apply(&cards, &mut grants), Err(PlanError::Unavailable));
    assert!(grants.matching("two", "fp-two").is_none());
}

#[test]
fn sample_and_invalid_requests_do_not_create_grants() {
    let cards = [Candidate { id: "one", fingerprint: Some("fp") }];
    let request = parse_args(args(&["--saved-summary=one"]));
    let (mut sample, notice) = initialize(true, request);
    let mut grants = Grants::default();
    assert!(notice.is_some());
    assert_eq!(sample.apply(&cards, &mut grants), Ok(0));
    assert!(grants.matching("one", "fp").is_none());
    let (mut invalid, notice) = initialize(false, StartupRequest::Invalid);
    assert!(notice.is_some());
    assert_eq!(invalid.apply(&cards, &mut grants), Ok(0));
    assert!(grants.matching("one", "fp").is_none());
}

#[test]
fn ledger_change_revokes_startup_grant_without_reauthorizing() {
    let mut grants = Grants::default();
    let mut pending = PendingSelection::new(parse_args(args(&["--saved-summary=one"])));
    let cards = [Candidate { id: "one", fingerprint: Some("old-fp") }];
    assert_eq!(pending.apply(&cards, &mut grants), Ok(1));
    let old = grants.matching("one", "old-fp").unwrap();
    grants.reconcile(&std::collections::HashMap::from([("one".into(), "new-fp".into())]));
    assert!(!old.is_active());
    let changed = [Candidate { id: "one", fingerprint: Some("new-fp") }];
    assert_eq!(pending.apply(&changed, &mut grants), Ok(0));
    assert!(grants.matching("one", "new-fp").is_none());
}

#[cfg(windows)]
#[test]
fn invalid_unicode_arguments_fail_closed() {
    use std::os::windows::ffi::OsStringExt;
    assert_eq!(parse_args(vec![OsString::from_wide(&[0xd800])]), StartupRequest::Invalid);
}
