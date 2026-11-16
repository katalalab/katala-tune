//! Explicit notification operations and fixed managed login startup.
use super::{Completion, Effect, Observation, Service, State, windows_login, windows_notifications as notifications, windows_startup as startup};
use std::{io, path::Path};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE},
};
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
pub struct Windows {
    profile: Result<notifications::Profile, String>,
    mode: Result<startup::Mode, State>,
}
impl Windows {
    pub fn new(config: &Path, data: &Path, diagnostic: bool, resident: bool, hidden: bool) -> Self {
        let profile = (|| {
            if diagnostic {
                return Err("診断モードでは OS 操作を行いません".into());
            }
            let executable = std::env::current_exe().and_then(std::fs::canonicalize).map_err(|_| "実行ファイルを確認できません")?;
            let config = crate::startup::resolved(config).map_err(|_| "台帳パスが不正です")?;
            let data = crate::startup::resolved(data).map_err(|_| "データパスが不正です")?;
            Ok(notifications::Profile {
                executable: executable.to_str().ok_or("実行ファイルの文字列が不正です")?.to_owned(),
                config: config.to_str().ok_or("台帳パスが不正です")?.to_owned(),
                data: data.to_str().ok_or("データパスが不正です")?.to_owned(),
            })
        })();
        Self { profile, mode: startup::Mode::from_flags(resident, hidden) }
    }
    fn read(&self) -> State {
        let result = (|| {
            let profile = self.profile.as_ref().map_err(|reason| State::Unsupported(reason.clone()))?;
            Ok(startup::query(&startup::native::files()?, profile, &mut NativeRegistry::current()))
        })();
        result.unwrap_or_else(|state| state)
    }
}
fn failure(error: io::Error) -> State {
    if error.kind() == io::ErrorKind::PermissionDenied { State::Denied } else { State::Failed(format!("Windows registry error {:?}", error.raw_os_error())) }
}
fn effect(result: Result<(), State>) -> Effect {
    match result {
        Ok(()) => Effect::Accepted,
        Err(State::Denied) => Effect::Denied,
        Err(State::Unsupported(reason)) => Effect::Unsupported(reason),
        Err(State::Failed(reason)) => Effect::Failed(reason),
        Err(_) => Effect::Failed("起動の承認状態を確認できません".into()),
    }
}
/// Only current Run identity is accessed. Tests substitute an owned namespace;
/// neither implementation nor tests write the user's StartupApproved branch.
pub(super) struct NativeRegistry {
    root: RegKey,
    run: String,
    approved: String,
}
impl NativeRegistry {
    pub(super) fn current() -> Self {
        Self { root: RegKey::predef(HKEY_CURRENT_USER), run: RUN.into(), approved: APPROVED.into() }
    }
    fn raw(&self, path: &str, name: &str) -> Result<Option<(u32, Vec<u8>)>, State> {
        use windows::{
            Win32::System::Registry::{HKEY, REG_VALUE_TYPE, RegQueryValueExW},
            core::HSTRING,
        };
        let key = match self.root.open_subkey_with_flags(path, KEY_READ) {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(failure(error)),
        };
        // One bounded native read; a concurrent growth or oversized foreign
        // value fails, without asking winreg to allocate its advertised length.
        let mut bytes = vec![0u8; 64 * 1024];
        let mut length = bytes.len() as u32;
        let mut kind = REG_VALUE_TYPE(0);
        let status =
            unsafe { RegQueryValueExW(HKEY(key.raw_handle()), &HSTRING::from(name), None, Some(&mut kind), Some(bytes.as_mut_ptr()), Some(&mut length)) };
        if status.0 == 2 {
            return Ok(None);
        }
        if status.0 != 0 {
            return Err(failure(io::Error::from_raw_os_error(status.0 as i32)));
        }
        if length as usize > bytes.len() {
            return Err(State::Failed("Registry value exceeds startup bound".into()));
        }
        bytes.truncate(length as usize);
        Ok(Some((kind.0, bytes)))
    }
}
impl startup::Registry for NativeRegistry {
    fn read(&mut self, name: &str) -> Result<startup::Snapshot, State> {
        let command = match self.raw(&self.run, name)? {
            None => None,
            Some((1, bytes)) => Some(notifications::registry_string(&bytes).map_err(|_| State::Failed("Run の登録文字列が不正です".into()))?),
            Some(_) => return Err(State::Failed("Run の登録型が REG_SZ ではありません".into())),
        };
        let approval = match self.raw(&self.approved, name)? {
            None => State::Enabled,
            Some((3, bytes)) => windows_login::observed(Some("owned"), "owned", Some(&bytes)),
            Some(_) => State::Unknown,
        };
        Ok(startup::Snapshot { command, approval })
    }
    fn write(&mut self, name: &str, command: &str) -> Result<(), State> {
        self.root.create_subkey_with_flags(&self.run, KEY_SET_VALUE).and_then(|(key, _)| key.set_value(name, &command)).map_err(failure)
    }
    fn delete(&mut self, name: &str) -> Result<(), State> {
        match self.root.open_subkey_with_flags(&self.run, KEY_SET_VALUE).and_then(|key| key.delete_value(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failure(error)),
        }
    }
}
impl Service for Windows {
    fn query(&self, done: Completion<Observation>) {
        let mut boundary = notifications::native::Native::default();
        notifications::complete_query(self.profile.as_ref().map_err(|e| State::Unsupported(e.clone())), &mut boundary, || self.read(), done);
    }
    fn permission(&self, done: Completion<Effect>) {
        done(Effect::Unsupported("Classic Windows 通知に許可要求ダイアログはありません。通知設定から変更してください".into()));
    }
    fn settings(&self, done: Completion<Effect>) {
        let mut boundary = notifications::native::Native::default();
        done(notifications::settings(self.profile.as_ref().map_err(|e| State::Unsupported(e.clone())), &mut boundary));
    }
    fn notify(&self, title: String, body: String, done: Completion<Effect>) {
        let mut boundary = notifications::native::Native::default();
        done(notifications::send(self.profile.as_ref().map_err(|e| State::Unsupported(e.clone())), &mut boundary, &title, &body));
    }
    fn login(&self, enabled: bool, done: Completion<Effect>) {
        done(effect((|| {
            let profile = self.profile.as_ref().map_err(|reason| State::Unsupported(reason.clone()))?;
            let files = startup::native::files()?;
            let mut registry = NativeRegistry::current();
            if enabled {
                startup::enable(&files, profile, *self.mode.as_ref().map_err(Clone::clone)?, &mut registry)
            } else {
                startup::disable(&files, profile, &mut registry)
            }
        })()));
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use startup::Registry;
    pub(in crate::os) struct Fixture {
        pub registry: NativeRegistry,
        path: String,
    }
    impl Fixture {
        pub(in crate::os) fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = format!(
                r"Software\KatalaTuneGPUIStartupTests\{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let root = RegKey::predef(HKEY_CURRENT_USER);
            let (key, disposition) = root.create_subkey(&path).unwrap();
            assert_eq!(disposition, winreg::enums::REG_CREATED_NEW_KEY);
            Self { registry: NativeRegistry { root: key, run: "RunFixture".into(), approved: "ApprovalFixture".into() }, path }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.path);
        }
    }
    #[test]
    fn native_registry_boundary_is_bounded_and_never_mutates_approval() {
        use winreg::{
            RegValue,
            enums::{REG_BINARY, REG_DWORD, REG_SZ},
        };
        let mut fixture = Fixture::new();
        let registry = &mut fixture.registry;
        let name = "owned-fixture";
        assert_eq!(registry.read(name).unwrap(), startup::Snapshot { command: None, approval: State::Enabled });
        let command = windows_login::command(r"C:\Unicode 空\preview.exe", &["--installed-startup", &"a".repeat(64)]).unwrap();
        registry.write(name, &command).unwrap();
        assert_eq!(registry.read(name).unwrap().command.as_deref(), Some(command.as_str()));
        let (approved, _) = registry.root.create_subkey("ApprovalFixture").unwrap();
        let denied = RegValue { vtype: REG_BINARY, bytes: vec![3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0].into() };
        approved.set_raw_value(name, &denied).unwrap();
        registry.delete(name).unwrap();
        assert_eq!(registry.read(name).unwrap().approval, State::Denied);
        assert_eq!(approved.get_raw_value(name).unwrap(), denied);
        let (run, _) = registry.root.create_subkey("RunFixture").unwrap();
        for value in [
            RegValue { vtype: REG_DWORD, bytes: vec![0; 4].into() },
            RegValue { vtype: REG_SZ, bytes: vec![b'a', 0].into() },
            RegValue { vtype: REG_SZ, bytes: vec![0, 0xd8, 0, 0].into() },
            RegValue { vtype: REG_SZ, bytes: vec![0; 64 * 1024 + 2].into() },
        ] {
            run.set_raw_value(name, &value).unwrap();
            assert!(registry.read(name).is_err());
            assert_eq!(run.get_raw_value(name).unwrap(), value);
            assert_eq!(approved.get_raw_value(name).unwrap(), denied);
        }
    }
}
