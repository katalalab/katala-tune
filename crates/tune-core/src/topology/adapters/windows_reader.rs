//! One finite Windows reader over an existing SSH surface. No ledger, database,
//! arbitrary command, persistent process, or EOF-dependent request framing.
use std::io::{Read, Write};

use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;

use super::remote::{self, RemotePlatform, RemoteRequest, RemoteRuntimeKind, RemoteSourceCheckout};
use crate::collect::{BoundedResult, ProcessIdentity, ProcessReceipt, ProcessStop};
use crate::topology::{TopologyLimits, config::TopologyConfig};

const MAGIC: &[u8; 8] = b"KTRDR001";
const PREFIX: usize = 16;
pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
/// 4096 retained identities fit, including full decimal FILETIME and session IDs.
pub const MAX_RESPONSE_HEADER_BYTES: usize = 1024 * 1024;
pub const MAX_BUDGET_MS: u64 = 3_600_000;
pub const REQUEST_SCHEMA: &str = "katala_windows_reader_request.v1";
const RESPONSE_SCHEMA: &str = "katala_windows_reader_response.v1";

/// Intentionally contains no raw IO errors or private input in Debug/Display.
#[derive(Debug)]
pub struct ReaderError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderFailureStage {
    RequestValidation,
    Identity,
    ProbePrepare,
    ProbeResult,
    CaptureValidation,
    Deadline,
    ResponseEncode,
}

