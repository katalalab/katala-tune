//! Allowlist-only source capture. Injected IO is not execution authority.
use std::{
    collections::HashSet,
    ffi::OsString,
    io::{self, Write},
    time::Instant,
};

use super::remote::{self, RemotePlatform, RemoteRequest, RemoteRuntimeKind};
use crate::topology::{RawCapture, RawGitMetadata, RawManifest, RawRepository, config::TopologyConfig, normalize_git_origin};

const GIT_BYTES: usize = 8192;
// The independent observer's Job also includes supervisor and reader roots.
pub(crate) const MAX_GIT_CHILDREN: usize = 4094;
const MAX_PATH_BYTES: usize = 4096;

#[cfg(windows)]
pub mod windows;

/// Finite, non-sensitive errors; no paths or raw process output are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeReaderError {
    Unsupported,
    Policy,
    Limit,
    Timeout,
    Source,
    Utf8,
    Git,
    Identity,
}

pub trait SourceIo {
    fn identity_hash(&self) -> Result<String, NativeReaderError>;
    /// System Windows directory, then system binary directory, from OS APIs.
    fn system_directories(&self) -> Result<(String, String), NativeReaderError>;
    fn verify_checkout(&self, root: &str, checkout: &str, deadline: Instant) -> Result<(), NativeReaderError>;
    fn read_approved(&self, root: &str, relative: &str, maximum: usize, deadline: Instant) -> Result<Vec<u8>, NativeReaderError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitOperation {
    GitDir,
    CommonDir,
    Origin,
    Head,
}

/// Constructed only by the validated collector. Contains no arbitrary argv.
pub struct GitCommand {
    program: String,
    directory: String,
    operation: GitOperation,
    environment: Vec<(OsString, OsString)>,
    deadline: Instant,
    stdout_limit: usize,
    stderr_limit: usize,
}

impl GitCommand {
    pub fn program(&self) -> &str {
        &self.program
    }
    pub fn directory(&self) -> &str {
        &self.directory
    }
    pub fn operation(&self) -> GitOperation {
        self.operation
    }
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn stdout_limit(&self) -> usize {
        self.stdout_limit
    }
    pub fn stderr_limit(&self) -> usize {
        self.stderr_limit
    }
    pub fn arguments(&self) -> Vec<&'static str> {
        let mut args = vec!["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=NUL"];
        args.extend(match self.operation {
            GitOperation::GitDir => &["rev-parse", "--absolute-git-dir"][..],
            GitOperation::CommonDir => &["rev-parse", "--path-format=absolute", "--git-common-dir"][..],
            GitOperation::Origin => &["config", "--local", "--get", "remote.origin.url"][..],
            GitOperation::Head => &["rev-parse", "--verify", "HEAD"][..],
        });
        args
    }
}

/// Implementations must enforce both pipe limits, EOF, exit status and the
/// supplied absolute deadline. Success is metadata, never an admission witness.
pub trait GitRunner {
    fn run(&self, command: &GitCommand) -> Result<Vec<u8>, NativeReaderError>;
}

pub struct UnsupportedGitRunner;
impl GitRunner for UnsupportedGitRunner {
    fn run(&self, _: &GitCommand) -> Result<Vec<u8>, NativeReaderError> {
        Err(NativeReaderError::Unsupported)
    }
}

fn time_left(deadline: Instant) -> Result<(), NativeReaderError> {
    if Instant::now() >= deadline { Err(NativeReaderError::Timeout) } else { Ok(()) }
}

