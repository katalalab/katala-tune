//! Closed COM entry and bounded reopen broker. No Engine, DB or GPUI dependency.
use super::{
    State,
    windows_notifications::{self as notifications, Boundary, Profile},
};
use std::{
    ffi::OsString,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Ordinary,
    Server,
}
pub(crate) fn mode(args: &[OsString]) -> Result<Mode, &'static str> {
    if !args.iter().any(|arg| arg.to_str().is_some_and(|arg| arg.starts_with("--notification-com-server") || arg.starts_with("-Embedding"))) {
        return Ok(Mode::Ordinary);
    }
    if args.first().is_some_and(|arg| arg == "--notification-com-server") && (args.len() == 1 || (args.len() == 2 && args[1] == "-Embedding")) {
        Ok(Mode::Server)
    } else {
        Err("invalid COM server arguments")
    }
}
#[derive(Clone, Copy)]
enum Reference {
    Object,
    Lock,
}
#[derive(Default)]
struct Pending {
    pending: bool,
    closed: bool,
    objects: usize,
    locks: usize,
}
#[derive(Default)]
struct Queue {
    state: Mutex<Pending>,
    wake: Condvar,
}
impl Queue {
    fn submit(&self, aumid: &[u16], token: &[u16], count: u32) -> Result<(), State> {
        if count != 0 || !aumid.iter().copied().eq(notifications::AUMID.encode_utf16()) || !token.iter().copied().eq(notifications::REOPEN.encode_utf16()) {
            return Err(State::Unsupported("通知の activation payload が一致しません".into()));
        }
        let mut state = self.state.lock().map_err(|_| State::Failed("通知 broker queue が停止しました".into()))?;
        if state.closed {
            return Err(State::Failed("通知 broker は終了中です".into()));
        }
        state.pending = true;
        self.wake.notify_one();
        Ok(())
    }
    fn reference(&self, kind: Reference, acquire: bool) -> Result<(), State> {
        let mut state = self.state.lock().map_err(|_| State::Failed("通知 broker queue が停止しました".into()))?;
        if acquire && state.closed {
            return Err(State::Failed("通知 broker は終了中です".into()));
        }
        let count = match kind {
            Reference::Object => &mut state.objects,
            Reference::Lock => &mut state.locks,
        };
        if acquire {
            *count += 1;
        } else {
            *count = count.saturating_sub(1);
        }
        Ok(())
    }
    fn close_if_idle(&self) -> bool {
        // The idle decision and close linearize with callback/object/lock
        // admission. An admitted request or reference wins over idle shutdown;
        // once closed, subsequent admissions fail before teardown starts.
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.pending || state.objects != 0 || state.locks != 0 {
            return false;
        }
        state.closed = true;
        true
    }
    fn complete(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).pending = false;
    }
    fn close(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).closed = true;
        self.wake.notify_all();
    }
}
const IDLE: Duration = Duration::from_secs(30);
const LIFETIME: Duration = Duration::from_secs(120);
trait Driver {
    fn start(&mut self, queue: Arc<Queue>) -> Result<(), State>;
    fn elapsed(&self) -> Duration;
    fn wait(&mut self, timeout: Duration) -> Result<bool, State>;
    fn reopen(&mut self) -> Result<(), State>;
    fn stop(&mut self) -> Result<(), State>;
}
fn serve(driver: &mut impl Driver) -> Result<(), State> {
    let queue = Arc::new(Queue::default());
    let result = (|| {
        driver.start(queue.clone())?;
        let mut last = driver.elapsed();
        loop {
            let now = driver.elapsed();
            if now >= LIFETIME {
                break;
            }
            let idle = now.saturating_sub(last);
            if idle >= IDLE && queue.close_if_idle() {
                break;
            }
            let timeout = if idle >= IDLE { LIFETIME - now } else { (IDLE - idle).min(LIFETIME - now) };
            if driver.wait(timeout)? {
                if driver.elapsed() >= LIFETIME {
                    break;
                }
                driver.reopen()?;
                queue.complete();
                last = driver.elapsed();
            }
        }
        Ok(())
    })();
    // Synchronizes with callback admission; no GUI response or client Release is
    // awaited. The driver revokes/drops its factory before leaving the apartment.
    queue.close();
    let stopped = driver.stop();
    result.and(stopped)
}
pub(super) trait Launcher: Boundary {
    fn spawn(&mut self, executable: &str, args: &[&str]) -> Result<(), State>;
}
pub(super) fn reopen(profile: &Profile, boundary: &mut impl Launcher) -> Result<(), State> {
    notifications::registration(profile, boundary)?;
    boundary.spawn(&profile.executable, &["--config", &profile.config, "--data", &profile.data, "--open", "dashboard"])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_com_argv_never_accepts_gui_configuration_or_embedding_alone() {
        for args in [vec!["--notification-com-server"], vec!["--notification-com-server", "-Embedding"]] {
            assert_eq!(mode(&args.into_iter().map(OsString::from).collect::<Vec<_>>()), Ok(Mode::Server));
        }
        for args in [
            vec!["-Embedding"],
            vec!["--notification-com-server=other"],
            vec!["-Embedding=other"],
            vec!["-Embedding", "--notification-com-server"],
            vec!["--notification-com-server", "--config", "private"],
            vec!["--notification-com-server", "-Embedding", "-Embedding"],
            vec!["--selftest", "--notification-com-server"],
        ] {
            assert!(mode(&args.into_iter().map(OsString::from).collect::<Vec<_>>()).is_err());
        }
        assert_eq!(mode(&["--selftest".into()]), Ok(Mode::Ordinary));
    }
    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    #[test]
    fn callback_is_closed_and_coalesces_until_reopen_finishes() {
        let q = Queue::default();
        for (aumid, token, count) in
            [("foreign", "reopen-v1", 0), (notifications::AUMID, "reopen-v1 --config other", 0), (notifications::AUMID, "reopen-v1", 1)]
        {
            assert!(q.submit(&utf16(aumid), &utf16(token), count).is_err());
            assert!(!q.state.lock().unwrap().pending);
        }
        assert!(q.submit(&[0xd800], &utf16("reopen-v1"), 0).is_err());
        for _ in 0..100 {
            q.submit(&utf16(notifications::AUMID), &utf16("reopen-v1"), 0).unwrap();
        }
        assert!(q.state.lock().unwrap().pending);
        q.complete();
        assert!(!q.state.lock().unwrap().pending);
        q.close();
        assert!(q.submit(&utf16(notifications::AUMID), &utf16("reopen-v1"), 0).is_err());
    }
    #[derive(Default)]
    struct Fake {
        log: Vec<&'static str>,
        now: Duration,
        queue: Option<Arc<Queue>>,
        fail: Option<&'static str>,
        busy: bool,
        held: bool,
        after_timeout: bool,
        reference_after_timeout: Option<Reference>,
    }
    impl Driver for Fake {
        fn start(&mut self, queue: Arc<Queue>) -> Result<(), State> {
            if self.held {
                queue.reference(Reference::Object, true)?;
            }
            self.queue = Some(queue);
            self.log.push("start");
            if self.fail == Some("start") { Err(State::Denied) } else { Ok(()) }
        }
        fn elapsed(&self) -> Duration {
            self.now
        }
        fn wait(&mut self, timeout: Duration) -> Result<bool, State> {
            self.log.push("wait");
            if self.fail == Some("wait") {
                return Err(State::Denied);
            }
            if self.queue.as_ref().unwrap().state.lock().unwrap().pending {
                return Ok(true);
            }
            if self.busy || self.fail == Some("spawn") {
                self.now += Duration::from_secs(1);
                self.queue.as_ref().unwrap().submit(&utf16(notifications::AUMID), &utf16("reopen-v1"), 0)?;
                Ok(true)
            } else {
                self.now += timeout;
                if self.after_timeout {
                    self.after_timeout = false;
                    // The OS wait has timed out; a callback is accepted before
                    // the worker decides whether to stop. Its client has released.
                    let queue = self.queue.as_ref().unwrap();
                    queue.reference(Reference::Object, true)?;
                    queue.submit(&utf16(notifications::AUMID), &utf16("reopen-v1"), 0)?;
                    queue.reference(Reference::Object, false)?;
                }
                if let Some(kind) = self.reference_after_timeout.take() {
                    self.queue.as_ref().unwrap().reference(kind, true)?;
                }
                Ok(false)
            }
        }
        fn reopen(&mut self) -> Result<(), State> {
            self.log.push("spawn");
            if self.fail == Some("spawn") { Err(State::Denied) } else { Ok(()) }
        }
        fn stop(&mut self) -> Result<(), State> {
            assert!(self.queue.as_ref().unwrap().state.lock().unwrap().closed);
            self.log.push("revoke/drop/apartment");
            if self.fail == Some("stop") { Err(State::Denied) } else { Ok(()) }
        }
    }
    #[test]
    fn pending_accepted_after_idle_timeout_is_reopened_before_idle_close() {
        let mut fake = Fake { after_timeout: true, ..Fake::default() };
        serve(&mut fake).unwrap();
        assert_eq!(fake.log.iter().filter(|entry| **entry == "spawn").count(), 1);
        assert_eq!(fake.now, IDLE + IDLE);
    }
    #[test]
    fn reference_admission_and_idle_close_share_one_decision() {
        for kind in [Reference::Object, Reference::Lock] {
            let queue = Queue::default();
            // Admission wins: idle may not close until the client releases.
            queue.reference(kind, true).unwrap();
            assert!(!queue.close_if_idle());
            queue.reference(kind, false).unwrap();
            // Idle wins: before any later teardown, native shared admission
            // must reject both CreateInstance and LockServer(true).
            assert!(queue.close_if_idle());
            assert!(queue.reference(kind, true).is_err());
            assert!(queue.submit(&utf16(notifications::AUMID), &utf16("reopen-v1"), 0).is_err());
        }
    }
    #[test]
    fn reference_admitted_after_timeout_prevents_ordinary_idle_shutdown() {
        for kind in [Reference::Object, Reference::Lock] {
            let mut fake = Fake { reference_after_timeout: Some(kind), ..Fake::default() };
            serve(&mut fake).unwrap();
            assert_eq!(fake.now, LIFETIME);
            assert!(!fake.log.contains(&"spawn"));
        }
    }
    #[test]
    fn lifecycle_closes_queue_and_cleans_up_on_idle_errors_and_absolute_deadline() {
        for fail in [None, Some("start"), Some("wait"), Some("spawn"), Some("stop")] {
            let mut fake = Fake { fail, ..Fake::default() };
            assert_eq!(serve(&mut fake).is_err(), fail.is_some());
            assert_eq!(fake.log.last(), Some(&"revoke/drop/apartment"));
        }
        let mut fake = Fake { busy: true, ..Fake::default() };
        serve(&mut fake).unwrap();
        assert_eq!(fake.now, LIFETIME);
        assert_eq!(fake.log.last(), Some(&"revoke/drop/apartment"));
        let mut held = Fake { held: true, ..Fake::default() };
        serve(&mut held).unwrap();
        assert_eq!(held.now, LIFETIME);
        assert!(!held.log.contains(&"spawn"));
        assert_eq!(held.log.last(), Some(&"revoke/drop/apartment"));
    }
}

