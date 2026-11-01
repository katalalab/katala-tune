//! Visible, generation-bound interaction with LLM endpoints.
use crate::llm::{Endpoint, ModelConfirmation, ModelEntry, ModelGrant, ModelQuery, ModelRevocation, ModelScope};
use std::sync::Arc;

#[derive(Clone)]
pub struct ModelViewTicket(Arc<()>);

pub struct ModelViewGate {
    current: Arc<()>,
    visible: Vec<Endpoint>,
}

impl Default for ModelViewGate {
    fn default() -> Self {
        Self { current: Arc::new(()), visible: Vec::new() }
    }
}

impl ModelViewGate {
    pub fn ticket(&self) -> ModelViewTicket {
        ModelViewTicket(self.current.clone())
    }

    pub fn is_current(&self, ticket: &ModelViewTicket) -> bool {
        Arc::ptr_eq(&self.current, &ticket.0)
    }

    pub fn invalidate(&mut self) {
        self.current = Arc::new(());
    }

    pub fn observe(&mut self, visible: &[Endpoint]) {
        if self.visible != visible {
            self.visible = visible.to_vec();
            self.invalidate();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelKey {
    Select,
    Next,
    Previous,
    Inspect,
    Confirm,
    Revoke,
    Leave,
    Consume,
    Unsupported,
}

pub fn consume_blocked_shortcut(key: &str, editing: bool, blocked: bool) -> bool {
    !editing && blocked && matches!(key, "enter" | "up" | "down" | "r" | "a" | "m" | "l" | "x" | "y" | "escape" | "/" | "slash")
}

pub fn key(key: &str, active: bool, editing: bool, blocked: bool, windows: bool) -> Option<ModelKey> {
    if editing {
        return None;
    }
    if !windows {
        return if active {
            Some(ModelKey::Consume)
        } else if key == "l" && !blocked {
            Some(ModelKey::Unsupported)
        } else {
            None
        };
    }
    if blocked {
        return active.then_some(ModelKey::Consume);
    }
    match key {
        "l" => Some(ModelKey::Select),
        "down" if active => Some(ModelKey::Next),
        "up" if active => Some(ModelKey::Previous),
        "enter" if active => Some(ModelKey::Inspect),
        "y" if active => Some(ModelKey::Confirm),
        "x" if active => Some(ModelKey::Revoke),
        "escape" if active => Some(ModelKey::Leave),
        _ => None,
    }
}

pub fn matches(filter: &str, label: &str, status: &str) -> bool {
    let filter = filter.to_lowercase();
    filter.is_empty() || label.to_lowercase().contains(&filter) || status.to_lowercase().contains(&filter)
}

pub fn banner(active: bool, endpoint: Option<&Endpoint>, confirmation: Option<&ModelConfirmation>, authorized: bool, notice: Option<&str>) -> Option<String> {
    if !active {
        return notice.map(str::to_owned);
    }
    let Some(endpoint) = endpoint else {
        return Some(notice.unwrap_or("追加先が未選択です。検索条件を確認し、上下キーで選択してください。").into());
    };
    let message = if let Some(confirmation) = confirmation.filter(|confirmation| confirmation.endpoint() == endpoint) {
        format!("接続内容を確認: {} 内容を確認後 Y で許可、Esc で中止。", confirmation.description())
    } else if authorized {
        format!("{} / {} / {} / 平文 HTTP: X で許可取消、Esc で選択終了。", endpoint.label, endpoint.address, endpoint.protocol.label())
    } else {
        format!("{} / {} / {} / 平文 HTTP: Enter で接続内容を確認。Esc で選択終了。", endpoint.label, endpoint.address, endpoint.protocol.label())
    };
    Some(match notice {
        Some(notice) => format!("{message} {notice}"),
        None => message,
    })
}

fn selectable<'a>(scope: &'a ModelScope, visible: &[Endpoint], endpoint: &Endpoint) -> Option<&'a ModelEntry> {
    if endpoint.address.ip().is_loopback() || !visible.contains(endpoint) {
        return None;
    }
    scope.entries().iter().find(|entry| &entry.endpoint == endpoint)
}

#[derive(Clone)]
struct Selection {
    endpoint: Endpoint,
    grant: Option<ModelGrant>,
}

#[derive(Default)]
pub struct ModelKeyboard {
    active: bool,
    selected: Option<Selection>,
}

impl ModelKeyboard {
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn begin(&mut self, scope: &ModelScope, visible: &[Endpoint]) {
        self.active = true;
        self.selected = visible
            .iter()
            .find_map(|endpoint| selectable(scope, visible, endpoint).map(|entry| Selection { endpoint: endpoint.clone(), grant: entry.grant().cloned() }));
    }

