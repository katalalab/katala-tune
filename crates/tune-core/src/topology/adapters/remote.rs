//! Existing strict SSH surfaces only. Payloads execute in memory through stdin.
use super::super::model::{CaptureCounters, RawCapture};
use super::super::{Failure, FailureCode, TopologyLimits, normalize_git_origin};
use super::{CommandRunner, SystemRunner, failure_from_process};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

pub const POSIX_PROBE: &str = include_str!("../../../../../probes/topology_posix.py");
pub const WINDOWS_PROBE: &str = include_str!("../../../../../probes/topology_win.ps1");

/// Existing Windows bootstrap pattern: consume exactly the bounded script bytes, without waiting for EOF.
pub const WINDOWS_STDIN_LOADER: &str = r#"[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);$i=[Console]::OpenStandardInput();$b=New-Object byte[] __BYTES__;$p=0;while($p -lt __BYTES__){$r=$i.Read($b,$p,(__BYTES__-$p));if($r -eq 0){exit 2};$p+=$r};$s=[Text.Encoding]::UTF8.GetString($b);. ([ScriptBlock]::Create($s))"#;
const MAX_REMOTE_INPUT: usize = 2 * 1024 * 1024;

fn remote_work_timeout_ms(timeout_seconds: u64) -> u64 {
    let total = timeout_seconds.saturating_mul(1_000);
    let reserve = (total / 5).clamp(100, 3_000);
    total.saturating_sub(reserve).max(100)
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RemotePlatform {
    Posix,
    Windows,
}
#[derive(Clone, Copy, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRuntimeKind {
    Native,
    Wsl,
}

/// Private, explicit permission for one checkout and the exact manifest bytes.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteSourceCheckout {
    pub root_index: usize,
    pub relative_path: String,
    pub expected_head: String,
    pub manifests: Vec<RemoteSourceManifest>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteSourceManifest {
    pub relative_path: String,
    pub bytes: usize,
    pub sha256: String,
}

/// Private request: roots, aliases and selected configuration paths are never logged.
pub struct RemoteRequest {
    pub scope_id: String,
    pub alias: String,
    pub expected_identity_hash: Option<String>,
    pub platform: RemotePlatform,
    pub expected_runtime: RemoteRuntimeKind,
    pub expected_distro: Option<String>,
    pub cursor: Option<Value>,
    /// WSL surfaces must already have a direct SSH alias. No distro launcher exists here.
    pub roots: Vec<String>,
    pub configs: Vec<Value>,
    pub source_allowlist: Option<Vec<RemoteSourceCheckout>>,
    pub python_program: String,
    pub windows_reader_program: Option<String>,
    pub git_program: Option<String>,
    pub observed_at_ms: i64,
    pub limits: TopologyLimits,
}

pub fn identity_hash(hostname: &str) -> String {
    crate::db::sha256_hex(hostname.trim().to_ascii_lowercase().trim_end_matches(".local").as_bytes())
}

fn canonical_relative(value: &str, checkout: bool) -> bool {
    (checkout && value == ".") || (safe_relative(value) && !value.split('/').any(|part| part == "."))
}