impl ReaderFailureStage {
    pub const fn name(self) -> &'static str {
        match self {
            Self::RequestValidation => "request_validation",
            Self::Identity => "identity",
            Self::ProbePrepare => "probe_prepare",
            Self::ProbeResult => "probe_result",
            Self::CaptureValidation => "capture_validation",
            Self::Deadline => "deadline",
            Self::ResponseEncode => "response_encode",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderBackendExit {
    Missing,
    Zero,
    Nonzero,
}

impl ReaderBackendExit {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Zero => "zero",
            Self::Nonzero => "nonzero",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReaderBackendFailure {
    pub exit: ReaderBackendExit,
    pub spawn_failed: bool,
    pub timed_out: bool,
    pub output_limited: bool,
    pub stop: Option<ProcessStop>,
    pub root_exit_zero: bool,
    pub cleanup_confirmed: bool,
    pub receipt_accepted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReaderFailure {
    pub stage: ReaderFailureStage,
    pub backend: Option<ReaderBackendFailure>,
}

#[cfg(windows)]
impl ReaderFailure {
    const fn at(stage: ReaderFailureStage) -> Self {
        Self { stage, backend: None }
    }

    fn probe_result(output: &BoundedResult, nonce: &str) -> Self {
        let receipt = output.process_receipt.as_ref();
        Self {
            stage: ReaderFailureStage::ProbeResult,
            backend: Some(ReaderBackendFailure {
                exit: match output.code {
                    None => ReaderBackendExit::Missing,
                    Some(0) => ReaderBackendExit::Zero,
                    Some(_) => ReaderBackendExit::Nonzero,
                },
                spawn_failed: output.spawn_failed,
                timed_out: output.timed_out,
                output_limited: output.output_limited,
                stop: receipt.map(|value| value.stop),
                root_exit_zero: receipt.is_some_and(|value| value.root_exit_code == Some(0)),
                cleanup_confirmed: receipt.is_some_and(ProcessReceipt::cleanup_confirmed),
                receipt_accepted: receipt.is_some_and(|value| receipt_ok(value, nonce)),
            }),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderRequest {
    pub schema: String,
    pub nonce: String,
    pub scope_id: String,
    pub observed_at_ms: i64,
    pub expected_identity_hash: String,
    pub expected_runtime: RemoteRuntimeKind,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub expected_distro: Option<String>,
    pub roots: Vec<String>,
    pub configs: Vec<Value>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub cursor: Option<Value>,
    pub source_allowlist: Vec<RemoteSourceCheckout>,
    pub git_program: String,
    pub limits: TopologyLimits,
    pub budget_ms: u64,
    /// Legacy wire key: v1 hashes embedded PS; native v2 hashes the source backend.
    /// Neither digest establishes executable artifact identity or permission.
    pub probe_sha256: String,
    /// Explicit operator-approved Git executable digest for native v2. Never
    /// discovered from PATH or accepted from an executable's self-report.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "git_artifact")]
    pub git_artifact_sha256: Option<String>,
}

fn git_artifact<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    // Missing is the unchanged legacy wire shape. Explicit null must not turn
    // an added native-only field into an accepted legacy request extension.
    String::deserialize(deserializer).map(Some)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseHeader {
    schema: String,
    nonce: String,
    scope_id: String,
    observed_at_ms: i64,
    expected_identity_hash: String,
    request_sha256: String,
    probe_sha256: String,
    budget_ms: u64,
    helper_identity: ProcessIdentity,
    process_receipt: ProcessReceipt,
    capture_sha256: String,
}

pub fn probe_sha256() -> String {
    crate::db::sha256_hex(remote::WINDOWS_PROBE.as_bytes())
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Narrow, quoted Git Bash executable argument; no UNC, expansion, alternate
/// data stream, dot segment, trailing-dot/space component or option ambiguity.
pub fn valid_program_path(value: &str) -> bool {
    value.len() >= 7
        && value.len() <= 4096
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value.as_bytes()[1..3] == *b":/"
        && value.to_ascii_lowercase().ends_with(".exe")
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b" :/._-".contains(&b))
        && value[3..].split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.ends_with([' ', '.']) && !part.contains(':'))
}

impl ReaderRequest {
    pub fn from_remote_native(request: &RemoteRequest, nonce: String, budget_ms: u64, git_artifact_sha256: String) -> Result<Self, ReaderError> {
        let mut value = Self::from_remote(request, nonce, budget_ms)?;
        value.schema = super::native_protocol::REQUEST_SCHEMA.into();
        value.probe_sha256 = super::native_protocol::source_backend_sha256();
        value.git_artifact_sha256 = Some(git_artifact_sha256);
        value.validate()?;
        Ok(value)
    }
    pub fn from_remote(request: &RemoteRequest, nonce: String, budget_ms: u64) -> Result<Self, ReaderError> {
        let value = Self {
            schema: REQUEST_SCHEMA.into(),
            nonce,
            scope_id: request.scope_id.clone(),
            observed_at_ms: request.observed_at_ms,
            expected_identity_hash: request.expected_identity_hash.clone().ok_or(ReaderError)?,
            expected_runtime: request.expected_runtime,
            expected_distro: request.expected_distro.clone(),
            roots: request.roots.clone(),
            configs: request.configs.clone(),
            cursor: request.cursor.clone(),
            source_allowlist: request.source_allowlist.clone().ok_or(ReaderError)?,
            git_program: request.git_program.clone().ok_or(ReaderError)?,
            limits: request.limits.clone(),
            budget_ms,
            probe_sha256: probe_sha256(),
            git_artifact_sha256: None,
        };
        value.validate()?;
        Ok(value)
    }
    pub(crate) fn remote_request(&self) -> RemoteRequest {
        RemoteRequest {
            scope_id: self.scope_id.clone(),
            alias: String::new(),
            expected_identity_hash: Some(self.expected_identity_hash.clone()),
            platform: RemotePlatform::Windows,
            expected_runtime: self.expected_runtime,
            expected_distro: self.expected_distro.clone(),
            cursor: self.cursor.clone(),
            roots: self.roots.clone(),
            configs: self.configs.clone(),
            source_allowlist: Some(self.source_allowlist.clone()),
            python_program: String::new(),
            git_program: Some(self.git_program.clone()),
            windows_reader_program: None,
            observed_at_ms: self.observed_at_ms,
            limits: self.limits.clone(),
        }
    }
    pub fn validate(&self) -> Result<(), ReaderError> {
        let native = self.schema == super::native_protocol::REQUEST_SCHEMA;
        if (!native && self.schema != REQUEST_SCHEMA)
            || !hex(&self.nonce, 32)
            || self.scope_id.is_empty()
            || self.scope_id.len() > 512
            || self.scope_id.chars().any(char::is_control)
            || self.observed_at_ms <= 0
            || !hex(&self.expected_identity_hash, 64)
            || self.expected_runtime != RemoteRuntimeKind::Native
            || self.expected_distro.is_some()
            || (!native && self.probe_sha256 != probe_sha256())
            || (!native && self.git_artifact_sha256.is_some())
            || self.budget_ms == 0
            || self.budget_ms > MAX_BUDGET_MS
            || self.budget_ms > self.limits.remote_timeout_seconds.saturating_mul(1000)
            || !valid_program_path(&self.git_program)
        {
            return Err(ReaderError);
        }
        // Reuse the product limit ceilings. Deserializing usize alone is not admission.
        TopologyConfig::from_value(Some(&serde_json::json!({"limits":self.limits}))).map_err(|_| ReaderError)?;
        if native {
            super::native_protocol::validate_native(self)?;
        }
        remote::validate_collection(&self.remote_request()).map_err(|_| ReaderError)
    }
}

// Check duplicate keys at every nesting level, including Value-shaped cursor
// fields, before the typed parser can discard or normalize them.
struct NoDuplicates;
impl<'de> Deserialize<'de> for NoDuplicates {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Check;
        impl<'de> Visitor<'de> for Check {
            type Value = NoDuplicates;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON without duplicate fields")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut keys = std::collections::HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key) {
                        return Err(de::Error::custom("duplicate field"));
                    }
                    map.next_value::<NoDuplicates>()?;
                }
                Ok(NoDuplicates)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<NoDuplicates>()?.is_some() {}
                Ok(NoDuplicates)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(NoDuplicates)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(NoDuplicates)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(NoDuplicates)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(NoDuplicates)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(NoDuplicates)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(NoDuplicates)
            }
        }
        d.deserialize_any(Check)
    }
}
pub(crate) fn strict_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ReaderError> {
    serde_json::from_slice::<NoDuplicates>(bytes).map_err(|_| ReaderError)?;
    serde_json::from_slice(bytes).map_err(|_| ReaderError)
}

fn lengths(prefix: &[u8], max_header: usize, max_capture: usize) -> Result<(usize, usize), ReaderError> {
    if prefix.len() != PREFIX || &prefix[..8] != MAGIC {
        return Err(ReaderError);
    }
    let header = u32::from_le_bytes(prefix[8..12].try_into().map_err(|_| ReaderError)?) as usize;
    let capture = u32::from_le_bytes(prefix[12..16].try_into().map_err(|_| ReaderError)?) as usize;
    if header == 0 || header > max_header || capture > max_capture {
        return Err(ReaderError);
    }
    Ok((header, capture))
}
fn write_frame(header: &[u8], capture: &[u8]) -> Result<Vec<u8>, ReaderError> {
    let header_len = u32::try_from(header.len()).map_err(|_| ReaderError)?;
    let capture_len = u32::try_from(capture.len()).map_err(|_| ReaderError)?;
    let capacity = PREFIX.checked_add(header.len()).and_then(|n| n.checked_add(capture.len())).ok_or(ReaderError)?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&header_len.to_le_bytes());
    output.extend_from_slice(&capture_len.to_le_bytes());
    output.extend_from_slice(header);
    output.extend_from_slice(capture);
    Ok(output)
}
pub fn encode_request(request: &ReaderRequest) -> Result<Vec<u8>, ReaderError> {
    request.validate()?;
    let body = serde_json::to_vec(request).map_err(|_| ReaderError)?;
    if body.len() > MAX_REQUEST_BYTES || body.len() > request.limits.file_bytes {
        return Err(ReaderError);
    }
    write_frame(&body, &[])
}
/// Read one declared frame, without EOF or reads beyond it. The process-wide
/// CLI deadline, not a Tokio blocking task, bounds a stalled Read implementation.
pub fn read_request(reader: &mut impl Read) -> Result<(ReaderRequest, Vec<u8>), ReaderError> {
    let mut prefix = [0; PREFIX];
    reader.read_exact(&mut prefix).map_err(|_| ReaderError)?;
    let (header, _) = lengths(&prefix, MAX_REQUEST_BYTES, 0)?;
    let mut bytes = vec![0; header];
    reader.read_exact(&mut bytes).map_err(|_| ReaderError)?;
    let request: ReaderRequest = strict_json(&bytes)?;
    request.validate()?;
    if bytes.len() > request.limits.file_bytes {
        return Err(ReaderError);
    }
    Ok((request, bytes))
}
fn receipt_ok(receipt: &ProcessReceipt, nonce: &str) -> bool {
    receipt.stop == ProcessStop::Completed && receipt.root_exit_code == Some(0) && receipt.owner_session != nonce && receipt.cleanup_confirmed()
}
pub fn encode_response(request: &ReaderRequest, request_bytes: &[u8], output: BoundedResult) -> Result<Vec<u8>, ReaderError> {
    if request.schema != REQUEST_SCHEMA
        || output.code != Some(0)
        || output.spawn_failed
        || output.timed_out
        || output.output_limited
        || output.stdout.len() > request.limits.stdout_bytes
    {
        return Err(ReaderError);
    }
    let receipt = output.process_receipt.ok_or(ReaderError)?;
    if !receipt_ok(&receipt, &request.nonce) {
        return Err(ReaderError);
    }
    let header = ResponseHeader {
        schema: RESPONSE_SCHEMA.into(),
        nonce: request.nonce.clone(),
        scope_id: request.scope_id.clone(),
        observed_at_ms: request.observed_at_ms,
        expected_identity_hash: request.expected_identity_hash.clone(),
        request_sha256: crate::db::sha256_hex(request_bytes),
        probe_sha256: request.probe_sha256.clone(),
        budget_ms: request.budget_ms,
        helper_identity: receipt.owner.clone().ok_or(ReaderError)?,
        process_receipt: receipt,
        capture_sha256: crate::db::sha256_hex(&output.stdout),
    };
    let header = serde_json::to_vec(&header).map_err(|_| ReaderError)?;
    if header.len() > MAX_RESPONSE_HEADER_BYTES {
        return Err(ReaderError);
    }
    write_frame(&header, &output.stdout)
}
pub fn decode_response<'a>(bytes: &'a [u8], request: &ReaderRequest, request_bytes: &[u8]) -> Result<&'a [u8], ReaderError> {
    if request.schema != REQUEST_SCHEMA {
        return Err(ReaderError);
    }
    let prefix = bytes.get(..PREFIX).ok_or(ReaderError)?;
    let (header_len, capture_len) = lengths(prefix, MAX_RESPONSE_HEADER_BYTES, request.limits.stdout_bytes)?;
    let capture_start = PREFIX.checked_add(header_len).ok_or(ReaderError)?;
    let end = capture_start.checked_add(capture_len).ok_or(ReaderError)?;
    if end != bytes.len() {
        return Err(ReaderError);
    }
    let header: ResponseHeader = strict_json(&bytes[PREFIX..capture_start])?;
    if header.schema != RESPONSE_SCHEMA
        || header.nonce != request.nonce
        || header.scope_id != request.scope_id
        || header.observed_at_ms != request.observed_at_ms
        || header.expected_identity_hash != request.expected_identity_hash
        || header.request_sha256 != crate::db::sha256_hex(request_bytes)
        || header.probe_sha256 != request.probe_sha256
        || header.budget_ms != request.budget_ms
        || header.process_receipt.owner.as_ref() != Some(&header.helper_identity)
        || !receipt_ok(&header.process_receipt, &request.nonce)
        || header.capture_sha256 != crate::db::sha256_hex(&bytes[capture_start..])
    {
        return Err(ReaderError);
    }
    Ok(&bytes[capture_start..])
}