fn component_ok(part: &str) -> bool {
    let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    !part.is_empty()
        && !matches!(part, "." | "..")
        && !part.ends_with(['.', ' '])
        && !part.chars().any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
        && !matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "COM¹" | "COM²" | "COM³" | "LPT¹" | "LPT²" | "LPT³")
        && !(base.len() == 4 && (base.starts_with("COM") || base.starts_with("LPT")) && matches!(base.as_bytes()[3], b'1'..=b'9'))
}
fn relative_ok(path: &str, checkout: bool) -> bool {
    path.len() <= MAX_PATH_BYTES && ((checkout && path == ".") || path.split('/').all(component_ok))
}
fn root_ok(path: &str) -> bool {
    let path = path.replace('\\', "/");
    path.len() >= 3
        && path.len() <= MAX_PATH_BYTES
        && path.as_bytes()[0].is_ascii_alphabetic()
        && &path.as_bytes()[1..3] == b":/"
        && (path.len() == 3 || path[3..].split('/').all(component_ok))
}
fn parts(relative: &str) -> usize {
    if relative == "." { 0 } else { relative.split('/').count() }
}
fn path_cost(root: &str, relative: &str) -> usize {
    root.replace('\\', "/").split('/').filter(|part| !part.is_empty()).count() + parts(relative)
}
fn joined(root: &str, relative: &str) -> String {
    let root = root.replace('\\', "/");
    if relative == "." { root } else { format!("{}/{relative}", root.trim_end_matches('/')) }
}
fn kind(path: &str) -> Option<&'static str> {
    if path.starts_with(".github/workflows/") && path.matches('/').count() == 2 && (path.ends_with(".yml") || path.ends_with(".yaml")) {
        return Some("workflow");
    }
    match path.rsplit('/').next()? {
        "package.json" => Some("node"),
        "Cargo.toml" => Some("cargo"),
        "pyproject.toml" => Some("python"),
        "go.mod" => Some("go"),
        _ => None,
    }
}

/// Preflight the whole atomic selection before even querying host identity.
/// The return value counts conservative filesystem path checks, not enumeration.
fn preflight(request: &RemoteRequest, deadline: Instant) -> Result<usize, NativeReaderError> {
    time_left(deadline)?;
    if request.platform != RemotePlatform::Windows
        || request.expected_runtime != RemoteRuntimeKind::Native
        || request.expected_distro.is_some()
        || request.cursor.is_some()
        || !request.configs.is_empty()
    {
        return Err(NativeReaderError::Unsupported);
    }
    if request.scope_id.is_empty()
        || request.scope_id.len() > 512
        || request.scope_id.chars().any(char::is_control)
        || request.observed_at_ms <= 0
        || !request.expected_identity_hash.as_deref().is_some_and(remote::hash)
        || !request.git_program.as_deref().is_some_and(super::windows_reader::valid_program_path)
        || request.roots.len() > 128
        || !request.roots.iter().all(|root| root_ok(root))
    {
        return Err(NativeReaderError::Policy);
    }
    TopologyConfig::from_value(Some(&serde_json::json!({"limits":request.limits}))).map_err(|_| NativeReaderError::Limit)?;
    let policy = request.source_allowlist.as_ref().ok_or(NativeReaderError::Policy)?;
    if policy.len() > request.limits.repositories_per_environment || policy.len().checked_mul(5).is_none_or(|n| n > MAX_GIT_CHILDREN) {
        return Err(NativeReaderError::Limit);
    }
    #[derive(serde::Serialize)]
    struct Selection<'a> {
        roots: &'a [String],
        source_allowlist: &'a [remote::RemoteSourceCheckout],
    }
    serde_json::to_writer(
        &mut LimitedSize { bytes: 0, limit: super::windows_reader::MAX_REQUEST_BYTES },
        &Selection { roots: &request.roots, source_allowlist: policy },
    )
    .map_err(|_| NativeReaderError::Limit)?;
    remote::validate_collection(request).map_err(|_| NativeReaderError::Policy)?;
    let mut cost = 0usize;
    let mut raw_bytes = 0usize;
    let mut selected = HashSet::new();
    let mut checkouts = HashSet::new();
    for checkout in policy {
        time_left(deadline)?;
        let root = &request.roots[checkout.root_index];
        if !relative_ok(&checkout.relative_path, true)
            || parts(&checkout.relative_path) > request.limits.depth
            || joined(root, &checkout.relative_path).len() > MAX_PATH_BYTES
            || !checkouts.insert((remote::source_root_fingerprint(root, RemotePlatform::Windows), checkout.relative_path.to_lowercase()))
        {
            return Err(NativeReaderError::Policy);
        }
        cost = cost
            .checked_add(path_cost(root, &checkout.relative_path).checked_mul(2).and_then(|n| n.checked_add(1)).ok_or(NativeReaderError::Limit)?)
            .ok_or(NativeReaderError::Limit)?;
        for file in &checkout.manifests {
            time_left(deadline)?;
            let selected_path =
                if checkout.relative_path == "." { file.relative_path.clone() } else { format!("{}/{}", checkout.relative_path, file.relative_path) };
            if !relative_ok(&file.relative_path, false)
                || parts(&selected_path).saturating_sub(1) > request.limits.depth
                || !selected.insert((remote::source_root_fingerprint(root, RemotePlatform::Windows), selected_path.to_lowercase()))
            {
                return Err(NativeReaderError::Policy);
            }
            if joined(root, &selected_path).len() > MAX_PATH_BYTES {
                return Err(NativeReaderError::Limit);
            }
            raw_bytes = raw_bytes.checked_add(file.bytes).filter(|n| *n <= request.limits.stdout_bytes).ok_or(NativeReaderError::Limit)?;
            cost = cost.checked_add(path_cost(root, &selected_path)).ok_or(NativeReaderError::Limit)?;
            if cost > request.limits.entries_per_environment {
                return Err(NativeReaderError::Limit);
            }
        }
        if cost > request.limits.entries_per_environment {
            return Err(NativeReaderError::Limit);
        }
    }
    Ok(cost)
}