/// Match the probe platform's lexical absolute-root normalization, without IO.
pub fn source_root_fingerprint(root: &str, platform: RemotePlatform) -> String {
    let path = if platform == RemotePlatform::Windows { root.replace('\\', "/").to_lowercase() } else { root.to_owned() };
    let (prefix, rest) = if platform == RemotePlatform::Windows { (path.get(..2).unwrap_or(""), path.get(2..).unwrap_or("")) } else { ("", path.as_str()) };
    let mut parts = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let normalized = format!("{prefix}/{}", parts.join("/"));
    crate::db::sha256_hex(normalized.as_bytes())
}
fn source_kind(path: &str) -> Option<&'static str> {
    if path.starts_with(".github/workflows/") && path.matches('/').count() == 2 && (path.ends_with(".yml") || path.ends_with(".yaml")) {
        return Some("workflow");
    }
    match path.rsplit('/').next()? {
        "Cargo.toml" => Some("cargo"),
        "package.json" => Some("node"),
        "pyproject.toml" => Some("python"),
        "go.mod" => Some("go"),
        _ => None,
    }
}
fn canonical_root(root: &str, platform: RemotePlatform) -> bool {
    let path = if platform == RemotePlatform::Windows { root.replace('\\', "/") } else { root.to_owned() };
    let rest = if platform == RemotePlatform::Windows { path.get(3..).unwrap_or("") } else { path.get(1..).unwrap_or("") };
    (rest.is_empty() || !rest.split('/').any(|part| matches!(part, "" | "." | ".."))) && (platform == RemotePlatform::Windows || !path.contains('\\'))
}
fn valid_source_allowlist(request: &RemoteRequest) -> bool {
    let Some(policy) = &request.source_allowlist else { return false };
    if policy.is_empty()
        || policy.len() > request.limits.entries_per_environment
        || !request.configs.is_empty()
        || request.roots.iter().any(|root| !canonical_root(root, request.platform))
    {
        return false;
    }
    let mut total_files = 0usize;
    let mut total_bytes = 0usize;
    let mut checkouts = HashSet::new();
    for checkout in policy {
        if checkout.root_index >= request.roots.len()
            || !canonical_relative(&checkout.relative_path, true)
            || !oid(&checkout.expected_head)
            || checkout.expected_head != checkout.expected_head.to_ascii_lowercase()
            || !checkouts.insert((source_root_fingerprint(&request.roots[checkout.root_index], request.platform), checkout.relative_path.as_str()))
        {
            return false;
        }
        let mut files = HashSet::new();
        for file in &checkout.manifests {
            total_files = total_files.saturating_add(1);
            total_bytes = total_bytes.saturating_add(file.bytes);
            if total_files > request.limits.manifests_per_environment
                || total_bytes > request.limits.total_bytes_per_environment
                || !canonical_relative(&file.relative_path, false)
                || source_kind(&file.relative_path).is_none()
                || !files.insert(file.relative_path.as_str())
                || !hash(&file.sha256)
                || file.sha256 != file.sha256.to_ascii_lowercase()
                || file.bytes > request.limits.file_bytes
                || (source_kind(&file.relative_path) == Some("workflow") && file.bytes > request.limits.workflow_bytes)
            {
                return false;
            }
        }
    }
    true
}

/// A subset may be returned for continuation, but cannot become a Complete scan.
fn validate_sources(request: &RemoteRequest, capture: &mut RawCapture) -> Result<bool, ()> {
    let policy = request.source_allowlist.as_ref().ok_or(())?;
    let approved_checkouts = policy
        .iter()
        .enumerate()
        .map(|(index, approved)| {
            ((source_root_fingerprint(&request.roots[approved.root_index], request.platform), approved.relative_path.as_str()), (index, approved))
        })
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut complete = true;
    for repo in &mut capture.repositories {
        let Some((index, approved)) = approved_checkouts.get(&(repo.root_fingerprint.clone(), repo.relative_path.as_str())) else {
            return Err(());
        };
        if !seen.insert(*index) || repo.git.head.as_deref() != Some(approved.expected_head.as_str()) || repo.config_file_read || !repo.configs.is_empty() {
            return Err(());
        }
        let allowed_files = approved.manifests.iter().map(|allowed| (allowed.relative_path.as_str(), allowed)).collect::<HashMap<_, _>>();
        let mut files = HashSet::new();
        for file in &repo.manifests {
            let Some(allowed) = allowed_files.get(file.relative_path.as_str()) else { return Err(()) };
            if !files.insert(file.relative_path.as_str())
                || file.revision.as_deref() != Some(approved.expected_head.as_str())
                || file.content.len() != allowed.bytes
                || file.content_sha256 != allowed.sha256
                || crate::db::sha256_hex(file.content.as_bytes()) != allowed.sha256
                || source_kind(&file.relative_path) != Some(file.kind.as_str())
            {
                return Err(());
            }
        }
        if files.len() != approved.manifests.len() {
            complete = false;
            if repo.failures.len() < 64 {
                repo.failures.push(Failure::new(FailureCode::PermissionDenied, "source allowlist incomplete"));
            }
        }
    }
    Ok(complete && seen.len() == policy.len())
}

