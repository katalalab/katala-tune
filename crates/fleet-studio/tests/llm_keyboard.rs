use fleet_studio::confirmation_keys::{ConfirmKeyGate, NavigationKeyGate};
use fleet_studio::llm::{self, Endpoint, ModelRevocation, ModelScope};
use fleet_studio::llm_keyboard::{self, ModelKey, ModelKeyboard, ModelViewGate};

const FIRST: &str = "fixture one|10.0.0.101:12345|openai";
const SECOND: &str = "fixture two|10.0.0.102:12345|ollama";

fn configured() -> (ModelScope, Endpoint, Endpoint) {
    let mut scope = ModelScope::default();
    let _ = scope.refresh(Some(&format!("{FIRST};{SECOND}")));
    (scope, llm::endpoint(FIRST).unwrap(), llm::endpoint(SECOND).unwrap())
}

#[test]
fn a_held_yes_after_a_delayed_release_requires_a_fresh_press() {
    let mut gate = ConfirmKeyGate::<u64>::default();

    assert!(!gate.down(None, false, false));
    gate.arm(41);
    assert!(!gate.down(Some(&41), false, false));
    gate.up();
    assert!(!gate.down(Some(&41), true, false));
    gate.up();
    assert!(gate.down(Some(&41), false, false));

    gate.up();
    gate.arm(42);
    assert!(!gate.down(Some(&41), false, false));
    gate.up();
    assert!(gate.down(Some(&42), false, false));
}

#[test]
fn a_known_release_recovers_a_confirmation_after_yes_was_held() {
    let mut gate = ConfirmKeyGate::<u64>::default();
    assert!(!gate.down(None, false, false));
    gate.arm(41);
    assert!(!gate.down(Some(&41), false, false));
    gate.up();
    assert!(gate.down(Some(&41), false, false));
}

#[test]
fn release_outside_confirmation_clears_unknown_focus_state() {
    let mut gate = ConfirmKeyGate::<u64>::default();
    gate.focus_lost();
    gate.up();
    gate.arm(41);
    assert!(gate.down(Some(&41), false, false));
}

#[test]
fn focus_loss_modifiers_and_old_confirmation_generations_cannot_authorize() {
    let mut gate = ConfirmKeyGate::<u64>::default();
    gate.arm(1);
    assert!(!gate.down(Some(&1), false, true));
    gate.up();
    gate.arm(2);
    assert!(!gate.down(Some(&1), false, false));
    gate.up();
    gate.focus_lost();
    gate.arm(3);
    assert!(!gate.down(Some(&3), false, false));
    gate.up();
    assert!(gate.down(Some(&3), false, false));
    gate.up();
    gate.arm(4);
    assert!(!gate.down(Some(&4), true, false));
}

#[test]
fn focus_recovery_requires_a_new_confirmation_matched_key_cycle() {
    let mut gate = ConfirmKeyGate::<u64>::default();
    gate.focus_lost();
    gate.arm(41);
    gate.up();
    assert!(!gate.down(Some(&41), false, false));
    gate.up();
    assert!(gate.down(Some(&41), false, false));
}

#[test]
fn an_old_release_after_a_confirmation_change_does_not_arm_the_new_target() {
    let mut gate = ConfirmKeyGate::<u64>::default();
    gate.focus_lost();
    gate.arm(41);
    assert!(!gate.down(Some(&41), false, false));
    gate.arm(42);
    gate.up();
    assert!(!gate.down(Some(&42), false, false));
    gate.up();
    gate.arm(43);
    assert!(gate.down(Some(&43), false, false));
}

#[test]
fn navigation_repeat_without_held_flag_is_consumed_until_release() {
    let mut gate = NavigationKeyGate::default();
    assert!(gate.down("down", false));
    assert!(!gate.down("down", false));
    gate.up("down");
    assert!(gate.down("down", false));
    gate.focus_lost();
    assert!(!gate.down("down", false));
    gate.up("down");
    assert!(gate.down("down", false));
    assert!(!gate.down("up", true));
}

