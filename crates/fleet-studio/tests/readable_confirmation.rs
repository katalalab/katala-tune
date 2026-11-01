use fleet_studio::Observation;
use fleet_studio::confirmation_keys::ConfirmKeyGate;
use fleet_studio::llm::{self, Endpoint, ModelScope};
use fleet_studio::llm_keyboard;
use fleet_studio::scope::{Confirmation, Confirmations, Grants};
use fleet_studio::source::{NodeCard, SavedState};
use fleet_studio::ui_consent::{
    self, ConfirmationAction, ConsentFrame, ConsentPointer, ConsentScreen, ConsentTarget, MachineConfirmationResult, ModelConfirmationResult, PointerMark,
    Presentation, ScrollMetrics, Viewport,
};

const MODEL: &str = "fixture model|10.0.0.101:12345|openai";

fn machine() -> (Confirmations, Grants, Confirmation) {
    let mut pending = Confirmations::default();
    let confirmation = pending.begin("fixture-node", "fixture-fingerprint").unwrap();
    (pending, Grants::default(), confirmation)
}

fn node() -> NodeCard {
    NodeCard {
        id: "fixture-node".into(),
        fingerprint: Some("fixture-fingerprint".into()),
        permit: None,
        saved_state: SavedState::Unapproved,
        previous: false,
        role: "research".into(),
        operating_state: "idle".into(),
        local: None,
        last_at: None,
        observation: Observation::Unknown,
        failure_reason: None,
        failure_at: None,
        max_age_ms: None,
        score: None,
        critical: None,
        warnings: None,
    }
}

fn model(sample: bool) -> (ModelScope, Endpoint, llm::ModelConfirmation) {
    let mut scope = if sample { ModelScope::sample() } else { ModelScope::default() };
    if !sample {
        let _unstarted_requests = scope.refresh(Some(MODEL));
    }
    let endpoint = if sample { scope.entries()[0].endpoint.clone() } else { llm::endpoint(MODEL).unwrap() };
    let confirmation = llm_keyboard::prepare(&mut scope, &[endpoint.clone()], &endpoint, false).unwrap();
    (scope, endpoint, confirmation)
}

fn viewport() -> Viewport {
    Viewport::new(542.0, 399.0)
}

fn metrics() -> ScrollMetrics {
    ScrollMetrics { bounds: viewport(), offset_x: 0.0, offset_y: 0.0, max_x: 0.0, max_y: 0.0 }
}

fn present(screen: &mut ConsentScreen) -> ConsentFrame {
    let frame = screen.present(viewport(), (0.0, 0.0)).unwrap();
    assert!(screen.acknowledge_layout(&frame, viewport(), metrics()));
    frame
}

#[test]
fn confirmation_without_rendered_scroll_bounds_is_rejected() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    screen.begin(target.clone());
    let frame = screen.present(viewport(), (0.0, 0.0)).unwrap();
    assert!(!screen.accepts(&frame, &target, viewport(), true, metrics()));
}

#[test]
fn scroll_must_reach_the_end_in_the_current_rendered_frame() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    screen.begin(target.clone());
    let frame = screen.present(viewport(), (0.0, 0.0)).unwrap();
    let top = ScrollMetrics { max_y: 500.0, ..metrics() };
    assert!(screen.acknowledge_layout(&frame, viewport(), top));
    assert!(!screen.accepts(&frame, &target, viewport(), true, top));
    let bottom = ScrollMetrics { offset_y: -500.0, ..top };
    assert!(!screen.accepts(&frame, &target, viewport(), true, bottom));
    let bottom_frame = screen.present(viewport(), (0.0, -500.0)).unwrap();
    assert!(screen.acknowledge_layout(&bottom_frame, viewport(), bottom));
    assert!(screen.accepts(&bottom_frame, &target, viewport(), true, bottom));
    let next = screen.present(viewport(), (0.0, -499.0)).unwrap();
    assert!(!screen.accepts(&bottom_frame, &target, viewport(), true, bottom));
    assert!(!screen.accepts(&next, &target, viewport(), true, bottom));
    assert!(!screen.acknowledge_layout(&bottom_frame, viewport(), bottom));
}

#[test]
fn mismatched_or_invalid_scroll_measurements_cannot_acknowledge_a_frame() {
    let (_, _, confirmation) = machine();
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Machine(confirmation));
    let frame = screen.present(viewport(), (0.0, 0.0)).unwrap();
    assert!(!screen.acknowledge_layout(&frame, viewport(), ScrollMetrics { bounds: Viewport::new(0.0, 0.0), ..metrics() }));
    assert!(!screen.acknowledge_layout(&frame, viewport(), ScrollMetrics { offset_y: f32::NAN, ..metrics() }));
    assert!(!screen.acknowledge_layout(&frame, Viewport::new(541.0, 399.0), metrics()));
}

