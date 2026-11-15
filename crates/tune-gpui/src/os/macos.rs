//! UN callbacks are delivered by the OS, with no dispatch to or wait on GPUI.
#![allow(unsafe_code)] // Audited, narrow calls into the pinned Apple bindings.
use super::{Completion, Effect, Observation, Service, State};
use block2::RcBlock;
use objc2::{
    ClassType, available, define_class, msg_send,
    rc::Retained,
    runtime::{Bool, ProtocolObject},
};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationSettings, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::{
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

// Stateless delegate: the OS callback immediately supplies presentation options.
// It has no UI/Engine reference and no dispatch or blocking wait.
define_class!(
    #[unsafe(super = NSObject)]
    struct NotificationDelegate;
    unsafe impl NSObjectProtocol for NotificationDelegate {}
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(&self, _: &UNUserNotificationCenter, _: &UNNotification, done: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
            let options = if available!(macos = 11.0) {
                UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List
            } else {
                #[allow(deprecated)]
                {
                    UNNotificationPresentationOptions::Alert
                }
            };
            done.call((options,));
        }
    }
);
// SAFETY: This NSObject subclass has no ivars or mutable state. Its sole
// callback only invokes the OS-provided completion on the calling thread.
unsafe impl Send for NotificationDelegate {}
unsafe impl Sync for NotificationDelegate {}
fn presentation(center: &UNUserNotificationCenter) {
    static DELEGATE: std::sync::OnceLock<Retained<NotificationDelegate>> = std::sync::OnceLock::new();
    let delegate = DELEGATE.get_or_init(|| unsafe { msg_send![NotificationDelegate::class(), new] });
    center.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
}