#[test]
fn focus_recovery_accepts_first_search_shortcut_and_requires_release() {
    for (key, alternate) in [("/", "slash"), ("slash", "/")] {
        let mut gate = NavigationKeyGate::default();
        gate.focus_lost();
        assert!(gate.down(key, false));
        assert!(!gate.down(alternate, false));
        gate.up(alternate);
        assert!(gate.down(key, false));
        gate.focus_lost();
        assert!(gate.down(key, false));
    }
}

#[test]
fn search_shortcut_keeps_held_and_modified_events_blocked() {
    use fleet_studio::search_input::{KeyAction, KeyModifiers, search_key};
    for key in ["/", "slash"] {
        for (held, modified) in [(true, false), (false, true)] {
            let mut gate = NavigationKeyGate::default();
            gate.focus_lost();
            let fresh = gate.down(key, held);
            assert!(llm_keyboard::consume_blocked_shortcut(key, false, held || modified || !fresh));
            assert!(!gate.down(key, false));
            gate.up(key);
            assert!(gate.down(key, false));
        }
        assert_eq!(search_key(key, KeyModifiers::default(), true, false), KeyAction::Ime);
    }
}

#[test]
fn search_recovery_keeps_action_keys_and_old_confirmation_blocked() {
    let mut navigation = NavigationKeyGate::default();
    let mut confirm = ConfirmKeyGate::<u64>::default();
    confirm.arm(41);
    navigation.focus_lost();
    confirm.focus_lost();
    assert!(navigation.down("/", false));
    for key in ["up", "down", "enter", "x", "r", "a", "escape"] {
        let blocked = !navigation.down(key, false);
        assert!(blocked);
        assert!(llm_keyboard::consume_blocked_shortcut(key, false, blocked));
    }
    assert!(!confirm.down(Some(&41), false, false));
    for key in ["enter", "x", "y"] {
        assert_eq!(llm_keyboard::key(key, true, true, false, true), None);
        assert_eq!(fleet_studio::scope::machine_key(key, true, true, false), None);
    }
}

#[test]
fn focus_recovery_allows_l_and_m_to_start_selection_once() {
    let mut gate = NavigationKeyGate::default();
    gate.focus_lost();
    for key in ["l", "m"] {
        assert!(gate.down(key, false));
        assert!(!gate.down(key, false));
        gate.up(key);
        assert!(!gate.down(key, true));
        gate.up(key);
    }
    for key in ["up", "down", "enter", "x", "r", "a", "escape"] {
        assert!(!gate.down(key, false));
    }
    let mut confirm = ConfirmKeyGate::<u64>::default();
    confirm.focus_lost();
    confirm.arm(1);
    assert!(!confirm.down(Some(&1), false, false));
}

#[test]
fn empty_machine_selection_cancels_confirmation_and_confirmation_key() {
    let mut confirmations = fleet_studio::scope::Confirmations::default();
    let old = confirmations.begin("fixture", "fingerprint").unwrap();
    let mut gate = ConfirmKeyGate::default();
    gate.arm(old.clone());
    let mut repository = fleet_studio::navigation::Selection::default();
    repository.next(1);
    assert_eq!(repository.index(), Some(0));
    let selected = fleet_studio::scope::cycle_machine_confirmation(&mut confirmations, &mut gate, &mut repository, &[], None, false);
    assert_eq!(selected, None);
    assert_eq!(repository.index(), None);
    assert!(confirmations.current().is_none());
    assert!(!confirmations.take_if(&old));
    assert!(!gate.down(Some(&old), false, false));
}

