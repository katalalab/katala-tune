//! Explicit, read-only notification query; no installer, delivery or activation.
use super::{Completion, Effect, Observation, Service, State, windows_login, windows_notifications as notifications};
use std::{io, path::Path};
use winreg::types::FromRegValue;
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_SZ},
};
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
pub struct Windows {
    launch: Result<(String, String), String>,
    profile: Result<notifications::Profile, String>,
}
impl Windows {
    pub fn new(config: &Path, data: &Path, diagnostic: bool) -> Self {
        let profile = (|| {
            if diagnostic {
                return Err("診断モードでは OS 操作を行いません".into());
            }
            let executable = std::env::current_exe().and_then(std::fs::canonicalize).map_err(|_| "実行ファイルを確認できません")?;
            // canonicalize on Windows may introduce a verbatim path prefix. It
            // is kept intact; no shell or percent-variable expansion is used.
            let executable = executable.to_str().ok_or("実行ファイルの文字列が不正です")?.to_owned();
            let config = crate::startup::resolved(config).map_err(|_| "台帳パスが不正です")?;
            let data = crate::startup::resolved(data).map_err(|_| "データパスが不正です")?;
            Ok(notifications::Profile {
                executable,
                config: config.to_str().ok_or("台帳パスが不正です")?.to_owned(),
                data: data.to_str().ok_or("データパスが不正です")?.to_owned(),
            })
        })();
        let launch = profile.as_ref().map_err(Clone::clone).and_then(|profile| {
            let command = windows_login::command(&profile.executable, &["--config", &profile.config, "--data", &profile.data])?;
            Ok((windows_login::identity(&profile.executable), command))
        });
        Self { launch, profile }
    }
    fn read(&self) -> Result<State, io::Error> {
        let (name, command) = self.launch.as_ref().map_err(|_| io::Error::other("launch unavailable"))?;
        let user = RegKey::predef(HKEY_CURRENT_USER);
        let raw = optional(user.open_subkey_with_flags(RUN, KEY_READ).and_then(|key| key.get_raw_value(name)))?;
        if raw.as_ref().is_some_and(|value| value.vtype != REG_SZ) {
            return Ok(State::Failed("Run の登録型が REG_SZ ではありません".into()));
        }
        let registered = raw.as_ref().map(String::from_reg_value).transpose()?;
        let approval = optional(user.open_subkey_with_flags(APPROVED, KEY_READ).and_then(|key| key.get_raw_value(name)))?;
        if approval.as_ref().is_some_and(|value| value.vtype != REG_BINARY) {
            return Ok(State::Unknown);
        }
        Ok(windows_login::observed(registered.as_deref(), command, approval.as_ref().map(|v| v.bytes.as_ref())))
    }
}
fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn failure(error: io::Error) -> Effect {
    if error.kind() == io::ErrorKind::PermissionDenied { Effect::Denied } else { Effect::Failed(format!("Windows error {:?}", error.raw_os_error())) }
}
impl Service for Windows {
    fn query(&self, done: Completion<Observation>) {
        // NativeHost invokes query on its blocking worker. All apartment and
        // COM/WinRT objects are local to this call and dropped on that worker.
        let mut boundary = notifications::native::Native::default();
        notifications::complete_query(
            self.profile.as_ref().map_err(|e| State::Unsupported(e.clone())),
            &mut boundary,
            || match &self.launch {
                Err(e) => State::Unsupported(e.clone()),
                Ok(_) => match self.read() {
                    Ok(state) => state,
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => State::Denied,
                    Err(e) => State::Failed(format!("Windows error {:?}", e.raw_os_error())),
                },
            },
            done,
        );
    }
    fn permission(&self, done: Completion<Effect>) {
        done(Effect::Unsupported(notifications::UNIMPLEMENTED.into()));
    }
    fn settings(&self, done: Completion<Effect>) {
        done(Effect::Unsupported(notifications::UNIMPLEMENTED.into()));
    }
    fn notify(&self, _: String, _: String, done: Completion<Effect>) {
        done(Effect::Unsupported(notifications::UNIMPLEMENTED.into()));
    }
    fn login(&self, enabled: bool, done: Completion<Effect>) {
        let (name, command) = match &self.launch {
            Ok(v) => v,
            Err(e) => {
                done(Effect::Unsupported(e.clone()));
                return;
            }
        };
        let state = match self.read() {
            Ok(s) => s,
            Err(e) => {
                done(failure(e));
                return;
            }
        };
        // Never reset StartupApproved or overwrite an unrelated command.
        if matches!(state, State::Failed(_)) || (enabled && state == State::Unknown) {
            done(Effect::Failed("登録状態を確認できません".into()));
            return;
        }
        if enabled && state == State::Denied {
            done(Effect::Denied);
            return;
        }
        if state.observed() == Some(enabled) {
            done(Effect::Accepted);
            return;
        }
        let user = RegKey::predef(HKEY_CURRENT_USER);
        let result = if enabled {
            user.create_subkey_with_flags(RUN, KEY_SET_VALUE).and_then(|(key, _)| key.set_value(name, command))
        } else {
            optional(user.open_subkey_with_flags(RUN, KEY_SET_VALUE).and_then(|key| key.delete_value(name))).map(|_| ())
        };
        done(match result {
            Ok(()) => Effect::Accepted,
            Err(e) => failure(e),
        });
    }
}
