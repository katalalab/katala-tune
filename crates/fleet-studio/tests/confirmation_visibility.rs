use fleet_studio::llm::{self, Endpoint, ModelRevocation, ModelScope};
use fleet_studio::llm_keyboard;
use fleet_studio::scope::{Confirmations, Grants};
use fleet_studio::ui_consent::{self, MachineConfirmationResult, ModelConfirmationResult};

const REMOTE: &str = "fixture model|10.0.0.101:12345|openai";

fn real_model_fixture() -> (ModelScope, Endpoint) {
    let mut scope = ModelScope::default();
    let _unstarted_requests = scope.refresh(Some(REMOTE));
    (scope, llm::endpoint(REMOTE).unwrap())
}

#[test]
fn real_machine_confirmation_cannot_authorize_when_disclosure_visibility_is_unproven() {
    let mut confirmations = Confirmations::default();
    let mut grants = Grants::default();
    let confirmation = confirmations.begin("fixture-node", "fixture-fingerprint").unwrap();

    let result = ui_consent::confirm_machine(false, &mut confirmations, &mut grants, &confirmation);

    assert_eq!(result, MachineConfirmationResult::Unavailable);
    assert!(grants.matching("fixture-node", "fixture-fingerprint").is_none());
    assert!(confirmations.current().is_none());
}

#[test]
fn real_model_confirmation_cannot_create_a_query_when_disclosure_visibility_is_unproven() {
    let (mut scope, endpoint) = real_model_fixture();
    let visible = vec![endpoint.clone()];
    let confirmation = llm_keyboard::prepare(&mut scope, &visible, &endpoint, false).unwrap();
    let mut current = Some(confirmation.clone());

    let result = ui_consent::confirm_model(false, &mut scope, &visible, &mut current, &confirmation, false);

    assert!(matches!(result, ModelConfirmationResult::Unavailable));
    assert!(!scope.entries().iter().find(|entry| entry.endpoint == endpoint).unwrap().authorized);
    assert!(current.is_none());
    assert!(scope.confirm(&confirmation).is_none());
}

#[test]
fn sample_machine_can_be_authorized_and_revoked_without_io() {
    let mut confirmations = Confirmations::default();
    let mut grants = Grants::default();
    let confirmation = confirmations.begin("fixture-node", "fixture-fingerprint").unwrap();

    let result = ui_consent::confirm_machine(true, &mut confirmations, &mut grants, &confirmation);

    assert_eq!(result, MachineConfirmationResult::Authorized);
    let permit = grants.matching("fixture-node", "fixture-fingerprint").unwrap();
    assert!(grants.revoke_if_current("fixture-node", &permit));
    assert!(grants.matching("fixture-node", "fixture-fingerprint").is_none());
}

#[test]
fn sample_model_confirmation_remains_simulated_and_cannot_dispatch() {
    let mut scope = ModelScope::sample();
    let endpoint = scope.entries()[0].endpoint.clone();
    let visible = vec![endpoint.clone()];
    let confirmation = llm_keyboard::prepare(&mut scope, &visible, &endpoint, false).unwrap();
    let mut current = Some(confirmation.clone());

    let ModelConfirmationResult::Authorized(query) = ui_consent::confirm_model(true, &mut scope, &visible, &mut current, &confirmation, false) else {
        panic!("sample confirmation should produce a simulated query");
    };
    let mut called = false;
    assert!(!scope.dispatch(&query));
    assert!(query.start(|_| called = true).is_none());
    assert!(!called);
    let grant = scope.entries()[0].grant().unwrap().clone();
    assert_eq!(llm_keyboard::revoke(&mut scope, &visible, &grant), Some(ModelRevocation::Prevented));
}

#[test]
fn old_machine_callback_cannot_cancel_a_newer_confirmation() {
    let mut confirmations = Confirmations::default();
    let mut grants = Grants::default();
    let old = confirmations.begin("fixture-node", "fixture-fingerprint").unwrap();
    let newer = confirmations.begin("fixture-node", "fixture-fingerprint").unwrap();

    let result = ui_consent::confirm_machine(false, &mut confirmations, &mut grants, &old);

    assert_eq!(result, MachineConfirmationResult::Stale);
    assert_eq!(confirmations.current(), Some(&newer));
    assert!(grants.matching("fixture-node", "fixture-fingerprint").is_none());
    assert!(confirmations.cancel_if(&newer));
}

#[test]
fn old_model_callback_cannot_cancel_a_newer_confirmation() {
    let (mut scope, endpoint) = real_model_fixture();
    let visible = vec![endpoint.clone()];
    let old = llm_keyboard::prepare(&mut scope, &visible, &endpoint, false).unwrap();
    let newer = llm_keyboard::prepare(&mut scope, &visible, &endpoint, false).unwrap();
    let mut current = Some(newer.clone());

    let result = ui_consent::confirm_model(false, &mut scope, &visible, &mut current, &old, false);

    assert!(matches!(result, ModelConfirmationResult::Stale));
    assert_eq!(current.as_ref(), Some(&newer));
    assert!(llm_keyboard::cancel(&mut scope, &visible, current.as_ref(), &newer));
}

#[test]
fn paused_real_consent_does_not_stop_automatic_loopback_requests() {
    let mut scope = ModelScope::default();

    let requests = scope.refresh(None);

    assert!(!requests.is_empty());
    assert!(requests.iter().all(|request| request.endpoint().address.ip().is_loopback()));
}

#[test]
fn preparation_is_temporarily_unavailable_only_for_real_mode() {
    assert!(!ui_consent::preparation_available(false));
    assert!(ui_consent::preparation_available(true));
    for windows in [true, false] {
        let real = ui_consent::footer_notice(false, windows);
        assert!(real.contains("新規許可停止"));
        assert!(!real.contains("Y 許可"));
        assert!(!real.contains("許可はマウス"));
        assert!(ui_consent::footer_notice(true, windows).contains("模擬許可"));
    }
}