#[test]
fn machine_selection_preserves_cycling_order_after_confirmation_is_cleared() {
    let mut confirmations = fleet_studio::scope::Confirmations::default();
    let old = confirmations.begin("fixture-a", "fingerprint-a").unwrap();
    let mut gate = ConfirmKeyGate::default();
    gate.arm(old.clone());
    let mut repository = fleet_studio::navigation::Selection::default();
    let choices = vec![("fixture-a".to_owned(), "fingerprint-a".to_owned()), ("fixture-b".to_owned(), "fingerprint-b".to_owned())];
    assert_eq!(
        fleet_studio::scope::cycle_machine_confirmation(&mut confirmations, &mut gate, &mut repository, &choices, Some(&choices[0]), true),
        Some(choices[1].clone())
    );
    assert!(!confirmations.take_if(&old));
    assert_eq!(
        fleet_studio::scope::cycle_machine_confirmation(&mut confirmations, &mut gate, &mut repository, &choices, Some(&choices[1]), false),
        Some(choices[0].clone())
    );
}

#[test]
fn selection_keeps_remote_unapproved_and_invalidates_old_confirmations() {
    let (mut models, first, second) = configured();
    let visible = vec![first.clone(), second];
    let old_model = llm_keyboard::prepare(&mut models, &visible, &first, false).unwrap();
    let mut current_model = Some(old_model.clone());
    llm_keyboard::dismiss(&mut models, &mut current_model);
    let mut selection = ModelKeyboard::default();
    selection.begin(&models, &visible);
    assert_eq!(selection.current(&models, &visible), Some(&first));
    assert!(models.confirm(&old_model).is_none());
    assert!(!models.entries().iter().find(|entry| entry.endpoint == first).unwrap().authorized);

    let mut confirmations = fleet_studio::scope::Confirmations::default();
    let old_machine = confirmations.begin("fixture", "fingerprint").unwrap();
    let mut gate = ConfirmKeyGate::default();
    gate.arm(old_machine.clone());
    let mut repository = fleet_studio::navigation::Selection::default();
    let choices = vec![("fixture".to_owned(), "fingerprint".to_owned())];
    assert_eq!(
        fleet_studio::scope::cycle_machine_confirmation(&mut confirmations, &mut gate, &mut repository, &choices, None, false),
        Some(choices[0].clone())
    );
    assert!(!confirmations.take_if(&old_machine));
    assert!(!gate.down(Some(&old_machine), false, false));
    assert!(!models.entries().iter().find(|entry| entry.endpoint == first).unwrap().authorized);
}

#[test]
fn keyboard_routing_gives_search_and_blocked_input_priority() {
    assert_eq!(llm_keyboard::key("l", false, false, false, true), Some(ModelKey::Select));
    assert_eq!(llm_keyboard::key("enter", true, false, false, true), Some(ModelKey::Inspect));
    assert_eq!(llm_keyboard::key("y", true, false, false, true), Some(ModelKey::Confirm));
    assert_eq!(llm_keyboard::key("x", true, false, false, true), Some(ModelKey::Revoke));
    for key in ["enter", "y", "x", "up", "down", "escape"] {
        assert_eq!(llm_keyboard::key(key, true, false, true, true), Some(ModelKey::Consume));
        assert_eq!(llm_keyboard::key(key, true, true, false, true), None);
        assert_eq!(llm_keyboard::key(key, true, false, false, false), Some(ModelKey::Consume));
    }
    assert_eq!(llm_keyboard::key("l", false, false, false, false), Some(ModelKey::Unsupported));
}

#[test]
fn blocked_global_shortcuts_do_not_escape_into_normal_actions_or_override_ime() {
    use fleet_studio::search_input::{KeyAction, KeyModifiers, search_key};
    for key in ["enter", "up", "down", "r", "a", "m", "l", "x", "y", "escape", "/", "slash"] {
        assert!(llm_keyboard::consume_blocked_shortcut(key, false, true));
        assert!(!llm_keyboard::consume_blocked_shortcut(key, true, true));
        assert!(!llm_keyboard::consume_blocked_shortcut(key, false, false));
    }
    assert!(!llm_keyboard::consume_blocked_shortcut("unrelated", false, true));
    assert_eq!(search_key("enter", KeyModifiers::default(), true, false), KeyAction::Ime);
    let mut gate = NavigationKeyGate::default();
    gate.focus_lost();
    assert!(!gate.down("r", false));
    assert!(!gate.down("a", false));
    gate.up("r");
    gate.up("a");
    assert!(gate.down("r", false));
    assert!(gate.down("a", false));
}

