//! Preview notification installation contract and explicit effects; portable policy tests.
use super::{Completion, Effect, Observation, State};

pub(super) const AUMID: &str = "org.katala.tune.gpui.preview";
#[cfg(target_os = "windows")]
pub(super) const DISPLAY_NAME: &str = "Katala Tune GPUI Preview";
pub(super) const CLSID: &str = "{7FA17089-16BD-4C17-8035-3CB6ACAF04AC}";
pub(super) const SOURCE: &str = "Windows · この実行ファイル · HKCU Run / StartupApproved · 通知の明示送信 / COM 経由 Reopen（表示・実機動作は未確認）";

const LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub(super) struct Profile {
    pub executable: String,
    pub config: String,
    pub data: String,
}
pub(super) struct Shortcut {
    pub target: String,
    pub arguments: Vec<String>,
    pub aumid: String,
    pub activator_clsid: String,
}
pub(super) struct Server {
    pub command: Vec<String>,
    pub executable: String,
}
pub(super) trait Boundary {
    fn installation(&mut self) -> Result<Vec<u8>, State>;
    fn same_path(&mut self, left: &str, right: &str) -> Result<bool, State>;
    fn shortcut(&mut self) -> Result<Shortcut, State>;
    fn server(&mut self) -> Result<Server, State>;
    fn setting(&mut self) -> Result<i32, State>;
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Installation {
    schema: String,
    aumid: String,
    activator_clsid: String,
    pub(super) executable: String,
    pub(super) config: String,
    pub(super) data: String,
}
fn invalid() -> State {
    State::Failed("通知の導入情報が不正です".into())
}
fn mismatch() -> State {
    State::Unsupported("通知の登録がこの Preview の導入先と一致しません".into())
}
fn api_failure(code: i32) -> State {
    match code as u32 {
        0x80070005 => State::Denied,
        code => State::Failed(format!("Windows HRESULT 0x{code:08X}")),
    }
}
pub(super) fn registry_string(bytes: &[u8]) -> Result<String, State> {
    if bytes.len() > LIMIT || !bytes.len().is_multiple_of(2) {
        return Err(invalid());
    }
    let mut units: Vec<_> = bytes.as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes).collect();
    if units.pop() != Some(0) {
        return Err(invalid());
    }
    let value = String::from_utf16(&units).map_err(|_| invalid())?;
    if value.is_empty() || value.contains(['\0', '\r', '\n']) {
        return Err(invalid());
    }
    Ok(value)
}
pub(super) fn installation(bytes: &[u8]) -> Result<Installation, State> {
    if bytes.len() > LIMIT {
        return Err(invalid());
    }
    let installed: Installation = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if installed.schema != "katala-tune/gpui-preview-installation/v1"
        || [&installed.executable, &installed.config, &installed.data].iter().any(|v| v.is_empty() || v.contains(['\0', '\r', '\n']))
    {
        return Err(invalid());
    }
    if installed.aumid != AUMID || installed.activator_clsid != CLSID {
        return Err(mismatch());
    }
    Ok(installed)
}
pub(super) fn registration(profile: &Profile, boundary: &mut impl Boundary) -> Result<(), State> {
    let installed = installation(&boundary.installation()?)?;
    if !boundary.same_path(&installed.executable, &profile.executable)?
        || !boundary.same_path(&installed.config, &profile.config)?
        || !boundary.same_path(&installed.data, &profile.data)?
    {
        return Err(mismatch());
    }
    let shortcut = boundary.shortcut()?;
    if shortcut.aumid != AUMID
        || shortcut.activator_clsid != CLSID
        || !boundary.same_path(&shortcut.target, &profile.executable)?
        || shortcut.arguments.len() != 4
        || shortcut.arguments[0] != "--config"
        || shortcut.arguments[2] != "--data"
        || !boundary.same_path(&shortcut.arguments[1], &profile.config)?
        || !boundary.same_path(&shortcut.arguments[3], &profile.data)?
    {
        return Err(mismatch());
    }
    let server = boundary.server()?;
    if server.command.len() != 2
        || server.command[1] != "--notification-com-server"
        || !boundary.same_path(&server.command[0], &profile.executable)?
        || !boundary.same_path(&server.executable, &profile.executable)?
    {
        return Err(mismatch());
    }
    Ok(())
}
fn notification(profile: &Profile, boundary: &mut impl Boundary) -> Result<State, State> {
    registration(profile, boundary)?;
    Ok(match boundary.setting()? {
        0 => State::Enabled,
        1 | 2 => State::Disabled,
        3 => State::Denied,
        4 => State::Unsupported("OS の manifest 条件で通知が無効です".into()),
        _ => State::Unknown,
    })
}
pub(super) fn complete_query(profile: Result<&Profile, State>, boundary: &mut impl Boundary, login: impl FnOnce() -> State, done: Completion<Observation>) {
    let notification = profile.and_then(|profile| notification(profile, boundary)).unwrap_or_else(|state| state);
    done(Observation { source: SOURCE.into(), notification, login: login() });
}

