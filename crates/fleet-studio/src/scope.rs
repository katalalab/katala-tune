//! Session-only access to saved machine observations.
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Clone, Debug)]
pub struct Permit {
    fingerprint: String,
    serial: u64,
    active: Arc<AtomicBool>,
    gate: Arc<Mutex<()>>,
}

impl Permit {
    pub fn serial(&self) -> u64 {
        self.serial
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    pub(crate) fn cancel(&self) {
        self.active.store(false, Ordering::SeqCst);
    }

    /// A cancellation never waits for a read. A read already inside this gate
    /// may finish, but later calls fail before accessing the database.
    pub fn read<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let _guard = self.gate.lock().ok()?;
        self.is_active().then(f)
    }

    pub(crate) fn same_as(&self, other: &Self) -> bool {
        self.is_active() && other.is_active() && Arc::ptr_eq(&self.active, &other.active)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Grants {
    entries: HashMap<String, Permit>,
    next_serial: Arc<AtomicU64>,
}

impl Grants {
    /// This records only the operator's action; the source validates the
    /// fingerprint and uniqueness again before reading a particular ID.
    pub fn authorize(&mut self, id: &str, fingerprint: &str) -> bool {
        let Ok(previous) = self.next_serial.try_update(Ordering::SeqCst, Ordering::SeqCst, |serial| serial.checked_add(1)) else { return false };
        let serial = previous + 1;
        self.revoke(id);
        self.entries.insert(
            id.to_owned(),
            Permit { fingerprint: fingerprint.to_owned(), serial, active: Arc::new(AtomicBool::new(true)), gate: Arc::new(Mutex::new(())) },
        );
        true
    }

    pub fn revoke(&mut self, id: &str) {
        if let Some(permit) = self.entries.remove(id) {
            permit.cancel();
        }
    }

    pub fn revoke_if_current(&mut self, id: &str, permit: &Permit) -> bool {
        if self.contains(id, permit) {
            self.revoke(id);
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        for (_, permit) in self.entries.drain() {
            permit.cancel();
        }
    }

    pub fn matching(&self, id: &str, fingerprint: &str) -> Option<Permit> {
        self.entries.get(id).filter(|permit| permit.is_active() && permit.fingerprint == fingerprint).cloned()
    }

    pub fn contains(&self, id: &str, permit: &Permit) -> bool {
        self.entries.get(id).is_some_and(|current| current.same_as(permit))
    }

    pub fn reconcile(&mut self, fingerprints: &HashMap<String, String>) {
        self.entries.retain(|id, permit| {
            let valid = permit.is_active() && fingerprints.get(id) == Some(&permit.fingerprint);
            if !valid {
                permit.cancel();
            }
            valid
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirmation {
    pub id: String,
    pub fingerprint: String,
    pub nonce: u64,
}

#[derive(Default)]
pub struct Confirmations {
    next_nonce: u64,
    pending: Option<Confirmation>,
}

impl Confirmations {
    pub fn begin(&mut self, id: &str, fingerprint: &str) -> Option<Confirmation> {
        let nonce = self.next_nonce.checked_add(1)?;
        self.next_nonce = nonce;
        let confirmation = Confirmation { id: id.into(), fingerprint: fingerprint.into(), nonce };
        self.pending = Some(confirmation.clone());
        Some(confirmation)
    }

    pub fn current(&self) -> Option<&Confirmation> {
        self.pending.as_ref()
    }

    pub fn take_if(&mut self, confirmation: &Confirmation) -> bool {
        if self.pending.as_ref() == Some(confirmation) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    pub fn cancel_if(&mut self, confirmation: &Confirmation) -> bool {
        self.take_if(confirmation)
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefreshTicket {
    sequence: u64,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefreshFinish {
    pub accept: bool,
    pub restart: bool,
}

#[derive(Default)]
pub struct RefreshGate {
    revision: u64,
    sequence: u64,
    active: bool,
    pending: bool,
}

impl RefreshGate {
    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn request(&mut self) -> Option<RefreshTicket> {
        if self.active {
            self.pending = true;
            return None;
        }
        self.active = true;
        self.pending = false;
        self.sequence = self.sequence.wrapping_add(1);
        Some(RefreshTicket { sequence: self.sequence, revision: self.revision })
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_current(&self, ticket: RefreshTicket) -> bool {
        self.active && self.sequence == ticket.sequence
    }

    pub fn finish(&mut self, ticket: RefreshTicket) -> Option<RefreshFinish> {
        if !self.is_current(ticket) {
            return None;
        }
        self.active = false;
        Some(RefreshFinish { accept: self.revision == ticket.revision, restart: std::mem::take(&mut self.pending) })
    }
}

/// Values may be drawn only while their exact grant is current and the
/// ledger is not being rechecked.
pub fn may_show_saved(grants: &Grants, id: &str, permit: Option<&Permit>, rechecking: bool) -> bool {
    !rechecking && permit.is_some_and(|permit| grants.contains(id, permit))
}

pub fn cycle_machine_confirmation<T: PartialEq>(
    confirmations: &mut Confirmations,
    gate: &mut crate::confirmation_keys::ConfirmKeyGate<T>,
    repository: &mut crate::navigation::Selection,
    choices: &[(String, String)],
    current: Option<&(String, String)>,
    reverse: bool,
) -> Option<(String, String)> {
    repository.invalidate();
    confirmations.clear();
    gate.clear();
    if choices.is_empty() {
        return None;
    }
    let current = current.and_then(|current| choices.iter().position(|choice| choice == current));
    let index = match (current, reverse) {
        (Some(0), true) | (None, true) => choices.len() - 1,
        (Some(index), true) => index - 1,
        (Some(index), false) => (index + 1) % choices.len(),
        (None, false) => 0,
    };
    Some(choices[index].clone())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MachineKey {
    Next,
    Previous,
    Activate,
    Confirm,
    Leave,
    Consume,
}

/// Suppress machine actions during text editing, key repeat, or modifier use.
pub fn machine_key(key: &str, active: bool, editing: bool, blocked: bool) -> Option<MachineKey> {
    if editing {
        return None;
    }
    if blocked {
        return (active && matches!(key, "enter" | "y" | "up" | "down" | "escape")).then_some(MachineKey::Consume);
    }
    match key {
        "m" => Some(MachineKey::Next),
        "down" if active => Some(MachineKey::Next),
        "up" if active => Some(MachineKey::Previous),
        "enter" if active => Some(MachineKey::Activate),
        "y" if active => Some(MachineKey::Confirm),
        "escape" if active => Some(MachineKey::Leave),
        _ => None,
    }
}

/// Confirmation text shown before reading a machine ID.
pub const MACHINE_CONFIRMATION: &str =
    "この台帳 ID に保存された過去の要約を読む権限と適用ルールを確認しました。許可中は更新時に再読取し、取消できます。保存元の実機は未確認です。";
/// Fixed keyboard confirmation, shown beside the selected ID.
pub const MACHINE_CONFIRMATION_KEYBOARD: &str =
    "保存要約の閲覧権限と適用ルールを確認した場合のみ Y で表示。許可中は更新時に再読取し、取消できます。保存元の実機は未確認。Esc で中止。";

#[derive(Clone, Debug)]
pub struct AuditTicket {
    active: Arc<AtomicBool>,
}

impl AuditTicket {
    pub fn is_current(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

/// Explicitly invalidates both displayed audit data and every old callback.
pub struct AuditGate {
    current: Arc<AtomicBool>,
}

impl Default for AuditGate {
    fn default() -> Self {
        Self { current: Arc::new(AtomicBool::new(true)) }
    }
}

impl AuditGate {
    pub fn ticket(&self) -> AuditTicket {
        AuditTicket { active: Arc::clone(&self.current) }
    }

    pub fn invalidate_and_clear<T>(&mut self, value: &mut Option<T>) {
        self.current.store(false, Ordering::SeqCst);
        *value = None;
        self.current = Arc::new(AtomicBool::new(true));
    }

    pub fn close(&self) {
        self.current.store(false, Ordering::SeqCst);
    }
}