#[test]
fn machine_confirmation_uses_a_distinct_key_and_suppresses_modified_actions() {
    use fleet_studio::scope::{MachineKey, machine_key};
    assert_eq!(machine_key("enter", true, false, false), Some(MachineKey::Activate));
    assert_eq!(machine_key("y", true, false, false), Some(MachineKey::Confirm));
    assert_eq!(machine_key("y", true, false, true), Some(MachineKey::Consume));
    assert_eq!(machine_key("y", true, true, false), None);
}

#[test]
fn selecting_uses_endpoint_identity_and_hidden_targets_do_not_reappear() {
    let (scope, first, second) = configured();
    let local = scope.entries().iter().find(|item| item.endpoint.address.ip().is_loopback()).unwrap().endpoint.clone();
    let mut selection = ModelKeyboard::default();
    let visible = vec![local, first.clone(), second.clone()];
    selection.begin(&scope, &visible);
    assert_eq!(selection.current(&scope, &visible), Some(&first));
    selection.cycle(&scope, &visible, false);
    assert_eq!(selection.current(&scope, &visible), Some(&second));
    let reordered = vec![second.clone(), first.clone()];
    assert_eq!(selection.current(&scope, &reordered), Some(&second));
    selection.reconcile(&scope, std::slice::from_ref(&first));
    assert!(selection.active());
    assert_eq!(selection.current(&scope, &reordered), None);
    selection.cycle(&scope, &reordered, false);
    assert_eq!(selection.current(&scope, &reordered), Some(&second));
}

#[test]
fn stale_or_hidden_confirmations_and_grants_cannot_change_permissions() {
    let (mut scope, first, second) = configured();
    let visible = vec![first.clone(), second.clone()];
    let old = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    assert!(llm_keyboard::confirm(&mut scope, std::slice::from_ref(&second), Some(&old), &old, false).is_none());
    assert!(llm_keyboard::cancel(&mut scope, &visible, Some(&old), &old));
    let current = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    assert!(llm_keyboard::confirm(&mut scope, &visible, Some(&current), &old, false).is_none());
    let _ = llm_keyboard::confirm(&mut scope, &visible, Some(&current), &current, false).unwrap();
    let old_grant = scope.entries().iter().find(|item| item.endpoint == first).unwrap().grant().unwrap().clone();
    assert_eq!(llm_keyboard::revoke(&mut scope, std::slice::from_ref(&second), &old_grant), None);
    assert_eq!(llm_keyboard::revoke(&mut scope, &visible, &old_grant), Some(ModelRevocation::Prevented));
    let newer = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    let _ = llm_keyboard::confirm(&mut scope, &visible, Some(&newer), &newer, false).unwrap();
    assert_eq!(llm_keyboard::revoke(&mut scope, &visible, &old_grant), None);
    assert!(scope.entries().iter().find(|item| item.endpoint == first).unwrap().authorized);
}

#[test]
fn switching_context_dismisses_both_copies_of_the_confirmation() {
    let (mut scope, first, second) = configured();
    let visible = vec![first.clone(), second];
    let old = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    let mut current = Some(old.clone());
    llm_keyboard::dismiss(&mut scope, &mut current);
    assert!(current.is_none());
    assert!(scope.confirm(&old).is_none());
    let newer = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    assert!(llm_keyboard::confirm(&mut scope, &visible, Some(&newer), &old, false).is_none());
}

