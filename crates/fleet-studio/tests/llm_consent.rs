use fleet_studio::llm::{self, Endpoint, ModelEntry, ModelError, ModelGrant, ModelRevocation, ModelScope, RefreshQueue};

const REMOTE: &str = "example remote|10.0.0.1:12345|openai";

fn entry<'a>(scope: &'a ModelScope, target: &Endpoint) -> &'a ModelEntry {
    scope.entries().iter().find(|item| &item.endpoint == target).unwrap()
}

fn grant(scope: &ModelScope, target: &Endpoint) -> ModelGrant {
    entry(scope, target).grant().unwrap().clone()
}

fn finish_requests(scope: &mut ModelScope, requests: Vec<llm::ModelQuery>) {
    for request in requests {
        assert!(scope.dispatch(&request));
        assert!(request.start(|_| ()).is_some());
        assert!(scope.complete(&request, Some(Ok(Vec::new())), 1));
    }
}

#[test]
fn remote_candidate_remains_visible_without_an_automatic_request() {
    let mut scope = ModelScope::default();
    let configured = format!("local example|127.0.0.1:12345|ollama;{REMOTE}");
    let requests = scope.refresh(Some(&configured));
    let remote = llm::endpoint(REMOTE).unwrap();

    assert_eq!(requests.len(), 4);
    assert!(!scope.querying());
    assert!(requests.iter().all(|request| request.endpoint().address.ip().is_loopback()));
    assert_eq!(scope.entries().len(), 5);
    assert!(!entry(&scope, &remote).authorized);
    assert_eq!(entry(&scope, &remote).observation.latest, None);
    assert_eq!(entry(&scope, &remote).observation.checked_at, None);
}

#[test]
fn revocation_clears_results_and_rejects_late_or_prior_generation_responses() {
    let mut scope = ModelScope::default();
    let initial = scope.refresh(Some(REMOTE));
    finish_requests(&mut scope, initial);
    let remote = llm::endpoint(REMOTE).unwrap();
    let confirm = scope.prepare_authorization(&remote).unwrap();
    let first = scope.confirm(&confirm).unwrap();
    let active_grant = grant(&scope, &remote);
    assert!(scope.dispatch(&first));
    assert!(first.start(|_| ()).is_some());
    assert!(scope.finish(&first, Ok(vec!["example model".into()]), 100));

    let old = scope.refresh(Some(REMOTE)).into_iter().find(|request| request.endpoint() == &remote).unwrap();
    assert!(scope.dispatch(&old));
    assert_eq!(entry(&scope, &remote).observation.last_good, Some((vec!["example model".into()], 100)));
    assert_eq!(scope.revoke(&active_grant), Some(ModelRevocation::Prevented));
    assert_eq!(scope.pending_cancellations(), 1);
    assert_eq!(scope.cancellation(&old), Some(ModelRevocation::Prevented));
    assert!(!old.active());
    let mut invoked = false;
    assert_eq!(
        old.start(|_| {
            invoked = true;
            1
        }),
        None
    );
    assert!(!invoked);
    assert!(!scope.complete(&old, None, 200));
    assert_eq!(scope.pending_cancellations(), 0);
    assert_eq!(scope.cancellation(&old), None);
    assert!(!entry(&scope, &remote).authorized);
    assert_eq!(entry(&scope, &remote).observation.latest, None);
    assert_eq!(entry(&scope, &remote).observation.checked_at, None);
    assert_eq!(entry(&scope, &remote).observation.last_good, None);
    assert!(!scope.finish(&old, Ok(vec!["late result".into()]), 200));

    let confirm = scope.prepare_authorization(&remote).unwrap();
    let current = scope.confirm(&confirm).unwrap();
    assert!(!scope.finish(&old, Ok(vec!["late result".into()]), 300));
    assert!(scope.dispatch(&current));
    assert!(current.start(|_| ()).is_some());
    assert!(scope.finish(&current, Ok(vec!["new result".into()]), 400));
    assert_eq!(entry(&scope, &remote).observation.checked_at, Some(400));
}

#[test]
fn changing_endpoint_identity_does_not_inherit_consent() {
    for changed in [
        "example remote|10.0.0.2:12345|openai",
        "example remote|10.0.0.1:12346|openai",
        "example remote|10.0.0.1:12345|ollama",
        "changed label|10.0.0.1:12345|openai",
    ] {
        let mut scope = ModelScope::default();
        scope.refresh(Some(REMOTE));
        let old_endpoint = llm::endpoint(REMOTE).unwrap();
        let confirm = scope.prepare_authorization(&old_endpoint).unwrap();
        let old = scope.confirm(&confirm).unwrap();

        let requests = scope.refresh(Some(changed));
        let changed_endpoint = llm::endpoint(changed).unwrap();
        assert!(!old.active());
        assert!(!scope.finish(&old, Ok(vec!["late result".into()]), 100));
        assert!(!entry(&scope, &changed_endpoint).authorized);
        assert!(requests.iter().all(|request| request.endpoint() != &changed_endpoint));
    }
}

