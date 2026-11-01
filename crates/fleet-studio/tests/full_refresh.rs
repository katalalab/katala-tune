use fleet_studio::{
    llm::{self, ModelConfirmation, ModelScope, RefreshQueue},
    recent::{PrState, RecentEntry, RecentGate, RecentSlot, RecentWork},
    refresh::{self, FullRefresh},
    scope::{AuditGate, Confirmations, Grants, RefreshGate, may_show_saved},
};

#[test]
fn full_refresh_invalidates_both_busy_scopes_and_replays_only_current_work() {
    let mut machine_confirmations = Confirmations::default();
    let old_machine_confirmation = machine_confirmations.begin("example", "fingerprint").unwrap();
    let mut grants = Grants::default();
    assert!(grants.authorize("example", "fingerprint"));
    let permit = grants.matching("example", "fingerprint").unwrap();
    let mut dashboard_gate = RefreshGate::default();
    let old_dashboard = dashboard_gate.request().unwrap();
    let mut scope_check_pending = false;
    let mut generation = 5;

    let mut audit_gate = AuditGate::default();
    let old_audit_ticket = audit_gate.ticket();
    let mut audit = Some("old audit");
    let mut recent = RecentSlot::default();
    let mut recent_gate = RecentGate::default();
    let old_recent_ticket = recent_gate.ticket();
    let old_recent = recent.request().unwrap();
    recent.data = Some(RecentWork::default());

    let mut models = ModelScope::default();
    let config = "example remote|10.0.0.1:12345|openai";
    let local = models.refresh(Some(config)).remove(0);
    assert!(models.dispatch(&local));
    assert!(local.start(|_| ()).is_some());
    let remote = llm::endpoint(config).unwrap();
    let old_model_confirmation = models.prepare_authorization(&remote).unwrap();
    let mut visible_confirmation: Option<ModelConfirmation> = Some(old_model_confirmation.clone());
    let mut model_queue = RefreshQueue::default();

    let outcome = refresh::begin(FullRefresh {
        sample: false,
        machine_confirmations: &mut machine_confirmations,
        dashboard_gate: &mut dashboard_gate,
        scope_check_pending: &mut scope_check_pending,
        generation: &mut generation,
        audit_gate: &mut audit_gate,
        audit: &mut audit,
        recent: &mut recent,
        recent_gate: &mut recent_gate,
        models: &mut models,
        model_confirmation: &mut visible_confirmation,
        model_queue: &mut model_queue,
    });

    assert!(outcome.had_audit);
    assert!(outcome.recent.is_none());
    assert!(!outcome.models_now);
    assert_eq!(generation, 6);
    assert!(scope_check_pending);
    assert!(!may_show_saved(&grants, "example", Some(&permit), scope_check_pending));
    assert!(machine_confirmations.current().is_none());
    assert!(!machine_confirmations.take_if(&old_machine_confirmation));
    assert!(audit.is_none());
    assert!(!old_audit_ticket.is_current());
    assert!(recent.data.is_none());
    assert!(!old_recent_ticket.is_current());
    assert!(!old_recent_ticket.revalidate(|| panic!("old handler must not revalidate")));
    assert!(visible_confirmation.is_none());
    assert!(models.confirm(&old_model_confirmation).is_none());
    assert!(models.prepare_authorization(&remote).is_none());
    assert!(model_queue.pending());

    // The view requests the next machine read immediately; the old one may only finish.
    assert!(dashboard_gate.request().is_none());
    let finished = dashboard_gate.finish(old_dashboard).unwrap();
    assert!(!finished.accept);
    assert!(finished.restart);
    let current_dashboard = dashboard_gate.request().unwrap();
    assert!(dashboard_gate.finish(current_dashboard).unwrap().accept);

    let current_recent = recent.complete(old_recent, RecentWork::default()).unwrap();
    assert!(recent.data.is_none());
    assert!(recent.complete(current_recent, RecentWork::default()).is_none());
    assert!(recent.data.is_some());

    assert!(!models.complete(&local, Some(Ok(Vec::new())), 10));
    assert!(model_queue.ready(models.querying()));
    models.refresh(Some(config));
    assert!(models.confirm(&old_model_confirmation).is_none());
    assert!(models.prepare_authorization(&remote).is_some());
}