pub(crate) fn environment(source: &dyn SourceIo) -> Result<Vec<(OsString, OsString)>, NativeReaderError> {
    let (root, system) = source.system_directories()?;
    if !root_ok(&root) || !root_ok(&system) {
        return Err(NativeReaderError::Source);
    }
    Ok([
        ("SystemRoot", root.as_str()),
        ("WINDIR", root.as_str()),
        ("PATH", system.as_str()),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "NUL"),
        ("GIT_CONFIG_SYSTEM", "NUL"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect())
}

struct LimitedSize {
    bytes: usize,
    limit: usize,
}
impl Write for LimitedSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.checked_add(bytes.len()).filter(|n| *n <= self.limit).ok_or_else(|| io::Error::from(io::ErrorKind::FileTooLarge))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn collect_with(request: &RemoteRequest, deadline: Instant, source: &dyn SourceIo, git: &dyn GitRunner) -> Result<RawCapture, NativeReaderError> {
    let started = Instant::now();
    let entries = preflight(request, deadline)?;
    let identity = source.identity_hash()?;
    if request.expected_identity_hash.as_deref() != Some(identity.as_str()) {
        return Err(NativeReaderError::Identity);
    }
    time_left(deadline)?;
    let env = environment(source)?;
    time_left(deadline)?;
    let mut capture = RawCapture {
        scope_id: request.scope_id.clone(),
        adapter: "remote".into(),
        target_identity: Some(identity),
        observed_at_ms: request.observed_at_ms,
        limits: request.limits.clone(),
        repositories: vec![],
        failures: vec![],
        counters: Default::default(),
        progress: None,
    };
    for approved in request.source_allowlist.as_ref().ok_or(NativeReaderError::Policy)? {
        time_left(deadline)?;
        let root = &request.roots[approved.root_index];
        source.verify_checkout(root, &approved.relative_path, deadline)?;
        let directory = joined(root, &approved.relative_path);
        let mut metadata = |operation| -> Result<String, NativeReaderError> {
            time_left(deadline)?;
            let command = GitCommand {
                program: request.git_program.clone().ok_or(NativeReaderError::Policy)?,
                directory: directory.clone(),
                operation,
                environment: env.clone(),
                deadline,
                stdout_limit: GIT_BYTES.min(request.limits.stdout_bytes),
                stderr_limit: GIT_BYTES.min(request.limits.stderr_bytes),
            };
            capture.counters.children += 1;
            let bytes = git.run(&command)?;
            time_left(deadline)?;
            if bytes.len() > command.stdout_limit {
                return Err(NativeReaderError::Limit);
            }
            let value = String::from_utf8(bytes).map_err(|_| NativeReaderError::Utf8)?;
            if value.contains('\0') {
                return Err(NativeReaderError::Git);
            }
            Ok(value.trim().to_owned())
        };
        let git_dir = metadata(GitOperation::GitDir)?;
        let common_dir = metadata(GitOperation::CommonDir)?;
        let origin = metadata(GitOperation::Origin)?;
        let head = metadata(GitOperation::Head)?;
        if git_dir.is_empty() || common_dir.is_empty() || head != approved.expected_head {
            return Err(NativeReaderError::Source);
        }
        let mut repo = RawRepository {
            root_fingerprint: remote::source_root_fingerprint(root, RemotePlatform::Windows),
            relative_path: approved.relative_path.clone(),
            git: RawGitMetadata {
                origin: normalize_git_origin(&origin),
                head: Some(head.clone()),
                git_dir: Some(crate::db::sha256_hex(git_dir.as_bytes())),
                common_dir: Some(crate::db::sha256_hex(common_dir.as_bytes())),
                is_worktree: git_dir.to_lowercase() != common_dir.to_lowercase(),
            },
            manifests: vec![],
            configs: vec![],
            config_file_read: false,
            failures: vec![],
        };
        for file in &approved.manifests {
            time_left(deadline)?;
            let maximum = if kind(&file.relative_path) == Some("workflow") {
                request.limits.workflow_bytes.min(request.limits.file_bytes)
            } else {
                request.limits.file_bytes
            };
            let selected_path =
                if approved.relative_path == "." { file.relative_path.clone() } else { format!("{}/{}", approved.relative_path, file.relative_path) };
            let bytes = source.read_approved(root, &selected_path, maximum.min(file.bytes), deadline)?;
            time_left(deadline)?;
            // This exact buffer is authenticated before ANY UTF-8 decoding.
            if bytes.len() != file.bytes || crate::db::sha256_hex(&bytes) != file.sha256 {
                return Err(NativeReaderError::Source);
            }
            let content = String::from_utf8(bytes).map_err(|_| NativeReaderError::Utf8)?;
            if content.contains('\0') {
                return Err(NativeReaderError::Source);
            }
            capture.counters.files += 1;
            capture.counters.manifests += 1;
            capture.counters.bytes += content.len();
            repo.manifests.push(RawManifest {
                relative_path: file.relative_path.clone(),
                kind: kind(&file.relative_path).ok_or(NativeReaderError::Policy)?.into(),
                content_sha256: file.sha256.clone(),
                content,
                revision: Some(head.clone()),
            });
        }
        capture.repositories.push(repo);
        capture.counters.repositories += 1;
    }
    // Recheck ALL checkout heads only after every approved source has been read.
    // An early checkout can change while a later checkout is being collected.
    for approved in request.source_allowlist.as_ref().ok_or(NativeReaderError::Policy)? {
        time_left(deadline)?;
        let command = GitCommand {
            program: request.git_program.clone().ok_or(NativeReaderError::Policy)?,
            directory: joined(&request.roots[approved.root_index], &approved.relative_path),
            operation: GitOperation::Head,
            environment: env.clone(),
            deadline,
            stdout_limit: GIT_BYTES.min(request.limits.stdout_bytes),
            stderr_limit: GIT_BYTES.min(request.limits.stderr_bytes),
        };
        capture.counters.children += 1;
        let bytes = git.run(&command)?;
        time_left(deadline)?;
        if bytes.len() > command.stdout_limit {
            return Err(NativeReaderError::Limit);
        }
        if String::from_utf8(bytes).map_err(|_| NativeReaderError::Utf8)?.trim() != approved.expected_head {
            return Err(NativeReaderError::Source);
        }
    }
    capture.counters.entries = entries;
    capture.counters.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    time_left(deadline)?;
    // Count escaping and metadata without allocating a potentially oversized JSON buffer.
    serde_json::to_writer(&mut LimitedSize { bytes: 0, limit: request.limits.stdout_bytes }, &capture).map_err(|_| NativeReaderError::Limit)?;
    time_left(deadline)?;
    Ok(capture)
}