#[test]
fn focus_loss_clears_repository_selection_and_invalidates_the_model_confirmation() {
    let (mut scope, first, _) = configured();
    let visible = vec![first.clone()];
    let mut keyboard = ModelKeyboard::default();
    keyboard.begin(&scope, &visible);
    let old = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    let mut confirmation = Some(old.clone());
    let mut repository = fleet_studio::navigation::Selection::default();
    repository.next(1);
    assert_eq!(repository.index(), Some(0));
    llm_keyboard::focus_lost(&mut keyboard, &mut scope, &mut confirmation, &mut repository);
    assert!(!keyboard.active());
    assert_eq!(repository.index(), None);
    assert!(confirmation.is_none());
    assert!(scope.confirm(&old).is_none());
}

#[test]
fn a_mouse_handler_from_before_a_hidden_endpoint_reappeared_is_stale() {
    let (mut scope, first, second) = configured();
    let mut gate = ModelViewGate::default();
    let visible = vec![first.clone(), second.clone()];
    gate.observe(&visible);
    let old = gate.ticket();
    gate.observe(std::slice::from_ref(&second));
    gate.observe(&visible);
    assert!(!gate.is_current(&old));
    let stale_confirmation = gate.is_current(&old).then(|| llm_keyboard::prepare(&mut scope, &visible, &first, false)).flatten();
    assert!(stale_confirmation.is_none());
    let fresh = gate.ticket();
    assert!(gate.is_current(&fresh));
    assert!(llm_keyboard::prepare(&mut scope, &visible, &first, false).is_some());
}

#[test]
fn a_mouse_revoke_from_an_old_view_cannot_revoke_a_still_current_grant() {
    let (mut scope, first, second) = configured();
    let visible = vec![first.clone(), second.clone()];
    let confirmation = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    let _ = llm_keyboard::confirm(&mut scope, &visible, Some(&confirmation), &confirmation, false).unwrap();
    let grant = scope.entries().iter().find(|item| item.endpoint == first).unwrap().grant().unwrap().clone();
    let mut gate = ModelViewGate::default();
    gate.observe(&visible);
    let old = gate.ticket();
    gate.observe(std::slice::from_ref(&second));
    gate.observe(&visible);
    let stale_revoke = gate.is_current(&old).then(|| llm_keyboard::revoke(&mut scope, &visible, &grant)).flatten();
    assert!(stale_revoke.is_none());
    assert!(scope.entries().iter().find(|item| item.endpoint == first).unwrap().authorized);
    let current = gate.ticket();
    assert!(gate.is_current(&current));
    assert_eq!(llm_keyboard::revoke(&mut scope, &visible, &grant), Some(ModelRevocation::Prevented));
}

#[test]
fn selection_from_an_old_grant_does_not_attach_to_a_new_grant() {
    let (mut scope, first, _) = configured();
    let visible = vec![first.clone()];
    let confirmation = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    let _ = llm_keyboard::confirm(&mut scope, &visible, Some(&confirmation), &confirmation, false).unwrap();
    let old = scope.entries().iter().find(|item| item.endpoint == first).unwrap().grant().unwrap().clone();
    let mut selection = ModelKeyboard::default();
    selection.begin(&scope, &visible);
    assert_eq!(selection.current(&scope, &visible), Some(&first));
    assert!(llm_keyboard::revoke(&mut scope, &visible, &old).is_some());
    let newer = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    let _ = llm_keyboard::confirm(&mut scope, &visible, Some(&newer), &newer, false).unwrap();
    assert!(selection.reconcile(&scope, &visible));
    assert!(selection.active());
    assert_eq!(selection.current(&scope, &visible), None);
    assert!(llm_keyboard::revoke(&mut scope, &visible, &old).is_none());
}