pub(super) const REOPEN: &str = "reopen-v1";
const SETTINGS: &str = "ms-settings:notifications";
pub(super) trait Effects: Boundary {
    fn show(&mut self, title: &str, body: &str, token: &str) -> Result<(), State>;
    fn open_settings(&mut self, uri: &str) -> Result<(), State>;
}
fn effect(result: Result<(), State>) -> Effect {
    match result {
        Ok(()) => Effect::Accepted,
        Err(State::Denied | State::Disabled) => Effect::Denied,
        Err(State::Unsupported(reason)) => Effect::Unsupported(reason),
        Err(State::Failed(reason)) => Effect::Failed(reason),
        Err(_) => Effect::Failed("通知の設定を確認できません".into()),
    }
}
fn text_valid(title: &str, body: &str) -> Result<(), State> {
    // Product bound below the platform's 5 KB XML limit. Charge XML escaping
    // conservatively in UTF-16 bytes, plus 512 bytes for the fixed DOM envelope.
    // No truncation, interpolation, logging, or unbounded walk of oversized input.
    if title.len().saturating_add(body.len()) > 2048 {
        return Err(payload_error());
    }
    let mut bytes = 512;
    for ch in title.chars().chain(body.chars()) {
        if !matches!(ch, '\u{9}' | '\u{a}' | '\u{d}' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}') {
            return Err(payload_error());
        }
        bytes += match ch {
            '&' => 10,
            '<' | '>' => 8,
            '\'' | '"' => 12,
            _ => ch.len_utf16() * 2,
        };
        if bytes > 5000 {
            return Err(payload_error());
        }
    }
    Ok(())
}
fn payload_error() -> State {
    State::Failed("通知の文字が XML 条件またはサイズ上限を超えています".into())
}
pub(super) fn send(profile: Result<&Profile, State>, boundary: &mut impl Effects, title: &str, body: &str) -> Effect {
    effect((|| {
        let profile = profile?;
        text_valid(title, body)?;
        match notification(profile, boundary)? {
            State::Enabled => boundary.show(title, body, REOPEN),
            state => Err(state),
        }
    })())
}
pub(super) fn settings(profile: Result<&Profile, State>, boundary: &mut impl Effects) -> Effect {
    effect(profile.and_then(|profile| {
        registration(profile, boundary)?;
        boundary.open_settings(SETTINGS)
    }))
}