#[test]
fn sample_refresh_requests_no_real_sources() {
    let mut machine_confirmations = Confirmations::default();
    let mut dashboard_gate = RefreshGate::default();
    let mut scope_check_pending = false;
    let mut generation = 0;
    let mut audit_gate = AuditGate::default();
    let mut audit: Option<()> = None;
    let mut recent = RecentSlot::default();
    let mut recent_gate = RecentGate::default();
    let mut models = ModelScope::default();
    let mut visible_confirmation = None;
    let mut model_queue = RefreshQueue::default();
    let outcome = refresh::begin(FullRefresh {
        sample: true,
        machine_confirmations: &mut machine_confirmations,
        dashboard_gate: &mut dashboard_gate,
        scope_check_pending: &mut scope_check_pending,
        generation: &mut generation,
        audit_gate: &mut audit_gate,
        audit: &mut audit,
        recent: &mut recent,
        recent_gate: &mut recent_gate,
        models: &mut models,
        model_confirmation: &mut visible_confirmation,
        model_queue: &mut model_queue,
    });
    assert!(outcome.recent.is_none());
    assert!(!outcome.models_now);
    assert!(!dashboard_gate.is_active());
    assert!(!models.querying());
}

#[test]
fn model_refresh_can_resume_while_the_old_machine_read_is_still_finishing() {
    let mut machine_confirmations = Confirmations::default();
    let mut dashboard_gate = RefreshGate::default();
    let old_dashboard = dashboard_gate.request().unwrap();
    let mut scope_check_pending = false;
    let mut generation = 0;
    let mut audit_gate = AuditGate::default();
    let mut audit: Option<()> = None;
    let mut recent = RecentSlot::default();
    let mut recent_gate = RecentGate::default();
    let mut models = ModelScope::default();
    let config = "example remote|10.0.0.1:12345|openai";
    let local = models.refresh(Some(config)).remove(0);
    assert!(models.dispatch(&local));
    assert!(local.start(|_| ()).is_some());
    let mut visible_confirmation = None;
    let mut model_queue = RefreshQueue::default();
    let decision = refresh::begin(FullRefresh {
        sample: false,
        machine_confirmations: &mut machine_confirmations,
        dashboard_gate: &mut dashboard_gate,
        scope_check_pending: &mut scope_check_pending,
        generation: &mut generation,
        audit_gate: &mut audit_gate,
        audit: &mut audit,
        recent: &mut recent,
        recent_gate: &mut recent_gate,
        models: &mut models,
        model_confirmation: &mut visible_confirmation,
        model_queue: &mut model_queue,
    });
    assert!(!decision.models_now);
    assert!(dashboard_gate.request().is_none());
    assert!(!models.complete(&local, Some(Ok(Vec::new())), 10));
    assert!(model_queue.ready(models.querying()));
    assert!(dashboard_gate.is_active());
    models.refresh(Some(config));
    let remote = llm::endpoint(config).unwrap();
    assert!(!models.entries().iter().find(|item| item.endpoint == remote).unwrap().authorized);
    let finished = dashboard_gate.finish(old_dashboard).unwrap();
    assert!(!finished.accept);
    assert!(finished.restart);
}

#[test]
fn cancelling_one_kind_of_grant_does_not_cancel_the_other_kind() {
    let mut grants = Grants::default();
    assert!(grants.authorize("example", "fingerprint"));
    let machine = grants.matching("example", "fingerprint").unwrap();
    let mut models = ModelScope::default();
    let config = "example remote|10.0.0.1:12345|openai";
    models.refresh(Some(config));
    let remote = llm::endpoint(config).unwrap();
    let confirmation = models.prepare_authorization(&remote).unwrap();
    let request = models.confirm(&confirmation).unwrap();
    let model_grant = models.entries().iter().find(|item| item.endpoint == remote).unwrap().grant().unwrap().clone();

    grants.revoke("example");
    assert!(!machine.is_active());
    assert!(request.active());
    assert!(grants.authorize("example", "fingerprint"));
    let current_machine = grants.matching("example", "fingerprint").unwrap();
    assert!(models.revoke(&model_grant).is_some());
    assert!(current_machine.is_active());
}

