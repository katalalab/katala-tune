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
    login_supported: bool,
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
        Self {
            identity: actual,
            login_supported: !inherited_paths && config == tune_core::nodes::user_config_path() && data == tune_core::nodes::data_dir().join("gpui-preview"),
        }
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
        if enabled && !self.login_supported {
            done(Effect::Unsupported("引数付き起動は SMAppService の mainAppService で再現できません".into()));
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