pub(super) fn validate_collection(request: &RemoteRequest) -> Result<(), Failure> {
    let distro_ok = request.expected_distro.as_deref().is_none_or(|value| {
        !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    });
    let runtime_ok = request.expected_runtime != RemoteRuntimeKind::Wsl || (request.platform == RemotePlatform::Posix && request.expected_distro.is_some());
    let roots_ok = request.roots.iter().all(|root| {
        !root.chars().any(char::is_control)
            && if request.platform == RemotePlatform::Posix {
                root.starts_with('/')
            } else {
                root.len() > 2 && root.as_bytes()[0].is_ascii_alphabetic() && root.as_bytes()[1] == b':' && matches!(root.as_bytes()[2], b'/' | b'\\')
            }
    });
    if !distro_ok || !runtime_ok || !roots_ok || request.limits.stdout_bytes < 2048 || request.roots.is_empty() {
        return Err(Failure::new(FailureCode::Unsupported, "invalid existing SSH surface or roots"));
    }
    if !valid_source_allowlist(request) {
        return Err(Failure::new(FailureCode::PermissionDenied, "explicit source allowlist required"));
    }
    if request.roots.len() > request.limits.roots_per_environment {
        return Err(Failure::new(FailureCode::LimitReached, "root budget exceeded"));
    }
    if request.cursor.as_ref().is_some_and(|cursor| !valid_cursor(cursor, request.roots.len(), request.limits.depth)) {
        return Err(Failure::new(FailureCode::PermissionDenied, "invalid continuation cursor"));
    }
    Ok(())
}

pub async fn collect_remote(request: &RemoteRequest) -> RawCapture {
    collect_remote_with(&SystemRunner, request).await
}

pub async fn collect_remote_with(runner: &dyn CommandRunner, request: &RemoteRequest) -> RawCapture {
    let mut failed = empty_capture(request);
    let alias_ok =
        !request.alias.is_empty() && !request.alias.starts_with('-') && request.alias.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    let python_ok = request.python_program.starts_with('/') && request.python_program.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b));
    if !alias_ok || (request.platform == RemotePlatform::Posix && !python_ok) {
        failed.failures.push(Failure::new(FailureCode::Unsupported, "invalid existing SSH surface or roots"));
        return failed;
    }
    if let Err(failure) = validate_collection(request) {
        failed.failures.push(failure);
        return failed;
    }
    let config = json!({"source_allowlist":request.source_allowlist,"scope_id":request.scope_id,"observed_at_ms":request.observed_at_ms,"roots":request.roots,"configs":request.configs,"git_program":request.git_program,"limits":request.limits,"work_timeout_ms":remote_work_timeout_ms(request.limits.remote_timeout_seconds),"cursor":request.cursor,"expected_runtime":request.expected_runtime,"expected_distro":request.expected_distro,"expected_identity_hash":request.expected_identity_hash});
    let serialized = serde_json::to_vec(&config).unwrap_or_default();
    if serialized.len() > request.limits.file_bytes || serialized.len() > MAX_REMOTE_INPUT {
        failed.failures.push(Failure::new(FailureCode::LimitReached, "remote request byte budget exceeded"));
        return failed;
    }
    let mut reader_binding = None;
    let (command, stdin) = if request.platform == RemotePlatform::Posix {
        let encoded_config = crate::logs::base64(&serialized);
        (
            format!("exec {} -B -", request.python_program),
            format!("import base64,json\nCONFIG=json.loads(base64.b64decode(\"{encoded_config}\"))\n{POSIX_PROBE}").into_bytes(),
        )
    } else {
        use super::windows_reader::{ReaderRequest, encode_request, fresh_nonce, valid_program_path};
        let Some(program) = request.windows_reader_program.as_deref().filter(|p| valid_program_path(p)) else {
            failed.failures.push(Failure::new(FailureCode::Unsupported, "explicit Windows reader executable required"));
            return failed;
        };
        let prepared = fresh_nonce()
            .and_then(|nonce| ReaderRequest::from_remote(request, nonce, remote_work_timeout_ms(request.limits.remote_timeout_seconds)))
            .and_then(|reader| encode_request(&reader).map(|frame| (reader, frame)));
        let Ok((reader, frame)) = prepared else {
            failed.failures.push(Failure::new(FailureCode::PermissionDenied, "remote reader request invalid"));
            return failed;
        };
        let command = format!("exec '{program}' topology-reader --budget-ms {}", reader.budget_ms);
        // The frame prefix is fixed at sixteen bytes; preserve the exact body
        // sent to the remote helper, never a reserialized approximation.
        reader_binding = Some((reader, frame[16..].to_vec()));
        (command, frame)
    };
    let stdin_limit = MAX_REMOTE_INPUT + if reader_binding.is_some() { 16 } else { 0 };
    if stdin.len() > stdin_limit {
        failed.failures.push(Failure::new(FailureCode::LimitReached, "remote stdin byte budget exceeded"));
        return failed;
    }
    let mut args: Vec<String> = crate::collect::SSH_OPTS.iter().map(|arg| (*arg).into()).collect();
    args.extend([
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        "ClearAllForwardings=yes".into(),
        "-o".into(),
        "PermitLocalCommand=no".into(),
        "-o".into(),
        "RemoteCommand=none".into(),
        "-o".into(),
        "ProxyCommand=none".into(),
        "-o".into(),
        "ProxyJump=none".into(),
        request.alias.clone(),
        command,
    ]);
    let stdout_limit = if reader_binding.is_some() {
        request.limits.stdout_bytes.saturating_add(super::windows_reader::MAX_RESPONSE_HEADER_BYTES + 16)
    } else {
        request.limits.stdout_bytes
    };
    let output = runner.run("ssh", &args, &stdin, Duration::from_secs(request.limits.remote_timeout_seconds), stdout_limit, request.limits.stderr_bytes).await;
    if let Some(failure) = failure_from_process(&output) {
        failed.failures.push(failure);
        return failed;
    }
    let bytes = if let Some((reader, body)) = &reader_binding {
        match super::windows_reader::decode_response(&output.stdout, reader, body) {
            Ok(bytes) => bytes,
            Err(_) => {
                failed.failures.push(Failure::new(FailureCode::PermissionDenied, "remote reader acknowledgement invalid"));
                return failed;
            }
        }
    } else {
        output.stdout.as_slice()
    };
    match validate_capture_bytes(request, bytes) {
        Ok(capture) => capture,
        Err(failure) => {
            failed.failures.push(failure);
            failed
        }
    }
}

