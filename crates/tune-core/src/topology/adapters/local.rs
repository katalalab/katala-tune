use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use serde_json::{Value, json};

use crate::topology::model::PARSER_VERSION;
use crate::topology::{
    CaptureCounters, DependencySource, Edge, EdgeKind, Entity, EntityKind, Failure, FailureCode, ParsedDependency, RawCapture, RawConfig, RawGitMetadata,
    RawManifest, RawRepository, ScopeStatus, SourceCacheEntry, SourceCacheKey, SourceFacts, TopologyLimits, TopologySnapshot, canonical_entity_id,
    normalize_git_origin, normalize_path,
    parsers::{parse_cargo, parse_go, parse_node, parse_python, parse_workflow},
};

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|value| value.as_millis() as i64).unwrap_or_default()
}

fn sha256(value: &[u8]) -> String {
    crate::db::sha256_hex(value)
}

fn failure(code: FailureCode, message: &'static str) -> Failure {
    Failure::new(code, message)
}

fn compact_failures(capture: &mut RawCapture) {
    capture.failures.sort_by(|left, right| {
        (left.code as u8, left.message.as_str(), left.line, left.column).cmp(&(right.code as u8, right.message.as_str(), right.line, right.column))
    });
    capture.failures.dedup();
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(|value| value.to_str())
        .map(|value| value.replace('\\', "/"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".".into())
}

fn git_path_reference(root: &Path, value: &str) -> String {
    let path = Path::new(value);
    if path.strip_prefix(root).is_ok() {
        return relative(root, path);
    }
    format!("opaque:{}", sha256(normalize_path(value, if cfg!(windows) { "windows" } else { "posix" }).as_bytes()))
}

fn marker(path: &Path) -> bool {
    path.join(".git").symlink_metadata().is_ok()
}

// These inherited variables can override `git -C <checkout>` or inject config.
const GIT_ENV_REMOVE: [&str; 11] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CEILING_DIRECTORIES",
];

fn git(path: &Path, args: &[&str], limits: &TopologyLimits, deadline: Instant, counters: Option<&mut CaptureCounters>) -> Result<Option<String>, Failure> {
    if let Some(counters) = counters {
        counters.children = counters.children.saturating_add(1);
    }
    let mut command = vec!["--no-optional-locks".into(), "-c".into(), "core.fsmonitor=false".into(), "-C".into(), path.to_string_lossy().into_owned()];
    command.extend(args.iter().map(|value| (*value).to_owned()));
    let timeout = deadline.checked_duration_since(Instant::now()).ok_or_else(|| failure(FailureCode::Timeout, "local scan timed out"))?;
    let stdout_limit = limits.stdout_bytes;
    let stderr_limit = limits.stderr_bytes;
    let result = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build().map(|runtime| {
            runtime.block_on(crate::collect::run_bounded_with_env_clean(
                "git",
                &command,
                &[],
                timeout,
                stdout_limit,
                stderr_limit,
                crate::collect::BoundedEnvironment { set: &[("GIT_NO_LAZY_FETCH", "1"), ("GIT_OPTIONAL_LOCKS", "0")], remove: &GIT_ENV_REMOVE },
            ))
        })
    })
    .join()
    .map_err(|_| failure(FailureCode::Internal, "git read failed"))?
    .map_err(|_| failure(FailureCode::Internal, "git transport initialization failed"))?;
    if result.spawn_failed {
        return Err(failure(FailureCode::ToolMissing, "git is unavailable"));
    }
    if result.timed_out {
        return Err(failure(FailureCode::Timeout, "git read timed out"));
    }
    if result.output_limited {
        return Err(failure(FailureCode::LimitReached, "git output budget exceeded"));
    }
    if result.code != Some(0) {
        return Ok(None);
    }
    Ok(Some(String::from_utf8(result.stdout).map_err(|_| failure(FailureCode::ParseError, "git output is not utf-8"))?.trim().to_owned()))
}

fn git_metadata(path: &Path, root: &Path, limits: &TopologyLimits, deadline: Instant, counters: &mut CaptureCounters) -> Result<RawGitMetadata, Failure> {
    let Some(_) = git(path, &["rev-parse", "--is-inside-work-tree"], limits, deadline, Some(counters))? else {
        return Err(failure(FailureCode::ParseError, "git metadata is unavailable"));
    };
    let git_dir = git(path, &["rev-parse", "--path-format=absolute", "--absolute-git-dir"], limits, deadline, Some(counters))?;
    let common_dir = git(path, &["rev-parse", "--path-format=absolute", "--git-common-dir"], limits, deadline, Some(counters))?;
    let is_worktree = git_dir.as_deref() != common_dir.as_deref();
    let origin = git(path, &["config", "--get", "remote.origin.url"], limits, deadline, Some(counters))?.and_then(|value| normalize_git_origin(&value));
    Ok(RawGitMetadata {
        origin,
        head: git(path, &["rev-parse", "--verify", "HEAD"], limits, deadline, Some(counters))?,
        git_dir: git_dir.map(|value| git_path_reference(root, &value)),
        common_dir: common_dir.map(|value| git_path_reference(root, &value)),
        is_worktree,
    })
}

fn manifest_kind(path: &Path) -> Option<&'static str> {
    match path.file_name().and_then(|value| value.to_str()) {
        Some("Cargo.toml") => Some("cargo"),
        Some("package.json") => Some("node"),
        Some("pyproject.toml") => Some("python"),
        Some("go.mod") => Some("go"),
        Some(name)
            if (name.ends_with(".yml") || name.ends_with(".yaml"))
                && path.parent().and_then(Path::file_name).is_some_and(|value| value == "workflows")
                && path.parent().and_then(Path::parent).and_then(Path::file_name).is_some_and(|value| value == ".github") =>
        {
            Some("workflow")
        }
        _ => None,
    }
}

fn root_fingerprint(root: &Path) -> String {
    sha256(normalize_path(&root.to_string_lossy(), if cfg!(windows) { "windows" } else { "posix" }).as_bytes())
}

fn ensure_fallback(capture: &mut RawCapture, root: &Path, fallback: &mut Option<usize>, limits: &TopologyLimits) -> Option<usize> {
    if fallback.is_none() && capture.counters.repositories >= limits.repositories_per_environment {
        capture.failures.push(failure(FailureCode::LimitReached, "repository budget exceeded"));
        return None;
    }
    Some(*fallback.get_or_insert_with(|| {
        capture.repositories.push(RawRepository {
            relative_path: relative(root, root),
            root_fingerprint: root_fingerprint(root),
            git: RawGitMetadata::default(),
            manifests: Vec::new(),
            configs: Vec::new(),
            config_file_read: false,
            failures: Vec::new(),
        });
        capture.counters.repositories += 1;
        capture.repositories.len() - 1
    }))
}