#[test]
fn scroll_changed_after_render_cannot_be_acknowledged_as_the_rendered_frame() {
    let (_, _, confirmation) = machine();
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Machine(confirmation));
    let frame = screen.present(viewport(), (0.0, 0.0)).unwrap();
    let after_input = ScrollMetrics { offset_y: -500.0, max_y: 500.0, ..metrics() };
    assert!(!screen.acknowledge_layout(&frame, viewport(), after_input));
}

#[test]
fn horizontal_overflow_cannot_authorize_even_at_the_vertical_end() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    screen.begin(target.clone());
    let frame = screen.present(viewport(), (0.0, 0.0)).unwrap();
    let overflowing = ScrollMetrics { max_x: 1.1, ..metrics() };
    assert!(screen.acknowledge_layout(&frame, viewport(), overflowing));
    assert!(!screen.accepts(&frame, &target, viewport(), true, overflowing));
}

#[test]
fn small_rounding_tolerance_is_consistent_for_both_axes() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    screen.begin(target.clone());
    let frame = screen.present(viewport(), (0.0, 0.0)).unwrap();
    let short = ScrollMetrics { max_x: 0.5, max_y: 0.5, ..metrics() };
    assert!(screen.acknowledge_layout(&frame, viewport(), short));
    assert!(screen.accepts(&frame, &target, viewport(), true, short));
    let too_wide = ScrollMetrics { max_x: 1.1, ..short };
    assert!(screen.acknowledge_layout(&frame, viewport(), too_wide));
    assert!(!screen.accepts(&frame, &target, viewport(), true, too_wide));
    let too_tall = ScrollMetrics { max_y: 1.1, ..short };
    assert!(screen.acknowledge_layout(&frame, viewport(), too_tall));
    assert!(!screen.accepts(&frame, &target, viewport(), true, too_tall));
}

#[test]
fn identical_redraw_keeps_the_current_presented_frame_valid() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    screen.begin(target.clone());
    let frame = present(&mut screen);
    let redraw = screen.present(viewport(), (0.0, 0.0)).unwrap();
    assert!(screen.accepts(&frame, &target, viewport(), true, metrics()));
    assert!(screen.accepts(&redraw, &target, viewport(), true, metrics()));
}

#[test]
fn unchanged_redraw_preserves_a_matching_pointer_press() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    let mut pointer = ConsentPointer::default();
    screen.begin(target.clone());
    let frame = present(&mut screen);
    let mark = PointerMark { x: 40.0, y: 80.0, clicks: 1 };
    let down = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.down(down, &target, mark, true));
    let redraw = screen.present(viewport(), (0.0, 0.0)).unwrap();
    let up = Presentation { screen: &screen, frame: &redraw, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.release(up, &target, mark, true).is_some());
}

#[test]
fn pointer_press_is_rejected_after_scroll_or_viewport_change() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    let mut pointer = ConsentPointer::default();
    screen.begin(target.clone());
    let frame = present(&mut screen);
    let mark = PointerMark { x: 40.0, y: 80.0, clicks: 1 };
    let down = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.down(down, &target, mark, true));
    let scrolled = ScrollMetrics { offset_y: -1.0, max_y: 1.0, ..metrics() };
    let new_frame = screen.present(viewport(), (0.0, -1.0)).unwrap();
    assert!(screen.acknowledge_layout(&new_frame, viewport(), scrolled));
    let up = Presentation { screen: &screen, frame: &new_frame, viewport: viewport(), active: true, metrics: scrolled };
    assert!(pointer.release(up, &target, mark, true).is_none());
    let down = Presentation { screen: &screen, frame: &new_frame, viewport: viewport(), active: true, metrics: scrolled };
    assert!(pointer.down(down, &target, mark, true));
    let up = Presentation { screen: &screen, frame: &new_frame, viewport: Viewport::new(541.0, 399.0), active: true, metrics: scrolled };
    assert!(pointer.release(up, &target, mark, true).is_none());
}

#[test]
fn pointer_press_is_rejected_after_target_change_or_cancellation() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation.clone());
    let mut screen = ConsentScreen::default();
    let mut pointer = ConsentPointer::default();
    screen.begin(target.clone());
    let frame = present(&mut screen);
    let mark = PointerMark { x: 40.0, y: 80.0, clicks: 1 };
    let down = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.down(down, &target, mark, true));
    let mut newer = confirmation;
    newer.nonce += 1;
    let new_target = ConsentTarget::Machine(newer);
    screen.begin(new_target.clone());
    let new_frame = present(&mut screen);
    let up = Presentation { screen: &screen, frame: &new_frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.release(up, &new_target, mark, true).is_none());
    pointer.clear();
    let up = Presentation { screen: &screen, frame: &new_frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.release(up, &target, mark, true).is_none());
}

