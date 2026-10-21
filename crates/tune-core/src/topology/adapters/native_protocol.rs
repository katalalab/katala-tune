//! Native processing/supervision codecs. Decoded diagnostics create no authority.
//! The caller must consume its retained OS witness/pin before controller decode.
use super::{
    remote,
    windows_reader::{self as reader, ReaderError, ReaderRequest},
};
use crate::collect::{ProcessIdentity, ProcessReceipt, ProcessStop};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Cursor, Write};

pub const REQUEST_SCHEMA: &str = "katala_native_reader_request.v2";
const PROCESSING_SCHEMA: &str = "katala_native_processing_response.v2";
const OUTER_SCHEMA: &str = "katala_native_supervision_response.v2";
const PROCESSING_MAGIC: &[u8; 8] = b"KTNAT002";
const OUTER_MAGIC: &[u8; 8] = b"KTSUP002";
const PREFIX: usize = 16;
pub const MAX_PROCESSING_HEADER_BYTES: usize = 4096;

/// Versioned digest of the two source-reader modules, not an executable hash or
/// permission. Whole-helper artifact identity is checked independently.
pub fn source_backend_sha256() -> String {
    let mut hash = Sha256::new();
    hash.update(b"katala-native-source-backend.v1\0");
    for source in [include_bytes!("native_reader.rs").as_slice(), include_bytes!("native_reader/windows.rs").as_slice()] {
        hash.update((source.len() as u64).to_le_bytes());
        hash.update(source);
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identity_ok(identity: &ProcessIdentity) -> bool {
    identity.pid != 0 && identity.created_filetime.parse::<u64>().is_ok_and(|n| n != 0 && n.to_string() == identity.created_filetime)
}
struct SizeLimit {
    used: usize,
    maximum: usize,
}
impl Write for SizeLimit {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.used = self.used.checked_add(bytes.len()).filter(|n| *n <= self.maximum).ok_or_else(|| io::Error::from(io::ErrorKind::FileTooLarge))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Called after the shared request scalar/limit checks and before cloning policy.
pub(crate) fn validate_native(request: &ReaderRequest) -> Result<(), ReaderError> {
    if request.schema != REQUEST_SCHEMA
        || request.probe_sha256 != source_backend_sha256()
        || !request.git_artifact_sha256.as_deref().is_some_and(|digest| hex(digest, 64))
        || request.cursor.is_some()
        || !request.configs.is_empty()
        || request.source_allowlist.len().checked_mul(5).is_none_or(|n| n > super::native_reader::MAX_GIT_CHILDREN)
        || request.source_allowlist.len() > request.limits.repositories_per_environment
    {
        return Err(ReaderError);
    }
    serde_json::to_writer(&mut SizeLimit { used: 0, maximum: reader::MAX_REQUEST_BYTES.min(request.limits.file_bytes) }, request).map_err(|_| ReaderError)
}
fn exact_body(request: &ReaderRequest, bytes: &[u8]) -> Result<(), ReaderError> {
    request.validate()?;
    if request.schema != REQUEST_SCHEMA || bytes.len() > reader::MAX_REQUEST_BYTES || bytes.len() > request.limits.file_bytes {
        return Err(ReaderError);
    }
    let parsed: ReaderRequest = reader::strict_json(bytes)?;
    parsed.validate()?;
    if serde_json::to_vec(&parsed).map_err(|_| ReaderError)? != serde_json::to_vec(request).map_err(|_| ReaderError)? {
        return Err(ReaderError);
    }
    Ok(())
}
fn write_frame(magic: &[u8; 8], header: &[u8], payload: &[u8]) -> Result<Vec<u8>, ReaderError> {
    let header_len = u32::try_from(header.len()).map_err(|_| ReaderError)?;
    let payload_len = u32::try_from(payload.len()).map_err(|_| ReaderError)?;
    let size = PREFIX.checked_add(header.len()).and_then(|n| n.checked_add(payload.len())).ok_or(ReaderError)?;
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(magic);
    bytes.extend_from_slice(&header_len.to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(header);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}
fn parts<'a>(bytes: &'a [u8], magic: &[u8; 8], header_max: usize, payload_max: usize) -> Result<(&'a [u8], &'a [u8]), ReaderError> {
    let prefix = bytes.get(..PREFIX).ok_or(ReaderError)?;
    if &prefix[..8] != magic {
        return Err(ReaderError);
    }
    let header = u32::from_le_bytes(prefix[8..12].try_into().map_err(|_| ReaderError)?) as usize;
    let payload = u32::from_le_bytes(prefix[12..16].try_into().map_err(|_| ReaderError)?) as usize;
    if header == 0 || header > header_max || payload > payload_max {
        return Err(ReaderError);
    }
    let start = PREFIX.checked_add(header).ok_or(ReaderError)?;
    if start.checked_add(payload) != Some(bytes.len()) {
        return Err(ReaderError);
    }
    Ok((&bytes[PREFIX..start], &bytes[start..]))
}

/// Canonical derivation v1: only nonce and budget change, with one serde DTO.
pub fn derive_inner(original: &ReaderRequest, nonce: String, budget_ms: u64) -> Result<(ReaderRequest, Vec<u8>), ReaderError> {
    original.validate()?;
    if original.schema != REQUEST_SCHEMA || nonce == original.nonce || !hex(&nonce, 32) || budget_ms == 0 || budget_ms > original.budget_ms {
        return Err(ReaderError);
    }
    let mut inner = original.clone();
    inner.nonce = nonce;
    inner.budget_ms = budget_ms;
    let frame = reader::encode_request(&inner)?;
    Ok((inner, frame))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessingHeader {
    schema: String,
    complete: bool,
    nonce: String,
    budget_ms: u64,
    request_sha256: String,
    source_backend_sha256: String,
    reader_identity: ProcessIdentity,
    capture_sha256: String,
}
fn processing_parts<'a>(frame: &'a [u8], request: &ReaderRequest, body: &[u8]) -> Result<(ProcessingHeader, &'a [u8]), ReaderError> {
    exact_body(request, body)?;
    let (header, capture) = parts(frame, PROCESSING_MAGIC, MAX_PROCESSING_HEADER_BYTES, request.limits.stdout_bytes)?;
    let header: ProcessingHeader = reader::strict_json(header)?;
    if header.schema != PROCESSING_SCHEMA
        || !header.complete
        || header.nonce != request.nonce
        || header.budget_ms != request.budget_ms
        || header.request_sha256 != crate::db::sha256_hex(body)
        || header.source_backend_sha256 != request.probe_sha256
        || !identity_ok(&header.reader_identity)
    {
        return Err(ReaderError);
    }
    Ok((header, capture))
}
fn capture_ok(request: &ReaderRequest, header: &ProcessingHeader, capture: &[u8]) -> Result<(), ReaderError> {
    if capture.len() > request.limits.stdout_bytes || header.capture_sha256 != crate::db::sha256_hex(capture) {
        return Err(ReaderError);
    }
    remote::validate_capture_bytes(&request.remote_request(), capture).map_err(|_| ReaderError)?;
    Ok(())
}
/// Processing completion only. It contains no exit/cleanup claim.
pub fn encode_processing_response(request: &ReaderRequest, body: &[u8], identity: &ProcessIdentity, capture: &[u8]) -> Result<Vec<u8>, ReaderError> {
    exact_body(request, body)?;
    if !identity_ok(identity) || capture.len() > request.limits.stdout_bytes {
        return Err(ReaderError);
    }
    let header = ProcessingHeader {
        schema: PROCESSING_SCHEMA.into(),
        complete: true,
        nonce: request.nonce.clone(),
        budget_ms: request.budget_ms,
        request_sha256: crate::db::sha256_hex(body),
        source_backend_sha256: request.probe_sha256.clone(),
        reader_identity: identity.clone(),
        capture_sha256: crate::db::sha256_hex(capture),
    };
    capture_ok(request, &header, capture)?;
    let header = serde_json::to_vec(&header).map_err(|_| ReaderError)?;
    if header.len() > MAX_PROCESSING_HEADER_BYTES {
        return Err(ReaderError);
    }
    write_frame(PROCESSING_MAGIC, &header, capture)
}
pub fn decode_processing_response<'a>(frame: &'a [u8], request: &ReaderRequest, body: &[u8]) -> Result<&'a [u8], ReaderError> {
    let (header, capture) = processing_parts(frame, request, body)?;
    capture_ok(request, &header, capture)?;
    Ok(capture)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OuterHeader {
    schema: String,
    original_request_body_sha256: String,
    derived_request_frame_sha256: String,
    inner_response_frame_sha256: String,
    artifact_sha256: String,
    inner_nonce: String,
    inner_budget_ms: u64,
    inner_receipt: ProcessReceipt,
}
fn receipt_ok(receipt: &ProcessReceipt, original: &ReaderRequest, inner: &ReaderRequest, identity: &ProcessIdentity, outer: &ProcessIdentity) -> bool {
    identity_ok(outer)
        && receipt.owner.as_ref() == Some(outer)
        && receipt.root.as_ref() == Some(identity)
        && receipt.stop == ProcessStop::Completed
        && receipt.root_exit_code == Some(0)
        && receipt.cleanup_confirmed()
        && hex(&receipt.owner_session, 32)
        && receipt.owner_session != original.nonce
        && receipt.owner_session != inner.nonce
}
fn inner_frame_max(request: &ReaderRequest) -> Result<usize, ReaderError> {
    request.limits.stdout_bytes.checked_add(MAX_PROCESSING_HEADER_BYTES + PREFIX).ok_or(ReaderError)
}

/// The receipt argument is diagnostic. The caller obtains it from its actual
/// retained inner process/Job; this codec cannot establish that provenance.
pub fn encode_outer_response(
    original: &ReaderRequest,
    body: &[u8],
    derived: &[u8],
    response: &[u8],
    receipt: &ProcessReceipt,
    expected_artifact: &str,
) -> Result<Vec<u8>, ReaderError> {
    exact_body(original, body)?;
    if !hex(expected_artifact, 64) || derived.len() > reader::MAX_REQUEST_BYTES + PREFIX || response.len() > inner_frame_max(original)? {
        return Err(ReaderError);
    }
    let mut cursor = Cursor::new(derived);
    let (inner, inner_body) = reader::read_request(&mut cursor)?;
    if cursor.position() != derived.len() as u64 {
        return Err(ReaderError);
    }
    let (_, canonical) = derive_inner(original, inner.nonce.clone(), inner.budget_ms)?;
    if canonical != derived {
        return Err(ReaderError);
    }
    let (processing, capture) = processing_parts(response, &inner, &inner_body)?;
    let outer = receipt.owner.as_ref().ok_or(ReaderError)?;
    if !receipt_ok(receipt, original, &inner, &processing.reader_identity, outer) {
        return Err(ReaderError);
    }
    capture_ok(&inner, &processing, capture)?;
    let header = OuterHeader {
        schema: OUTER_SCHEMA.into(),
        original_request_body_sha256: crate::db::sha256_hex(body),
        derived_request_frame_sha256: crate::db::sha256_hex(derived),
        inner_response_frame_sha256: crate::db::sha256_hex(response),
        artifact_sha256: expected_artifact.into(),
        inner_nonce: inner.nonce,
        inner_budget_ms: inner.budget_ms,
        inner_receipt: receipt.clone(),
    };
    let header = serde_json::to_vec(&header).map_err(|_| ReaderError)?;
    if header.len() > reader::MAX_RESPONSE_HEADER_BYTES {
        return Err(ReaderError);
    }
    write_frame(OUTER_MAGIC, &header, response)
}
/// expected_outer_identity must come from the independent retained OS observer,
/// after consuming its matching artifact pin/witness. Never derive it from wire.
pub fn decode_outer_response<'a>(
    frame: &'a [u8],
    original: &ReaderRequest,
    body: &[u8],
    expected_artifact: &str,
    expected_outer_identity: &ProcessIdentity,
) -> Result<&'a [u8], ReaderError> {
    exact_body(original, body)?;
    if !hex(expected_artifact, 64) || !identity_ok(expected_outer_identity) {
        return Err(ReaderError);
    }
    let (header, response) = parts(frame, OUTER_MAGIC, reader::MAX_RESPONSE_HEADER_BYTES, inner_frame_max(original)?)?;
    let header: OuterHeader = reader::strict_json(header)?;
    if header.schema != OUTER_SCHEMA
        || header.artifact_sha256 != expected_artifact
        || header.original_request_body_sha256 != crate::db::sha256_hex(body)
        || header.inner_response_frame_sha256 != crate::db::sha256_hex(response)
    {
        return Err(ReaderError);
    }
    let (inner, derived) = derive_inner(original, header.inner_nonce, header.inner_budget_ms)?;
    if header.derived_request_frame_sha256 != crate::db::sha256_hex(&derived) {
        return Err(ReaderError);
    }
    let (processing, capture) = processing_parts(response, &inner, &derived[PREFIX..])?;
    if !receipt_ok(&header.inner_receipt, original, &inner, &processing.reader_identity, expected_outer_identity) {
        return Err(ReaderError);
    }
    // Capture JSON/Gate1 is reached only after the inner exit/cleanup chain.
    capture_ok(&inner, &processing, capture)?;
    Ok(capture)
}
