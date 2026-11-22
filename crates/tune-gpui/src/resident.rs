//! One-window admission shared by native Open, Reopen, tray and secondary launch.
//! No Engine/Live methods belong here: changing visibility grants no permission.
pub struct Windows<T> {
    pub current: Option<T>,
    pub resident: bool,
    pub exiting: bool,
}
impl<T> Windows<T> {
    pub fn new(resident: bool) -> Self {
        Self { current: None, resident, exiting: false }
    }
    /// Called after the actual window has released its UI owner and gate.
    pub fn closed(&mut self) -> bool {
        self.current = None;
        if !self.resident {
            self.exiting = true;
        }
        self.exiting
    }
    pub fn quit(&mut self) {
        self.exiting = true;
    }
}

/// The production AppKit/Win32 adapter and fake UI exercise this same admission.
pub trait Backend<T: Copy> {
    type Target;
    type Error;
    fn activate(&mut self, handle: T, target: Option<&Self::Target>) -> bool;
    fn create(&mut self, target: Option<&Self::Target>) -> Result<T, Self::Error>;
}
impl<T: Copy> Windows<T> {
    pub fn open<B: Backend<T>>(&mut self, backend: &mut B, target: Option<&B::Target>) -> Result<Option<T>, B::Error> {
        if self.exiting {
            return Ok(None);
        }
        if let Some(window) = self.current {
            if backend.activate(window, target) {
                return Ok(Some(window));
            }
            self.current = None;
        }
        let window = backend.create(target)?;
        self.current = Some(window);
        Ok(Some(window))
    }
}