#[test]
fn pointer_rejects_double_click_inactive_or_unmatched_events() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    let mut pointer = ConsentPointer::default();
    screen.begin(target.clone());
    let frame = present(&mut screen);
    let double = PointerMark { x: 40.0, y: 80.0, clicks: 2 };
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(!pointer.down(shown, &target, double, true));
    let mark = PointerMark { clicks: 1, ..double };
    let inactive = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: false, metrics: metrics() };
    assert!(!pointer.down(inactive, &target, mark, true));
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.release(shown, &target, mark, true).is_none());
}

#[test]
fn pointer_requires_primary_down_and_primary_up_and_discards_other_release() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    let mut pointer = ConsentPointer::default();
    screen.begin(target.clone());
    let frame = present(&mut screen);
    let mark = PointerMark { x: 40.0, y: 80.0, clicks: 1 };
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(!pointer.down(shown, &target, mark, false));
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.down(shown, &target, mark, true));
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.release(shown, &target, mark, false).is_none());
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.release(shown, &target, mark, true).is_none());
}

#[test]
fn pointer_rejects_changed_layout_even_when_offset_is_unchanged() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    let mut pointer = ConsentPointer::default();
    screen.begin(target.clone());
    let frame = present(&mut screen);
    let mark = PointerMark { x: 40.0, y: 80.0, clicks: 1 };
    let down = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(pointer.down(down, &target, mark, true));
    let changed = ScrollMetrics { max_y: 0.5, ..metrics() };
    assert!(screen.acknowledge_layout(&frame, viewport(), changed));
    let up = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: changed };
    assert!(pointer.release(up, &target, mark, true).is_none());
}

#[test]
fn real_preparation_can_begin_for_a_readable_confirmation() {
    assert!(ui_consent::preparation_available(false));
}

#[test]
fn confirmation_cannot_start_while_search_ime_is_composing() {
    assert!(!ui_consent::can_prepare_confirmation(true));
    assert!(ui_consent::can_prepare_confirmation(false));
}

#[test]
fn stale_machine_click_is_rejected_when_search_no_longer_shows_target() {
    let node = node();
    assert!(node.is_visible_target("fixture-node", "fixture-fingerprint", "research"));
    assert!(!node.is_visible_target("fixture-node", "fixture-fingerprint", "different"));
    assert!(!node.is_visible_target("fixture-node", "other-fingerprint", "research"));
    assert!(!node.is_visible_target("other-node", "fixture-fingerprint", "research"));
}

#[test]
fn viewport_requires_finite_minimum_dimensions() {
    assert!(Viewport::new(320.0, 200.0).usable());
    assert!(!Viewport::new(319.0, 200.0).usable());
    assert!(!Viewport::new(320.0, 199.0).usable());
    assert!(!Viewport::new(f32::NAN, 399.0).usable());
    assert!(!Viewport::new(f32::INFINITY, 399.0).usable());
}

#[test]
fn presentation_must_match_frame_target_viewport_and_active_window() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation.clone());
    let mut screen = ConsentScreen::default();
    screen.begin(target.clone());
    assert!(screen.present(Viewport::new(100.0, 100.0), (0.0, 0.0)).is_none());
    let frame = present(&mut screen);
    assert!(screen.accepts(&frame, &target, viewport(), true, metrics()));
    assert!(!screen.accepts(&frame, &target, viewport(), false, metrics()));
    assert!(!screen.accepts(&frame, &target, Viewport::new(541.0, 399.0), true, metrics()));
    let (_, _, other) = machine();
    let mut changed = other;
    changed.id = "different-fixture".into();
    assert!(!screen.accepts(&frame, &ConsentTarget::Machine(changed), viewport(), true, metrics()));
}

#[test]
fn old_frames_are_rejected_after_scroll_reopen_and_invalidation() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    screen.begin(target.clone());
    let old = present(&mut screen);
    let newer = screen.present(viewport(), (0.0, -1.0)).unwrap();
    let scrolled = ScrollMetrics { offset_y: -1.0, max_y: 1.0, ..metrics() };
    assert!(screen.acknowledge_layout(&newer, viewport(), scrolled));
    assert!(!screen.accepts(&old, &target, viewport(), true, scrolled));
    assert!(screen.accepts(&newer, &target, viewport(), true, scrolled));
    screen.invalidate();
    assert!(!screen.accepts(&newer, &target, viewport(), true, metrics()));
    screen.begin(target.clone());
    let reopened = present(&mut screen);
    assert!(!screen.accepts(&newer, &target, viewport(), true, metrics()));
    assert!(screen.accepts(&reopened, &target, viewport(), true, metrics()));
}