pub(super) fn validate_capture_bytes(request: &RemoteRequest, bytes: &[u8]) -> Result<RawCapture, Failure> {
    if bytes.contains(&0) {
        return Err(Failure::new(FailureCode::ParseError, "transport contains invalid NUL"));
    }
    let raw = super::windows_reader::strict_json::<Value>(bytes);
    let Ok(raw) = raw else {
        return Err(Failure::new(FailureCode::ParseError, "remote report invalid"));
    };
    let runtime = &raw["runtime"];
    let expected_kind = if request.expected_runtime == RemoteRuntimeKind::Wsl { "wsl" } else { "native" };
    let expected_platform = if request.platform == RemotePlatform::Windows { "windows" } else { "posix" };
    if runtime["kind"].as_str() != Some(expected_kind)
        || runtime["platform"].as_str() != Some(expected_platform)
        || runtime["kernel_wsl"].as_bool() != Some(request.expected_runtime == RemoteRuntimeKind::Wsl)
        || (request.expected_runtime == RemoteRuntimeKind::Wsl && runtime["distro"].as_str() != request.expected_distro.as_deref())
    {
        return Err(Failure::new(FailureCode::PermissionDenied, "remote runtime identity mismatch"));
    }
    let capture = serde_json::from_value::<RawCapture>(raw);
    let Ok(mut capture) = capture else {
        return Err(Failure::new(FailureCode::ParseError, "remote report invalid"));
    };
    if capture.scope_id != request.scope_id
        || capture.adapter != "remote"
        || capture.observed_at_ms != request.observed_at_ms
        || capture.limits != request.limits
        || request.expected_identity_hash.as_ref().is_some_and(|expected| capture.target_identity.as_ref() != Some(expected))
    {
        return Err(Failure::new(FailureCode::PermissionDenied, "remote identity or request mismatch"));
    }
    if !valid_capture(&capture) || capture.progress.as_ref().is_some_and(|cursor| !valid_cursor(cursor, request.roots.len(), request.limits.depth)) {
        return Err(Failure::new(FailureCode::ParseError, "remote report violates bounded schema"));
    }
    match validate_sources(request, &mut capture) {
        Err(()) => {
            return Err(Failure::new(FailureCode::PermissionDenied, "remote source allowlist mismatch"));
        }
        Ok(false) if capture.failures.len() < 64 => capture.failures.push(Failure::new(FailureCode::PermissionDenied, "source allowlist incomplete")),
        Ok(false) => {}
        Ok(true) => {}
    }
    for failure in &mut capture.failures {
        failure.message = safe_failure_message(&failure.message, false);
    }
    for repo in &mut capture.repositories {
        repo.git.origin = repo.git.origin.as_deref().and_then(normalize_git_origin);
        for failure in &mut repo.failures {
            failure.message = safe_failure_message(&failure.message, true);
        }
    }
    Ok(capture)
}

