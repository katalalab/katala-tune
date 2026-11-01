use std::sync::Arc;

enum PressState {
    Up,
    Down(Option<Arc<()>>),
    Unknown,
}

/// Allows a distinct confirmation key only for the confirmation that armed it.
pub struct ConfirmKeyGate<T> {
    press: PressState,
    generation: Arc<()>,
    ready: bool,
    recovery: bool,
    recovery_owner: Option<Arc<()>>,
    confirmation: Option<T>,
}

impl<T> Default for ConfirmKeyGate<T> {
    fn default() -> Self {
        Self { press: PressState::Up, generation: Arc::new(()), ready: false, recovery: false, recovery_owner: None, confirmation: None }
    }
}

impl<T: PartialEq> ConfirmKeyGate<T> {
    pub fn down(&mut self, current: Option<&T>, held: bool, modified: bool) -> bool {
        let fresh = matches!(self.press, PressState::Up) && !held && !modified;
        let was_unknown = matches!(self.press, PressState::Unknown);
        if !matches!(self.press, PressState::Down(_)) {
            self.press = PressState::Down(self.confirmation.as_ref().map(|_| self.generation.clone()));
        }
        let matches_confirmation = current.is_some() && self.confirmation.as_ref() == current;
        if self.recovery && !held && !modified && matches_confirmation && (fresh || was_unknown) {
            self.recovery_owner = Some(self.generation.clone());
            return false;
        }
        if fresh && !was_unknown && self.ready && matches_confirmation {
            self.clear();
            true
        } else {
            false
        }
    }

    pub fn arm(&mut self, confirmation: T) {
        self.confirmation = Some(confirmation);
        self.generation = Arc::new(());
        self.ready = matches!(self.press, PressState::Up);
        self.recovery = matches!(self.press, PressState::Unknown);
        self.recovery_owner = None;
        if let PressState::Down(owner @ None) = &mut self.press {
            *owner = Some(self.generation.clone());
            self.recovery_owner = Some(self.generation.clone());
        }
    }

    pub fn up(&mut self) {
        if self.recovery_owner.as_ref().is_some_and(|owner| Arc::ptr_eq(owner, &self.generation)) {
            self.ready = true;
            self.recovery = false;
        }
        self.recovery_owner = None;
        if !matches!(self.press, PressState::Unknown) || self.confirmation.is_none() {
            self.press = PressState::Up;
        }
    }

    pub fn clear(&mut self) {
        self.confirmation = None;
        self.ready = false;
        self.recovery = false;
        self.recovery_owner = None;
    }

    pub fn focus_lost(&mut self) {
        self.clear();
        self.press = PressState::Unknown;
    }
}

/// Prevent repeated selection or action keys from moving to a different row.
#[derive(Default)]
pub struct NavigationKeyGate {
    up: bool,
    down: bool,
    enter: bool,
    revoke: bool,
    select: bool,
    refresh: bool,
    audit: bool,
    machine: bool,
    escape: bool,
    search: bool,
}

impl NavigationKeyGate {
    fn slot(&mut self, key: &str) -> Option<&mut bool> {
        match key {
            "up" => Some(&mut self.up),
            "down" => Some(&mut self.down),
            "enter" => Some(&mut self.enter),
            "x" => Some(&mut self.revoke),
            "l" => Some(&mut self.select),
            "r" => Some(&mut self.refresh),
            "a" => Some(&mut self.audit),
            "m" => Some(&mut self.machine),
            "escape" => Some(&mut self.escape),
            "/" | "slash" => Some(&mut self.search),
            _ => None,
        }
    }

    pub fn down(&mut self, key: &str, held: bool) -> bool {
        let Some(pressed) = self.slot(key) else { return !held };
        let fresh = !*pressed && !held;
        *pressed = true;
        fresh
    }

    pub fn up(&mut self, key: &str) {
        if let Some(pressed) = self.slot(key) {
            *pressed = false;
        }
    }

    pub fn focus_lost(&mut self) {
        self.up = true;
        self.down = true;
        self.enter = true;
        self.revoke = true;
        self.select = true;
        self.refresh = true;
        self.audit = true;
        self.machine = true;
        self.escape = true;
        self.search = true;
    }
}