#[test]
fn machine_permission_requires_the_matching_current_presentation() {
    let (mut pending, mut grants, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation.clone());
    let mut screen = ConsentScreen::default();
    screen.begin(target);
    let frame = present(&mut screen);
    let offscreen = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: false, metrics: metrics() };
    assert_eq!(ui_consent::confirm_machine_presented(offscreen, &mut pending, &mut grants, &confirmation), MachineConfirmationResult::Stale);
    assert!(grants.matching("fixture-node", "fixture-fingerprint").is_none());
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert_eq!(ui_consent::confirm_machine_presented(shown, &mut pending, &mut grants, &confirmation), MachineConfirmationResult::Authorized);
    assert!(grants.matching("fixture-node", "fixture-fingerprint").is_some());
}

#[test]
fn old_machine_callback_cannot_authorize_a_newer_pending_confirmation() {
    let (mut pending, mut grants, old) = machine();
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Machine(old.clone()));
    let frame = present(&mut screen);
    let newer = pending.begin("fixture-node", "fixture-fingerprint").unwrap();
    screen.begin(ConsentTarget::Machine(newer.clone()));
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert_eq!(ui_consent::confirm_machine_presented(shown, &mut pending, &mut grants, &old), MachineConfirmationResult::Stale);
    assert_eq!(pending.current(), Some(&newer));
    assert!(grants.matching("fixture-node", "fixture-fingerprint").is_none());
}

#[test]
fn cancelled_machine_confirmation_cannot_authorize() {
    let (mut pending, mut grants, confirmation) = machine();
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Machine(confirmation.clone()));
    let frame = present(&mut screen);
    assert!(pending.cancel_if(&confirmation));
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert_eq!(ui_consent::confirm_machine_presented(shown, &mut pending, &mut grants, &confirmation), MachineConfirmationResult::Stale);
    assert!(grants.matching("fixture-node", "fixture-fingerprint").is_none());
}

#[test]
fn sample_machine_permission_can_be_revoked_without_io() {
    let (mut pending, mut grants, confirmation) = machine();
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Machine(confirmation.clone()));
    let frame = present(&mut screen);
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert_eq!(ui_consent::confirm_machine_presented(shown, &mut pending, &mut grants, &confirmation), MachineConfirmationResult::Authorized);
    let permit = grants.matching("fixture-node", "fixture-fingerprint").unwrap();
    assert!(grants.revoke_if_current("fixture-node", &permit));
    assert!(grants.matching("fixture-node", "fixture-fingerprint").is_none());
}

#[test]
fn model_permission_requires_the_matching_current_presentation() {
    let (mut scope, endpoint, confirmation) = model(false);
    let visible = [endpoint.clone()];
    let mut current = Some(confirmation.clone());
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Model(confirmation.clone()));
    let frame = present(&mut screen);
    let inactive = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: false, metrics: metrics() };
    assert!(matches!(ui_consent::confirm_model_presented(inactive, &mut scope, &visible, &mut current, &confirmation, false), ModelConfirmationResult::Stale));
    assert!(!scope.entries().iter().find(|entry| entry.endpoint == endpoint).unwrap().authorized);
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(matches!(
        ui_consent::confirm_model_presented(shown, &mut scope, &visible, &mut current, &confirmation, false),
        ModelConfirmationResult::Authorized(_)
    ));
}

#[test]
fn cancelled_or_refresh_pending_model_confirmation_cannot_authorize() {
    let (mut scope, endpoint, confirmation) = model(false);
    let visible = [endpoint.clone()];
    let mut current = Some(confirmation.clone());
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Model(confirmation.clone()));
    let frame = present(&mut screen);
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(matches!(ui_consent::confirm_model_presented(shown, &mut scope, &visible, &mut current, &confirmation, true), ModelConfirmationResult::Stale));
    llm_keyboard::dismiss(&mut scope, &mut current);
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(matches!(ui_consent::confirm_model_presented(shown, &mut scope, &visible, &mut current, &confirmation, false), ModelConfirmationResult::Stale));
    assert!(!scope.entries().iter().find(|entry| entry.endpoint == endpoint).unwrap().authorized);
}

