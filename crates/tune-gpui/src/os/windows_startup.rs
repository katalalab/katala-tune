//! Closed installed-startup profiles and explicit Run operations. No notification APIs.
use super::{
    State, windows_login as login,
    windows_notifications::{self, Profile},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

const LIMIT: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Mode {
    Windowed,
    ResidentVisible,
    ResidentHidden,
}
impl Mode {
    pub(super) fn from_flags(resident: bool, hidden: bool) -> Result<Self, State> {
        match (resident, hidden) {
            (false, false) => Ok(Self::Windowed),
            (true, false) => Ok(Self::ResidentVisible),
            (true, true) => Ok(Self::ResidentHidden),
            (false, true) => Err(invalid()),
        }
    }
    fn flags(self) -> &'static [&'static str] {
        match self {
            Self::Windowed => &[],
            Self::ResidentVisible => &["--resident"],
            Self::ResidentHidden => &["--resident", "--hidden"],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub command: Option<String>,
    pub approval: State,
}
/// No approval mutation exists at this boundary. The host serializes operations;
/// fresh reads reduce external races but do not claim registry compare-and-swap.
pub(super) trait Registry {
    fn read(&mut self, name: &str) -> Result<Snapshot, State>;
    fn write(&mut self, name: &str, command: &str) -> Result<(), State>;
    fn delete(&mut self, name: &str) -> Result<(), State>;
}
pub(super) struct Files {
    pub home: PathBuf,
    pub local: PathBuf,
    pub executable: PathBuf,
}
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    executable: String,
    config: String,
    data: String,
    mode: Mode,
}
impl Document {
    fn new(profile: &Profile, mode: Mode) -> Self {
        Self {
            schema: "katala-tune/gpui-preview-startup/v1".into(),
            executable: profile.executable.clone(),
            config: profile.config.clone(),
            data: profile.data.clone(),
            mode,
        }
    }
    fn bytes(&self) -> Result<Vec<u8>, State> {
        serde_json::to_vec(self).map_err(|_| invalid())
    }
}
trait ProfileFile: Write {
    fn sync(&self) -> io::Result<()>;
}
impl ProfileFile for fs::File {
    fn sync(&self) -> io::Result<()> {
        self.sync_all()
    }
}
fn persist_profile(file: &mut impl ProfileFile, bytes: &[u8]) -> Result<(), State> {
    file.write_all(bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync())
        .map_err(|error| State::Failed(format!("Startup profile write failed; partial file retained ({:?})", error.raw_os_error())))
}
fn invalid() -> State {
    State::Failed("Windows startup profile is invalid or changed".into())
}
fn unsupported() -> State {
    State::Unsupported("Compact startup requires the fixed managed Preview installation".into())
}
fn io_error(error: io::Error) -> State {
    if error.kind() == io::ErrorKind::PermissionDenied { State::Denied } else { State::Failed(format!("Windows startup IO error {:?}", error.raw_os_error())) }
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn path_text(path: &Path) -> Result<String, State> {
    path.to_str().filter(|value| !value.contains(['\0', '\r', '\n'])).map(str::to_owned).ok_or_else(invalid)
}
fn same_location(value: &str, expected: &Path) -> Result<bool, State> {
    if value.contains(['\0', '\r', '\n']) || !Path::new(value).is_absolute() {
        return Err(invalid());
    }
    let expected_text = path_text(expected)?;
    // Windows canonical names carry a verbatim prefix. Accept that spelling or
    // its exact ordinary disk spelling, but never canonicalize an alias into
    // authority (junctions, dot segments and different release paths fail).
    let spelling = |s: &str| s.strip_prefix(r"\\?\").unwrap_or(s).to_owned();
    Ok(spelling(value) == spelling(&expected_text) && fs::canonicalize(value).map_err(io_error)? == expected)
}
fn plain(metadata: &fs::Metadata, directory: bool) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        } // FILE_ATTRIBUTE_REPARSE_POINT
    }
    !metadata.file_type().is_symlink() && if directory { metadata.is_dir() } else { metadata.is_file() }
}
/// The known-folder root may be OS-redirected. Every managed descendant must be
/// a literal regular component, with no junction, symlink or canonical alias.
fn checked(root: &Path, path: &Path, directory: bool) -> Result<(), State> {
    let relative = path.strip_prefix(root).map_err(|_| invalid())?;
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return Err(invalid());
    }
    for (index, component) in components.iter().enumerate() {
        if !matches!(component, Component::Normal(_)) {
            return Err(invalid());
        }
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current).map_err(io_error)?;
        if !plain(&metadata, index + 1 < components.len() || directory) {
            return Err(invalid());
        }
    }
    if fs::canonicalize(path).map_err(io_error)? != path {
        return Err(invalid());
    }
    Ok(())
}
fn read_file(root: &Path, path: &Path) -> Result<Vec<u8>, State> {
    checked(root, path, false)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    } // OPEN_REPARSE_POINT
    let file = options.open(path).map_err(io_error)?;
    if !plain(&file.metadata().map_err(io_error)?, false) {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64).read_to_end(&mut bytes).map_err(io_error)?;
    if bytes.len() > LIMIT {
        return Err(invalid());
    }
    checked(root, path, false)?;
    Ok(bytes)
}
impl Files {
    fn managed(&self, expected: Option<&Profile>) -> Result<Profile, State> {
        // Resolve only OS roots; do not canonicalize an untrusted descendant
        // before checking its exact layout and reparse attributes.
        let home = fs::canonicalize(&self.home).map_err(io_error)?;
        let local = fs::canonicalize(&self.local).map_err(io_error)?;
        let root = home.join(".katala-tune/gpui-preview");
        let relative = self.executable.strip_prefix(root.join("releases")).map_err(|_| unsupported())?;
        let parts: Vec<_> = relative.components().collect();
        if parts.len() != 2
            || !matches!(parts[0], Component::Normal(_))
            || parts[1].as_os_str() != "katala-tune-gpui.exe"
            || !parts[0].as_os_str().to_str().is_some_and(|v| hex(v, 40))
        {
            return Err(unsupported());
        }
        let config = self.executable.parent().ok_or_else(unsupported)?.join("nodes.json");
        let data = root.join("data");
        checked(&home, &self.executable, false)?;
        checked(&home, &config, false)?;
        checked(&home, &data, true)?;
        let profile = Profile { executable: path_text(&self.executable)?, config: path_text(&config)?, data: path_text(&data)? };
        if expected.is_some_and(|p| p.executable != profile.executable || p.config != profile.config || p.data != profile.data) {
            return Err(unsupported());
        }
        let metadata = read_file(&local, &local.join("Katala Tune GPUI Preview/installation.json"))?;
        let installed = windows_notifications::installation(&metadata)?;
        if !same_location(&installed.executable, &self.executable)? || !same_location(&installed.config, &config)? || !same_location(&installed.data, &data)? {
            return Err(invalid());
        }
        Ok(profile)
    }
    fn directory(&self) -> Result<PathBuf, State> {
        Ok(self.executable.parent().ok_or_else(invalid)?.join("startup-profiles"))
    }
    fn read_profile(&self, profile: &Profile, token: &str) -> Result<Document, State> {
        if !hex(token, 64) {
            return Err(invalid());
        }
        let bytes = read_file(&fs::canonicalize(&self.home).map_err(io_error)?, &self.directory()?.join(format!("{token}.json")))?;
        if digest(&bytes) != token {
            return Err(invalid());
        }
        let document: Document = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if document != Document::new(profile, document.mode) || document.bytes()? != bytes {
            return Err(invalid());
        }
        Ok(document)
    }
    fn materialize(&self, profile: &Profile, mode: Mode) -> Result<String, State> {
        let document = Document::new(profile, mode);
        let bytes = document.bytes()?;
        let token = digest(&bytes);
        let directory = self.directory()?;
        match fs::create_dir(&directory) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(io_error(e)),
        }
        let home = fs::canonicalize(&self.home).map_err(io_error)?;
        checked(&home, &directory, true)?;
        let path = directory.join(format!("{token}.json"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                // Keep even our own partial file on failure. No implicit repair,
                // overwrite, GC, or promise of power-loss atomic publication.
                persist_profile(&mut file, &bytes)?;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(io_error(e)),
        }
        if self.read_profile(profile, &token)? != document {
            return Err(invalid());
        }
        Ok(token)
    }
}
fn command(profile: &Profile, mode: Mode) -> Result<String, State> {
    login::lossless_command(&profile.executable, &["--installed-startup", &digest(&Document::new(profile, mode).bytes()?)]).map_err(State::Failed)
}
fn owned(files: &Files, profile: &Profile, registered: Option<&str>) -> Result<(), State> {
    let Some(registered) = registered else {
        return Ok(());
    };
    for mode in [Mode::Windowed, Mode::ResidentVisible, Mode::ResidentHidden] {
        let mut args = vec!["--config", &profile.config, "--data", &profile.data];
        args.extend(mode.flags());
        if registered == login::lossless_command(&profile.executable, &args).map_err(State::Failed)? {
            return Ok(());
        }
    }
    for mode in [Mode::Windowed, Mode::ResidentVisible, Mode::ResidentHidden] {
        if registered == command(profile, mode)? {
            files.managed(Some(profile))?;
            let token = digest(&Document::new(profile, mode).bytes()?);
            files.read_profile(profile, &token)?;
            return Ok(());
        }
    }
    Err(State::Failed("Run registration is not owned by this profile".into()))
}
fn allowed(snapshot: &Snapshot) -> Result<(), State> {
    match snapshot.approval {
        State::Enabled => Ok(()),
        State::Denied => Err(State::Denied),
        _ => Err(State::Unknown),
    }
}
pub(super) fn query(files: &Files, profile: &Profile, registry: &mut impl Registry) -> State {
    (|| {
        let snapshot = registry.read(&login::identity(&profile.executable))?;
        owned(files, profile, snapshot.command.as_deref())?;
        Ok(if snapshot.command.is_none() { State::Disabled } else { snapshot.approval })
    })()
    .unwrap_or_else(|state| state)
}
fn rollback(registry: &mut impl Registry, name: &str, desired: &str, before: &Snapshot) -> Result<(), State> {
    let current = registry.read(name)?;
    if current.command == before.command {
        return Ok(());
    }
    if current.command.as_deref() != Some(desired) {
        return Err(State::Failed("Run rollback preserved a foreign replacement".into()));
    }
    match &before.command {
        Some(old) => registry.write(name, old)?,
        None => registry.delete(name)?,
    }
    if registry.read(name)?.command != before.command {
        return Err(State::Failed("Run rollback readback failed".into()));
    }
    Ok(())
}
pub(super) fn enable(files: &Files, profile: &Profile, mode: Mode, registry: &mut impl Registry) -> Result<(), State> {
    files.managed(Some(profile))?;
    let name = login::identity(&profile.executable);
    let before = registry.read(&name)?;
    owned(files, profile, before.command.as_deref())?;
    allowed(&before)?;
    let token = digest(&Document::new(profile, mode).bytes()?);
    let desired = login::command(&profile.executable, &["--installed-startup", &token]).map_err(State::Unsupported)?;
    files.materialize(profile, mode)?;
    files.managed(Some(profile))?;
    files.read_profile(profile, &token)?;
    let current = registry.read(&name)?;
    allowed(&current)?;
    if current != before {
        return Err(State::Failed("Run registration changed before write".into()));
    }
    if current.command.as_deref() == Some(&desired) {
        return Ok(());
    }
    let result = registry.write(&name, &desired).and_then(|_| {
        let after = registry.read(&name)?;
        if after.command.as_deref() != Some(&desired) {
            return Err(invalid());
        }
        allowed(&after)
    });
    if let Err(error) = result {
        if rollback(registry, &name, &desired, &before).is_err() {
            return Err(State::Failed("Run update failed and rollback could not be verified".into()));
        }
        return Err(error);
    }
    Ok(())
}
pub(super) fn disable(files: &Files, profile: &Profile, registry: &mut impl Registry) -> Result<(), State> {
    let name = login::identity(&profile.executable);
    let before = registry.read(&name)?;
    owned(files, profile, before.command.as_deref())?;
    if before.command.is_none() {
        return Ok(());
    }
    let current = registry.read(&name)?;
    if current.command != before.command {
        return Err(State::Failed("Run registration changed before delete".into()));
    }
    owned(files, profile, current.command.as_deref())?;
    registry.delete(&name)?;
    if registry.read(&name)?.command.is_some() {
        return Err(State::Failed("Run removal could not be verified".into()));
    }
    Ok(())
}
pub(super) fn resolve(files: &Files, token: &str, registry: &mut impl Registry) -> Result<crate::startup::Options, State> {
    let profile = files.managed(None)?;
    let document = files.read_profile(&profile, token)?;
    let expected = login::command(&profile.executable, &["--installed-startup", token]).map_err(State::Unsupported)?;
    let snapshot = registry.read(&login::identity(&profile.executable))?;
    if snapshot.command.as_deref() != Some(&expected) {
        return Err(invalid());
    }
    allowed(&snapshot)?;
    let mut args = vec!["--config", &profile.config, "--data", &profile.data];
    args.extend(document.mode.flags());
    crate::startup::parse(args.into_iter().map(std::ffi::OsString::from)).map_err(State::Failed)?.ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Fixture {
        root: PathBuf,
        files: Files,
        profile: Profile,
    }
    impl Fixture {
        fn new() -> Self {
            Self::with_home("u 空")
        }
        fn with_home(home_name: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            #[cfg(unix)]
            let temp = PathBuf::from("/tmp");
            #[cfg(not(unix))]
            let temp = std::env::temp_dir();
            let root = temp.join(format!("ks-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let home = root.join(home_name);
            let local = root.join("l");
            let managed = home.join(".katala-tune/gpui-preview");
            let release = managed.join("releases").join("a".repeat(40));
            std::fs::create_dir_all(&release).unwrap();
            std::fs::create_dir(managed.join("data")).unwrap();
            std::fs::create_dir_all(local.join("Katala Tune GPUI Preview")).unwrap();
            let executable = release.join("katala-tune-gpui.exe");
            std::fs::write(&executable, b"synthetic executable").unwrap();
            std::fs::write(release.join("nodes.json"), b"synthetic unread config").unwrap();
            let profile = Profile {
                executable: executable.to_str().unwrap().into(),
                config: release.join("nodes.json").to_str().unwrap().into(),
                data: managed.join("data").to_str().unwrap().into(),
            };
            std::fs::write(
                local.join("Katala Tune GPUI Preview/installation.json"),
                serde_json::to_vec(&serde_json::json!({
                    "schema": "katala-tune/gpui-preview-installation/v1", "aumid": "org.katala.tune.gpui.preview",
                    "activator_clsid": "{7FA17089-16BD-4C17-8035-3CB6ACAF04AC}", "executable": profile.executable,
                    "config": profile.config, "data": profile.data
                }))
                .unwrap(),
            )
            .unwrap();
            Self { root, files: Files { home, local, executable }, profile }
        }
        fn profiles(&self) -> PathBuf {
            self.files.executable.parent().unwrap().join("startup-profiles")
        }
        fn token(registry: &Memory) -> String {
            registry.state.command.as_ref().unwrap().rsplit('"').nth(1).unwrap().into()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    // Stateful registry model: writes persist even when a later read fails.
    struct Memory {
        state: Snapshot,
        writes: usize,
        deletes: usize,
        reads: VecDeque<Result<Snapshot, State>>,
        fail_write: bool,
        fail_after_write: bool,
        replace_after_write: bool,
    }
    impl Default for Memory {
        fn default() -> Self {
            Self {
                state: Snapshot { command: None, approval: State::Enabled },
                writes: 0,
                deletes: 0,
                reads: VecDeque::new(),
                fail_write: false,
                fail_after_write: false,
                replace_after_write: false,
            }
        }
    }
    impl Registry for Memory {
        fn read(&mut self, _: &str) -> Result<Snapshot, State> {
            self.reads.pop_front().unwrap_or_else(|| Ok(self.state.clone()))
        }
        fn write(&mut self, _: &str, command: &str) -> Result<(), State> {
            self.writes += 1;
            if self.fail_write {
                return Err(invalid());
            }
            self.state.command = Some(command.into());
            if self.fail_after_write {
                self.fail_after_write = false;
                self.reads.push_back(Err(invalid()));
            }
            if self.replace_after_write {
                self.state.command = Some("foreign replacement".into());
            }
            Ok(())
        }
        fn delete(&mut self, _: &str) -> Result<(), State> {
            self.deletes += 1;
            self.state.command = None;
            Ok(())
        }
    }
    #[test]
    fn partial_profile_write_failure_is_failed_and_retained_without_repair() {
        struct Partial(fs::File);
        impl Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.write_all(&bytes[..5])?;
                Err(io::ErrorKind::PermissionDenied.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.0.flush()
            }
        }
        impl ProfileFile for Partial {
            fn sync(&self) -> io::Result<()> {
                self.0.sync_all()
            }
        }
        let f = Fixture::new();
        let mut r = Memory::default();
        let bytes = Document::new(&f.profile, Mode::Windowed).bytes().unwrap();
        let token = digest(&bytes);
        std::fs::create_dir(f.profiles()).unwrap();
        let path = f.profiles().join(format!("{token}.json"));
        let file = OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        assert!(matches!(persist_profile(&mut Partial(file), &bytes), Err(State::Failed(_))));
        assert_eq!(std::fs::read(&path).unwrap(), &bytes[..5]);
        assert!(enable(&f.files, &f.profile, Mode::Windowed, &mut r).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), &bytes[..5]);
        assert_eq!(r.writes, 0);
    }
    #[test]
    fn same_executable_mode_migration_retains_all_previous_profiles() {
        let f = Fixture::new();
        let mut r = Memory::default();
        let mut prior = Vec::new();
        for mode in [Mode::Windowed, Mode::ResidentVisible, Mode::ResidentHidden] {
            enable(&f.files, &f.profile, mode, &mut r).unwrap();
            let token = Fixture::token(&r);
            assert!(!prior.contains(&token));
            prior.push(token);
            for old in &prior {
                assert!(f.profiles().join(format!("{old}.json")).is_file());
            }
        }
        assert_eq!(std::fs::read_dir(f.profiles()).unwrap().count(), 3);
        assert_eq!(r.deletes, 0);
        assert_eq!(r.writes, 3);
    }

    #[test]
    fn overlong_compact_command_does_not_create_any_profile() {
        let f = Fixture::with_home(&"x".repeat(100));
        let mut r = Memory::default();
        assert!(command(&f.profile, Mode::Windowed).unwrap().encode_utf16().count() > 260);
        assert!(matches!(enable(&f.files, &f.profile, Mode::Windowed, &mut r), Err(State::Unsupported(_))));
        assert_eq!(r.writes, 0);
        assert!(!f.profiles().exists());
    }
    #[test]
    fn entry_rejects_schema_mode_duplicate_foreign_paths_and_missing_approval_before_options() {
        let f = Fixture::new();
        let mut r = Memory::default();
        enable(&f.files, &f.profile, Mode::Windowed, &mut r).unwrap();
        let token = Fixture::token(&r);
        let bytes = std::fs::read(f.profiles().join(format!("{token}.json"))).unwrap();
        let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for (field, value) in [("schema", "other/v1"), ("mode", "hidden"), ("config", "relative.json"), ("data", "/foreign"), ("extra", "unexpected")] {
            let mut altered = original.clone();
            altered[field] = value.into();
            let bytes = serde_json::to_vec(&altered).unwrap();
            let token = digest(&bytes);
            std::fs::write(f.profiles().join(format!("{token}.json")), bytes).unwrap();
            r.state.command = Some(login::command(&f.profile.executable, &["--installed-startup", &token]).unwrap());
            assert!(resolve(&f.files, &token, &mut r).is_err());
        }
        let text = String::from_utf8(bytes).unwrap();
        let duplicate = text.replacen("{", "{\"mode\":\"windowed\",", 1).into_bytes();
        let duplicate_token = digest(&duplicate);
        std::fs::write(f.profiles().join(format!("{duplicate_token}.json")), duplicate).unwrap();
        assert!(resolve(&f.files, &duplicate_token, &mut r).is_err());
        r.state.command = Some(login::command(&f.profile.executable, &["--installed-startup", &token]).unwrap());
        for approval in [State::Denied, State::Unknown] {
            r.state.approval = approval;
            assert!(resolve(&f.files, &token, &mut r).is_err());
        }
        r.state.approval = State::Enabled;
        r.state.command = None;
        assert!(resolve(&f.files, &token, &mut r).is_err());
    }
    #[test]
    fn rollback_failure_is_explicit_and_preserves_complete_profile() {
        let f = Fixture::new();
        let mut r = Memory::default();
        enable(&f.files, &f.profile, Mode::Windowed, &mut r).unwrap();
        let old = r.state.clone();
        // Preflight, recheck, readback failure, then failure to freshly inspect
        // ownership during rollback. Nothing is deleted speculatively.
        r.reads.extend([Ok(old.clone()), Ok(old), Err(invalid()), Err(invalid())]);
        assert!(matches!(enable(&f.files, &f.profile, Mode::ResidentHidden, &mut r), Err(State::Failed(_))));
        assert_eq!(r.deletes, 0);
        assert_eq!(std::fs::read_dir(f.profiles()).unwrap().count(), 2);
    }

    #[cfg(windows)]
    #[test]
    #[allow(unsafe_code)] // This fixture audits the real Windows serializer in its owned namespace.
    fn native_files_registry_and_windows_argv_roundtrip_in_owned_test_namespace() {
        use windows::{
            Win32::{
                Foundation::{HLOCAL, LocalFree},
                UI::Shell::CommandLineToArgvW,
            },
            core::HSTRING,
        };
        let f = Fixture::new();
        let mut fixture = super::super::windows::tests::Fixture::new();
        let name = login::identity(&f.profile.executable);
        for mode in [Mode::Windowed, Mode::ResidentVisible, Mode::ResidentHidden] {
            enable(&f.files, &f.profile, mode, &mut fixture.registry).unwrap();
            let command = fixture.registry.read(&name).unwrap().command.unwrap();
            assert!(command.encode_utf16().count() <= 260);
            let mut count = 0;
            // SAFETY: the bounded command HSTRING is NUL-terminated and alive
            // throughout the call; count points to a valid writable integer.
            // A non-null result owns one LocalFree allocation containing count
            // pointers and their NUL-terminated strings, per the Win32 contract.
            let argv = unsafe { CommandLineToArgvW(&HSTRING::from(&command), &mut count) };
            assert!(!argv.is_null());
            let values = if count == 3 {
                // SAFETY: successful CommandLineToArgvW guarantees count valid
                // entries. The exact closed-command count is checked before
                // forming this bounded slice, and the allocation remains live.
                unsafe { std::slice::from_raw_parts(argv, 3) }
                    .iter()
                    .map(|p| {
                        // SAFETY: each PWSTR belongs to the still-live argv
                        // allocation and is NUL-terminated by the API. Copying
                        // produces an owned String; no borrowed pointer escapes.
                        unsafe { p.to_string() }.map_err(|_| "invalid Windows argv UTF-16")
                    })
                    .collect::<Result<Vec<String>, _>>()
            } else {
                Err("unexpected Windows argv count")
            };
            // SAFETY: argv is the non-null base returned by CommandLineToArgvW;
            // this is its only free. Release it before propagating a count or
            // string-decoding failure, after all pointer access has ended.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(argv.cast())));
            }
            let values = values.expect("closed Windows startup command roundtrip");
            assert_eq!(values.len(), 3);
            assert_eq!(values[0], f.profile.executable);
            assert_eq!(values[1], "--installed-startup");
            let options = resolve(&f.files, &values[2], &mut fixture.registry).unwrap();
            assert_eq!(Mode::from_flags(options.resident, options.hidden).unwrap(), mode);
            assert_eq!(options.config, PathBuf::from(&f.profile.config));
            assert_eq!(options.data, PathBuf::from(&f.profile.data));
            disable(&f.files, &f.profile, &mut fixture.registry).unwrap();
        }
        assert_eq!(std::fs::read_dir(f.profiles()).unwrap().count(), 3);
        assert!(fixture.registry.read(&name).unwrap().command.is_none());
    }

    #[test]
    fn legacy_long_command_is_queryable_and_removable_without_installation_or_profile() {
        let f = Fixture::new();
        let mut r = Memory::default();
        let legacy = login::lossless_command(&f.profile.executable, &["--config", &f.profile.config, "--data", &f.profile.data]).unwrap();
        assert!(legacy.encode_utf16().count() > 260);
        r.state.command = Some(legacy);
        std::fs::remove_file(f.files.local.join("Katala Tune GPUI Preview/installation.json")).unwrap();
        assert_eq!(query(&f.files, &f.profile, &mut r), State::Enabled);
        disable(&f.files, &f.profile, &mut r).unwrap();
        assert!(r.state.command.is_none());
        assert_eq!(r.writes, 0);
        assert!(!f.profiles().exists());
        assert!(Mode::from_flags(false, true).is_err());
        assert_eq!(Mode::from_flags(true, true).unwrap(), Mode::ResidentHidden);
    }
    #[test]
    fn managed_binding_rejects_other_data_unknown_metadata_and_redirected_components() {
        let f = Fixture::new();
        let mut r = Memory::default();
        let foreign = Profile { executable: f.profile.executable.clone(), config: f.profile.config.clone(), data: f.root.to_str().unwrap().into() };
        assert!(enable(&f.files, &foreign, Mode::Windowed, &mut r).is_err());
        let metadata = f.files.local.join("Katala Tune GPUI Preview/installation.json");
        let original = std::fs::read(&metadata).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["extra"] = true.into();
        std::fs::write(&metadata, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(enable(&f.files, &f.profile, Mode::Windowed, &mut r).is_err());
        std::fs::write(&metadata, original).unwrap();
        let data = PathBuf::from(&f.profile.data);
        let elsewhere = f.root.join("elsewhere");
        std::fs::rename(&data, &elsewhere).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, &data).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&elsewhere, &data).unwrap();
        assert!(enable(&f.files, &f.profile, Mode::Windowed, &mut r).is_err());
        assert_eq!(r.writes, 0);
        assert!(!f.profiles().exists());
    }
    #[test]
    fn failed_mode_change_restores_owned_legacy_and_keeps_new_complete_profile() {
        let f = Fixture::new();
        let mut r = Memory::default();
        let legacy = login::lossless_command(&f.profile.executable, &["--config", &f.profile.config, "--data", &f.profile.data]).unwrap();
        r.state.command = Some(legacy.clone());
        r.fail_after_write = true;
        assert!(enable(&f.files, &f.profile, Mode::ResidentHidden, &mut r).is_err());
        assert_eq!(r.state.command.as_deref(), Some(legacy.as_str()));
        assert_eq!(r.writes, 2);
        assert_eq!(std::fs::read_dir(f.profiles()).unwrap().count(), 1);
    }
    #[test]
    fn approval_change_after_profile_creation_stops_before_run_write() {
        let f = Fixture::new();
        let mut r = Memory::default();
        r.reads.push_back(Ok(r.state.clone()));
        r.reads.push_back(Ok(Snapshot { command: None, approval: State::Denied }));
        assert_eq!(enable(&f.files, &f.profile, Mode::Windowed, &mut r), Err(State::Denied));
        assert_eq!(r.writes, 0);
        assert_eq!(std::fs::read_dir(f.profiles()).unwrap().count(), 1);
    }
    #[test]
    fn profile_digest_schema_and_collision_are_closed() {
        let f = Fixture::new();
        let mut r = Memory::default();
        enable(&f.files, &f.profile, Mode::Windowed, &mut r).unwrap();
        let token = Fixture::token(&r);
        let path = f.profiles().join(format!("{token}.json"));
        let original = std::fs::read(&path).unwrap();
        disable(&f.files, &f.profile, &mut r).unwrap();
        for bytes in [b"partial".to_vec(), [original.clone(), b" ".to_vec()].concat()] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(enable(&f.files, &f.profile, Mode::Windowed, &mut r).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(r.state.command.is_none());
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(enable(&f.files, &f.profile, Mode::Windowed, &mut r).is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn explicit_enable_roundtrips_three_modes_and_reuses_only_three_files() {
        let f = Fixture::new();
        let mut r = Memory::default();
        assert_eq!(query(&f.files, &f.profile, &mut r), State::Disabled);
        disable(&f.files, &f.profile, &mut r).unwrap();
        assert!(!f.profiles().exists());
        for _ in 0..2 {
            for (mode, flags) in [(Mode::Windowed, (false, false)), (Mode::ResidentVisible, (true, false)), (Mode::ResidentHidden, (true, true))] {
                enable(&f.files, &f.profile, mode, &mut r).unwrap();
                assert_eq!(query(&f.files, &f.profile, &mut r), State::Enabled);
                let options = resolve(&f.files, &Fixture::token(&r), &mut r).unwrap();
                assert_eq!((options.resident, options.hidden), flags);
                assert_eq!(options.config, PathBuf::from(&f.profile.config));
                assert_eq!(options.data, PathBuf::from(&f.profile.data));
                assert!(!options.selftest && !options.logs && options.live_id.is_none());
                disable(&f.files, &f.profile, &mut r).unwrap();
            }
        }
        assert_eq!(std::fs::read_dir(f.profiles()).unwrap().count(), 3);
    }
    #[test]
    fn tombstones_foreign_values_and_unknown_approval_do_not_create_profiles() {
        for (command, approval) in [(None, State::Denied), (None, State::Unknown), (Some("foreign".into()), State::Enabled)] {
            let f = Fixture::new();
            let mut r = Memory { state: Snapshot { command, approval }, ..Default::default() };
            let before = r.state.clone();
            assert!(enable(&f.files, &f.profile, Mode::Windowed, &mut r).is_err());
            assert_eq!(r.state, before);
            assert_eq!(r.writes, 0);
            assert!(!f.profiles().exists());
        }
    }
    #[test]
    fn readback_failure_rolls_back_only_our_value_and_keeps_complete_orphan() {
        let f = Fixture::new();
        let mut r = Memory { fail_after_write: true, ..Default::default() };
        assert!(enable(&f.files, &f.profile, Mode::Windowed, &mut r).is_err());
        assert!(r.state.command.is_none());
        assert_eq!(r.deletes, 1);
        assert_eq!(std::fs::read_dir(f.profiles()).unwrap().count(), 1);
        r.replace_after_write = true;
        assert!(enable(&f.files, &f.profile, Mode::ResidentHidden, &mut r).is_err());
        assert_eq!(r.state.command.as_deref(), Some("foreign replacement"));
        assert_eq!(r.deletes, 1);
        assert_eq!(std::fs::read_dir(f.profiles()).unwrap().count(), 2);
    }
    #[test]
    fn corrupt_or_missing_profile_never_repairs_or_deletes_registration() {
        let f = Fixture::new();
        let mut r = Memory::default();
        enable(&f.files, &f.profile, Mode::Windowed, &mut r).unwrap();
        let token = Fixture::token(&r);
        let path = f.profiles().join(format!("{token}.json"));
        std::fs::write(&path, b"partial write").unwrap();
        let original = r.state.clone();
        assert!(resolve(&f.files, &token, &mut r).is_err());
        assert!(enable(&f.files, &f.profile, Mode::Windowed, &mut r).is_err());
        assert!(disable(&f.files, &f.profile, &mut r).is_err());
        assert_eq!(r.state, original);
        assert_eq!(std::fs::read(&path).unwrap(), b"partial write");
        std::fs::remove_file(path).unwrap();
        assert!(matches!(query(&f.files, &f.profile, &mut r), State::Failed(_)));
        assert!(disable(&f.files, &f.profile, &mut r).is_err());
        assert_eq!(r.state, original);
    }
}

#[cfg(windows)]
pub(super) mod native {
    use super::*;
    use windows::{
        Win32::{
            System::Com::CoTaskMemFree,
            UI::Shell::{FOLDERID_LocalAppData, FOLDERID_Profile, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath},
        },
        core::GUID,
    };
    #[allow(unsafe_code)] // Own and release the one buffer returned by the known-folder API.
    fn folder(id: &GUID) -> Result<PathBuf, State> {
        // SAFETY: id borrows a valid GUID for this synchronous call; None uses
        // the current user's token. A successful result transfers ownership of
        // a NUL-terminated CoTaskMem allocation, retained until the free below.
        let value = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None) }.map_err(|error| {
            if error.code().0 as u32 == 0x80070005 {
                State::Denied
            } else {
                State::Failed(format!("Windows known-folder HRESULT 0x{:08X}", error.code().0 as u32))
            }
        })?;
        // SAFETY: the successful API result is a live NUL-terminated UTF-16
        // buffer. Decode into owned storage while its allocation is still live.
        let result = unsafe { value.to_string() }.map(PathBuf::from).map_err(|_| invalid());
        // SAFETY: free exactly the returned base with its matching allocator,
        // once, even if UTF-16 decoding failed; no pointer escapes this function.
        unsafe { CoTaskMemFree(Some(value.0.cast())) };
        fs::canonicalize(result?).map_err(io_error)
    }
    pub(in crate::os) fn files() -> Result<Files, State> {
        let executable = std::env::current_exe().map_err(io_error)?;
        let canonical = fs::canonicalize(&executable).map_err(io_error)?;
        if !same_location(&path_text(&executable)?, &canonical)? {
            return Err(invalid());
        }
        Ok(Files { home: folder(&FOLDERID_Profile)?, local: folder(&FOLDERID_LocalAppData)?, executable: canonical })
    }
}

#[cfg(windows)]
pub(crate) fn launch(arguments: &[std::ffi::OsString]) -> Result<Option<crate::startup::Options>, String> {
    crate::startup::installed(arguments, |token| {
        resolve(&native::files().map_err(|state| state.label())?, token, &mut super::windows::NativeRegistry::current()).map_err(|state| state.label())
    })
}