fn add_repository(capture: &mut RawCapture, root: &Path, path: &Path, limits: &TopologyLimits, deadline: Instant) -> Option<usize> {
    if capture.counters.repositories >= limits.repositories_per_environment {
        capture.failures.push(failure(FailureCode::LimitReached, "repository budget exceeded"));
        return None;
    }
    let mut repo = RawRepository {
        relative_path: relative(root, path),
        root_fingerprint: root_fingerprint(root),
        git: RawGitMetadata::default(),
        manifests: Vec::new(),
        configs: Vec::new(),
        config_file_read: false,
        failures: Vec::new(),
    };
    match git_metadata(path, root, limits, deadline, &mut capture.counters) {
        Ok(metadata) => repo.git = metadata,
        Err(error) => repo.failures.push(error),
    }
    capture.counters.repositories += 1;
    capture.repositories.push(repo);
    Some(capture.repositories.len() - 1)
}

const SKIPPED_DIRECTORIES: [&str; 12] =
    [".git", ".branch-recovery", "target", "node_modules", ".cache", "cache", "__pycache__", ".venv", "venv", "vendor", "dist", "build"];

fn is_skipped_directory(name: &str) -> bool {
    SKIPPED_DIRECTORIES.contains(&name)
}

#[derive(Clone)]
struct CursorFrame {
    directory: PathBuf,
    depth: usize,
    after: Option<String>,
}

fn safe_cursor_relative(value: &str) -> bool {
    value == "."
        || (!value.is_empty()
            && !value.starts_with('/')
            && !value.contains(['\\', ':'])
            && !value.split('/').any(|part| part.is_empty() || part == "..")
            && !value.chars().any(char::is_control))
}

fn safe_cursor_entry(value: &str) -> bool {
    !value.is_empty() && !value.contains(['/', '\\']) && !value.chars().any(char::is_control)
}

fn cursor_frames(root: &Path, value: Option<&Value>, limits: &TopologyLimits) -> Vec<CursorFrame> {
    let Some(values) = value.and_then(|value| value.get("frames")).and_then(Value::as_array) else {
        return Vec::new();
    };
    if values.len() > 64 {
        return Vec::new();
    }
    let mut frames = Vec::new();
    for value in values {
        let Some(directory) = value.get("dir").and_then(Value::as_str).filter(|value| safe_cursor_relative(value)) else {
            return Vec::new();
        };
        let after = value.get("after").and_then(Value::as_str).map(str::to_owned);
        if after.as_deref().is_some_and(|value| !safe_cursor_entry(value)) {
            return Vec::new();
        }
        let path = if directory == "." { root.to_path_buf() } else { root.join(directory) };
        let depth = if directory == "." { 0 } else { directory.split('/').count() };
        if depth > limits.depth || !path.starts_with(root) {
            return Vec::new();
        }
        frames.push(CursorFrame { directory: path, depth, after });
    }
    frames.reverse();
    frames
}

fn serialize_frames(root: &Path, frames: &[CursorFrame]) -> Result<Value, Failure> {
    if frames.len() > 64 {
        return Err(failure(FailureCode::LimitReached, "directory cursor frame budget exceeded"));
    }
    let values = frames
        .iter()
        .rev()
        .map(|frame| {
            let directory = relative(root, &frame.directory);
            json!({"dir":directory,"after":frame.after})
        })
        .collect::<Vec<_>>();
    let value = Value::Array(values);
    (serde_json::to_vec(&value).map(|bytes| bytes.len() <= 8 * 1024).unwrap_or(false))
        .then_some(value)
        .ok_or_else(|| failure(FailureCode::LimitReached, "directory cursor byte budget exceeded"))
}

fn repository_for_directory(capture: &mut RawCapture, root: &Path, directory: &Path, limits: &TopologyLimits, deadline: Instant) -> Option<usize> {
    let mut current = Some(directory);
    while let Some(path) = current {
        if !path.starts_with(root) {
            break;
        }
        if marker(path) {
            let relative_path = relative(root, path);
            if let Some(index) =
                capture.repositories.iter().position(|repo| repo.relative_path == relative_path && repo.root_fingerprint == root_fingerprint(root))
            {
                return Some(index);
            }
            return add_repository(capture, root, path, limits, deadline);
        }
        if path == root {
            break;
        }
        current = path.parent();
    }
    None
}