#[test]
fn recent_ticket_rejects_a_refresh_before_or_during_revalidation() {
    let mut gate = RecentGate::default();
    let old = gate.ticket();
    gate.invalidate();
    assert!(!old.revalidate(|| panic!("old operation must not begin")));
    let entry = RecentEntry {
        node: "example".into(),
        local: true,
        tool: "example".into(),
        cwd: None,
        activity_at: None,
        directory_url: None,
        prs: Vec::new(),
        pr_state: PrState::Empty,
    };
    let reappeared = entry.clone();
    assert_eq!(entry, reappeared);
    assert!(!old.revalidate(|| entry == reappeared));
    assert!(gate.ticket().revalidate(|| entry == reappeared));

    let in_progress = gate.ticket();
    assert!(!in_progress.revalidate(|| {
        gate.invalidate();
        true
    }));
    assert!(gate.ticket().revalidate(|| true));
}

#[test]
fn closing_the_view_invalidates_recent_operation_tickets() {
    let ticket = {
        let gate = RecentGate::default();
        gate.ticket()
    };
    assert!(!ticket.is_current());
    assert!(!ticket.revalidate(|| panic!("closed view must not begin an operation")));
}

#[test]
fn earlier_model_results_are_discarded_until_all_refresh_waiters_finish() {
    let mut models = ModelScope::default();
    let mut queries = models.refresh(None);
    let first = queries.remove(0);
    let second = queries.remove(0);
    assert!(models.dispatch(&first));
    assert!(models.dispatch(&second));
    assert!(first.start(|_| ()).is_some());
    assert!(second.start(|_| ()).is_some());
    let first_endpoint = first.endpoint().clone();
    let mut queue = RefreshQueue::default();

    models.invalidate_for_refresh();
    assert!(!queue.request(models.querying()));
    assert!(models.cancellation_for_refresh(&first));
    assert!(!models.complete(&first, Some(Ok(vec!["old result".into()])), 10));
    let first_entry = models.entries().iter().find(|item| item.endpoint == first_endpoint).unwrap();
    assert!(first_entry.observation.latest.is_none());
    assert!(first_entry.observation.checked_at.is_none());
    assert!(!queue.ready(models.querying()));
    assert!(!models.complete(&second, Some(Ok(vec!["old result".into()])), 20));
    assert!(queue.ready(models.querying()));
}

#[test]
fn revoking_during_refresh_still_reports_a_started_query() {
    let mut models = ModelScope::default();
    let config = "example remote|10.0.0.1:12345|openai";
    models.refresh(Some(config));
    let remote = llm::endpoint(config).unwrap();
    let confirmation = models.prepare_authorization(&remote).unwrap();
    let request = models.confirm(&confirmation).unwrap();
    let grant = models.entries().iter().find(|item| item.endpoint == remote).unwrap().grant().unwrap().clone();
    assert!(models.dispatch(&request));
    assert!(request.start(|_| ()).is_some());
    models.invalidate_for_refresh();
    assert!(models.cancellation_for_refresh(&request));

    assert_eq!(models.revoke(&grant), Some(llm::ModelRevocation::InProgress));
    assert!(!models.cancellation_for_refresh(&request));
    assert!(!models.complete(&request, Some(Ok(vec!["old result".into()])), 10));
}

#[test]
fn machine_only_refresh_keeps_recent_and_model_confirmations_unchanged() {
    let mut dashboard_gate = RefreshGate::default();
    let mut recent = RecentSlot::default();
    recent.data = Some(RecentWork::default());
    let mut models = ModelScope::default();
    let config = "example remote|10.0.0.1:12345|openai";
    models.refresh(Some(config));
    let remote = llm::endpoint(config).unwrap();
    let confirmation = models.prepare_authorization(&remote).unwrap();

    let ticket = dashboard_gate.request().unwrap();
    assert!(recent.data.is_some());
    assert!(models.confirm(&confirmation).is_some());
    assert!(dashboard_gate.finish(ticket).unwrap().accept);
}