#[cfg(target_os = "windows")]
pub(crate) mod native {
    #![allow(unsafe_code)] // COM ABI pointers are supplied by COM's marshaler.
    use super::*;
    use notifications::native::{Native, api, io_error};
    use std::{ffi::c_void, time::Instant};
    use windows::{
        Win32::{
            Foundation::{CLASS_E_NOAGGREGATION, E_INVALIDARG, E_POINTER, RPC_E_DISCONNECTED},
            System::Com::{CLSCTX_LOCAL_SERVER, CoRegisterClassObject, CoRevokeClassObject, IClassFactory, IClassFactory_Impl, REGCLS_MULTIPLEUSE},
            UI::Notifications::{INotificationActivationCallback, INotificationActivationCallback_Impl, NOTIFICATION_USER_INPUT_DATA},
        },
        core::{BOOL, GUID, IUnknown, Interface, PCWSTR, Ref, implement},
    };
    #[implement(INotificationActivationCallback)]
    struct Callback {
        queue: Arc<Queue>,
    }
    impl Drop for Callback {
        fn drop(&mut self) {
            let _ = self.queue.reference(Reference::Object, false);
        }
    }
    // Safety: non-null pointers must designate readable NUL-terminated UTF-16
    // per the native [in] LPCWSTR contract. Stop at NUL or the exact field cap;
    // never use unbounded PCWSTR::to_string or dereference input data. Rust cannot
    // validate arbitrary forged addresses; tests use owned buffers, never UB.
    unsafe fn bounded(value: PCWSTR, limit: usize) -> windows::core::Result<Vec<u16>> {
        if value.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let mut units = Vec::with_capacity(limit);
        for index in 0..=limit {
            let unit = unsafe { *value.0.add(index) };
            if unit == 0 {
                return Ok(units);
            }
            if index == limit {
                break;
            }
            units.push(unit);
        }
        Err(E_INVALIDARG.into())
    }
    impl INotificationActivationCallback_Impl for Callback_Impl {
        fn Activate(&self, aumid: &PCWSTR, args: &PCWSTR, _data: *const NOTIFICATION_USER_INPUT_DATA, count: u32) -> windows::core::Result<()> {
            if count != 0 {
                return Err(E_INVALIDARG.into());
            }
            let aumid = unsafe { bounded(*aumid, notifications::AUMID.len()) }?;
            let token = unsafe { bounded(*args, notifications::REOPEN.len()) }?;
            self.queue.submit(&aumid, &token, count).map_err(|_| E_INVALIDARG.into())
        }
    }
    #[implement(IClassFactory)]
    struct Factory {
        queue: Arc<Queue>,
    }
    impl IClassFactory_Impl for Factory_Impl {
        fn CreateInstance(&self, outer: Ref<'_, IUnknown>, iid: *const GUID, object: *mut *mut c_void) -> windows::core::Result<()> {
            // COM's [out] slot and [in] IID must point to valid storage. Nulls are
            // rejected and the out slot is cleared even for aggregation failure.
            if object.is_null() {
                return Err(E_POINTER.into());
            }
            unsafe {
                *object = std::ptr::null_mut();
            }
            if iid.is_null() {
                return Err(E_POINTER.into());
            }
            if !outer.is_null() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            self.queue.reference(Reference::Object, true).map_err(|_| windows::core::Error::from(RPC_E_DISCONNECTED))?;
            let callback: INotificationActivationCallback = Callback { queue: self.queue.clone() }.into();
            unsafe { callback.query(iid, object).ok() }
        }
        fn LockServer(&self, lock: BOOL) -> windows::core::Result<()> {
            self.queue.reference(Reference::Lock, lock.as_bool()).map_err(|_| RPC_E_DISCONNECTED.into())
        }
    }