pub struct Mac {
    identity: Result<String, String>,
    login_paths: LoginPaths,
}
// Capture the resolved launch paths; only login enable needs reproducibility.
struct LoginPaths {
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    inherited_paths: bool,
}
impl LoginPaths {
    fn check(&self, enabled: bool, account_home: impl FnOnce() -> Option<PathBuf>) -> Result<(), Effect> {
        if !enabled {
            return Ok(());
        }
        let home = account_home().filter(|path| path.is_absolute()).ok_or_else(|| Effect::Unsupported("OS アカウントの既定ホームを確認できません".into()))?;
        let unsupported = || Effect::Unsupported("起動時の台帳・データパスを OS のログイン環境で再現できません".into());
        if self.home != home || self.inherited_paths {
            return Err(unsupported());
        }
        // Compare the same resolved defaults that a standard relaunch uses.
        let config = crate::startup::resolved(&home.join(".config/katala-tune/nodes.json")).map_err(|_| unsupported())?;
        let data = crate::startup::resolved(&home.join("Library/Application Support").join(tune_core::nodes::PRODUCT_NAME).join("data/gpui-preview"))
            .map_err(|_| unsupported())?;
        if self.config == config && self.data == data { Ok(()) } else { Err(unsupported()) }
    }
}
// Resolve the UID's account record independently of HOME/dirs. Called only
// from the explicit enable action's background worker, never query/startup.
fn account_home() -> Option<PathBuf> {
    use std::{
        ffi::{CStr, OsStr},
        mem::MaybeUninit,
        os::unix::ffi::OsStrExt,
    };
    let mut record = MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // A bounded buffer is sufficient for ordinary records. Any lookup error,
    // including ERANGE, fails closed rather than assuming the process HOME.
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: Both output pointers and the byte buffer remain live for the
    // call. getpwuid_r initializes the record/pointers only on success.
    let status = unsafe { libc::getpwuid_r(libc::getuid(), record.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: Success with a non-null result supplies a valid account record.
    let directory = unsafe { (*result).pw_dir };
    if directory.is_null() {
        return None;
    }
    // SAFETY: pw_dir is a NUL-terminated string in our still-live buffer.
    // Copy only the home path; no other account fields are retained or logged.
    Some(PathBuf::from(OsStr::from_bytes(unsafe { CStr::from_ptr(directory) }.to_bytes())))
}
fn identity(bundle: &Path, executable: &Path, current: &Path, id: &str) -> Result<String, String> {
    if bundle.extension().is_none_or(|e| e != "app")
        || executable.parent() != Some(bundle.join("Contents/MacOS").as_path())
        || executable != current
        || id.is_empty()
        || id.len() > 255
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err("実行中の .app bundle identity を確認できません".into());
    }
    Ok(id.into())
}
impl Mac {
    pub fn new(config: &Path, data: &Path, diagnostic: bool) -> Self {
        let bundle = NSBundle::mainBundle();
        let actual = (|| {
            if diagnostic {
                return Err("診断モードでは OS 操作を行いません".into());
            }
            let current = std::env::current_exe().and_then(std::fs::canonicalize).map_err(|_| "実行ファイルを確認できません")?;
            let executable = bundle.executablePath().ok_or("bundle executable がありません")?;
            let executable = std::fs::canonicalize(PathBuf::from(executable.to_string())).map_err(|_| "bundle executable を確認できません")?;
            let path = std::fs::canonicalize(PathBuf::from(bundle.bundlePath().to_string())).map_err(|_| "bundle path を確認できません")?;
            let id = bundle.bundleIdentifier().ok_or("bundle identifier がありません")?;
            identity(&path, &executable, &current, &id.to_string())
        })();
        // mainAppService does not preserve this process's environment at login.
        let inherited_paths = ["KATALA_TUNE_CONFIG", "KATALA_TUNE_DATA_DIR"].iter().any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
        Self { identity: actual, login_paths: LoginPaths { home: tune_core::nodes::home(), config: config.into(), data: data.into(), inherited_paths } }
    }
    fn center(&self) -> Result<objc2::rc::Retained<UNUserNotificationCenter>, String> {
        self.identity.as_ref().map_err(Clone::clone)?;
        if !available!(macos = 10.14) {
            return Err("通知には macOS 10.14 以降が必要です".into());
        }
        Ok(UNUserNotificationCenter::currentNotificationCenter())
    }
    fn login_state(&self) -> State {
        if !available!(macos = 13.0) {
            return State::Unsupported("ログイン起動には macOS 13 以降が必要です".into());
        }
        // mainAppService binds to the calling app; no hardcoded stable/Tauri ID.
        match unsafe { SMAppService::mainAppService().status() } {
            SMAppServiceStatus::Enabled => State::Enabled,
            SMAppServiceStatus::NotRegistered => State::Disabled,
            SMAppServiceStatus::RequiresApproval => State::Denied,
            SMAppServiceStatus::NotFound => State::Failed("ServiceManagement がアプリを見つけられません".into()),
            _ => State::Unknown,
        }
    }
}
fn authorization(value: UNAuthorizationStatus) -> State {
    match value {
        UNAuthorizationStatus::Authorized | UNAuthorizationStatus::Provisional => State::Enabled,
        UNAuthorizationStatus::Denied => State::Denied,
        _ => State::Unknown,
    }
}
fn error(error: *mut NSError) -> Effect {
    // Never retain/log localized descriptions, notification content, or raw paths.
    if error.is_null() { Effect::Accepted } else { Effect::Failed(format!("Apple OS error {}", unsafe { &*error }.code())) }
}
impl Service for Mac {
    fn query(&self, done: Completion<Observation>) {
        let center = match self.center() {
            Ok(center) => center,
            Err(reason) => {
                done(Observation { source: "macOS bundle".into(), notification: State::Unsupported(reason.clone()), login: State::Unsupported(reason) });
                return;
            }
        };
        let login = self.login_state();
        let source = format!("macOS · {} · UNUserNotificationCenter / SMAppService", self.identity.as_ref().unwrap());
        let callback = Mutex::new(Some(done));
        center.getNotificationSettingsWithCompletionHandler(&RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            if let Some(done) = callback.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                done(Observation {
                    source: source.clone(),
                    notification: authorization(unsafe { settings.as_ref() }.authorizationStatus()),
                    login: login.clone(),
                });
            }
        }));
    }
    fn permission(&self, done: Completion<Effect>) {
        let center = match self.center() {
            Ok(c) => c,
            Err(e) => {
                done(Effect::Unsupported(e));
                return;
            }
        };
        let callback = Mutex::new(Some(done));
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert,
            &RcBlock::new(move |granted: Bool, err: *mut NSError| {
                if let Some(done) = callback.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                    done(if !err.is_null() {
                        error(err)
                    } else if granted.as_bool() {
                        Effect::Accepted
                    } else {
                        Effect::Denied
                    });
                }
            }),
        );
    }
    fn settings(&self, done: Completion<Effect>) {
        if let Err(reason) = &self.identity {
            done(Effect::Unsupported(reason.clone()));
            return;
        }
        // Fixed URI and separate argv; never use an unrelated script app identity.
        done(match std::process::Command::new("/usr/bin/open").arg("x-apple.systempreferences:com.apple.Notifications-Settings.extension").status() {
            Ok(status) if status.success() => Effect::Accepted,
            _ => Effect::Failed("通知設定を開けませんでした".into()),
        });
    }
    fn login(&self, enabled: bool, done: Completion<Effect>) {
        if let Err(reason) = &self.identity {
            done(Effect::Unsupported(reason.clone()));
            return;
        }
        if let Err(effect) = self.login_paths.check(enabled, account_home) {
            done(effect);
            return;
        }
        let state = self.login_state();
        if let State::Unsupported(reason) = state {
            done(Effect::Unsupported(reason));
            return;
        }
        // Revoked consent is left to System Settings, never overwritten.
        if enabled && state == State::Denied {
            done(Effect::Denied);
            return;
        }
        if state.observed() == Some(enabled) {
            done(Effect::Accepted);
            return;
        }
        let service = unsafe { SMAppService::mainAppService() };
        let result = unsafe { if enabled { service.registerAndReturnError() } else { service.unregisterAndReturnError() } };
        done(match result {
            Ok(()) => Effect::Accepted,
            Err(e) => error(&*e as *const NSError as *mut NSError),
        });
    }
    fn notify(&self, title: String, body: String, done: Completion<Effect>) {
        let center = match self.center() {
            Ok(c) => c,
            Err(e) => {
                done(Effect::Unsupported(e));
                return;
            }
        };
        presentation(&center);
        let callback = Mutex::new(Some(done));
        // Sending never requests authorization. Querying here avoids describing
        // a silently discarded request under denied permission as accepted.
        center.getNotificationSettingsWithCompletionHandler(&RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            let Some(done) = callback.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else {
                return;
            };
            if authorization(unsafe { settings.as_ref() }.authorizationStatus()) != State::Enabled {
                done(Effect::Denied);
                return;
            }
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&title));
            content.setBody(&NSString::from_str(&body));
            let identifier =
                NSString::from_str(&format!("tune-{}-{}-{}", std::process::id(), tune_core::db::now_ms(), SEQUENCE.fetch_add(1, Ordering::Relaxed)));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None);
            let callback = Mutex::new(Some(done));
            UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(
                &request,
                Some(&RcBlock::new(move |err: *mut NSError| {
                    if let Some(done) = callback.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
                        done(error(err));
                    }
                })),
            );
        }));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn launch(home: impl AsRef<Path>) -> LoginPaths {
        let home = home.as_ref().to_path_buf();
        LoginPaths {
            config: home.join(".config/katala-tune/nodes.json"),
            data: home.join("Library/Application Support/Katala Tune/data/gpui-preview"),
            home,
            inherited_paths: false,
        }
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(case: &str) -> Self {
            let root = std::env::temp_dir().join(format!("kt-login-paths-{case}-{}-{}", std::process::id(), tune_core::db::now_ms()));
            std::fs::create_dir(&root).unwrap();
            let fixture = Self(root.canonicalize().unwrap());
            std::fs::create_dir(fixture.0.join("home")).unwrap();
            fixture
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn login_reproducibility_resolves_owned_default_symlinks_and_missing_tails() {
        use std::os::unix::fs::symlink;
        for case in ["config-directory", "config-file", "data-ancestor", "missing-tails"] {
            let fixture = Fixture::new(case);
            let home = fixture.0.join("home");
            match case {
                "config-directory" => {
                    std::fs::create_dir_all(fixture.0.join("storage/katala-tune")).unwrap();
                    std::fs::write(fixture.0.join("storage/katala-tune/nodes.json"), b"{}").unwrap();
                    symlink(fixture.0.join("storage"), home.join(".config")).unwrap();
                }
                "config-file" => {
                    std::fs::create_dir_all(home.join(".config/katala-tune")).unwrap();
                    std::fs::write(fixture.0.join("ledger.json"), b"{}").unwrap();
                    symlink(fixture.0.join("ledger.json"), home.join(".config/katala-tune/nodes.json")).unwrap();
                }
                "data-ancestor" => {
                    std::fs::create_dir(home.join("Library")).unwrap();
                    std::fs::create_dir(fixture.0.join("storage")).unwrap();
                    symlink(fixture.0.join("storage"), home.join("Library/Application Support")).unwrap();
                }
                _ => {}
            }
            let mut paths = launch(&home);
            // The same production resolver that startup uses; no helper binding,
            // account lookup, bundle query or ServiceManagement call is needed.
            paths.config = crate::startup::resolved(&paths.config).unwrap();
            paths.data = crate::startup::resolved(&paths.data).unwrap();
            assert!(paths.check(true, || Some(home.clone())).is_ok(), "resolved standard paths must remain reproducible: {case}");
            if case == "missing-tails" {
                assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0, "checking must not create missing defaults");
            }
            eprintln!("login default paths case={case}: passed");
        }
    }
    #[test]
    fn login_reproducibility_refuses_unresolvable_owned_defaults() {
        use std::os::unix::fs::symlink;
        for case in ["config-loop", "data-loop"] {
            let fixture = Fixture::new(case);
            let home = fixture.0.join("home");
            let paths = launch(&home);
            let default = if case == "config-loop" {
                symlink(home.join(".config"), home.join(".config")).unwrap();
                &paths.config
            } else {
                std::fs::create_dir(home.join("Library")).unwrap();
                symlink(home.join("Library/Application Support"), home.join("Library/Application Support")).unwrap();
                &paths.data
            };
            assert!(matches!(paths.check(true, || Some(home.clone())), Err(Effect::Unsupported(_))), "unresolvable default must not authorize login: {case}");
            assert!(crate::startup::resolved(default).is_err(), "the shared resolver must propagate the filesystem error: {case}");
            assert!(paths.check(false, || panic!("disable must not query account or resolve defaults")).is_ok());
            eprintln!("login default paths case={case}: refused");
        }
    }
    #[test]
    fn login_reproducibility_keeps_raw_home_environment_and_explicit_path_guards() {
        let fixture = Fixture::new("guards");
        let home = fixture.0.join("home");
        let alias = fixture.0.join("home-alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        assert_eq!(crate::startup::resolved(&alias).unwrap(), home);
        for field in ["home-alias", "environment", "config", "data"] {
            let mut paths = launch(&home);
            match field {
                "home-alias" => paths.home = alias.clone(),
                "environment" => paths.inherited_paths = true,
                "config" => paths.config = fixture.0.join("other/nodes.json"),
                _ => paths.data = fixture.0.join("other/data"),
            }
            assert!(matches!(paths.check(true, || Some(home.clone())), Err(Effect::Unsupported(_))), "guard must remain closed: {field}");
            assert!(paths.check(false, || panic!("disable must not query account or resolve defaults")).is_ok());
        }
    }
    #[test]
    fn login_reproducibility_accepts_normal_home_but_refuses_changed_or_unknown_account_home() {
        let account = PathBuf::from("/fixture/account");
        assert!(launch("/fixture/account").check(true, || Some(account.clone())).is_ok());
        assert!(matches!(launch("/fixture/qa-home").check(true, || Some(account.clone())), Err(Effect::Unsupported(_))));
        assert!(matches!(launch("/fixture/account").check(true, || None), Err(Effect::Unsupported(_))));
        assert!(matches!(launch("/fixture/account").check(true, || Some(PathBuf::from("relative"))), Err(Effect::Unsupported(_))));
    }
    #[test]
    fn login_reproducibility_preserves_explicit_path_and_environment_guards_and_allows_disable() {
        let account = PathBuf::from("/fixture/account");
        for field in ["config", "data", "environment"] {
            let mut paths = launch("/fixture/account");
            match field {
                "config" => paths.config = PathBuf::from("/fixture/other/nodes.json"),
                "data" => paths.data = PathBuf::from("/fixture/other/data"),
                _ => paths.inherited_paths = true,
            }
            assert!(matches!(paths.check(true, || Some(account.clone())), Err(Effect::Unsupported(_))));
            assert!(paths.check(false, || panic!("disable must not query the account database")).is_ok());
        }
        assert!(launch("/fixture/qa-home").check(false, || panic!("disable must remain available with unknown account home")).is_ok());
    }
    #[test]
    fn identity_uses_running_bundle_including_qa_and_rejects_other_executable() {
        let bundle = Path::new("/fixture/Preview & QA.app");
        let executable = bundle.join("Contents/MacOS/katala-tune-gpui");
        assert_eq!(identity(bundle, &executable, &executable, "org.example.qa.isolated").unwrap(), "org.example.qa.isolated");
        assert!(identity(bundle, &executable, Path::new("/other/app"), "org.example.qa").is_err());
        assert!(identity(Path::new("/fixture"), &executable, &executable, "org.example.qa").is_err());
        assert!(identity(bundle, &executable, &executable, "bad\nidentity").is_err());
    }
    #[test]
    fn permission_denial_and_undetermined_are_not_enabled() {
        assert_eq!(authorization(UNAuthorizationStatus::Denied), State::Denied);
        assert_eq!(authorization(UNAuthorizationStatus::NotDetermined), State::Unknown);
        assert_eq!(authorization(UNAuthorizationStatus::Authorized), State::Enabled);
    }
}