fn capture_root(
    capture: &mut RawCapture,
    root: &Path,
    limits: &TopologyLimits,
    started: Instant,
    initial_frames: Vec<CursorFrame>,
) -> Option<Vec<CursorFrame>> {
    let deadline = started + Duration::from_secs(limits.local_timeout_seconds);
    let mut fallback = None;
    let mut walk_failures = Vec::<(PathBuf, Failure)>::new();
    let mut pending = None;
    macro_rules! scan_failure {
        ($directory:expr, $code:expr, $message:expr) => {{
            let item = failure($code, $message);
            capture.failures.push(item.clone());
            walk_failures.push(($directory, item));
        }};
    }
    // A parent frame is resumed after each child. Keep completed directory listings for
    // this capture so a wide root is not re-enumerated once per child. The cache never
    // crosses runs; opaque cursor state continues to contain only positions.
    let mut listing_cache = HashMap::<PathBuf, Vec<String>>::new();
    let mut cached_names = 0usize;
    let listing_cache_limit = limits.entries_per_environment;
    let mut stack = if initial_frames.is_empty() { vec![CursorFrame { directory: root.to_path_buf(), depth: 0, after: None }] } else { initial_frames };
    'walk: while let Some(mut frame) = stack.pop() {
        let listed_directory = frame.directory.clone();
        if Instant::now() >= deadline {
            scan_failure!(frame.directory.clone(), FailureCode::Timeout, "local scan timed out");
            stack.push(frame);
            pending = Some(stack);
            break 'walk;
        }
        if frame.depth > limits.depth {
            scan_failure!(frame.directory.clone(), FailureCode::LimitReached, "directory depth budget exceeded");
            continue;
        }
        let entries = if let Some(entries) = listing_cache.get(&frame.directory) {
            entries.clone()
        } else {
            let entries = match fs::read_dir(&frame.directory) {
                Ok(read) => read.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>(),
                Err(_) => {
                    scan_failure!(frame.directory.clone(), FailureCode::PermissionDenied, "directory cannot be read");
                    continue;
                }
            };
            let Ok(mut entries) = entries else {
                scan_failure!(frame.directory.clone(), FailureCode::Internal, "directory entry cannot be read");
                continue;
            };
            entries.sort();
            if entries.len() <= listing_cache_limit.saturating_sub(cached_names) {
                cached_names += entries.len();
                listing_cache.insert(frame.directory.clone(), entries.clone());
            }
            entries
        };
        for name in entries {
            if frame.after.as_deref().is_some_and(|after| name.as_str() <= after) {
                continue;
            }
            if capture.counters.entries >= limits.entries_per_environment {
                scan_failure!(frame.directory.clone(), FailureCode::LimitReached, "directory entry budget exceeded");
                stack.push(frame);
                pending = Some(stack);
                break 'walk;
            }
            if !safe_cursor_entry(&name) {
                scan_failure!(frame.directory.clone(), FailureCode::Unsupported, "directory entry cannot be cursorized");
                frame.after = Some(name);
                continue;
            }
            let path = frame.directory.join(&name);
            let kind = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata.file_type(),
                Err(_) => {
                    scan_failure!(frame.directory.clone(), FailureCode::Internal, "directory entry type is unavailable");
                    frame.after = Some(name);
                    continue;
                }
            };
            capture.counters.entries += 1;
            frame.after = Some(name.clone());
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if is_skipped_directory(&name) {
                    continue;
                }
                let child_depth = frame.depth + 1;
                stack.push(frame);
                stack.push(CursorFrame { directory: path, depth: child_depth, after: None });
                break;
            }
            if !kind.is_file() {
                continue;
            }
            let Some(kind) = manifest_kind(&path) else {
                continue;
            };
            capture.counters.files += 1;
            if capture.counters.manifests >= limits.manifests_per_environment {
                scan_failure!(frame.directory.clone(), FailureCode::LimitReached, "manifest budget exceeded");
                stack.push(frame);
                pending = Some(stack);
                break 'walk;
            }
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    scan_failure!(frame.directory.clone(), FailureCode::Internal, "manifest metadata is unavailable");
                    continue;
                }
            };
            let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
            if size > limits.file_bytes || capture.counters.bytes.saturating_add(size) > limits.total_bytes_per_environment {
                scan_failure!(frame.directory.clone(), FailureCode::LimitReached, "manifest byte budget exceeded");
                continue;
            }
            let mut bytes = Vec::with_capacity(size.min(limits.file_bytes));
            let mut file = match File::open(&path) {
                Ok(file) => file,
                Err(_) => {
                    scan_failure!(frame.directory.clone(), FailureCode::PermissionDenied, "manifest cannot be read");
                    continue;
                }
            };
            if file.by_ref().take((limits.file_bytes as u64).saturating_add(1)).read_to_end(&mut bytes).is_err() {
                scan_failure!(frame.directory.clone(), FailureCode::Internal, "manifest read failed");
                continue;
            }
            if bytes.len() > limits.file_bytes {
                scan_failure!(frame.directory.clone(), FailureCode::LimitReached, "manifest byte budget exceeded");
                continue;
            }
            let content = match String::from_utf8(bytes) {
                Ok(content) => content,
                Err(_) => {
                    scan_failure!(frame.directory.clone(), FailureCode::ParseError, "manifest is not utf-8");
                    continue;
                }
            };
            let Some(repo_index) =
                repository_for_directory(capture, root, &frame.directory, limits, deadline).or_else(|| ensure_fallback(capture, root, &mut fallback, limits))
            else {
                continue;
            };
            let revision = capture.repositories[repo_index].git.head.clone();
            capture.repositories[repo_index].manifests.push(RawManifest {
                relative_path: relative(root, &path),
                kind: kind.into(),
                content_sha256: sha256(content.as_bytes()),
                content,
                revision,
            });
            capture.counters.bytes += size;
            capture.counters.manifests += 1;
        }
        if let Some(entries) = listing_cache.remove(&listed_directory) {
            cached_names = cached_names.saturating_sub(entries.len());
        }
    }
    apply_walk_failures(capture, root, &walk_failures);
    pending
}

/// A repository is only complete when every directory below it was enumerated.
/// Directory failures stay in the coverage scope, so attach them to the deepest
/// containing repository as well; otherwise `assemble_capture` would report a
/// truncated walk as `Complete` and the store would remove the missing graph.
fn apply_walk_failures(capture: &mut RawCapture, root: &Path, failures: &[(PathBuf, Failure)]) {
    for (directory, item) in failures {
        let mut best: Option<(usize, usize)> = None;
        for (index, repository) in capture.repositories.iter().enumerate() {
            let repository_root = if repository.relative_path == "." { root.to_path_buf() } else { root.join(&repository.relative_path) };
            if directory.starts_with(&repository_root) {
                let depth = repository_root.components().count();
                if best.is_none_or(|(_, current)| depth > current) {
                    best = Some((index, depth));
                }
            }
        }
        if let Some((index, _)) = best {
            capture.repositories[index].failures.push(item.clone());
        }
    }
}

fn safe_config_key(pointer: &str, mode: &str) -> bool {
    mode != "literal"
        || !pointer.split('/').any(|part| {
            let part = part.to_ascii_lowercase();
            part.contains("secret") || part.contains("token") || part.contains("password") || part == "key" || part.ends_with("_key")
        })
}

fn open_regular_config(path: &Path) -> Result<File, Failure> {
    let before = fs::symlink_metadata(path).map_err(|_| failure(FailureCode::PermissionDenied, "configuration metadata is unavailable"))?;
    if before.file_type().is_symlink() || !before.file_type().is_file() {
        return Err(failure(FailureCode::Unsupported, "configuration must be a regular file"));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    // Do not follow a replacement symlink and do not block on a FIFO swapped in
    // after the metadata check. The opened handle is validated below.
    #[cfg(target_os = "macos")]
    options.custom_flags(0x0004 | 0x0100); // O_NONBLOCK | O_NOFOLLOW
    #[cfg(target_os = "linux")]
    options.custom_flags(0o4000 | 0o400000); // O_NONBLOCK | O_NOFOLLOW
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options.open(path).map_err(|_| failure(FailureCode::PermissionDenied, "configuration cannot be read"))?;
    let opened = file.metadata().map_err(|_| failure(FailureCode::PermissionDenied, "configuration metadata is unavailable"))?;
    if !opened.file_type().is_file() {
        return Err(failure(FailureCode::Unsupported, "configuration must be a regular file"));
    }
    #[cfg(unix)]
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err(failure(FailureCode::PermissionDenied, "configuration changed before open"));
    }
    Ok(file)
}