    struct Registration {
        cookie: Option<u32>,
        _factory: IClassFactory,
    }
    impl Registration {
        fn revoke(&mut self) -> Result<(), State> {
            if let Some(cookie) = self.cookie {
                unsafe { CoRevokeClassObject(cookie) }.map_err(api)?;
                self.cookie = None;
            }
            Ok(())
        }
    }
    impl Drop for Registration {
        fn drop(&mut self) {
            let _ = self.revoke();
        }
    }
    impl Launcher for Native {
        fn spawn(&mut self, executable: &str, args: &[&str]) -> Result<(), State> {
            // Ordinary startup -> instance::claim before DB. No shell, inherited
            // user profile overrides, collector, foreground forcing or payload args.
            std::process::Command::new(executable).args(args).spawn().map(|_| ()).map_err(io_error)
        }
    }
    struct Runtime {
        started: Instant,
        queue: Option<Arc<Queue>>,
        registration: Option<Registration>,
        boundary: Option<Native>,
        profile: Option<Profile>,
    }
    impl Driver for Runtime {
        fn start(&mut self, queue: Arc<Queue>) -> Result<(), State> {
            self.queue = Some(queue.clone());
            let boundary = self.boundary.insert(Native::default());
            self.profile = Some(boundary.installed_profile()?);
            let factory: IClassFactory = Factory { queue }.into();
            // Same MTA initialized by Native::installation. CLSID from the one
            // shared installation contract, never an independently copied GUID.
            let clsid = GUID::try_from(notifications::CLSID.trim_matches(['{', '}'])).map_err(api)?;
            let cookie = unsafe { CoRegisterClassObject(&clsid, &factory, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE) }.map_err(api)?;
            self.registration = Some(Registration { cookie: Some(cookie), _factory: factory });
            Ok(())
        }
        fn elapsed(&self) -> Duration {
            self.started.elapsed()
        }
        fn wait(&mut self, timeout: Duration) -> Result<bool, State> {
            let queue = self.queue.as_ref().expect("started queue");
            let state = queue.state.lock().map_err(|_| State::Failed("通知 broker queue が停止しました".into()))?;
            let (state, _) = queue
                .wake
                .wait_timeout_while(state, timeout, |s| !s.pending && !s.closed)
                .map_err(|_| State::Failed("通知 broker queue が停止しました".into()))?;
            Ok(state.pending && !state.closed)
        }
        fn reopen(&mut self) -> Result<(), State> {
            reopen(self.profile.as_ref().expect("verified profile"), self.boundary.as_mut().expect("initialized boundary"))
        }
        fn stop(&mut self) -> Result<(), State> {
            let result = self.registration.as_mut().map_or(Ok(()), Registration::revoke);
            self.registration.take(); // Drop retained factory before apartment.
            self.boundary.take(); // RoUninitialize, on the initializing MTA thread.
            result
        }
    }
    impl Drop for Runtime {
        fn drop(&mut self) {
            if let Some(queue) = &self.queue {
                queue.close();
            }
            let _ = self.stop();
        }
    }
    pub(crate) fn run() -> Result<(), State> {
        serve(&mut Runtime { started: Instant::now(), queue: None, registration: None, boundary: None, profile: None })
    }
}