/// Unique per transport attempt, independent from the guard's owner session.
pub fn fresh_nonce() -> Result<String, ReaderError> {
    let mut bytes = [0u8; 16];
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
        #[allow(unsafe_code)]
        // SAFETY: fixed writable buffer; the system RNG has no external handle.
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if status < 0 {
            return Err(ReaderError);
        }
    }
    #[cfg(unix)]
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|_| ReaderError)?;
    #[cfg(not(any(windows, unix)))]
    return Err(ReaderError);
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Writes exactly one prepared response. The standalone CLI must supervise this
/// blocking operation with its own hard deadline and self-only OS exit boundary.
pub fn write_response(writer: &mut impl Write, bytes: &[u8]) -> Result<(), ReaderError> {
    writer.write_all(bytes).and_then(|()| writer.flush()).map_err(|_| ReaderError)
}

#[cfg(windows)]
pub async fn execute(request: &ReaderRequest, request_bytes: &[u8], deadline: std::time::Instant) -> Result<Vec<u8>, ReaderError> {
    execute_diagnostic(request, request_bytes, deadline).await.map_err(|_| ReaderError)
}

#[cfg(windows)]
pub async fn execute_diagnostic(request: &ReaderRequest, request_bytes: &[u8], deadline: std::time::Instant) -> Result<Vec<u8>, ReaderFailure> {
    if request.schema != REQUEST_SCHEMA {
        return Err(ReaderFailure::at(ReaderFailureStage::RequestValidation));
    }
    request.validate().map_err(|_| ReaderFailure::at(ReaderFailureStage::RequestValidation))?;
    if remote::identity_hash(&gethostname::gethostname().to_string_lossy()) != request.expected_identity_hash {
        return Err(ReaderFailure::at(ReaderFailureStage::Identity));
    }
    // The guard may spend up to three seconds observing termination. Keep that
    // and output overhead inside the standalone CLI's absolute deadline.
    let work_deadline = deadline.checked_sub(std::time::Duration::from_millis(3500)).ok_or_else(|| ReaderFailure::at(ReaderFailureStage::Deadline))?;
    let output = run_probe(request, work_deadline).await.map_err(|_| ReaderFailure::at(ReaderFailureStage::ProbePrepare))?;
    if output.code != Some(0)
        || output.spawn_failed
        || output.timed_out
        || output.output_limited
        || !output.process_receipt.as_ref().is_some_and(|receipt| receipt_ok(receipt, &request.nonce))
    {
        return Err(ReaderFailure::probe_result(&output, &request.nonce));
    }
    // Gate1 remains enforced before returning source bytes, in addition to the
    // embedded probe's pre-return byte checks and the controller's own check.
    remote::validate_capture_bytes(&request.remote_request(), &output.stdout).map_err(|_| ReaderFailure::at(ReaderFailureStage::CaptureValidation))?;
    if std::time::Instant::now() >= deadline {
        return Err(ReaderFailure::at(ReaderFailureStage::Deadline));
    }
    encode_response(request, request_bytes, output).map_err(|_| ReaderFailure::at(ReaderFailureStage::ResponseEncode))
}

