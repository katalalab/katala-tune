//! Existing strict SSH surfaces only. Payloads execute in memory through stdin.
use super::super::model::{CaptureCounters, RawCapture};
use super::super::{Failure, FailureCode, TopologyLimits, normalize_git_origin};
use super::{CommandRunner, SystemRunner, failure_from_process};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

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
#[derive(Clone, Copy, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRuntimeKind {
    Native,
    Wsl,
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
    pub python_program: String,
    pub git_program: Option<String>,
    pub observed_at_ms: i64,
    pub limits: TopologyLimits,
}

pub fn identity_hash(hostname: &str) -> String {
    format!("{:x}", Sha256::digest(hostname.trim().to_ascii_lowercase().trim_end_matches(".local").as_bytes()))
}

pub async fn collect_remote(request: &RemoteRequest) -> RawCapture {
    collect_remote_with(&SystemRunner, request).await
}

pub async fn collect_remote_with(runner: &dyn CommandRunner, request: &RemoteRequest) -> RawCapture {
    let mut failed = empty_capture(request);
    let alias_ok =
        !request.alias.is_empty() && !request.alias.starts_with('-') && request.alias.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    let python_ok = request.python_program.starts_with('/') && request.python_program.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b));
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
    if !distro_ok
        || !runtime_ok
        || !roots_ok
        || request.limits.stdout_bytes < 2048
        || !alias_ok
        || (request.platform == RemotePlatform::Posix && !python_ok)
        || request.roots.is_empty()
    {
        failed.failures.push(Failure::new(FailureCode::Unsupported, "invalid existing SSH surface or roots"));
        return failed;
    }
    if request.roots.len() > request.limits.roots_per_environment {
        failed.failures.push(Failure::new(FailureCode::LimitReached, "root budget exceeded"));
        return failed;
    }
    if request.cursor.as_ref().is_some_and(|cursor| !valid_cursor(cursor, request.roots.len(), request.limits.depth)) {
        failed.failures.push(Failure::new(FailureCode::PermissionDenied, "invalid continuation cursor"));
        return failed;
    }
    let config = json!({"scope_id":request.scope_id,"observed_at_ms":request.observed_at_ms,"roots":request.roots,"configs":request.configs,"git_program":request.git_program,"limits":request.limits,"work_timeout_ms":remote_work_timeout_ms(request.limits.remote_timeout_seconds),"cursor":request.cursor,"expected_runtime":request.expected_runtime,"expected_distro":request.expected_distro,"expected_identity_hash":request.expected_identity_hash});
    let serialized = serde_json::to_vec(&config).unwrap_or_default();
    if serialized.len() > request.limits.file_bytes {
        failed.failures.push(Failure::new(FailureCode::LimitReached, "remote request byte budget exceeded"));
        return failed;
    }
    let encoded_config = crate::logs::base64(&serialized);
    let (command, stdin) = if request.platform == RemotePlatform::Posix {
        (
            format!("exec {} -B -", request.python_program),
            format!("import base64,json\nCONFIG=json.loads(base64.b64decode(\"{encoded_config}\"))\n{POSIX_PROBE}").into_bytes(),
        )
    } else {
        let script = format!(
            "$TopologyRequest = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(\"{encoded_config}\")) | ConvertFrom-Json\n{}",
            WINDOWS_PROBE.trim_start_matches('\u{feff}')
        );
        let stdin = script.into_bytes();
        let loader = WINDOWS_STDIN_LOADER.replace("__BYTES__", &stdin.len().to_string());
        let utf16: Vec<u8> = loader.encode_utf16().flat_map(u16::to_le_bytes).collect();
        (format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}", crate::logs::base64(&utf16)), stdin)
    };
    if stdin.len() > MAX_REMOTE_INPUT {
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
    let output = runner
        .run("ssh", &args, &stdin, Duration::from_secs(request.limits.remote_timeout_seconds), request.limits.stdout_bytes, request.limits.stderr_bytes)
        .await;
    if let Some(failure) = failure_from_process(&output) {
        failed.failures.push(failure);
        return failed;
    }
    if output.stdout.contains(&0) {
        failed.failures.push(Failure::new(FailureCode::ParseError, "transport contains invalid NUL"));
        return failed;
    }
    let raw = serde_json::from_slice::<Value>(&output.stdout);
    let Ok(raw) = raw else {
        failed.failures.push(Failure::new(FailureCode::ParseError, "remote report invalid"));
        return failed;
    };
    let runtime = &raw["runtime"];
    let expected_kind = if request.expected_runtime == RemoteRuntimeKind::Wsl { "wsl" } else { "native" };
    let expected_platform = if request.platform == RemotePlatform::Windows { "windows" } else { "posix" };
    if runtime["kind"].as_str() != Some(expected_kind)
        || runtime["platform"].as_str() != Some(expected_platform)
        || runtime["kernel_wsl"].as_bool() != Some(request.expected_runtime == RemoteRuntimeKind::Wsl)
        || (request.expected_runtime == RemoteRuntimeKind::Wsl && runtime["distro"].as_str() != request.expected_distro.as_deref())
    {
        failed.failures.push(Failure::new(FailureCode::PermissionDenied, "remote runtime identity mismatch"));
        return failed;
    }
    let capture = serde_json::from_value::<RawCapture>(raw);
    let Ok(mut capture) = capture else {
        failed.failures.push(Failure::new(FailureCode::ParseError, "remote report invalid"));
        return failed;
    };
    if capture.scope_id != request.scope_id
        || capture.adapter != "remote"
        || capture.observed_at_ms != request.observed_at_ms
        || capture.limits != request.limits
        || request.expected_identity_hash.as_ref().is_some_and(|expected| capture.target_identity.as_ref() != Some(expected))
    {
        failed.failures.push(Failure::new(FailureCode::PermissionDenied, "remote identity or request mismatch"));
        return failed;
    }
    if !valid_capture(&capture) || capture.progress.as_ref().is_some_and(|cursor| !valid_cursor(cursor, request.roots.len(), request.limits.depth)) {
        failed.failures.push(Failure::new(FailureCode::ParseError, "remote report violates bounded schema"));
        return failed;
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
    capture
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
                || format!("{:x}", Sha256::digest(manifest.content.as_bytes())) != manifest.content_sha256
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
    Ok(format!("{:x}", Sha256::digest(&bytes)))
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