#[test]
fn duplicate_and_invalid_candidates_are_counted_separately() {
    let mut scope = ModelScope::default();
    let configured = format!("{REMOTE};duplicate|10.0.0.1:12345|openai;invalid");
    let requests = scope.refresh(Some(&configured));
    assert_eq!(scope.duplicates(), 1);
    assert_eq!(scope.rejected(), 1);
    assert_eq!(scope.entries().len(), 4);
    assert_eq!(requests.len(), 3);
}

#[test]
fn only_a_configured_remote_can_be_prepared_for_authorization() {
    let mut scope = ModelScope::default();
    let configured = format!("{REMOTE};local|127.0.0.1:12345|openai");
    scope.refresh(Some(&configured));
    let absent = llm::endpoint("another|10.0.0.2:12345|openai").unwrap();
    let loopback = llm::endpoint("local|127.0.0.1:12345|openai").unwrap();
    assert!(scope.prepare_authorization(&absent).is_none());
    assert!(scope.prepare_authorization(&loopback).is_none());
}

#[test]
fn an_old_confirmation_cannot_authorize_after_configuration_changes() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let confirmation = scope.prepare_authorization(&remote).unwrap();

    scope.refresh(Some(REMOTE));
    assert!(scope.confirm(&confirmation).is_none());
    assert!(!entry(&scope, &remote).authorized);
}

#[test]
fn pending_refresh_suspends_old_and_new_confirmations_until_config_is_refreshed() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let old = scope.prepare_authorization(&remote).unwrap();

    scope.suspend_confirmations();
    assert!(scope.confirm(&old).is_none());
    assert!(scope.prepare_authorization(&remote).is_none());

    scope.refresh(Some(REMOTE));
    assert!(scope.confirm(&old).is_none());
    let current = scope.prepare_authorization(&remote).unwrap();
    assert!(scope.confirm(&current).is_some());
}

#[test]
fn a_revoked_confirmation_cannot_be_replayed() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let old_confirmation = scope.prepare_authorization(&remote).unwrap();
    let old_request = scope.confirm(&old_confirmation).unwrap();
    let old_grant = grant(&scope, &remote);
    assert_eq!(scope.revoke(&old_grant), Some(ModelRevocation::Prevented));
    assert_eq!(scope.pending_cancellations(), 0);
    assert!(!scope.dispatch(&old_request));

    let current_confirmation = scope.prepare_authorization(&remote).unwrap();
    assert!(scope.confirm(&old_confirmation).is_none());
    assert!(scope.confirm(&current_confirmation).is_some());
}

#[test]
fn closing_and_reopening_confirmation_invalidates_the_old_confirmation() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let old = scope.prepare_authorization(&remote).unwrap();
    assert!(scope.cancel_confirmation(&old));
    let current = scope.prepare_authorization(&remote).unwrap();
    assert!(!scope.cancel_confirmation(&old));
    assert!(scope.confirm(&old).is_none());
    assert!(scope.confirm(&current).is_some());
}

#[test]
fn stale_revoke_cannot_cancel_a_later_grant() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let old_confirmation = scope.prepare_authorization(&remote).unwrap();
    scope.confirm(&old_confirmation).unwrap();
    let old = grant(&scope, &remote);
    assert_eq!(scope.revoke(&old), Some(ModelRevocation::Prevented));
    let current_confirmation = scope.prepare_authorization(&remote).unwrap();
    let current = scope.confirm(&current_confirmation).unwrap();
    assert_eq!(scope.revoke(&old), None);
    assert!(entry(&scope, &remote).authorized);
    assert!(current.active());
}

#[test]
fn cancelled_completion_does_not_clear_a_new_grant_or_its_result() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let first_confirmation = scope.prepare_authorization(&remote).unwrap();
    let first = scope.confirm(&first_confirmation).unwrap();
    let first_grant = grant(&scope, &remote);
    assert!(scope.dispatch(&first));
    let current = first
        .start(|_| {
            assert_eq!(scope.revoke(&first_grant), Some(ModelRevocation::InProgress));
            let current_confirmation = scope.prepare_authorization(&remote).unwrap();
            scope.confirm(&current_confirmation).unwrap()
        })
        .unwrap();
    assert_eq!(scope.pending_cancellations(), 1);
    assert!(scope.dispatch(&current));
    assert!(current.start(|_| ()).is_some());
    assert!(scope.complete(&current, Some(Ok(vec!["new result".into()])), 200));
    assert!(!scope.complete(&first, Some(Ok(vec!["late result".into()])), 300));
    assert_eq!(scope.pending_cancellations(), 0);
    assert!(entry(&scope, &remote).authorized);
    assert_eq!(entry(&scope, &remote).observation.latest, Some(Ok(vec!["new result".into()])));
}