// Keep the exact product launch available to Windows unit diagnostics. This
// returns existing bounded data without adding a log or an admission boundary.
#[cfg(windows)]
async fn run_probe(request: &ReaderRequest, work_deadline: std::time::Instant) -> Result<BoundedResult, ReaderError> {
    let work = work_deadline.checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero()).ok_or(ReaderError)?;
    let config = serde_json::json!({"source_allowlist":request.source_allowlist,"scope_id":request.scope_id,"observed_at_ms":request.observed_at_ms,
        "roots":request.roots,"configs":request.configs,"git_program":request.git_program,"limits":request.limits,"cursor":request.cursor,
        "expected_runtime":request.expected_runtime,"expected_distro":request.expected_distro,"expected_identity_hash":request.expected_identity_hash,
        "work_timeout_ms":work.as_millis()});
    let encoded = crate::logs::base64(&serde_json::to_vec(&config).map_err(|_| ReaderError)?);
    let script = format!(
        "$TopologyRequest = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(\"{encoded}\")) | ConvertFrom-Json\n{}",
        remote::WINDOWS_PROBE.trim_start_matches('\u{feff}')
    )
    .into_bytes();
    if script.len() > MAX_REQUEST_BYTES {
        return Err(ReaderError);
    }
    // Windows PowerShell inserts user/all-users module paths at startup even
    // when the process environment is explicit. Disable autoload before any
    // loader cmdlet and import only the two fixed inbox modules by full path.
    let prelude = r#"$PSModuleAutoLoadingPreference="None";$env:PSModulePath=$PSHOME+"\Modules";Import-Module -Name ($PSHOME+"\Modules\Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1") -ErrorAction Stop;Import-Module -Name ($PSHOME+"\Modules\Microsoft.PowerShell.Management\Microsoft.PowerShell.Management.psd1") -ErrorAction Stop;"#;
    let loader = format!("{prelude}{}", remote::WINDOWS_STDIN_LOADER.replace("__BYTES__", &script.len().to_string()));
    let loader = crate::logs::base64(&loader.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
    let args = vec!["-NoLogo".into(), "-NoProfile".into(), "-NonInteractive".into(), "-EncodedCommand".into(), loader];
    let program = powershell_program()?;
    let mut command = std::process::Command::new(program);
    command.args(args);
    let environment = super::native_reader::environment(&super::native_reader::windows::WindowsSourceIo::default()).map_err(|_| ReaderError)?;
    let output = crate::collect::run_standalone_windows_bounded(
        &command,
        crate::collect::StandaloneWindowsEnvironment::new(&environment),
        &script,
        work_deadline,
        request.limits.stdout_bytes,
        request.limits.stderr_bytes,
    )
    .await;
    Ok(output)
}
#[cfg(windows)]
#[allow(unsafe_code)]
fn powershell_program() -> Result<String, ReaderError> {
    let mut buffer = [0u16; 32768];
    // SAFETY: bounded writable buffer; Windows supplies the native system path.
    let count = unsafe { windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if count == 0 || count >= buffer.len() {
        return Err(ReaderError);
    }
    let root = String::from_utf16(&buffer[..count]).map_err(|_| ReaderError)?;
    Ok(format!("{root}\\WindowsPowerShell\\v1.0\\powershell.exe"))
}

#[cfg(all(test, windows))]
mod native_diagnostics {
    use super::*;
    use std::{
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    const CHILD: &str = "--legacy-probe-diagnostic-child";
    const RECORD: &str = "KATALA_LEGACY_PROBE_DIAGNOSTIC=";
    const TEST: &str = "topology::adapters::windows_reader::native_diagnostics::actual_legacy_probe_reports_sanitized_failure_metadata";
    const MANIFEST: &[u8] = b"{\"name\":\"legacy-probe-diagnostic-fixture\"}";

    fn require<T>(result: Result<T, impl std::fmt::Debug>, stage: &str) -> T {
        result.unwrap_or_else(|_| panic!("synthetic legacy diagnostic setup failed: {stage}"))
    }

    fn finite(child: &mut Child, deadline: Instant) -> std::process::ExitStatus {
        loop {
            if let Some(status) = require(child.try_wait(), "wait") {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("synthetic legacy diagnostic exceeded its finite window");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn git(program: &Path, root: &Path, arguments: &[&str]) -> Vec<u8> {
        let mut command = Command::new(program);
        command.current_dir(root).args(arguments).env_clear();
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP", "COMSPEC"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_CONFIG_SYSTEM", "NUL")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = require(command.spawn(), "Git spawn");
        let stdout = child.stdout.take().unwrap_or_else(|| panic!("synthetic Git stdout missing"));
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            require(stdout.take(8193).read_to_end(&mut bytes), "Git stdout");
            bytes
        });
        let status = finite(&mut child, Instant::now() + Duration::from_secs(10));
        assert!(status.success(), "synthetic Git setup/query failed: code={:?}", status.code());
        let bytes = reader.join().unwrap_or_else(|_| panic!("synthetic Git reader failed"));
        assert!(bytes.len() <= 8192, "synthetic Git output exceeded its bound");
        bytes
    }

    struct Fixture {
        root: PathBuf,
        program: PathBuf,
        request: ReaderRequest,
        original_root: String,
        original_has_tilde: bool,
        original_lexical_differs: bool,
    }
    impl Fixture {
        fn new() -> Self {
            let program = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_else(|| panic!("synthetic Git discovery PATH missing")))
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| path.join("git.exe"))
                .find(|path| path.is_file())
                .unwrap_or_else(|| panic!("real fixture Git is required"));
            let program = require(program.canonicalize(), "Git absolute path");
            let stamp = require(SystemTime::now().duration_since(UNIX_EPOCH), "fixture clock").as_nanos();
            let root = std::env::temp_dir().join(format!("legacy-probe-diagnostic-{}-{stamp}", std::process::id()));
            require(std::fs::create_dir(&root), "exclusive fixture directory");
            require(std::fs::write(root.join("package.json"), MANIFEST), "approved manifest");
            git(&program, &root, &["init", "--quiet"]);
            git(&program, &root, &["add", "--", "package.json"]);
            git(
                &program,
                &root,
                &[
                    "-c",
                    "user.name=Legacy Fixture",
                    "-c",
                    "user.email=legacy-fixture@example.org",
                    "-c",
                    "core.hooksPath=NUL",
                    "commit",
                    "--quiet",
                    "-m",
                    "synthetic fixture",
                ],
            );
            let head = require(String::from_utf8(git(&program, &root, &["rev-parse", "--verify", "HEAD"])), "fixture HEAD UTF-8").trim().to_owned();
            let program_text = program.to_str().unwrap_or_else(|| panic!("fixture Git path requires UTF-8")).trim_start_matches(r"\\?\").replace('\\', "/");
            let original_root = root.to_str().unwrap_or_else(|| panic!("fixture root requires UTF-8")).replace('\\', "/");
            // Approve the independently resolved fixture path before building
            // the request. Never turn the probe's reported hash into policy.
            let approved_root = require(root.canonicalize(), "approved fixture root");
            let root_text =
                approved_root.to_str().unwrap_or_else(|| panic!("approved fixture root requires UTF-8")).trim_start_matches(r"\\?\").replace('\\', "/");
            let recanonical = require(Path::new(&root_text).canonicalize(), "approved root canonical check");
            let recanonical = recanonical.to_str().unwrap_or_else(|| panic!("canonical check requires UTF-8")).trim_start_matches(r"\\?\").replace('\\', "/");
            assert!(root_text == recanonical, "approved fixture root must be lexically canonical");
            let original_has_tilde = original_root.contains('~');
            let original_lexical_differs = remote::source_root_fingerprint(&original_root, RemotePlatform::Windows)
                != remote::source_root_fingerprint(&root_text, RemotePlatform::Windows);
            let request = require(
                serde_json::from_value(serde_json::json!({
                    "schema":REQUEST_SCHEMA,"nonce":"0123456789abcdef0123456789abcdef","scope_id":"fixture:legacy-diagnostic","observed_at_ms":1000,
                    "expected_identity_hash":remote::identity_hash(&gethostname::gethostname().to_string_lossy()),"expected_runtime":"native","expected_distro":null,
                    "roots":[root_text],"configs":[],"cursor":null,"source_allowlist":[{"root_index":0,"relative_path":".","expected_head":head,
                    "manifests":[{"relative_path":"package.json","bytes":MANIFEST.len(),"sha256":crate::db::sha256_hex(MANIFEST)}]}],
                    "git_program":program_text,"limits":TopologyLimits::default(),"budget_ms":30000,"probe_sha256":probe_sha256()
                })),
                "request DTO",
            );
            Self { root, program, request, original_root, original_has_tilde, original_lexical_differs }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only this exclusively created synthetic fixture belongs to this test.
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Serialize)]
    struct Diagnostic {
        stage: &'static str,
        code: Option<i32>,
        spawn_failed: bool,
        timed_out: bool,
        output_limited: bool,
        stop: Option<ProcessStop>,
        root_exit_code: Option<i32>,
        ack: [bool; 6],
        total_processes: Option<u32>,
        observed_count: usize,
        stderr_bytes: usize,
        stderr_sha256: String,
        known_error_categories: Vec<&'static str>,
        capture_valid: bool,
    }

    fn value_kind(value: Option<&Value>) -> &'static str {
        match value {
            None => "missing",
            Some(Value::Null) => "null",
            Some(Value::Bool(_)) => "bool",
            Some(Value::String(_)) => "string",
            Some(Value::Array(_)) => "array",
            Some(Value::Object(_)) => "object",
            Some(Value::Number(number)) if number.is_u64() => "u64",
            Some(Value::Number(number)) if number.is_i64() => "i64",
            Some(Value::Number(_)) => "f64",
        }
    }

    // Names below are fixed schema keys; no incoming key, value or serde error
    // string is copied into the diagnostic. Equality checks disclose booleans.
    fn capture_structure(request: &ReaderRequest, bytes: &[u8]) -> Value {
        use crate::topology::{CaptureCounters, RawCapture, RawRepository};
        let result = remote::validate_capture_bytes(&request.remote_request(), bytes);
        let message = result.as_ref().err().map(|failure| match failure.message.as_str() {
            "transport contains invalid NUL" => "transport contains invalid NUL",
            "remote report invalid" => "remote report invalid",
            "remote runtime identity mismatch" => "remote runtime identity mismatch",
            "remote identity or request mismatch" => "remote identity or request mismatch",
            "remote report violates bounded schema" => "remote report violates bounded schema",
            "remote source allowlist mismatch" => "remote source allowlist mismatch",
            _ => "unclassified validation failure",
        });
        let raw = serde_json::from_slice::<Value>(bytes);
        let parse_error = raw.as_ref().err().map(|error| {
            let category = match error.classify() {
                serde_json::error::Category::Io => "io",
                serde_json::error::Category::Syntax => "syntax",
                serde_json::error::Category::Data => "data",
                serde_json::error::Category::Eof => "eof",
            };
            serde_json::json!({"category":category,"line":error.line(),"column":error.column()})
        });
        let raw = raw.ok();
        let raw = raw.as_ref();
        let field_types =
            ["schema", "scope_id", "adapter", "observed_at_ms", "target_identity", "runtime", "limits", "repositories", "failures", "counters", "progress"]
                .into_iter()
                .map(|key| (key, value_kind(raw.and_then(|value| value.get(key)))))
                .collect::<Vec<_>>();
        let counter_types = ["entries", "files", "bytes", "repositories", "manifests", "children", "duration_ms", "config_files", "config_keys"]
            .into_iter()
            .map(|key| (key, value_kind(raw.and_then(|value| value.get("counters")).and_then(|value| value.get(key)))))
            .collect::<Vec<_>>();
        let first_repo = raw.and_then(|value| value.get("repositories")).and_then(Value::as_array).and_then(|values| values.first());
        let repository_types = ["relative_path", "root_fingerprint", "git", "manifests", "configs", "config_file_read", "failures"]
            .into_iter()
            .map(|key| (key, value_kind(first_repo.and_then(|value| value.get(key)))))
            .collect::<Vec<_>>();
        let first_manifest = first_repo.and_then(|value| value.get("manifests")).and_then(Value::as_array).and_then(|values| values.first());
        let manifest_types = ["relative_path", "kind", "content", "content_sha256", "revision"]
            .into_iter()
            .map(|key| (key, value_kind(first_manifest.and_then(|value| value.get(key)))))
            .collect::<Vec<_>>();
        let typed = raw.and_then(|value| serde_json::from_value::<RawCapture>(value.clone()).ok());
        let bindings = typed.as_ref().map(|capture| {
            [
                capture.scope_id == request.scope_id,
                capture.adapter == "remote",
                capture.observed_at_ms == request.observed_at_ms,
                capture.limits == request.limits,
                capture.target_identity.as_deref() == Some(request.expected_identity_hash.as_str()),
            ]
        });
        let source_bindings = typed.as_ref().and_then(|capture| capture.repositories.first()).map(|repo| {
            let approved = &request.source_allowlist[0];
            let manifest = repo.manifests.first();
            let file = &approved.manifests[0];
            [
                repo.root_fingerprint == remote::source_root_fingerprint(&request.roots[approved.root_index], RemotePlatform::Windows),
                repo.relative_path == approved.relative_path,
                repo.git.head.as_deref() == Some(approved.expected_head.as_str()),
                repo.git.git_dir.as_deref().is_some_and(remote::hash),
                repo.git.common_dir.as_deref().is_some_and(remote::hash),
                !repo.config_file_read && repo.configs.is_empty(),
                repo.manifests.len() == approved.manifests.len(),
                manifest.is_some_and(|value| value.relative_path == file.relative_path),
                manifest.is_some_and(|value| value.revision.as_deref() == Some(approved.expected_head.as_str())),
                manifest.is_some_and(|value| value.content.len() == file.bytes),
                manifest.is_some_and(|value| value.content_sha256 == file.sha256),
                manifest.is_some_and(|value| crate::db::sha256_hex(value.content.as_bytes()) == file.sha256),
                manifest.is_some_and(|value| value.kind == "node"),
            ]
        });
        let counts = typed.as_ref().map(|capture| {
            [
                capture.repositories.len(),
                capture.failures.len(),
                capture.repositories.iter().map(|repo| repo.failures.len()).sum(),
                capture.counters.entries,
                capture.counters.files,
                capture.counters.bytes,
                capture.counters.repositories,
                capture.counters.manifests,
                capture.counters.children,
                capture.counters.config_files,
                capture.counters.config_keys,
            ]
        });
        serde_json::json!({
            "failure_code":result.as_ref().err().map(|failure| &failure.code),"failure_message":message,
            "stdout_bytes":bytes.len(),"stdout_sha256":crate::db::sha256_hex(bytes),"contains_nul":bytes.contains(&0),
            "utf8_valid":std::str::from_utf8(bytes).is_ok(),"json_parse_error":parse_error,"strict_json_valid":strict_json::<Value>(bytes).is_ok(),
            "root_type":value_kind(raw),"field_types":field_types,"counter_types":counter_types,"repository_types":repository_types,"manifest_types":manifest_types,
            "capture_deserializable":typed.is_some(),"repository_deserializable":first_repo.is_some_and(|value| serde_json::from_value::<RawRepository>(value.clone()).is_ok()),
            "counters_deserializable":raw.and_then(|value|value.get("counters")).is_some_and(|value| serde_json::from_value::<CaptureCounters>(value.clone()).is_ok()),
            "request_bindings":bindings,"source_bindings":source_bindings,"counts":counts,
            "runtime_bindings":raw.map(|value| [value["runtime"]["kind"] == "native",value["runtime"]["platform"] == "windows",value["runtime"]["kernel_wsl"] == false])
        })
    }
    fn diagnostic(output: &BoundedResult, capture_valid: bool) -> Diagnostic {
        let receipt = output.process_receipt.as_ref();
        let categories: &[&str] = &[
            "CommandNotFoundException",
            "Modules_ModuleNotFound",
            "ModuleCannotBeLoaded",
            "ImportModuleException",
            "InvalidOperation",
            "ArgumentNullException",
            "UnauthorizedAccess",
            "ProviderNotFound",
            "TypeNotFound",
            "ParserError",
            "FileNotFoundException",
            "TypeInitializationException",
        ];
        Diagnostic {
            stage: if output.code == Some(0) && receipt.is_some_and(ProcessReceipt::cleanup_confirmed) { "capture_validation" } else { "guard_result" },
            code: output.code,
            spawn_failed: output.spawn_failed,
            timed_out: output.timed_out,
            output_limited: output.output_limited,
            stop: receipt.map(|value| value.stop),
            root_exit_code: receipt.and_then(|value| value.root_exit_code),
            ack: receipt.map_or([false; 6], |value| {
                [value.root_exited, value.job_empty, value.observed_exited, value.stdout_eof, value.stderr_eof, value.writer_finished]
            }),
            total_processes: receipt.map(|value| value.total_processes),
            observed_count: receipt.map_or(0, |value| value.observed.len()),
            stderr_bytes: output.stderr.len(),
            stderr_sha256: crate::db::sha256_hex(&output.stderr),
            known_error_categories: categories
                .iter()
                .copied()
                .filter(|category| output.stderr.windows(category.len()).any(|bytes| bytes == category.as_bytes()))
                .collect(),
            capture_valid,
        }
    }

    #[test]
    fn actual_legacy_probe_reports_sanitized_failure_metadata() {
        if !std::env::args().any(|argument| argument == CHILD) {
            // Exactly one libtest runs in the child. All fixture std launches
            // finish before the strict backend exposes inheritable endpoints.
            let mut child = require(
                Command::new(require(std::env::current_exe(), "unit artifact"))
                    .args(["--exact", TEST, "--nocapture", "--", CHILD])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn(),
                "isolated diagnostic spawn",
            );
            let stdout = child.stdout.take().unwrap_or_else(|| panic!("isolated diagnostic stdout missing"));
            let reader = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                require(stdout.take(65537).read_to_end(&mut bytes), "isolated diagnostic output");
                bytes
            });
            let status = finite(&mut child, Instant::now() + Duration::from_secs(60));
            let bytes = reader.join().unwrap_or_else(|_| panic!("isolated diagnostic output reader failed"));
            assert!(bytes.len() <= 65536, "isolated diagnostic output exceeded its bound");
            let text = require(String::from_utf8(bytes), "isolated diagnostic output encoding");
            let records = text.lines().filter_map(|line| line.strip_prefix(RECORD)).collect::<Vec<_>>();
            assert_eq!(records.len(), 1, "the exact isolated test must emit its structural record");
            // Only our fixed-shape structural record is forwarded, never raw
            // libtest/Git/PowerShell output, paths, process identities or source.
            let record: serde_json::Value = require(serde_json::from_str(records[0]), "structural record JSON");
            eprintln!("{RECORD}{record}");
            let failures = crate::collect::windows_test_io::attempt_failures(&text).expect("closed first IO attempt records required");
            for failure in failures {
                // Typed enum/numeric/null data only; no arbitrary child text.
                eprintln!("KATALA_LEGACY_FIRST_IO={}", serde_json::to_string(&failure).expect("closed first IO JSON"));
            }
            assert!(status.success(), "actual legacy probe failed: code={:?}", status.code());
            return;
        }
        let fixture = Fixture::new();
        assert!(fixture.request.validate().is_ok(), "synthetic request must pass admission before IO");
        let runtime = require(tokio::runtime::Builder::new_current_thread().enable_all().build(), "isolated runtime");
        let mut original = fixture.request.clone();
        original.roots[0] = fixture.original_root.clone();
        // When the actual temp spelling differs, retain the old approval as a
        // negative control. The canonical positive remains mandatory everywhere.
        let original_output = fixture.original_lexical_differs.then(|| {
            println!("\nKATALA_LEGACY_ATTEMPT=alias");
            runtime.block_on(run_probe(&original, Instant::now() + Duration::from_millis(26500)))
        });
        println!("\nKATALA_LEGACY_ATTEMPT=canonical");
        let output = runtime.block_on(run_probe(&fixture.request, Instant::now() + Duration::from_millis(26500)));
        drop(runtime);
        let output = match output {
            Ok(output) => output,
            Err(_) => {
                let record = diagnostic(&BoundedResult::default(), false);
                let mut record = require(serde_json::to_value(record), "launch-error diagnostic");
                record["stage"] = Value::String("launch_preparation".into());
                println!("\n{RECORD}{record}");
                panic!("actual legacy probe launch preparation failed");
            }
        };
        let capture = remote::validate_capture_bytes(&fixture.request.remote_request(), &output.stdout);
        let mut record = require(serde_json::to_value(diagnostic(&output, capture.is_ok())), "structural diagnostic");
        record["validation"] = capture_structure(&fixture.request, &output.stdout);
        record["original_has_tilde"] = Value::Bool(fixture.original_has_tilde);
        record["original_lexical_differs"] = Value::Bool(fixture.original_lexical_differs);
        record["original_alias_attempt"] = original_output.as_ref().map_or(Value::Null, |attempt| match attempt {
            Err(_) => serde_json::json!({"preparation_ok":false}),
            Ok(previous) => {
                let mut value = require(
                    serde_json::to_value(diagnostic(previous, remote::validate_capture_bytes(&original.remote_request(), &previous.stdout).is_ok())),
                    "original-alias diagnostic",
                );
                value["preparation_ok"] = Value::Bool(true);
                value["validation"] = capture_structure(&original, &previous.stdout);
                value["reported_capture_matches_canonical_approval"] =
                    Value::Bool(remote::validate_capture_bytes(&fixture.request.remote_request(), &previous.stdout).is_ok());
                value
            }
        });
        println!("\n{RECORD}{record}");
        assert!(
            output.code == Some(0) && !output.spawn_failed && !output.timed_out && !output.output_limited,
            "actual legacy probe must exit zero within limits"
        );
        assert!(output.process_receipt.as_ref().is_some_and(ProcessReceipt::cleanup_confirmed), "actual legacy probe requires every OS ACK");
        let capture = require(capture, "actual legacy capture validation");
        assert!(capture.failures.is_empty(), "actual legacy capture must have no failures");
        assert!(capture.repositories.len() == 1 && capture.repositories[0].manifests.len() == 1, "actual approved manifest required");
        assert!(capture.repositories[0].manifests[0].content.as_bytes() == MANIFEST, "actual approved bytes required");
        if let Some(attempt) = original_output {
            let previous = require(attempt, "original-alias actual probe");
            assert!(
                previous.code == Some(0) && !previous.spawn_failed && !previous.timed_out && !previous.output_limited,
                "original-alias probe must finish within limits"
            );
            assert!(previous.process_receipt.as_ref().is_some_and(ProcessReceipt::cleanup_confirmed), "original-alias probe requires every OS ACK");
            let denied = remote::validate_capture_bytes(&original.remote_request(), &previous.stdout);
            assert!(
                denied.as_ref().err().is_some_and(
                    |failure| failure.code == crate::topology::FailureCode::PermissionDenied && failure.message == "remote source allowlist mismatch"
                ),
                "different original lexical root must remain denied"
            );
            assert!(
                remote::validate_capture_bytes(&fixture.request.remote_request(), &previous.stdout).is_ok(),
                "old probe must report exactly the independently approved canonical root"
            );
        }
        assert!(require(std::fs::read(fixture.root.join("package.json")), "post-read manifest") == MANIFEST, "approved fixture must remain unchanged");
        let head = require(String::from_utf8(git(&fixture.program, &fixture.root, &["rev-parse", "--verify", "HEAD"])), "post-read HEAD");
        assert!(head.trim() == fixture.request.source_allowlist[0].expected_head, "fixture HEAD must remain unchanged");
    }
}
