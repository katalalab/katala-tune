//! Invalidate the independent read scopes as soon as a full refresh is requested.
use crate::{
    llm::{ModelConfirmation, ModelScope, RefreshQueue},
    recent::{RecentGate, RecentSlot},
    scope::{AuditGate, Confirmations, RefreshGate},
};

pub struct FullRefresh<'a, T> {
    pub sample: bool,
    pub machine_confirmations: &'a mut Confirmations,
    pub dashboard_gate: &'a mut RefreshGate,
    pub scope_check_pending: &'a mut bool,
    pub generation: &'a mut u64,
    pub audit_gate: &'a mut AuditGate,
    pub audit: &'a mut Option<T>,
    pub recent: &'a mut RecentSlot,
    pub recent_gate: &'a mut RecentGate,
    pub models: &'a mut ModelScope,
    pub model_confirmation: &'a mut Option<ModelConfirmation>,
    pub model_queue: &'a mut RefreshQueue,
}

pub struct RefreshDecision {
    pub recent: Option<u64>,
    pub models_now: bool,
    pub had_audit: bool,
}

pub fn begin<T>(state: FullRefresh<'_, T>) -> RefreshDecision {
    state.machine_confirmations.clear();
    *state.model_confirmation = None;
    state.models.invalidate_for_refresh();
    let had_audit = state.audit.is_some();
    state.audit_gate.invalidate_and_clear(state.audit);
    state.recent_gate.invalidate();
    state.dashboard_gate.invalidate();
    *state.generation = state.generation.wrapping_add(1);
    if state.sample {
        return RefreshDecision { recent: None, models_now: false, had_audit };
    }
    *state.scope_check_pending = true;
    let recent = state.recent.request();
    let models_now = state.model_queue.request(state.models.querying());
    RefreshDecision { recent, models_now, had_audit }
}
