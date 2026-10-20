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
    pub probe_sha256: String,
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
        if self.schema != REQUEST_SCHEMA
            || !hex(&self.nonce, 32)
            || self.scope_id.is_empty()
            || self.scope_id.len() > 512
            || self.scope_id.chars().any(char::is_control)
            || self.observed_at_ms <= 0
            || !hex(&self.expected_identity_hash, 64)
            || self.expected_runtime != RemoteRuntimeKind::Native
            || self.expected_distro.is_some()
            || self.probe_sha256 != probe_sha256()
            || self.budget_ms == 0
            || self.budget_ms > MAX_BUDGET_MS
            || self.budget_ms > self.limits.remote_timeout_seconds.saturating_mul(1000)
            || !valid_program_path(&self.git_program)
        {
            return Err(ReaderError);
        }
        // Reuse the product limit ceilings. Deserializing usize alone is not admission.
        TopologyConfig::from_value(Some(&serde_json::json!({"limits":self.limits}))).map_err(|_| ReaderError)?;
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
    if output.code != Some(0) || output.spawn_failed || output.timed_out || output.output_limited || output.stdout.len() > request.limits.stdout_bytes {
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
    request.validate()?;
    if remote::identity_hash(&gethostname::gethostname().to_string_lossy()) != request.expected_identity_hash {
        return Err(ReaderError);
    }
    let remaining = deadline.checked_duration_since(std::time::Instant::now()).ok_or(ReaderError)?;
    // The guard may spend up to three seconds observing termination. Keep that
    // and output overhead inside the standalone CLI's absolute deadline.
    let work = remaining.checked_sub(std::time::Duration::from_millis(3500)).filter(|d| !d.is_zero()).ok_or(ReaderError)?;
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
    let loader = remote::WINDOWS_STDIN_LOADER.replace("__BYTES__", &script.len().to_string());
    let loader = crate::logs::base64(&loader.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
    let args = vec!["-NoLogo".into(), "-NoProfile".into(), "-NonInteractive".into(), "-EncodedCommand".into(), loader];
    let program = powershell_program()?;
    let output = crate::collect::run_bounded(&program, &args, &script, work, request.limits.stdout_bytes, request.limits.stderr_bytes).await;
    if output.code != Some(0)
        || output.spawn_failed
        || output.timed_out
        || output.output_limited
        || !output.process_receipt.as_ref().is_some_and(|receipt| receipt_ok(receipt, &request.nonce))
    {
        return Err(ReaderError);
    }
    // Gate1 remains enforced before returning source bytes, in addition to the
    // embedded probe's pre-return byte checks and the controller's own check.
    remote::validate_capture_bytes(&request.remote_request(), &output.stdout).map_err(|_| ReaderError)?;
    if std::time::Instant::now() >= deadline {
        return Err(ReaderError);
    }
    encode_response(request, request_bytes, output)
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