#[test]
fn cancellation_after_start_is_distinct_and_still_discards_the_response() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let confirmation = scope.prepare_authorization(&remote).unwrap();
    let query = scope.confirm(&confirmation).unwrap();
    let active_grant = grant(&scope, &remote);

    assert!(scope.dispatch(&query));
    let outcome = query.start(|_| scope.revoke(&active_grant));
    assert_eq!(outcome, Some(Some(ModelRevocation::InProgress)));
    assert_eq!(scope.pending_cancellations(), 1);
    assert_eq!(scope.cancellation(&query), Some(ModelRevocation::InProgress));
    assert!(!query.active());
    assert!(!scope.complete(&query, Some(Ok(vec!["late result".into()])), 100));
    assert_eq!(scope.pending_cancellations(), 0);
    assert_eq!(scope.cancellation(&query), None);
    assert_eq!(entry(&scope, &remote).observation.latest, None);
}

#[test]
fn one_request_cannot_start_twice() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let confirmation = scope.prepare_authorization(&remote).unwrap();
    let query = scope.confirm(&confirmation).unwrap();
    assert!(scope.dispatch(&query));
    assert_eq!(query.start(|_| query.start(|_| 1)), Some(None));
}

#[test]
fn refresh_requested_while_busy_is_coalesced_and_replayed_once() {
    let mut refresh = RefreshQueue::default();
    assert!(!refresh.request(true));
    assert!(!refresh.request(true));
    assert!(refresh.pending());
    assert!(!refresh.ready(true));
    assert!(refresh.ready(false));
    assert!(!refresh.pending());
    assert!(!refresh.ready(false));
    assert!(refresh.request(false));
}

#[test]
fn direct_refresh_tracks_prior_requests_until_they_acknowledge_completion() {
    let mut scope = ModelScope::default();
    let mut old = scope.refresh(None);
    let running = old.remove(0);
    assert!(scope.dispatch(&running));
    assert!(running.start(|_| ()).is_some());
    assert!(scope.dispatch(&old[0]));

    let current = scope.refresh(None);
    assert_eq!(scope.pending_cancellations(), 2);
    assert!(scope.querying());
    assert!(!scope.complete(&running, Some(Ok(vec!["late result".into()])), 100));
    for queued in old {
        assert_eq!(queued.start(|_| ()), None);
        assert!(!scope.complete(&queued, None, 100));
    }
    assert_eq!(scope.pending_cancellations(), 0);
    finish_requests(&mut scope, current);
    assert!(!scope.querying());
}

#[test]
fn undispatched_requests_are_discarded_without_waiting_for_completion() {
    let mut scope = ModelScope::default();
    let old = scope.refresh(None);
    assert!(!scope.querying());
    let current = scope.refresh(None);
    assert_eq!(scope.pending_cancellations(), 0);
    assert!(!scope.querying());
    for request in old {
        assert!(!scope.dispatch(&request));
        assert_eq!(request.start(|_| ()), None);
    }
    finish_requests(&mut scope, current);
    assert!(!scope.querying());
}

#[test]
fn confirmation_discloses_the_exact_target_protocol_and_plain_http() {
    let mut scope = ModelScope::default();
    scope.refresh(Some(REMOTE));
    let remote = llm::endpoint(REMOTE).unwrap();
    let confirmation = scope.prepare_authorization(&remote).unwrap();
    let description = confirmation.description();
    assert!(description.contains("10.0.0.1:12345"));
    assert!(description.contains("openai"));
    assert!(description.contains("平文 HTTP"));
    assert!(description.contains("更新時にも"));
    assert!(description.contains("取り消せます"));
    assert!(description.contains("停止を保証"));
}

#[test]
fn dropping_the_session_invalidates_pending_request_permissions() {
    let pending = {
        let mut scope = ModelScope::default();
        scope.refresh(Some(REMOTE));
        let remote = llm::endpoint(REMOTE).unwrap();
        let confirmation = scope.prepare_authorization(&remote).unwrap();
        scope.confirm(&confirmation).unwrap()
    };
    assert!(!pending.active());
}

#[test]
fn failure_after_a_refresh_keeps_a_separately_timed_previous_result() {
    let mut scope = ModelScope::default();
    let mut first = scope.refresh(None);
    let query = first.remove(0);
    let endpoint = query.endpoint().clone();
    assert!(scope.dispatch(&query));
    assert!(query.start(|_| ()).is_some());
    assert!(scope.finish(&query, Ok(vec!["example model".into()]), 100));
    let later = scope.refresh(None).into_iter().find(|request| request.endpoint() == &endpoint).unwrap();
    assert!(scope.dispatch(&later));
    assert!(later.start(|_| ()).is_some());
    assert!(scope.finish(&later, Err(ModelError::TimedOut), 200));
    assert_eq!(entry(&scope, &endpoint).observation.latest, Some(Err(ModelError::TimedOut)));
    assert_eq!(entry(&scope, &endpoint).observation.checked_at, Some(200));
    assert_eq!(entry(&scope, &endpoint).observation.last_good, Some((vec!["example model".into()], 100)));
}