fn valid_capture(capture: &RawCapture) -> bool {
    let limit = &capture.limits;
    if capture.counters.entries > limit.entries_per_environment
        || capture.counters.repositories > limit.repositories_per_environment
        || capture.counters.manifests > limit.manifests_per_environment
        || capture.counters.config_files > limit.manifests_per_environment
        || capture.counters.config_keys > limit.manifests_per_environment
        || capture.counters.bytes > limit.total_bytes_per_environment
        || capture.counters.children > limit.repositories_per_environment.saturating_mul(5)
    {
        return false;
    }

    if !capture.target_identity.as_deref().is_some_and(hash) {
        return false;
    }
    if capture.repositories.len() > limit.repositories_per_environment || capture.failures.len() > 64 {
        return false;
    }
    let mut checkout_count = 0usize;
    let mut files = 0usize;
    let mut config_files = 0usize;
    let mut config_keys = 0usize;
    let mut bytes = 0usize;
    for repo in &capture.repositories {
        if repo.failures.len() > 64 {
            return false;
        }
        if let Some(path_id) = repo.relative_path.strip_prefix("configuration-") {
            if !repo.config_file_read && (repo.failures.is_empty() || !repo.configs.is_empty()) {
                return false;
            }
            config_files = config_files.saturating_add(usize::from(repo.config_file_read));
            config_keys = config_keys.saturating_add(repo.configs.len());
            if !hash(path_id)
                || !repo.manifests.is_empty()
                || repo.git != Default::default()
                || repo.configs.iter().any(|config| config.relative_path != path_id)
            {
                return false;
            }
        } else {
            if repo.config_file_read {
                return false;
            }
            checkout_count = checkout_count.saturating_add(1);
        }
        if !safe_relative(&repo.relative_path)
            || !hash(&repo.root_fingerprint)
            || repo.git.git_dir.as_ref().is_some_and(|id| !hash(id))
            || repo.git.common_dir.as_ref().is_some_and(|id| !hash(id))
            || repo.git.head.as_ref().is_some_and(|id| !oid(id))
        {
            return false;
        }
        for manifest in &repo.manifests {
            files = files.saturating_add(1);
            bytes = bytes.saturating_add(manifest.content.len());
            if !safe_relative(&manifest.relative_path)
                || !matches!(manifest.kind.as_str(), "cargo" | "node" | "python" | "go" | "workflow")
                || manifest.content.len() > limit.file_bytes
                || (manifest.kind == "workflow" && manifest.content.len() > limit.workflow_bytes)
                || crate::db::sha256_hex(manifest.content.as_bytes()) != manifest.content_sha256
            {
                return false;
            }
        }
        for config in &repo.configs {
            if !hash(&config.value_hash) || !hash(&config.relative_path) || config.mode != "hash_json_typed_v1" {
                return false;
            }
        }
    }
    capture.counters.repositories == capture.repositories.len()
        && checkout_count <= limit.repositories_per_environment
        && files == capture.counters.manifests
        && files <= limit.manifests_per_environment
        && config_files == capture.counters.config_files
        && config_keys == capture.counters.config_keys
        && bytes <= limit.total_bytes_per_environment
}
pub(crate) fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(crate) fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(crate) fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && !value.contains(':')
        && !value.split('/').any(|component| component == ".." || component.is_empty())
        && !value.chars().any(char::is_control)
}
fn empty_capture(request: &RemoteRequest) -> RawCapture {
    RawCapture {
        scope_id: request.scope_id.clone(),
        adapter: "remote".into(),
        target_identity: None,
        observed_at_ms: request.observed_at_ms,
        limits: request.limits.clone(),
        repositories: vec![],
        failures: vec![],
        counters: CaptureCounters::default(),
        progress: None,
    }
}

fn safe_failure_message(message: &str, repository: bool) -> String {
    if message.starts_with("stalled_directory:") {
        "stalled_directory: narrow root or raise limit".into()
    } else if message == "source allowlist incomplete" {
        message.into()
    } else if message == "continued-fragment" {
        "continued-fragment".into()
    } else if message == "continuation cursor invalidated" {
        message.into()
    } else if repository {
        "repository collection incomplete".into()
    } else {
        "remote collection incomplete".into()
    }
}

