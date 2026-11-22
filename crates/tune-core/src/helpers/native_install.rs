//! Explicit local native-sampler preparation and placement for the GPUI installer.
//!
//! Callers must establish maintenance ownership and collection quiescence before
//! installation. These values are integrity observations, not authorization,
//! publisher/protocol/CPU proof, or native OS acceptance. No startup work, remote
//! deployment, helper approval, settings mutation, or process control occurs here.
use std::{
    ffi::OsStr,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
mod artifact;
mod filesystem;
#[cfg(target_os = "macos")]
#[path = "native_install/macos.rs"]
mod os;
#[cfg(windows)]
#[path = "native_install/windows.rs"]
mod os;
#[cfg(not(any(target_os = "macos", windows)))]
#[path = "native_install/unsupported.rs"]
mod os;
use filesystem::{Directory, Observed, checked_close, io_error};

/// Transaction outcome. RecoveryRequired must block automatic reuse/retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Rejected,
    RolledBack,
    RecoveryRequired,
}
/// Bounded failure category; OS errors do not become successful Missing snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    UnsupportedPlatform,
    UnboundTarget,
    InvalidSource,
    InvalidArtifact,
    UnsafeFile,
    Missing,
    PermissionDenied,
    Limit,
    IdentityChanged,
    Integrity,
    Alias,
    TargetChanged,
    Collision,
    Io,
}
/// The operation at which an observation or transaction failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Platform,
    Binding,
    Resolve,
    Manifest,
    Open,
    Metadata,
    Read,
    ReadIdentity,
    Close,
    Target,
    CreateBin,
    CreateStage,
    Write,
    Flush,
    Permission,
    Sync,
    StageVerify,
    Backup,
    Place,
    FinalVerify,
    Cleanup,
    Rollback,
}
/// Typed local failure, including only this transaction's bounded recovery paths.
#[derive(Debug)]
pub struct InstallError {
    outcome: InstallOutcome,
    operation: Operation,
    cause: Cause,
    recovery: Vec<PathBuf>,
    rollback_cause: Option<Cause>,
    moved: bool,
}
impl InstallError {
    pub fn outcome(&self) -> InstallOutcome {
        self.outcome
    }
    pub fn operation(&self) -> Operation {
        self.operation
    }
    pub fn cause(&self) -> Cause {
        self.cause
    }
    pub fn recovery_paths(&self) -> &[PathBuf] {
        &self.recovery
    }
    pub fn rollback_cause(&self) -> Option<Cause> {
        self.rollback_cause
    }
}
impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native sampler {:?}/{:?}: {:?}", self.outcome, self.operation, self.cause)
    }
}
impl std::error::Error for InstallError {}
fn rejected(operation: Operation, cause: Cause) -> InstallError {
    InstallError { outcome: InstallOutcome::Rejected, operation, cause, recovery: Vec::new(), rollback_cause: None, moved: false }
}