    pub fn current<'a>(&'a self, scope: &ModelScope, visible: &[Endpoint]) -> Option<&'a Endpoint> {
        let selected = self.selected.as_ref()?;
        let entry = selectable(scope, visible, &selected.endpoint)?;
        (entry.grant() == selected.grant.as_ref()).then_some(&selected.endpoint)
    }

    pub fn reconcile(&mut self, scope: &ModelScope, visible: &[Endpoint]) -> bool {
        if self.selected.is_some() && self.current(scope, visible).is_none() {
            self.selected = None;
            true
        } else {
            false
        }
    }

    pub fn cycle(&mut self, scope: &ModelScope, visible: &[Endpoint], reverse: bool) {
        let choices: Vec<&Endpoint> = visible.iter().filter(|endpoint| selectable(scope, visible, endpoint).is_some()).collect();
        if choices.is_empty() {
            self.selected = None;
            return;
        }
        let current = self.current(scope, visible).and_then(|endpoint| choices.iter().position(|choice| *choice == endpoint));
        let index = match (current, reverse) {
            (Some(0), true) | (None, true) => choices.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % choices.len(),
            (None, false) => 0,
        };
        let endpoint = choices[index];
        self.selected = selectable(scope, visible, endpoint).map(|entry| Selection { endpoint: endpoint.clone(), grant: entry.grant().cloned() });
    }

    pub fn authorized(&mut self, scope: &ModelScope, visible: &[Endpoint], endpoint: &Endpoint) {
        if let Some(selected) = self.selected.as_mut().filter(|selected| &selected.endpoint == endpoint) {
            if let Some(entry) = selectable(scope, visible, endpoint) {
                selected.grant = entry.grant().cloned();
            } else {
                self.selected = None;
            }
        }
    }

    pub fn exit(&mut self) {
        self.active = false;
        self.selected = None;
    }
}

pub fn prepare(scope: &mut ModelScope, visible: &[Endpoint], endpoint: &Endpoint, pending: bool) -> Option<ModelConfirmation> {
    if pending || selectable(scope, visible, endpoint).is_none() {
        return None;
    }
    scope.prepare_authorization(endpoint)
}

pub fn confirm(
    scope: &mut ModelScope,
    visible: &[Endpoint],
    current: Option<&ModelConfirmation>,
    confirmation: &ModelConfirmation,
    pending: bool,
) -> Option<ModelQuery> {
    if pending || current != Some(confirmation) || selectable(scope, visible, confirmation.endpoint()).is_none() {
        return None;
    }
    scope.confirm(confirmation)
}

pub fn cancel(scope: &mut ModelScope, visible: &[Endpoint], current: Option<&ModelConfirmation>, confirmation: &ModelConfirmation) -> bool {
    current == Some(confirmation) && selectable(scope, visible, confirmation.endpoint()).is_some() && scope.cancel_confirmation(confirmation)
}

pub fn dismiss(scope: &mut ModelScope, current: &mut Option<ModelConfirmation>) {
    if let Some(confirmation) = current.take() {
        scope.cancel_confirmation(&confirmation);
    }
}

pub fn focus_lost(
    keyboard: &mut ModelKeyboard,
    scope: &mut ModelScope,
    confirmation: &mut Option<ModelConfirmation>,
    repository: &mut crate::navigation::Selection,
) {
    keyboard.exit();
    dismiss(scope, confirmation);
    repository.invalidate();
}

pub fn revoke(scope: &mut ModelScope, visible: &[Endpoint], grant: &ModelGrant) -> Option<ModelRevocation> {
    selectable(scope, visible, grant.endpoint())?;
    scope.revoke(grant)
}