/// Cursor contents are private root-relative directory boundaries, never arbitrary paths.
fn valid_cursor(value: &Value, roots: usize, depth: usize) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.len() != 4
        || value["version"].as_u64() != Some(1)
        || !value["fingerprint"].as_str().is_some_and(|text| text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        || serde_json::to_vec(value).map_or(true, |data| data.len() > 8192)
    {
        return false;
    }
    let Some(next_root) = value["next_root"].as_u64() else {
        return false;
    };
    let Some(frames) = value["frames"].as_array() else {
        return false;
    };
    if next_root > roots as u64 || frames.len() > 64.min(depth.saturating_add(1)) || (next_root == roots as u64 && !frames.is_empty()) {
        return false;
    }
    for (index, frame) in frames.iter().enumerate() {
        let Some(dir) = frame["dir"].as_str() else {
            return false;
        };
        let Some(after) = frame["after"].as_str() else {
            return false;
        };
        if frame.as_object().is_none_or(|object| object.len() != 2)
            || !safe_relative(dir)
            || (dir != "." && dir.split('/').any(|part| part == "."))
            || (dir != "." && dir.split('/').count() > depth)
            || (!after.is_empty() && (!safe_relative(after) || matches!(after, "." | "..") || after.contains('/')))
            || (index == 0 && dir != ".")
        {
            return false;
        }
        if index > 0 {
            let parent = &frames[index - 1];
            let expected = if parent["dir"] == "." {
                parent["after"].as_str().unwrap_or("").to_owned()
            } else {
                format!("{}/{}", parent["dir"].as_str().unwrap_or(""), parent["after"].as_str().unwrap_or(""))
            };
            if dir != expected {
                return false;
            }
        }
    }
    true
}

/// Versioned selected-value encoding, independent of provider JSON formatting.
/// Large integral values are rejected before conversion to binary64.
pub fn hash_json_typed_v1(value: &Value) -> Result<String, Failure> {
    fn encode(value: &Value, out: &mut Vec<u8>, depth: usize) -> Result<(), Failure> {
        if depth > 64 {
            return Err(Failure::new(FailureCode::ParseError, "configuration nesting exceeds hash limit"));
        }
        match value {
            Value::Null => out.extend_from_slice(b"z"),
            Value::Bool(value) => out.extend_from_slice(if *value { b"b1" } else { b"b0" }),
            Value::String(value) => {
                out.extend_from_slice(format!("s{}:", value.len()).as_bytes());
                out.extend_from_slice(value.as_bytes());
            }
            Value::Number(value) => {
                if value.as_i64().is_some_and(|value| value.unsigned_abs() > (1u64 << 53)) || value.as_u64().is_some_and(|value| value > (1u64 << 53)) {
                    return Err(Failure::new(FailureCode::ParseError, "configuration numeric range unsupported"));
                }
                let Some(number) = value.as_f64() else {
                    return Err(Failure::new(FailureCode::ParseError, "configuration numeric range unsupported"));
                };
                if !number.is_finite() || (number.fract() == 0.0 && number.abs() > (1u64 << 53) as f64) {
                    return Err(Failure::new(FailureCode::ParseError, "configuration numeric range unsupported"));
                }
                let bits = if number == 0.0 { 0 } else { number.to_bits() };
                out.extend_from_slice(format!("n{bits:016x}").as_bytes());
            }
            Value::Array(values) => {
                out.extend_from_slice(format!("a{}:[", values.len()).as_bytes());
                for child in values {
                    encode(child, out, depth + 1)?;
                }
                out.push(b']');
            }
            Value::Object(values) => {
                out.extend_from_slice(format!("o{}:{{", values.len()).as_bytes());
                let mut keys = values.keys().collect::<Vec<_>>();
                keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
                for key in keys {
                    encode(&Value::String(key.clone()), out, depth + 1)?;
                    encode(&values[key], out, depth + 1)?;
                }
                out.push(b'}');
            }
        }
        Ok(())
    }
    let mut bytes = Vec::new();
    encode(value, &mut bytes, 0)?;
    Ok(crate::db::sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::remote_work_timeout_ms;

    #[test]
    fn work_budget_reserves_transport_return_time() {
        assert_eq!(remote_work_timeout_ms(90), 87_000);
        assert_eq!(remote_work_timeout_ms(1), 800);
        assert_eq!(remote_work_timeout_ms(0), 100);
    }
}