/// Non-deserializable, immutable validated bytes. GUI and CLI bytes are not retained.
#[derive(Debug)]
pub struct PreparedNativeSampler {
    source: PathBuf,
    manifest: artifact::Manifest,
    manifest_hash: String,
    sampler_hash: String,
    bytes: Vec<u8>,
    sources: Vec<os::Identity>,
}
impl PreparedNativeSampler {
    pub(super) fn remote_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn source(&self) -> &Path {
        &self.source
    }
    pub fn version(&self) -> &str {
        &self.manifest.version
    }
    pub fn revision(&self) -> &str {
        &self.manifest.revision
    }
    pub fn source_tree(&self) -> &str {
        &self.manifest.source_tree
    }
    pub fn repository(&self) -> &str {
        &self.manifest.ci.repository
    }
    pub fn workflow_ref(&self) -> &str {
        &self.manifest.ci.workflow_ref
    }
    pub fn workflow_sha(&self) -> &str {
        &self.manifest.ci.workflow_sha
    }
    pub fn event_name(&self) -> &str {
        &self.manifest.ci.event_name
    }
    pub fn run_id(&self) -> &str {
        &self.manifest.ci.run_id
    }
    pub fn run_attempt(&self) -> &str {
        &self.manifest.ci.run_attempt
    }
    pub fn build_target(&self) -> &str {
        &self.manifest.build.target
    }
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_hash
    }
    pub fn sampler_sha256(&self) -> &str {
        &self.sampler_hash
    }
    pub fn sampler_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }
}
/// Private construction binds an observation to the configured target and directory
/// identities. It does not establish collection quiescence or an execution lease.
#[derive(Debug)]
pub struct NativeTargetSnapshot {
    target: PathBuf,
    data: os::Identity,
    bin: Option<os::Identity>,
    observed: Option<Observed>,
}
impl NativeTargetSnapshot {
    pub fn target(&self) -> &Path {
        &self.target
    }
    pub fn sha256(&self) -> Option<&str> {
        self.observed.as_ref().map(|v| v.hash.as_str())
    }
    pub fn is_missing(&self) -> bool {
        self.observed.is_none()
    }
}
/// Verified placement result. A retained backup is an installed result requiring
/// cleanup, never a false unchanged/rollback result. This is not persisted.
#[derive(Debug)]
pub struct LocalInstallReceipt {
    target: PathBuf,
    changed: bool,
    old_hash: Option<String>,
    sampler_hash: String,
    manifest_hash: String,
    retained_backup: Option<PathBuf>,
}
impl LocalInstallReceipt {
    pub fn target(&self) -> &Path {
        &self.target
    }
    pub fn changed(&self) -> bool {
        self.changed
    }
    pub fn old_sha256(&self) -> Option<&str> {
        self.old_hash.as_deref()
    }
    pub fn sampler_sha256(&self) -> &str {
        &self.sampler_hash
    }
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_hash
    }
    pub fn retained_backup(&self) -> Option<&Path> {
        self.retained_backup.as_deref()
    }
}