#[test]
fn old_model_frame_cannot_authorize_a_newer_confirmation() {
    let (mut scope, endpoint, old) = model(false);
    let visible = [endpoint.clone()];
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Model(old.clone()));
    let frame = present(&mut screen);
    let newer = llm_keyboard::prepare(&mut scope, &visible, &endpoint, false).unwrap();
    let mut current = Some(newer.clone());
    screen.begin(ConsentTarget::Model(newer.clone()));
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    assert!(matches!(ui_consent::confirm_model_presented(shown, &mut scope, &visible, &mut current, &old, false), ModelConfirmationResult::Stale));
    assert_eq!(current, Some(newer));
    assert!(!scope.entries().iter().find(|entry| entry.endpoint == endpoint).unwrap().authorized);
}

#[test]
fn sample_confirmation_does_not_dispatch_a_model_request() {
    let (mut scope, endpoint, confirmation) = model(true);
    let mut current = Some(confirmation.clone());
    let mut screen = ConsentScreen::default();
    screen.begin(ConsentTarget::Model(confirmation.clone()));
    let frame = present(&mut screen);
    let shown = Presentation { screen: &screen, frame: &frame, viewport: viewport(), active: true, metrics: metrics() };
    let ModelConfirmationResult::Authorized(request) = ui_consent::confirm_model_presented(shown, &mut scope, &[endpoint], &mut current, &confirmation, false)
    else {
        panic!("expected sample authorization");
    };
    assert!(!scope.dispatch(&request));
    let mut called = false;
    assert!(request.start(|_| called = true).is_none());
    assert!(!called);
}

#[test]
fn modal_keys_do_not_forward_ime_or_held_confirmation() {
    assert_eq!(ui_consent::confirmation_action("y", false, false, false), ConfirmationAction::Confirm);
    assert_eq!(ui_consent::confirmation_action("y", true, false, false), ConfirmationAction::Ignore);
    assert_eq!(ui_consent::confirmation_action("y", false, true, false), ConfirmationAction::Ignore);
    assert_eq!(ui_consent::confirmation_action("y", false, false, true), ConfirmationAction::Ignore);
    assert_eq!(ui_consent::confirmation_action("enter", false, false, false), ConfirmationAction::Ignore);
    assert_eq!(ui_consent::confirmation_action("escape", false, false, false), ConfirmationAction::Cancel);
    assert_eq!(ui_consent::confirmation_action("r", false, false, false), ConfirmationAction::Refresh);
    assert_eq!(ui_consent::confirmation_action("down", false, false, false), ConfirmationAction::Down);
    assert_eq!(ui_consent::confirmation_action("a", false, false, false), ConfirmationAction::Ignore);
}

#[test]
fn early_confirmation_key_can_recover_after_release_and_rendered_scroll_end() {
    let (_, _, confirmation) = machine();
    let target = ConsentTarget::Machine(confirmation);
    let mut screen = ConsentScreen::default();
    let mut gate = ConfirmKeyGate::default();
    gate.arm(target.clone());
    screen.begin(target.clone());
    let top_frame = screen.present(viewport(), (0.0, 0.0)).unwrap();
    let top = ScrollMetrics { max_y: 500.0, ..metrics() };
    assert!(screen.acknowledge_layout(&top_frame, viewport(), top));
    let top_presentation = Presentation { screen: &screen, frame: &top_frame, viewport: viewport(), active: true, metrics: top };
    assert!(!ui_consent::confirmation_key_down(top_presentation, &mut gate, &target, false, false));
    gate.up();
    let bottom_frame = screen.present(viewport(), (0.0, -500.0)).unwrap();
    let bottom = ScrollMetrics { offset_y: -500.0, ..top };
    assert!(screen.acknowledge_layout(&bottom_frame, viewport(), bottom));
    let bottom_presentation = Presentation { screen: &screen, frame: &bottom_frame, viewport: viewport(), active: true, metrics: bottom };
    assert!(ui_consent::confirmation_key_down(bottom_presentation, &mut gate, &target, false, false));
}

#[test]
fn old_held_confirmation_requires_a_recovery_cycle_before_new_target_can_confirm() {
    let mut gate = ConfirmKeyGate::<u64>::default();
    gate.arm(41);
    assert!(!gate.down(None, false, true));
    gate.clear();
    gate.arm(42);
    assert!(!gate.down(Some(&42), true, false));
    gate.up();
    assert!(!gate.down(Some(&42), false, false));
    gate.up();
    assert!(gate.down(Some(&42), false, false));
}

#[test]
fn invisible_directional_characters_are_not_rendered_as_instructions() {
    let shown = ui_consent::disclosure_text("fixture\u{202e}abc");
    assert!(shown.contains("\\u{202e}"));
    assert!(!shown.contains('\u{202e}'));
}