#[test]
fn pending_refresh_blocks_confirmation_but_keeps_current_revocation() {
    let (mut scope, first, _) = configured();
    let visible = vec![first.clone()];
    assert!(llm_keyboard::prepare(&mut scope, &visible, &first, true).is_none());
    let confirmation = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    assert!(llm_keyboard::confirm(&mut scope, &visible, Some(&confirmation), &confirmation, true).is_none());
    let _ = llm_keyboard::confirm(&mut scope, &visible, Some(&confirmation), &confirmation, false).unwrap();
    let grant = scope.entries().iter().find(|item| item.endpoint == first).unwrap().grant().unwrap().clone();
    assert_eq!(llm_keyboard::revoke(&mut scope, &visible, &grant), Some(ModelRevocation::Prevented));
}

#[test]
fn state_filter_hiding_a_just_authorized_endpoint_never_selects_another() {
    let (mut scope, first, second) = configured();
    assert!(llm_keyboard::matches("未照会", &first.label, "未照会"));
    assert!(!llm_keyboard::matches("未照会", &first.label, "取得中"));
    let before = vec![first.clone(), second.clone()];
    let mut selection = ModelKeyboard::default();
    selection.begin(&scope, &before);
    let confirmation = llm_keyboard::prepare(&mut scope, &before, &first, false).unwrap();
    let _ = llm_keyboard::confirm(&mut scope, &before, Some(&confirmation), &confirmation, false).unwrap();
    let after = vec![second.clone()];
    selection.authorized(&scope, &after, &first);
    assert!(selection.active());
    assert_eq!(selection.current(&scope, &after), None);
    assert_eq!(selection.current(&scope, &before), None);
}

#[test]
fn selected_endpoint_remains_identifiable_when_a_notice_is_shown() {
    let (_, first, _) = configured();
    let message = llm_keyboard::banner(true, Some(&first), None, true, Some("fixture notice")).unwrap();
    assert!(message.contains(&first.label));
    assert!(message.contains(&first.address.to_string()));
    assert!(message.contains(first.protocol.label()));
    assert!(message.contains("fixture notice"));
    assert!(llm_keyboard::banner(true, None, None, false, None).is_some());
}

#[test]
fn real_scope_still_dispatches_an_explicitly_authorized_synthetic_query() {
    let (mut scope, first, _) = configured();
    let visible = vec![first.clone()];
    let confirmation = llm_keyboard::prepare(&mut scope, &visible, &first, false).unwrap();
    let request = llm_keyboard::confirm(&mut scope, &visible, Some(&confirmation), &confirmation, false).unwrap();
    assert!(scope.dispatch(&request));
    assert_eq!(request.start(|_| 7), Some(7));
    assert!(scope.finish(&request, Ok(Vec::new()), 1));
}

#[test]
fn sample_selection_and_confirmation_cannot_dispatch_a_real_query() {
    let mut scope = ModelScope::sample();
    let endpoint = scope.entries()[0].endpoint.clone();
    let visible = vec![endpoint.clone()];
    let mut selection = ModelKeyboard::default();
    selection.begin(&scope, &visible);
    selection.cycle(&scope, &visible, false);
    assert_eq!(selection.current(&scope, &visible), Some(&endpoint));
    let confirmation = llm_keyboard::prepare(&mut scope, &visible, &endpoint, false).unwrap();
    let mut gate = ConfirmKeyGate::default();
    gate.arm(confirmation.clone());
    assert!(gate.down(Some(&confirmation), false, false));
    let request = llm_keyboard::confirm(&mut scope, &visible, Some(&confirmation), &confirmation, false).unwrap();
    let mut calls = 0;
    assert!(!scope.dispatch(&request));
    assert!(request.start(|_| calls += 1).is_none());
    assert_eq!(calls, 0);
    let grant = scope.entries()[0].grant().unwrap().clone();
    assert_eq!(llm_keyboard::revoke(&mut scope, &visible, &grant), Some(ModelRevocation::Prevented));
    selection.exit();
    scope.invalidate_for_refresh();
    assert!(!request.active());
    assert!(scope.refresh(Some(FIRST)).is_empty());
    assert_eq!(calls, 0);
}