/// Reopen explicit absolute raw-v2 source and verify all three role files. The
/// existing binary structure check is not publisher/protocol/CPU attestation.
pub fn prepare_native_sampler(source: &Path) -> Result<PreparedNativeSampler, InstallError> {
    artifact::tuple()?;
    if !source.is_absolute() {
        return Err(rejected(Operation::Resolve, Cause::InvalidSource));
    }
    let resolved = std::fs::canonicalize(source).map_err(|e| io_error(Operation::Resolve, e))?;
    let dir = Directory::open(&resolved)?;
    let raw = filesystem::read(&dir, OsStr::new("manifest.json"), artifact::MAX_MANIFEST, true)?;
    let manifest = artifact::parse(&raw.bytes)?;
    let mut sources = vec![raw.observed.state.identity];
    let mut sampler = Vec::new();
    let mut sampler_hash = String::new();
    for entry in &manifest.files {
        let keep = entry.role == "native-sampler";
        let file = filesystem::read(&dir, OsStr::new(&entry.file), artifact::MAX_BINARY, keep)?;
        if file.observed.state.size != entry.bytes || file.observed.hash != entry.sha256 {
            return Err(rejected(Operation::Read, Cause::Integrity));
        }
        sources.push(file.observed.state.identity);
        if keep {
            sampler = file.bytes;
            sampler_hash = file.observed.hash;
        }
    }
    let structure_ok = if cfg!(windows) { super::binary::windows_executable(&sampler) } else { super::binary::mac_executable(&sampler) };
    if !structure_ok {
        return Err(rejected(Operation::Read, Cause::InvalidArtifact));
    }
    dir.check()?;
    dir.close()?;
    Ok(PreparedNativeSampler { source: resolved, manifest, manifest_hash: raw.observed.hash, sampler_hash, bytes: sampler, sources })
}
fn bound_bin() -> Result<&'static Path, InstallError> {
    artifact::tuple()?;
    super::bound_bin_dir().ok_or_else(|| rejected(Operation::Binding, Cause::UnboundTarget))
}
/// Inspect only bound data/bin/tune-agent[.exe]. Never falls back to HOME or an
/// executable-adjacent source, and never creates a missing bin directory.
pub fn inspect_bound_target() -> Result<NativeTargetSnapshot, InstallError> {
    inspect_at(bound_bin()?)
}
/// Recheck the original confirmation observation without creating a directory or file.
pub fn revalidate_bound_target(expected: &NativeTargetSnapshot) -> Result<(), InstallError> {
    revalidate_at(bound_bin()?, expected)
}
fn revalidate_at(bin: &Path, expected: &NativeTargetSnapshot) -> Result<(), InstallError> {
    let current = inspect_at(bin)?;
    if current.target != expected.target || current.data != expected.data || current.bin != expected.bin || current.observed != expected.observed {
        return Err(target_changed());
    }
    Ok(())
}
fn inspect_at(bin: &Path) -> Result<NativeTargetSnapshot, InstallError> {
    artifact::tuple()?;
    let mut dir = Directory::open(bin.parent().ok_or_else(|| rejected(Operation::Target, Cause::InvalidSource))?)?;
    let data = dir.identity().clone();
    match os::named(dir.file(), OsStr::new("bin")) {
        Ok(_) => {
            dir.child(OsStr::new("bin"))?;
            let id = dir.identity().clone();
            let observed = filesystem::optional(&dir, OsStr::new(&super::exe("tune-agent")))?;
            dir.close()?;
            Ok(NativeTargetSnapshot { target: bin.join(super::exe("tune-agent")), data, bin: Some(id), observed })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            dir.check()?;
            dir.close()?;
            Ok(NativeTargetSnapshot { target: bin.join(super::exe("tune-agent")), data, bin: None, observed: None })
        }
        Err(e) => Err(io_error(Operation::Target, e)),
    }
}
/// Consume prepared bytes and compare the target snapshot at commit. The caller
/// must own maintenance and establish a quiet state. This does not stop producers.
pub fn install_native_sampler(prepared: PreparedNativeSampler, expected: NativeTargetSnapshot) -> Result<LocalInstallReceipt, InstallError> {
    let target = bound_bin()?.join(super::exe("tune-agent"));
    if target != expected.target {
        return Err(rejected(Operation::Target, Cause::TargetChanged));
    }
    install_at(prepared, expected)
}
fn target_changed() -> InstallError {
    rejected(Operation::Target, Cause::TargetChanged)
}
fn same_target(dir: &Directory, expected: &NativeTargetSnapshot) -> Result<(), InstallError> {
    if filesystem::optional(dir, OsStr::new(&super::exe("tune-agent")))? != expected.observed {
        return Err(target_changed());
    }
    Ok(())
}
fn aliases(dir: &Directory, prepared: &PreparedNativeSampler, target: Option<&Observed>) -> Result<(), InstallError> {
    let action = filesystem::optional(dir, OsStr::new(&super::exe("tune")))?;
    if target.is_some_and(|t| prepared.sources.contains(&t.state.identity))
        || action.as_ref().is_some_and(|a| prepared.sources.contains(&a.state.identity) || target.is_some_and(|t| t.state.identity == a.state.identity))
    {
        return Err(rejected(Operation::Target, Cause::Alias));
    }
    Ok(())
}
static NEXT_OPERATION: AtomicU64 = AtomicU64::new(0);
fn unique_name(role: &str) -> String {
    let n = NEXT_OPERATION.fetch_add(1, Ordering::Relaxed);
    let name = format!(".tune-agent-{role}-{}-{n}", std::process::id());
    #[cfg(all(test, any(target_os = "macos", windows)))]
    tests::name_chosen(role, &name);
    name
}
struct Owned {
    name: String,
    identity: os::Identity,
}
fn remove_owned(dir: &Directory, owned: &Owned, op: Operation) -> Result<(), InstallError> {
    dir.check()?;
    fault(op)?;
    os::remove(dir.file(), OsStr::new(&owned.name), &owned.identity).map_err(|e| io_error(op, e))
}
fn rename_owned(dir: &Directory, from: &str, to: &str, identity: &os::Identity, op: Operation) -> Result<(), InstallError> {
    dir.check()?;
    fault(op)?;
    os::rename(dir.file(), OsStr::new(from), OsStr::new(to), identity).map_err(|failure| {
        let mut e = io_error(op, failure.error);
        e.moved = failure.moved;
        e
    })
}
fn recovery(mut error: InstallError, dir: &Directory, stage: Option<&Owned>, backup: Option<&Owned>, rollback: InstallError) -> InstallError {
    error.outcome = InstallOutcome::RecoveryRequired;
    error.rollback_cause = Some(rollback.cause);
    if let Some(s) = stage {
        error.recovery.push(dir.path.join(&s.name));
    }
    if let Some(b) = backup {
        error.recovery.push(dir.path.join(&b.name));
    }
    error
}
fn discard_stage(dir: &Directory, stage: &Owned, error: InstallError) -> InstallError {
    match remove_owned(dir, stage, Operation::Cleanup) {
        Ok(()) => error,
        Err(e) => recovery(error, dir, Some(stage), None, e),
    }
}
fn stage(dir: &Directory, prepared: &PreparedNativeSampler) -> Result<Owned, InstallError> {
    let mut created = None;
    for _ in 0..8 {
        let name = unique_name("stage");
        fault(Operation::CreateStage)?;
        match os::open(dir.file(), OsStr::new(&name), filesystem::Kind::Create) {
            Ok(file) => {
                created = Some((name, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(Operation::CreateStage, e)),
        }
    }
    let (name, mut file) = created.ok_or_else(|| rejected(Operation::CreateStage, Cause::Collision))?;
    let state = match os::state(&file) {
        Ok(s) if s.regular && s.links == 1 && s.size == 0 => s,
        result => {
            let mut e = result.err().map_or_else(|| rejected(Operation::Metadata, Cause::UnsafeFile), |e| io_error(Operation::Metadata, e));
            let _ = checked_close(file);
            e.outcome = InstallOutcome::RecoveryRequired;
            e.recovery.push(dir.path.join(name));
            return Err(e);
        }
    };
    let owned = Owned { name, identity: state.identity };
    let result = (|| {
        fault(Operation::Write)?;
        file.write_all(&prepared.bytes).map_err(|e| io_error(Operation::Write, e))?;
        fault(Operation::Flush)?;
        file.flush().map_err(|e| io_error(Operation::Flush, e))?;
        fault(Operation::Permission)?;
        os::make_executable(&file).map_err(|e| io_error(Operation::Permission, e))?;
        fault(Operation::Sync)?;
        file.sync_all().map_err(|e| io_error(Operation::Sync, e))?;
        Ok(())
    })();
    let close = checked_close(file);
    if let Err(e) = result.and(close) {
        return Err(discard_stage(dir, &owned, e));
    }
    let verify = (|| {
        fault(Operation::StageVerify)?;
        let read = filesystem::read(dir, OsStr::new(&owned.name), artifact::MAX_BINARY, false)?;
        if read.observed.state.identity != owned.identity || read.observed.hash != prepared.sampler_hash || !os::executable(&read.observed.state) {
            return Err(rejected(Operation::StageVerify, Cause::Integrity));
        }
        Ok(())
    })();
    if let Err(e) = verify {
        return Err(discard_stage(dir, &owned, e));
    }
    Ok(owned)
}
fn rollback(dir: &Directory, stage: &Owned, backup: Option<&Owned>, expected: Option<&Observed>, mut error: InstallError) -> InstallError {
    let target = super::exe("tune-agent");
    let restore = (|| {
        // Do not hash or delete an unknown replacement. Only the stage identity
        // grants permission to remove the target installed by this transaction.
        match os::named(dir.file(), OsStr::new(&target)) {
            Ok(s) if s.identity == stage.identity => remove_owned(dir, &Owned { name: target.clone(), identity: stage.identity.clone() }, Operation::Rollback)?,
            Ok(_) => return Err(rejected(Operation::Rollback, Cause::TargetChanged)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(Operation::Rollback, e)),
        }
        if let Some(old) = backup {
            rename_owned(dir, &old.name, &target, &old.identity, Operation::Rollback)?;
            let actual = filesystem::read(dir, OsStr::new(&target), artifact::MAX_BINARY, false)?.observed;
            if expected.is_none_or(|e| actual.state.identity != e.state.identity || actual.hash != e.hash || actual.state.mode != e.state.mode) {
                return Err(rejected(Operation::Rollback, Cause::Integrity));
            }
        } else {
            match os::named(dir.file(), OsStr::new(&target)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error(Operation::Rollback, e)),
                Ok(_) => return Err(rejected(Operation::Rollback, Cause::TargetChanged)),
            }
        }
        // If final rename never happened, the stage still exists here.
        match os::named(dir.file(), OsStr::new(&stage.name)) {
            Ok(s) if s.identity == stage.identity => remove_owned(dir, stage, Operation::Cleanup)?,
            Ok(_) => return Err(rejected(Operation::Cleanup, Cause::IdentityChanged)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(Operation::Cleanup, e)),
        }
        Ok(())
    })();
    match restore {
        Ok(()) => {
            error.outcome = InstallOutcome::RolledBack;
            error
        }
        Err(e) => {
            let mut e = recovery(error, dir, Some(stage), backup, e);
            e.recovery.push(dir.path.join(target));
            e
        }
    }
}
fn close_after_rollback(dir: Directory, mut error: InstallError, target: &Path) -> InstallError {
    if let Err(close) = dir.close() {
        error.outcome = InstallOutcome::RecoveryRequired;
        error.rollback_cause = Some(close.cause);
        if !error.recovery.iter().any(|p| p == target) {
            error.recovery.push(target.to_owned());
        }
    }
    error
}
fn install_at(prepared: PreparedNativeSampler, expected: NativeTargetSnapshot) -> Result<LocalInstallReceipt, InstallError> {
    artifact::tuple()?;
    let bin = expected.target.parent().ok_or_else(target_changed)?;
    let mut dir = Directory::open(bin.parent().ok_or_else(target_changed)?)?;
    if dir.identity() != &expected.data {
        return Err(target_changed());
    }
    let bin_state = os::named(dir.file(), OsStr::new("bin"));
    match (&expected.bin, bin_state) {
        (Some(id), Ok(s)) if s.directory && &s.identity == id => dir.child(OsStr::new("bin"))?,
        (None, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            fault(Operation::CreateBin)?;
            os::mkdir(dir.file(), OsStr::new("bin")).map_err(|e| io_error(Operation::CreateBin, e))?;
            dir.child(OsStr::new("bin"))?;
        }
        (_, Err(e)) if e.kind() != std::io::ErrorKind::NotFound => return Err(io_error(Operation::Target, e)),
        _ => return Err(target_changed()),
    }
    same_target(&dir, &expected)?;
    aliases(&dir, &prepared, expected.observed.as_ref())?;
    if expected.observed.as_ref().is_some_and(|v| !os::executable(&v.state)) {
        return Err(rejected(Operation::Target, Cause::UnsafeFile));
    }
    let mut receipt = LocalInstallReceipt {
        target: expected.target.clone(),
        changed: false,
        old_hash: expected.observed.as_ref().map(|v| v.hash.clone()),
        sampler_hash: prepared.sampler_hash.clone(),
        manifest_hash: prepared.manifest_hash.clone(),
        retained_backup: None,
    };
    if receipt.old_hash.as_deref() == Some(&prepared.sampler_hash) {
        dir.check()?;
        dir.close()?;
        return Ok(receipt);
    }
    let stage = stage(&dir, &prepared)?;
    if let Err(e) = same_target(&dir, &expected).and_then(|()| aliases(&dir, &prepared, expected.observed.as_ref())) {
        return Err(discard_stage(&dir, &stage, e));
    }
    let mut backup = None;
    if let Some(old) = &expected.observed {
        for _ in 0..8 {
            let name = unique_name("backup");
            match rename_owned(&dir, &super::exe("tune-agent"), &name, &old.state.identity, Operation::Backup) {
                Ok(()) => {
                    backup = Some(Owned { name, identity: old.state.identity.clone() });
                    break;
                }
                Err(e) if e.moved => {
                    let old = Owned { name, identity: old.state.identity.clone() };
                    let error = rollback(&dir, &stage, Some(&old), expected.observed.as_ref(), e);
                    return Err(close_after_rollback(dir, error, &expected.target));
                }
                Err(e) if e.cause == Cause::Collision => continue,
                Err(e) => return Err(discard_stage(&dir, &stage, e)),
            }
        }
        if backup.is_none() {
            return Err(discard_stage(&dir, &stage, rejected(Operation::Backup, Cause::Collision)));
        }
    }
    let placement = (|| {
        if let (Some(old), Some(wanted)) = (&backup, &expected.observed) {
            let actual = filesystem::read(&dir, OsStr::new(&old.name), artifact::MAX_BINARY, false)?.observed;
            if actual.state.identity != wanted.state.identity || actual.hash != wanted.hash || actual.state.mode != wanted.state.mode {
                return Err(target_changed());
            }
        }
        rename_owned(&dir, &stage.name, &super::exe("tune-agent"), &stage.identity, Operation::Place)?;
        fault(Operation::FinalVerify)?;
        let final_file = filesystem::read(&dir, OsStr::new(&super::exe("tune-agent")), artifact::MAX_BINARY, false)?;
        if final_file.observed.state.identity != stage.identity
            || final_file.observed.hash != prepared.sampler_hash
            || !os::executable(&final_file.observed.state)
        {
            return Err(rejected(Operation::FinalVerify, Cause::Integrity));
        }
        dir.check()?;
        Ok(())
    })();
    if let Err(e) = placement {
        let error = rollback(&dir, &stage, backup.as_ref(), expected.observed.as_ref(), e);
        return Err(close_after_rollback(dir, error, &expected.target));
    }
    receipt.changed = true;
    if let Some(old) = backup.as_ref()
        && let Err(mut error) = remove_owned(&dir, old, Operation::Cleanup)
    {
        match os::named(dir.file(), OsStr::new(&old.name)) {
            Ok(s) if s.identity == old.identity && s.regular && s.links == 1 => receipt.retained_backup = Some(dir.path.join(&old.name)),
            _ => {
                // A post-delete close failure or replaced backup is uncertain,
                // not a claim that an old backup is still safely retained.
                error.outcome = InstallOutcome::RecoveryRequired;
                error.recovery = vec![expected.target.clone(), dir.path.join(&old.name)];
                return Err(close_after_rollback(dir, error, &expected.target));
            }
        }
    }
    if let Err(mut e) = dir.close() {
        e.outcome = InstallOutcome::RecoveryRequired;
        e.recovery.push(expected.target);
        if let Some(path) = receipt.retained_backup {
            e.recovery.push(path);
        }
        return Err(e);
    }
    Ok(receipt)
}
fn fault(operation: Operation) -> Result<(), InstallError> {
    #[cfg(all(test, any(target_os = "macos", windows)))]
    {
        tests::fault(operation)?;
    }
    let _ = operation;
    Ok(())
}
#[cfg(all(test, any(target_os = "macos", windows)))]
mod tests;