fn read_bounded_regular_file(path: &Path, limit: usize, deadline: Instant) -> Result<Vec<u8>, Failure> {
    if Instant::now() >= deadline {
        return Err(failure(FailureCode::Timeout, "local scan timed out"));
    }
    let mut file = open_regular_config(path)?;
    let metadata = file.metadata().map_err(|_| failure(FailureCode::PermissionDenied, "configuration metadata is unavailable"))?;
    if metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(failure(FailureCode::LimitReached, "configuration byte budget exceeded"));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(limit).min(limit));
    let mut chunk = [0u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Err(failure(FailureCode::Timeout, "local scan timed out"));
        }
        let read = file.read(&mut chunk).map_err(|_| failure(FailureCode::Internal, "configuration read failed"))?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > limit {
            return Err(failure(FailureCode::LimitReached, "configuration byte budget exceeded"));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

const CONFIG_HASH_ALGORITHM: &str = "hash_json_typed_v1";
const MAX_EXACT_JSON_INTEGER: u64 = 9_007_199_254_740_992;

fn canonical_json(value: &Value, out: &mut Vec<u8>) -> Result<(), Failure> {
    match value {
        Value::Null => out.extend_from_slice(b"z"),
        Value::Bool(value) => out.extend_from_slice(if *value { b"b1" } else { b"b0" }),
        Value::Number(value) => {
            let number = if let Some(value) = value.as_i64() {
                if value.unsigned_abs() > MAX_EXACT_JSON_INTEGER {
                    return Err(failure(FailureCode::Unsupported, "configuration number exceeds cross-provider precision"));
                }
                value as f64
            } else if let Some(value) = value.as_u64() {
                if value > MAX_EXACT_JSON_INTEGER {
                    return Err(failure(FailureCode::Unsupported, "configuration number exceeds cross-provider precision"));
                }
                value as f64
            } else {
                let number =
                    value.as_f64().filter(|value| value.is_finite()).ok_or_else(|| failure(FailureCode::Unsupported, "configuration number is unsupported"))?;
                if number.fract() == 0.0 && number.abs() > MAX_EXACT_JSON_INTEGER as f64 {
                    return Err(failure(FailureCode::Unsupported, "configuration number exceeds cross-provider precision"));
                }
                number
            };
            let bits = if number == 0.0 { 0 } else { number.to_bits() };
            out.extend_from_slice(format!("n{bits:016x}").as_bytes());
        }
        Value::String(value) => {
            out.extend_from_slice(format!("s{}:", value.len()).as_bytes());
            out.extend_from_slice(value.as_bytes());
        }
        Value::Array(values) => {
            out.extend_from_slice(format!("a{}:[", values.len()).as_bytes());
            for value in values {
                canonical_json(value, out)?;
            }
            out.push(b']');
        }
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            out.extend_from_slice(format!("o{}:{{", entries.len()).as_bytes());
            for (key, value) in entries {
                canonical_json(&Value::String(key.clone()), out)?;
                canonical_json(value, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn record_config_failure(capture: &mut RawCapture, index: usize, error: Failure) {
    capture.failures.push(error.clone());
    if let Some(repository) = capture.repositories.get_mut(index) {
        repository.failures.push(error);
    }
}

fn capture_configs(capture: &mut RawCapture, configs: &[Value], limits: &TopologyLimits, started: Instant) {
    let deadline = started + Duration::from_secs(limits.local_timeout_seconds);
    let mut config_key_attempts = 0usize;
    for (config_file_attempts, config) in configs.iter().enumerate() {
        if config_file_attempts >= limits.manifests_per_environment {
            capture.failures.push(failure(FailureCode::LimitReached, "configuration file budget exceeded"));
            return;
        }
        if Instant::now() >= deadline {
            capture.failures.push(failure(FailureCode::Timeout, "local scan timed out"));
            return;
        }
        let Some(path) = config.get("path").and_then(Value::as_str).map(PathBuf::from) else {
            capture.failures.push(failure(FailureCode::ParseError, "configuration path is invalid"));
            continue;
        };
        if !path.is_absolute() {
            capture.failures.push(failure(FailureCode::Unsupported, "configuration path must be absolute"));
            continue;
        }
        if config.get("format").and_then(Value::as_str).unwrap_or("json") != "json" {
            capture.failures.push(failure(FailureCode::Unsupported, "configuration format is unsupported"));
            continue;
        }
        if capture.counters.repositories >= limits.repositories_per_environment {
            capture.failures.push(failure(FailureCode::LimitReached, "configuration repository budget exceeded"));
            return;
        }
        let path_id = sha256(normalize_path(&path.to_string_lossy(), if cfg!(windows) { "windows" } else { "posix" }).as_bytes());
        let index = capture.repositories.len();
        capture.repositories.push(RawRepository {
            // Configuration is intentionally independent from repository discovery.
            // The opaque path fingerprint prevents file/pointer collisions without persisting a private path.
            relative_path: format!("configuration-{path_id}"),
            root_fingerprint: sha256(format!("{}|configuration|{path_id}", capture.scope_id).as_bytes()),
            git: RawGitMetadata::default(),
            manifests: Vec::new(),
            configs: Vec::new(),
            config_file_read: false,
            failures: Vec::new(),
        });
        capture.counters.repositories += 1;
        let bytes = match read_bounded_regular_file(&path, limits.file_bytes, deadline) {
            Ok(bytes) => bytes,
            Err(error) => {
                record_config_failure(capture, index, error);
                continue;
            }
        };
        if capture.counters.bytes.saturating_add(bytes.len()) > limits.total_bytes_per_environment {
            record_config_failure(capture, index, failure(FailureCode::LimitReached, "configuration byte budget exceeded"));
            continue;
        }
        capture.counters.files += 1;
        capture.counters.config_files += 1;
        capture.counters.bytes += bytes.len();
        capture.repositories[index].config_file_read = true;
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                record_config_failure(capture, index, failure(FailureCode::ParseError, "configuration is not valid json"));
                continue;
            }
        };
        for key in config.get("keys").and_then(Value::as_array).into_iter().flatten() {
            if config_key_attempts >= limits.manifests_per_environment {
                record_config_failure(capture, index, failure(FailureCode::LimitReached, "configuration key budget exceeded"));
                return;
            }
            config_key_attempts += 1;
            let Some(pointer) = key.get("pointer").and_then(Value::as_str) else {
                record_config_failure(capture, index, failure(FailureCode::ParseError, "configuration key is invalid"));
                continue;
            };
            let mode = key.get("mode").and_then(Value::as_str).unwrap_or("hash");
            if !matches!(mode, "hash" | "literal") {
                record_config_failure(capture, index, failure(FailureCode::Unsupported, "configuration mode is unsupported"));
                continue;
            }
            if !safe_config_key(pointer, mode) {
                record_config_failure(capture, index, failure(FailureCode::PermissionDenied, "literal secret configuration is rejected"));
                continue;
            }
            let Some(selected) = value.pointer(pointer) else {
                record_config_failure(capture, index, failure(FailureCode::ParseError, "configuration key is missing"));
                continue;
            };
            let mut encoded = Vec::new();
            if let Err(error) = canonical_json(selected, &mut encoded) {
                record_config_failure(capture, index, error);
                continue;
            }
            capture.counters.config_keys += 1;
            capture.repositories[index].configs.push(RawConfig {
                relative_path: path_id.clone(),
                key: pointer.into(),
                value_hash: sha256(&encoded),
                mode: CONFIG_HASH_ALGORITHM.into(),
            });
        }
    }
}

pub fn capture_local(scope_id: impl Into<String>, roots: &[PathBuf], configs: &[Value], limits: &TopologyLimits) -> RawCapture {
    capture_local_with_progress(scope_id, roots, configs, limits, None).0
}

/// Rotate configured roots after a fenced, opaque cursor. Repository-level
/// partial scopes remain receipts; a root limit never rewrites an older success.
pub fn capture_local_with_progress(
    scope_id: impl Into<String>,
    roots: &[PathBuf],
    configs: &[Value],
    limits: &TopologyLimits,
    progress: Option<&Value>,
) -> (RawCapture, Value) {
    let started = Instant::now();
    let scope_id = scope_id.into();
    let fingerprint = sha256(
        format!(
            "{}|{}|{}|{}",
            PARSER_VERSION,
            SKIPPED_DIRECTORIES.join(","),
            serde_json::to_string(limits).unwrap_or_default(),
            roots.iter().map(|root| normalize_path(&root.to_string_lossy(), if cfg!(windows) { "windows" } else { "posix" })).collect::<Vec<_>>().join("\n"),
        )
        .as_bytes(),
    );
    let valid_progress = progress
        .filter(|value| value.get("version").and_then(Value::as_u64) == Some(1) && value.get("fingerprint").and_then(Value::as_str) == Some(&fingerprint));
    let root_count = roots.len();
    let start = valid_progress
        .and_then(|value| value.get("next_root").and_then(Value::as_u64))
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value < root_count)
        .unwrap_or(0);
    let count = root_count.min(limits.roots_per_environment);
    let ordered = (0..count).map(|offset| (start + offset) % root_count).collect::<Vec<_>>();
    let mut capture = RawCapture {
        scope_id,
        adapter: "local".into(),
        target_identity: None,
        observed_at_ms: now_ms(),
        limits: limits.clone(),
        repositories: Vec::new(),
        failures: Vec::new(),
        counters: CaptureCounters::default(),
        progress: None,
    };
    if roots.is_empty() {
        capture.failures.push(failure(FailureCode::Unsupported, "no roots are configured"));
    } else if count == 0 {
        capture.failures.push(failure(FailureCode::LimitReached, "configured root budget is zero"));
    }
    let mut seen = HashSet::new();
    let mut next_root = 0usize;
    for index in &ordered {
        let root = &roots[*index];
        let failures_before = capture.failures.len();
        let normalized = match fs::canonicalize(root) {
            Ok(path) => path,
            Err(_) => {
                capture.failures.push(failure(FailureCode::PermissionDenied, "configured root is unavailable"));
                continue;
            }
        };
        if !seen.insert(normalize_path(&normalized.to_string_lossy(), if cfg!(windows) { "windows" } else { "posix" })) {
            continue;
        }
        if !normalized.is_dir() {
            capture.failures.push(failure(FailureCode::ParseError, "configured root is not a directory"));
            continue;
        }
        let frames = if *index == start { cursor_frames(&normalized, valid_progress, limits) } else { Vec::new() };
        let pending = capture_root(&mut capture, &normalized, limits, started, frames);
        let exhausted = capture.failures[failures_before..].iter().any(|failure| matches!(failure.code, FailureCode::LimitReached | FailureCode::Timeout));
        next_root = if root_count == 0 { 0 } else { (*index + 1) % root_count };
        if let Some(frames) = pending {
            match serialize_frames(&normalized, &frames) {
                Ok(frames) => {
                    let cursor = json!({"version":1,"fingerprint":fingerprint,"next_root":index,"frames":frames,"complete":false});
                    capture_configs(&mut capture, configs, limits, started);
                    capture.counters.duration_ms = started.elapsed().as_millis() as u64;
                    compact_failures(&mut capture);
                    capture.progress = Some(cursor.clone());
                    return (capture, cursor);
                }
                Err(error) => {
                    capture.failures.push(error);
                    next_root = if root_count == 0 { 0 } else { (*index + 1) % root_count };
                }
            }
        }
        if exhausted {
            break;
        }
    }
    if root_count > count {
        capture.failures.push(failure(FailureCode::LimitReached, "configured root budget reached"));
    }
    capture_configs(&mut capture, configs, limits, started);
    capture.counters.duration_ms = started.elapsed().as_millis() as u64;
    let complete = !capture.failures.iter().any(|failure| matches!(failure.code, FailureCode::LimitReached | FailureCode::Timeout));
    compact_failures(&mut capture);
    let cursor = json!({"version":1,"fingerprint":fingerprint,"next_root":if complete { 0 } else { next_root },"frames":[],"complete":complete});
    capture.progress = Some(cursor.clone());
    (capture, cursor)
}

fn attrs(manifest: &RawManifest, capture: &RawCapture) -> Value {
    json!({"content_sha256":manifest.content_sha256,"revision":manifest.revision,"parser":PARSER_VERSION,"observed_at_ms":capture.observed_at_ms,"declared":true})
}
fn add_entity(entities: &mut Vec<Entity>, kind: EntityKind, label: &str, key: &str, attributes: Value) -> Entity {
    let mut entity = Entity::with_key(kind, label, key);
    entity.attributes = attributes;
    entities.push(entity.clone());
    entity
}
fn cargo_members(manifest: &RawManifest) -> Result<Vec<String>, Failure> {
    let value: toml::Value = toml::from_str(&manifest.content).map_err(|_| failure(FailureCode::ParseError, "invalid cargo workspace"))?;
    Ok(value
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(str::to_owned)
        .collect())
}
fn node_members(manifest: &RawManifest) -> Result<Vec<String>, Failure> {
    let value: Value = serde_json::from_str(&manifest.content).map_err(|_| failure(FailureCode::ParseError, "invalid node workspace"))?;
    match value.get("workspaces") {
        Some(Value::Array(values)) => Ok(values.iter().filter_map(Value::as_str).map(str::to_owned).collect()),
        Some(Value::Object(value)) => {
            Ok(value.get("packages").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect())
        }
        Some(_) => Err(failure(FailureCode::Unsupported, "node workspace declaration is unsupported")),
        None => Ok(Vec::new()),
    }
}
fn member_matches(pattern: &str, path: &str) -> Result<bool, Failure> {
    if pattern.contains("**") {
        return Err(failure(FailureCode::Unsupported, "workspace glob is unsupported"));
    }
    let pattern = pattern.trim_end_matches('/').trim_end_matches("/Cargo.toml").trim_end_matches("/package.json");
    let path = path.trim_end_matches("/Cargo.toml").trim_end_matches("/package.json");
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        Ok(path.starts_with(prefix) && path.ends_with(suffix) && !path[prefix.len()..path.len() - suffix.len()].contains('/'))
    } else {
        Ok(pattern == path)
    }
}

fn checkout_key(capture: &RawCapture, raw: &RawRepository) -> String {
    format!("{}|{}|{}", capture.scope_id, raw.root_fingerprint, raw.relative_path)
}

fn mark_resolved(entity: &mut Entity, target: &str) {
    if let Some(attributes) = entity.attributes.as_object_mut() {
        attributes.insert("resolution".into(), Value::String("resolved".into()));
        attributes.remove("unresolved_reason");
        attributes.insert("resolved_target".into(), Value::String(target.into()));
    }
}

fn relative_path_target(manifest: &str, spec: &str) -> Option<String> {
    let reference = spec.strip_prefix("path:")?;
    if reference.is_empty() || Path::new(reference).is_absolute() {
        return None;
    }
    let mut parts = Vec::new();
    for component in Path::new(manifest).parent()?.join(reference).components() {
        use std::path::Component;
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn reusable_workflow_reference(value: &str) -> Option<(String, String)> {
    let (path, _) = value.split_once('@')?;
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repository = parts.next()?;
    let workflow = parts.collect::<Vec<_>>();
    if owner.is_empty() || repository.is_empty() || workflow.len() < 3 || workflow[0] != ".github" || workflow[1] != "workflows" {
        return None;
    }
    Some((format!("github.com/{}/{}", owner.to_ascii_lowercase(), repository.to_ascii_lowercase()), workflow.join("/")))
}

fn source_key(capture: &RawCapture, raw: &RawRepository, manifest: &RawManifest) -> SourceCacheKey {
    SourceCacheKey {
        source_id: canonical_entity_id(EntityKind::Artifact, &format!("{}|source|{}", checkout_key(capture, raw), manifest.relative_path)),
        content_sha256: manifest.content_sha256.clone(),
        parser_version: PARSER_VERSION.into(),
        kind: manifest.kind.clone(),
    }
}

pub fn capture_source_cache_keys(capture: &RawCapture) -> Vec<SourceCacheKey> {
    capture.repositories.iter().flat_map(|raw| raw.manifests.iter().map(move |manifest| source_key(capture, raw, manifest))).collect()
}

pub fn assemble_capture(capture: &RawCapture) -> Vec<TopologySnapshot> {
    assemble_capture_cached(capture, &HashMap::new()).0
}

pub fn assemble_capture_cached(capture: &RawCapture, cached: &HashMap<SourceCacheKey, SourceFacts>) -> (Vec<TopologySnapshot>, Vec<SourceCacheEntry>) {
    let host_label = capture.scope_id.split(':').next().unwrap_or("host");
    let observed_repositories = capture
        .repositories
        .iter()
        .filter_map(|raw| raw.git.origin.as_ref().map(|origin| (origin.clone(), canonical_entity_id(EntityKind::Repository, origin))))
        .collect::<BTreeMap<_, _>>();
    let observed_workflows = capture
        .repositories
        .iter()
        .filter_map(|raw| raw.git.origin.as_ref().map(|origin| (origin, raw)))
        .flat_map(|(origin, raw)| {
            let checkout = checkout_key(capture, raw);
            raw.manifests.iter().filter(|manifest| manifest.kind == "workflow").filter_map(move |manifest| {
                let relative = Path::new(&manifest.relative_path)
                    .strip_prefix(Path::new(&raw.relative_path))
                    .ok()
                    .and_then(|path| path.to_str())
                    .map(str::to_owned)
                    .or_else(|| (raw.relative_path == ".").then(|| manifest.relative_path.clone()))?;
                Some(((origin.clone(), relative), canonical_entity_id(EntityKind::Workflow, &format!("{}|workflow|{}", checkout, manifest.relative_path))))
            })
        })
        .collect::<BTreeMap<_, _>>();
    let mut snapshots = Vec::new();
    let mut cache_entries = Vec::new();
    for raw in &capture.repositories {
        let mut entities = Vec::new();
        let mut edges = Vec::new();
        let mut failures = raw.failures.clone();
        let host = add_entity(&mut entities, EntityKind::Host, host_label, host_label, json!({"observed":true}));
        let environment = add_entity(
            &mut entities,
            EntityKind::ExecutionEnvironment,
            "environment",
            &capture.scope_id,
            json!({"adapter":capture.adapter,"observed_at_ms":capture.observed_at_ms}),
        );
        edges.push(Edge::new(EdgeKind::ContainsEnvironment, &host.id, &environment.id));
        let repo_key = raw.git.origin.clone().unwrap_or_else(|| format!("{}|{}|{}", capture.scope_id, raw.root_fingerprint, raw.relative_path));
        let repository = add_entity(
            &mut entities,
            EntityKind::Repository,
            raw.git.origin.as_deref().unwrap_or("unresolved repository"),
            &repo_key,
            json!({"origin":raw.git.origin,"observed":true}),
        );
        let checkout = add_entity(
            &mut entities,
            EntityKind::Checkout,
            "checkout",
            &checkout_key(capture, raw),
            json!({"relative_path":raw.relative_path,"revision":raw.git.head,"git_dir":raw.git.git_dir,"common_dir":raw.git.common_dir}),
        );
        edges.push(Edge::new(EdgeKind::ContainsCheckout, &environment.id, &checkout.id));
        edges.push(Edge::new(EdgeKind::CheckoutOf, &checkout.id, &repository.id));
        if raw.git.is_worktree {
            let worktree = add_entity(
                &mut entities,
                EntityKind::Worktree,
                "worktree",
                &format!("{}|worktree", checkout.id),
                json!({"git_dir":raw.git.git_dir,"common_dir":raw.git.common_dir}),
            );
            edges.push(Edge::new(EdgeKind::WorktreeOf, &worktree.id, &checkout.id));
        }
        let mut packages = BTreeMap::new();
        let mut workspaces = Vec::new();
        let mut workspace_declarations = BTreeMap::new();
        let mut pending_dependencies: Vec<(String, ParsedDependency, String)> = Vec::new();
        for manifest in &raw.manifests {
            if !matches!(manifest.kind.as_str(), "cargo" | "node" | "python" | "go") {
                continue;
            }
            let cache_key = source_key(capture, raw, manifest);
            let package = match cached.get(&cache_key).and_then(SourceFacts::as_package).map(Ok).unwrap_or_else(|| match manifest.kind.as_str() {
                "cargo" => parse_cargo(&manifest.content),
                "node" => parse_node(&manifest.content),
                "python" => parse_python(&manifest.content),
                "go" => parse_go(&manifest.content),
                _ => Err(failure(FailureCode::Unsupported, "manifest parser is unsupported")),
            }) {
                Ok(package) => {
                    if !cached.contains_key(&cache_key) {
                        cache_entries.push(SourceCacheEntry {
                            key: cache_key,
                            revision: manifest.revision.clone(),
                            fetched_at_ms: capture.observed_at_ms,
                            facts: SourceFacts::from(package.clone()),
                        });
                    }
                    package
                }
                Err(error) => {
                    failures.push(error);
                    continue;
                }
            };
            let workspace_members = if manifest.kind == "cargo" {
                cargo_members(manifest)
            } else if manifest.kind == "node" {
                node_members(manifest)
            } else {
                Ok(Vec::new())
            };
            if let Err(error) = &workspace_members {
                failures.push(error.clone());
            }
            if manifest.kind == "cargo" && manifest.content.contains("[workspace]")
                || manifest.kind == "node" && !workspace_members.as_ref().unwrap_or(&Vec::new()).is_empty()
            {
                let workspace = add_entity(
                    &mut entities,
                    EntityKind::Workspace,
                    "workspace",
                    &format!("{}|workspace|{}", checkout.id, manifest.relative_path),
                    attrs(manifest, capture),
                );
                edges.push(Edge::new(EdgeKind::ContainsWorkspace, &checkout.id, &workspace.id));
                workspaces.push((manifest.relative_path.clone(), workspace, workspace_members.unwrap_or_default()));
            }
            for declaration in &package.workspace_dependencies {
                workspace_declarations.entry(declaration.name.clone()).or_insert_with(|| declaration.clone());
            }
            if package.name.is_empty() {
                continue;
            }
            let package_entity = add_entity(
                &mut entities,
                EntityKind::Package,
                &package.name,
                &format!("{}|package|{}|{}", checkout.id, manifest.relative_path, package.name),
                attrs(manifest, capture),
            );
            edges.push(Edge::new(EdgeKind::ContainsPackage, &checkout.id, &package_entity.id));
            for dependency in package.dependencies {
                let dependency_key = format!(
                    "{}|declaration|{}|{}|{}|{}|{}|{}|{}",
                    checkout.id,
                    manifest.relative_path,
                    dependency.kind.slug(),
                    dependency.target.as_deref().unwrap_or(""),
                    dependency.alias.as_deref().unwrap_or(""),
                    dependency.name,
                    serde_json::to_string(&dependency.source).unwrap_or_default(),
                    dependency.origin.as_deref().unwrap_or(""),
                );
                let dependency_entity = add_entity(
                    &mut entities,
                    EntityKind::Package,
                    dependency.alias.as_deref().unwrap_or(&dependency.name),
                    &dependency_key,
                    json!({"source":dependency.source,"kind":dependency.kind,"alias":dependency.alias,"name":dependency.name,"spec":dependency.spec,"origin":dependency.origin,"target":dependency.target,"declared":true,"resolution":"unresolved","unresolved_reason":"target was not observed"}),
                );
                edges.push(Edge::new(EdgeKind::DeclaresDependency, &package_entity.id, &dependency_entity.id));
                pending_dependencies.push((dependency_entity.id, dependency, manifest.relative_path.clone()));
            }
            packages.insert(manifest.relative_path.clone(), package_entity);
        }
        let mut workspace_member_ids = BTreeSet::new();
        for (workspace_path, workspace, members) in workspaces {
            let workspace_dir = Path::new(&workspace_path).parent().unwrap_or_else(|| Path::new(""));
            for (path, package) in &packages {
                let relative_member = Path::new(path).strip_prefix(workspace_dir).ok().and_then(|value| value.to_str()).unwrap_or(path);
                for member in &members {
                    match member_matches(member, relative_member) {
                        Ok(true) => {
                            edges.push(Edge::new(EdgeKind::WorkspaceMember, &workspace.id, &package.id));
                            workspace_member_ids.insert(package.id.clone());
                        }
                        Ok(false) => {}
                        Err(error) => failures.push(error),
                    }
                }
            }
        }
        for (dependency_id, dependency, manifest_path) in pending_dependencies {
            let target = match dependency.source {
                DependencySource::Git => dependency.origin.as_ref().and_then(|origin| observed_repositories.get(origin)).cloned(),
                DependencySource::Path => dependency.spec.as_deref().and_then(|spec| relative_path_target(&manifest_path, spec)).and_then(|path| {
                    [path.clone(), format!("{path}/Cargo.toml"), format!("{path}/package.json"), format!("{path}/pyproject.toml"), format!("{path}/go.mod")]
                        .into_iter()
                        .find_map(|path| packages.get(&path).map(|package| package.id.clone()))
                }),
                DependencySource::Workspace if workspace_declarations.contains_key(&dependency.name) => packages
                    .values()
                    .find(|package| package.label == dependency.name && workspace_member_ids.contains(&package.id))
                    .map(|package| package.id.clone()),
                _ => None,
            };
            if let Some(target) = target {
                if let Some(item) = entities.iter_mut().find(|entity| entity.id == dependency_id) {
                    mark_resolved(item, &target);
                }
                edges.push(Edge::new(EdgeKind::ResolvesTo, &dependency_id, &target));
            } else if let Some(item) = entities.iter_mut().find(|entity| entity.id == dependency_id) {
                let reason = if dependency.source == DependencySource::Workspace && !workspace_declarations.contains_key(&dependency.name) {
                    "workspace declaration was not observed"
                } else {
                    "target was not observed"
                };
                item.attributes["unresolved_reason"] = Value::String(reason.into());
            }
        }
        for manifest in raw.manifests.iter().filter(|manifest| manifest.kind == "workflow") {
            let cache_key = source_key(capture, raw, manifest);
            let workflow =
                match cached.get(&cache_key).and_then(SourceFacts::as_workflow).map(Ok).unwrap_or_else(|| parse_workflow(&manifest.content, &capture.limits)) {
                    Ok(workflow) => {
                        if !cached.contains_key(&cache_key) {
                            cache_entries.push(SourceCacheEntry {
                                key: cache_key,
                                revision: manifest.revision.clone(),
                                fetched_at_ms: capture.observed_at_ms,
                                facts: SourceFacts::from(workflow.clone()),
                            });
                        }
                        workflow
                    }
                    Err(error) => {
                        failures.push(error);
                        continue;
                    }
                };
            let mut workflow_attributes = attrs(manifest, capture);
            workflow_attributes["triggers"] = json!(workflow.triggers);
            workflow_attributes["trigger_filters"] = json!(workflow.trigger_filters);
            workflow_attributes["coverage"] = json!(workflow.coverage);
            let workflow_entity = add_entity(
                &mut entities,
                EntityKind::Workflow,
                "workflow",
                &format!("{}|workflow|{}", checkout.id, manifest.relative_path),
                workflow_attributes,
            );
            edges.push(Edge::new(EdgeKind::HasWorkflow, &repository.id, &workflow_entity.id));
            let mut workflow_jobs = BTreeMap::new();
            let mut job_needs = Vec::new();
            for job in workflow.jobs {
                let job_entity = add_entity(
                    &mut entities,
                    EntityKind::Job,
                    &job.id,
                    &format!("{}|job|{}", workflow_entity.id, job.id),
                    json!({"if":job.if_condition,"environment":job.environment,"declared":true}),
                );
                edges.push(Edge::new(EdgeKind::HasJob, &workflow_entity.id, &job_entity.id));
                workflow_jobs.insert(job.id.clone(), job_entity.id.clone());
                job_needs.push((job_entity.id.clone(), job.needs.clone()));
                if let Some(uses) = job.job_uses {
                    let mut item = add_entity(
                        &mut entities,
                        EntityKind::Workflow,
                        &uses,
                        &format!("{}|workflow-ref|{}", checkout.id, uses),
                        json!({"declared":true,"resolution":"unresolved","unresolved_reason":"workflow target was not observed"}),
                    );
                    edges.push(Edge::new(EdgeKind::UsesWorkflow, &job_entity.id, &item.id));
                    if let Some((origin, path)) = reusable_workflow_reference(&uses)
                        && let Some(target) = observed_workflows.get(&(origin, path))
                    {
                        mark_resolved(&mut item, target);
                        edges.push(Edge::new(EdgeKind::ResolvesTo, &item.id, target));
                        if let Some(existing) = entities.iter_mut().find(|entity| entity.id == item.id) {
                            *existing = item;
                        }
                    }
                }
                for uses in job.step_uses {
                    let item = add_entity(&mut entities, EntityKind::Artifact, &uses, &format!("{}|action|{}", checkout.id, uses), json!({"declared":true}));
                    edges.push(Edge::new(EdgeKind::UsesAction, &job_entity.id, &item.id));
                }
                for runner in job.runs_on {
                    let item = add_entity(
                        &mut entities,
                        EntityKind::Artifact,
                        &runner,
                        &format!("{}|runner|{}", checkout.id, runner),
                        json!({"declared":true,"physical_host_binding":"unknown"}),
                    );
                    edges.push(Edge::new(EdgeKind::RunsOn, &job_entity.id, &item.id));
                }
            }
            for (job, needs) in job_needs {
                for need in needs {
                    if let Some(target) = workflow_jobs.get(&need) {
                        edges.push(Edge::new(EdgeKind::NeedsJob, &job, target));
                    }
                }
            }
        }
        for config in &raw.configs {
            let item = add_entity(
                &mut entities,
                EntityKind::Configuration,
                "configuration",
                &format!("{}|config|{}|{}", capture.scope_id, config.relative_path, config.key),
                json!({"key":config.key,"value_hash":config.value_hash,"mode":config.mode,"observed_at_ms":capture.observed_at_ms}),
            );
            edges.push(Edge::new(EdgeKind::ReadsConfig, &environment.id, &item.id));
        }
        entities.sort_by(|left, right| left.id.cmp(&right.id));
        entities.dedup_by(|left, right| left.id == right.id);
        edges.sort_by(|left, right| left.id.cmp(&right.id));
        edges.dedup_by(|left, right| left.id == right.id);
        let status = if failures.is_empty() { ScopeStatus::Complete } else { ScopeStatus::Partial };
        snapshots.push(TopologySnapshot {
            scope_id: format!("{}:checkout:{}", capture.scope_id, checkout.id),
            status,
            entities,
            edges,
            failures,
            counters: capture.counters.clone(),
            observed_at_ms: capture.observed_at_ms,
        });
    }
    if !capture.failures.is_empty() {
        snapshots.push(TopologySnapshot {
            scope_id: format!("{}:coverage", capture.scope_id),
            status: ScopeStatus::Partial,
            entities: Vec::new(),
            edges: Vec::new(),
            failures: capture.failures.clone(),
            counters: capture.counters.clone(),
            observed_at_ms: capture.observed_at_ms,
        });
    }
    if snapshots.is_empty() {
        snapshots.push(TopologySnapshot {
            scope_id: capture.scope_id.clone(),
            status: ScopeStatus::Failed,
            entities: Vec::new(),
            edges: Vec::new(),
            failures: if capture.failures.is_empty() {
                vec![failure(FailureCode::Unsupported, "no repository was observed")]
            } else {
                capture.failures.clone()
            },
            counters: capture.counters.clone(),
            observed_at_ms: capture.observed_at_ms,
        });
    }
    (snapshots, cache_entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_rejects_an_elapsed_shared_deadline_before_spawning() {
        let deadline = Instant::now() - Duration::from_millis(1);
        let error = git(Path::new("."), &["rev-parse", "--is-inside-work-tree"], &TopologyLimits::default(), deadline, None).unwrap_err();
        assert_eq!(error.code, FailureCode::Timeout);
    }

    #[test]
    fn configuration_read_rejects_an_elapsed_shared_deadline_before_opening() {
        let path = std::env::temp_dir().join(format!("tune-topology-config-deadline-{}.json", std::process::id()));
        std::fs::write(&path, "{}").unwrap();
        let error = read_bounded_regular_file(&path, 16, Instant::now() - Duration::from_millis(1)).unwrap_err();
        assert_eq!(error.code, FailureCode::Timeout);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn typed_configuration_hash_rejects_large_exponent_integers_and_normalizes_negative_zero() {
        let mut exact = Vec::new();
        canonical_json(&serde_json::json!(9_007_199_254_740_992u64), &mut exact).unwrap();
        let mut zero = Vec::new();
        canonical_json(&serde_json::from_str("-0.0").unwrap(), &mut zero).unwrap();
        assert_eq!(zero, b"n0000000000000000");

        let mut oversized = Vec::new();
        let error = canonical_json(&serde_json::from_str("1e100").unwrap(), &mut oversized).unwrap_err();
        assert_eq!(error.code, FailureCode::Unsupported);
    }
}