#[cfg(target_os = "windows")]
pub(super) mod native {
    #![allow(unsafe_code)] // Narrow, read-only calls into pinned Windows bindings.
    use super::*;
    use std::{
        io::{self, Read},
        path::{Path, PathBuf},
    };
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_AppUserModel_ToastActivatorCLSID},
            System::{
                Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile, STGM_READ},
                Variant::{VT_CLSID, VT_LPWSTR},
                WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
            },
            UI::Shell::{
                CommandLineToArgvW, FOLDERID_LocalAppData, FOLDERID_Programs, IShellLinkW, KF_FLAG_DONT_VERIFY, PropertiesSystem::IPropertyStore,
                SHGetKnownFolderPath, SLGP_RAWPATH, ShellExecuteW, ShellLink,
            },
        },
        core::{GUID, HSTRING, Interface, PCWSTR, w},
    };
    use winreg::{
        RegKey,
        enums::{HKEY_CURRENT_USER, KEY_READ, REG_SZ},
    };

    // Prevent moving an initialized apartment to a different worker.
    struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);
    impl Drop for Apartment {
        fn drop(&mut self) {
            // Exactly one successful RoInitialize is balanced on this worker.
            unsafe { RoUninitialize() };
        }
    }
    #[derive(Default)]
    pub(crate) struct Native {
        apartment: Option<Apartment>,
    }
    pub(crate) fn api(error: windows::core::Error) -> State {
        api_failure(error.code().0)
    }
    pub(crate) fn io_error(error: io::Error) -> State {
        match error.kind() {
            io::ErrorKind::PermissionDenied => State::Denied,
            io::ErrorKind::NotFound => State::Unsupported("通知の登録がありません".into()),
            _ => State::Failed(format!("Windows error {:?}", error.raw_os_error())),
        }
    }
    fn folder(id: &GUID) -> Result<PathBuf, State> {
        // Known folders come from the OS, never from environment variables.
        let value = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None) }.map_err(api)?;
        let result = unsafe { value.to_string() }.map(PathBuf::from).map_err(|_| invalid());
        unsafe { CoTaskMemFree(Some(value.0.cast())) };
        result
    }
    fn wide(value: &[u16]) -> Result<String, State> {
        let end = value.iter().position(|ch| *ch == 0).ok_or_else(invalid)?;
        if end == value.len() - 1 {
            return Err(invalid());
        }
        String::from_utf16(&value[..end]).map_err(|_| invalid())
    }
    fn argv(command: &str) -> Result<Vec<String>, State> {
        if command.is_empty() || command.contains(['\0', '\r', '\n']) || command.encode_utf16().count() >= 32767 {
            return Err(invalid());
        }
        let mut count = 0;
        let command = HSTRING::from(command);
        let values = unsafe { CommandLineToArgvW(PCWSTR(command.as_ptr()), &mut count) };
        if values.is_null() {
            return Err(api(windows::core::Error::from_win32()));
        }
        let result = (|| {
            if !(1..=8).contains(&count) {
                return Err(invalid());
            }
            unsafe { std::slice::from_raw_parts(values, count as usize) }.iter().map(|value| unsafe { value.to_string() }.map_err(|_| invalid())).collect()
        })();
        unsafe {
            let _ = LocalFree(Some(HLOCAL(values.cast())));
        }
        result
    }
    fn normalized(value: &str) -> Result<PathBuf, State> {
        if value.is_empty() || value.contains(['\0', '\r', '\n']) || !Path::new(value).is_absolute() {
            return Err(invalid());
        }
        match std::fs::canonicalize(value) {
            Ok(path) => Ok(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => crate::startup::resolved(Path::new(value)).map_err(|_| invalid()),
            Err(error) => Err(io_error(error)),
        }
    }
    impl Native {
        pub(crate) fn installed_profile(&mut self) -> Result<Profile, State> {
            let installed = installation(&self.installation()?)?;
            let current = std::env::current_exe().map_err(io_error)?;
            let current = current.to_str().ok_or_else(invalid)?;
            if !self.same_path(current, &installed.executable)? {
                return Err(mismatch());
            }
            // Retain resolved names, not metadata aliases: revalidation before
            // spawn must detect an alias being redirected to another profile.
            let resolved = |value: &str| normalized(value)?.to_str().map(str::to_owned).ok_or_else(invalid);
            let profile = Profile { executable: resolved(current)?, config: resolved(&installed.config)?, data: resolved(&installed.data)? };
            registration(&profile, self)?;
            Ok(profile)
        }
    }
    impl Effects for Native {
        fn show(&mut self, title: &str, body: &str, token: &str) -> Result<(), State> {
            let document = XmlDocument::new().map_err(api)?;
            // Fixed schema only; title/body are DOM text nodes, never markup.
            document.LoadXml(&HSTRING::from("<toast><visual><binding template=\"ToastGeneric\"><text/><text/></binding></visual></toast>")).map_err(api)?;
            document.DocumentElement().map_err(api)?.SetAttribute(&HSTRING::from("launch"), &HSTRING::from(token)).map_err(api)?;
            let texts = document.GetElementsByTagName(&HSTRING::from("text")).map_err(api)?;
            for (index, value) in [title, body].iter().enumerate() {
                let text = document.CreateTextNode(&HSTRING::from(*value)).map_err(api)?;
                texts.Item(index as u32).map_err(api)?.AppendChild(&text).map_err(api)?;
            }
            let serialized = document.GetXml().map_err(api)?;
            if serialized.len() * 2 > 5000 || serialized.to_string().len() > 5000 {
                return Err(payload_error());
            }
            let toast = ToastNotification::CreateToastNotification(&document).map_err(api)?;
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID)).and_then(|notifier| notifier.Show(&toast)).map_err(api)
        }
        fn open_settings(&mut self, uri: &str) -> Result<(), State> {
            let uri = HSTRING::from(uri);
            let result = unsafe {
                ShellExecuteW(None, w!("open"), PCWSTR(uri.as_ptr()), PCWSTR::null(), PCWSTR::null(), windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL)
            };
            let code = result.0 as isize;
            match code {
                33.. => Ok(()),
                5 => Err(State::Denied),
                _ => Err(State::Failed(format!("Windows ShellExecute code {code}"))),
            }
        }
    }
    impl Boundary for Native {
        fn installation(&mut self) -> Result<Vec<u8>, State> {
            // No fallback on RPC_E_CHANGED_MODE; an incompatible apartment fails.
            if self.apartment.is_none() {
                unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(api)?;
                self.apartment = Some(Apartment(std::marker::PhantomData));
            }
            let path = folder(&FOLDERID_LocalAppData)?.join(DISPLAY_NAME).join("installation.json");
            let mut bytes = Vec::new();
            std::fs::File::open(path).map_err(io_error)?.take((LIMIT + 1) as u64).read_to_end(&mut bytes).map_err(io_error)?;
            Ok(bytes)
        }
        fn same_path(&mut self, left: &str, right: &str) -> Result<bool, State> {
            // canonicalize asks the filesystem for existing path casing and
            // handles verbatim prefixes, Unicode, aliases and case-sensitive dirs.
            // Missing tails use the exact startup resolver; ambiguous casing
            // there is conservatively a mismatch, never assumed to be a match.
            Ok(normalized(left)? == normalized(right)?)
        }
        fn shortcut(&mut self) -> Result<Shortcut, State> {
            let path = folder(&FOLDERID_Programs)?.join(format!("{DISPLAY_NAME}.lnk"));
            std::fs::metadata(&path).map_err(io_error)?;
            let path = HSTRING::from(path.as_os_str());
            // Load only. Never call Resolve, Save or property setters.
            let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.map_err(api)?;
            let file: IPersistFile = link.cast().map_err(api)?;
            unsafe { file.Load(PCWSTR(path.as_ptr()), STGM_READ) }.map_err(api)?;
            let (mut target, mut args) = (vec![0; 32768], vec![0; 32768]);
            unsafe {
                link.GetPath(&mut target, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32).map_err(api)?;
                link.GetArguments(&mut args).map_err(api)?;
            }
            let store: IPropertyStore = link.cast().map_err(api)?;
            let aumid = unsafe { store.GetValue(&PKEY_AppUserModel_ID) }.map_err(api)?;
            let clsid = unsafe { store.GetValue(&PKEY_AppUserModel_ToastActivatorCLSID) }.map_err(api)?;
            // Do not coerce unexpected PROPVARIANT types into plausible values.
            if aumid.vt() != VT_LPWSTR || clsid.vt() != VT_CLSID {
                return Err(State::Unknown);
            }
            let aumid = unsafe { aumid.Anonymous.Anonymous.Anonymous.pwszVal.to_string() }.map_err(|_| invalid())?;
            let clsid_ptr = unsafe { clsid.Anonymous.Anonymous.Anonymous.puuid };
            if clsid_ptr.is_null() {
                return Err(invalid());
            }
            let activator_clsid = format!("{{{:?}}}", unsafe { *clsid_ptr });
            let mut arguments = argv(&format!("preview {}", wide(&args)?))?;
            arguments.remove(0);
            Ok(Shortcut { target: wide(&target)?, arguments, aumid, activator_clsid })
        }
        fn server(&mut self) -> Result<Server, State> {
            let user = RegKey::predef(HKEY_CURRENT_USER);
            let key = user.open_subkey_with_flags(format!(r"Software\Classes\CLSID\{CLSID}\LocalServer32"), KEY_READ).map_err(io_error)?;
            let string = |name| -> Result<String, State> {
                let raw = key.get_raw_value(name).map_err(io_error)?;
                if raw.vtype != REG_SZ {
                    return Err(State::Unknown);
                }
                registry_string(&raw.bytes)
            };
            let command = string("")?;
            // The registration contract requires a quoted executable; compare
            // parsed, closed argv so whitespace/Unicode paths are never split.
            if !command.starts_with('"') {
                return Err(invalid());
            }
            Ok(Server { command: argv(&command)?, executable: string("ServerExecutable")? })
        }
        fn setting(&mut self) -> Result<i32, State> {
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))
                .and_then(|notifier| notifier.Setting())
                .map(|setting| setting.0)
                .map_err(api)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Fake {
        metadata: Result<Vec<u8>, State>,
        shortcut: Shortcut,
        server: Server,
        setting: Result<i32, State>,
        reads: Vec<&'static str>,
        effects: Vec<Vec<String>>,
        effect_error: Option<State>,
    }
    impl Fake {
        fn valid() -> Self {
            Self {
                metadata: Ok(r#"{"schema":"katala-tune/gpui-preview-installation/v1","aumid":"org.katala.tune.gpui.preview","activator_clsid":"{7FA17089-16BD-4C17-8035-3CB6ACAF04AC}","executable":"C:\\Preview Space\\通知.exe","config":"C:\\profile\\nodes.json","data":"C:\\profile\\data"}"#.as_bytes().to_vec()),
                shortcut: Shortcut {
                    target: r"C:\Preview Space\通知.exe".into(),
                    arguments: ["--config", r"C:\profile\nodes.json", "--data", r"C:\profile\data"].map(str::to_owned).into(),
                    aumid: "org.katala.tune.gpui.preview".into(),
                    activator_clsid: "{7FA17089-16BD-4C17-8035-3CB6ACAF04AC}".into(),
                },
                server: Server { command: [r"C:\Preview Space\通知.exe", "--notification-com-server"].map(str::to_owned).into(), executable: r"C:\Preview Space\通知.exe".into() },
                setting: Ok(0), reads: vec![], effects: vec![], effect_error: None,
            }
        }
        fn query(&mut self, diagnostic: bool) -> Observation {
            let profile =
                Profile { executable: r"C:\Preview Space\通知.exe".into(), config: r"C:\profile\nodes.json".into(), data: r"C:\profile\data".into() };
            let result = Arc::new(Mutex::new(None));
            let slot = result.clone();
            complete_query(
                if diagnostic { Err(State::Unsupported("診断モード".into())) } else { Ok(&profile) },
                self,
                || State::Denied,
                Box::new(move |observation| {
                    assert!(slot.lock().unwrap().replace(observation).is_none());
                }),
            );
            Arc::try_unwrap(result).unwrap().into_inner().unwrap().unwrap()
        }
    }
    impl Boundary for Fake {
        fn installation(&mut self) -> Result<Vec<u8>, State> {
            self.reads.push("installation");
            self.metadata.clone()
        }
        fn same_path(&mut self, left: &str, right: &str) -> Result<bool, State> {
            Ok(left == right)
        }
        fn shortcut(&mut self) -> Result<Shortcut, State> {
            self.reads.push("shortcut");
            Ok(Shortcut {
                target: self.shortcut.target.clone(),
                arguments: self.shortcut.arguments.clone(),
                aumid: self.shortcut.aumid.clone(),
                activator_clsid: self.shortcut.activator_clsid.clone(),
            })
        }
        fn server(&mut self) -> Result<Server, State> {
            self.reads.push("server");
            Ok(Server { command: self.server.command.clone(), executable: self.server.executable.clone() })
        }
        fn setting(&mut self) -> Result<i32, State> {
            self.reads.push("setting");
            self.setting.clone()
        }
    }
    impl Effects for Fake {
        fn show(&mut self, title: &str, body: &str, token: &str) -> Result<(), State> {
            self.effects.push([title, body, token].map(str::to_owned).into());
            self.effect_error.clone().map_or(Ok(()), Err)
        }
        fn open_settings(&mut self, uri: &str) -> Result<(), State> {
            self.effects.push(vec![uri.into()]);
            self.effect_error.clone().map_or(Ok(()), Err)
        }
    }
    impl super::super::windows_activation::Launcher for Fake {
        fn spawn(&mut self, exe: &str, args: &[&str]) -> Result<(), State> {
            self.effects.push(std::iter::once(exe).chain(args.iter().copied()).map(str::to_owned).collect());
            self.effect_error.clone().map_or(Ok(()), Err)
        }
    }
    #[test]
    fn reopen_uses_fresh_registration_and_fixed_profile_argv_and_propagates_spawn_failure() {
        use super::super::windows_activation::reopen;
        let mut fake = Fake::valid();
        reopen(&profile(), &mut fake).unwrap();
        assert_eq!(
            fake.effects,
            [vec![r"C:\Preview Space\通知.exe", "--config", r"C:\profile\nodes.json", "--data", r"C:\profile\data", "--open", "dashboard"]]
        );
        assert_eq!(fake.reads, ["installation", "shortcut", "server"]);
        fake.shortcut.arguments[1] = "foreign".into();
        assert!(reopen(&profile(), &mut fake).is_err());
        assert_eq!(fake.effects.len(), 1);
        let mut fake = Fake::valid();
        fake.effect_error = Some(State::Denied);
        assert_eq!(reopen(&profile(), &mut fake), Err(State::Denied));
        assert_eq!(fake.effects.len(), 1);
    }
    #[test]
    fn effect_api_failures_are_not_reported_as_accepted_and_query_has_no_effects() {
        let mut fake = Fake::valid();
        fake.query(false);
        assert!(fake.effects.is_empty());
        fake.effect_error = Some(State::Denied);
        assert_eq!(send(Ok(&profile()), &mut fake, "title", "body"), Effect::Denied);
        assert_eq!(settings(Ok(&profile()), &mut fake), Effect::Denied);
        fake.effect_error = Some(State::Failed("HRESULT".into()));
        assert_eq!(send(Ok(&profile()), &mut fake, "title", "body"), Effect::Failed("HRESULT".into()));
    }
    fn profile() -> Profile {
        Profile { executable: r"C:\Preview Space\通知.exe".into(), config: r"C:\profile\nodes.json".into(), data: r"C:\profile\data".into() }
    }
    #[test]
    fn explicit_send_rechecks_registration_and_setting_and_passes_text_as_data_once() {
        let mut fake = Fake::valid();
        assert_eq!(send(Ok(&profile()), &mut fake, "<title>&", "本文\n\"body\""), Effect::Accepted);
        assert_eq!(fake.effects, [vec!["<title>&", "本文\n\"body\"", "reopen-v1"]]);
        assert_eq!(fake.reads, ["installation", "shortcut", "server", "setting"]);
        fake.shortcut.aumid = "foreign".into();
        assert!(matches!(send(Ok(&profile()), &mut fake, "title", "body"), Effect::Unsupported(_)));
        assert_eq!(fake.effects.len(), 1);
    }
    #[test]
    fn send_rejects_disabled_unknown_errors_diagnostic_and_invalid_payload_without_effects() {
        for state in [Ok(1), Ok(2), Ok(3), Ok(4), Ok(99), Err(State::Denied), Err(State::Failed("api".into()))] {
            let mut fake = Fake::valid();
            fake.setting = state;
            assert_ne!(send(Ok(&profile()), &mut fake, "t", "b"), Effect::Accepted);
            assert!(fake.effects.is_empty());
        }
        for error in [State::Unsupported("missing".into()), State::Denied, State::Unknown, State::Failed("registration".into())] {
            let mut fake = Fake::valid();
            fake.metadata = Err(error);
            assert_ne!(send(Ok(&profile()), &mut fake, "t", "b"), Effect::Accepted);
            assert_eq!(fake.reads, ["installation"]);
            assert!(fake.effects.is_empty());
        }
        let mut fake = Fake::valid();
        for text in ["bad\0text".into(), "bad\u{b}".into(), "bad\u{fffe}".into(), "x".repeat(2049), "&".repeat(1000)] {
            assert!(matches!(send(Ok(&profile()), &mut fake, &text, "body"), Effect::Failed(_)));
        }
        assert!(matches!(send(Err(State::Unsupported("diagnostic".into())), &mut fake, "t", "b"), Effect::Unsupported(_)));
        assert!(fake.reads.is_empty());
        assert!(fake.effects.is_empty());
    }
    #[test]
    fn settings_requires_registration_and_uses_only_fixed_uri_without_setting_query() {
        let mut fake = Fake::valid();
        assert_eq!(settings(Ok(&profile()), &mut fake), Effect::Accepted);
        assert_eq!(fake.effects, [vec!["ms-settings:notifications"]]);
        assert!(!fake.reads.contains(&"setting"));
        fake.server.executable = "foreign".into();
        assert!(matches!(settings(Ok(&profile()), &mut fake), Effect::Unsupported(_)));
        assert_eq!(fake.effects.len(), 1);
        let mut fake = Fake::valid();
        assert!(matches!(settings(Err(State::Unsupported("diagnostic".into())), &mut fake), Effect::Unsupported(_)));
        assert!(fake.reads.is_empty());
    }
    #[test]
    fn explicit_query_checks_installation_before_observing_setting_and_preserves_login() {
        let mut fake = Fake::valid();
        let observation = fake.query(false);
        assert_eq!(observation.notification, State::Enabled);
        assert_eq!(observation.login, State::Denied);
        assert!(observation.source.contains("COM 経由 Reopen"));
        assert_eq!(fake.reads, ["installation", "shortcut", "server", "setting"]);
    }
    #[test]
    fn diagnostic_query_performs_no_notification_reads() {
        let mut fake = Fake::valid();
        assert!(matches!(fake.query(true).notification, State::Unsupported(_)));
        assert!(fake.reads.is_empty());
    }
    #[test]
    fn missing_foreign_and_mismatched_installations_never_reach_setting() {
        for change in 0..15 {
            let mut fake = Fake::valid();
            match change {
                0 => fake.metadata = Err(State::Unsupported("missing".into())),
                1 => fake.shortcut.aumid = "foreign".into(),
                2 => fake.shortcut.activator_clsid = "{00000000-0000-0000-0000-000000000000}".into(),
                3 => fake.shortcut.target = r"C:\foreign.exe".into(),
                4 => fake.shortcut.arguments[1] = r"C:\other.json".into(),
                5 => fake.shortcut.arguments[3] = r"C:\other-data".into(),
                6 => fake.shortcut.arguments.push("--resident".into()),
                7 => fake.server.command[0] = r"C:\foreign.exe".into(),
                8 => fake.server.executable = r"C:\foreign.exe".into(),
                9 => fake.server.command[1] = "--open".into(),
                10 => fake.server.command.push("-Embedding".into()),
                11 => fake.server.command.clear(),
                12 => fake.shortcut.arguments[0] = "--foreign".into(),
                13 => fake.shortcut.arguments.clear(),
                _ => fake.metadata = Ok(String::from_utf8(fake.metadata.unwrap()).unwrap().replace("C:\\\\profile\\\\data", "C:\\\\other-data").into_bytes()),
            }
            assert!(matches!(fake.query(false).notification, State::Unsupported(_) | State::Failed(_)), "case {change}");
            assert!(!fake.reads.contains(&"setting"), "case {change}");
        }
    }
    #[test]
    fn metadata_is_closed_and_each_installation_identity_field_is_enforced() {
        for field in ["schema", "aumid", "activator_clsid", "executable", "config", "data", "extra"] {
            let mut fake = Fake::valid();
            let mut json: serde_json::Value = serde_json::from_slice(fake.metadata.as_ref().unwrap()).unwrap();
            json[field] = serde_json::json!("foreign-private-value");
            fake.metadata = Ok(serde_json::to_vec(&json).unwrap());
            let observed = fake.query(false).notification;
            assert!(matches!(observed, State::Unsupported(_) | State::Failed(_)), "field {field}");
            assert_eq!(fake.reads, ["installation"], "field {field}");
            assert!(!format!("{observed:?}").contains("foreign-private-value"));
        }
        for invalid in ["null", "{}", r#"{"schema":1}"#] {
            let mut fake = Fake::valid();
            fake.metadata = Ok(invalid.as_bytes().to_vec());
            assert!(matches!(fake.query(false).notification, State::Failed(_)));
        }
        let mut fake = Fake::valid();
        let valid = String::from_utf8(fake.metadata.clone().unwrap()).unwrap();
        fake.metadata = Ok(valid.replacen('{', r#"{"aumid":"org.katala.tune.gpui.preview","#, 1).into_bytes());
        assert!(matches!(fake.query(false).notification, State::Failed(_)));
        assert_eq!(fake.reads, ["installation"]);
    }
    #[test]
    fn registration_read_failures_remain_distinct_and_do_not_query_setting() {
        for error in [State::Denied, State::Unknown, State::Failed("Windows HRESULT 0x80004005".into())] {
            let mut fake = Fake::valid();
            fake.metadata = Err(error.clone());
            assert_eq!(fake.query(false).notification, error);
            assert_eq!(fake.reads, ["installation"]);
        }
    }
    #[test]
    fn registry_text_rejects_lossy_utf16_odd_bytes_and_hidden_suffixes() {
        for bytes in [vec![0x61], vec![0x61, 0], vec![0, 0xD8, 0, 0], vec![0x61, 0, 0, 0, 0x62, 0, 0, 0]] {
            assert!(matches!(registry_string(&bytes), Err(State::Failed(_))));
        }
        let bytes: Vec<_> = "C:\\Preview Space\\通知.exe\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(registry_string(&bytes).unwrap(), "C:\\Preview Space\\通知.exe");
    }
    #[test]
    fn native_api_failures_propagate_sanitized_codes_through_the_query() {
        for (code, want) in [
            (0x80070005_u32 as i32, State::Denied),
            (0x80004005_u32 as i32, State::Failed("Windows HRESULT 0x80004005".into())),
            (0x80010106_u32 as i32, State::Failed("Windows HRESULT 0x80010106".into())),
            (0x80070002_u32 as i32, State::Failed("Windows HRESULT 0x80070002".into())),
        ] {
            let mut fake = Fake::valid();
            fake.setting = Err(api_failure(code));
            assert_eq!(fake.query(false).notification, want);
        }
    }
    #[test]
    fn setting_unknown_and_failures_are_never_enabled() {
        for (setting, want) in [
            (Ok(0), State::Enabled),
            (Ok(1), State::Disabled),
            (Ok(2), State::Disabled),
            (Ok(3), State::Denied),
            (Ok(99), State::Unknown),
            (Err(State::Denied), State::Denied),
            (Err(State::Failed("HRESULT 0x80004005".into())), State::Failed("HRESULT 0x80004005".into())),
        ] {
            let mut fake = Fake::valid();
            fake.setting = setting;
            assert_eq!(fake.query(false).notification, want);
        }
        let mut fake = Fake::valid();
        fake.setting = Ok(4);
        assert!(matches!(fake.query(false).notification, State::Unsupported(_)));
    }
    #[test]
    fn malformed_or_oversized_metadata_fails_without_leaking_input() {
        for bytes in [b"private-path-invalid-json".to_vec(), vec![b'x'; 65537], br#"{"schema":"foreign","secret":"private-marker"}"#.to_vec()] {
            let mut fake = Fake::valid();
            fake.metadata = Ok(bytes);
            let observed = fake.query(false).notification;
            assert!(matches!(observed, State::Failed(_)));
            assert!(!format!("{observed:?}").contains("private-marker"));
            assert_eq!(fake.reads, ["installation"]);
        }
    }
}
