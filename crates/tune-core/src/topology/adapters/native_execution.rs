//! Finite standalone Windows execution. No application engine, store or configuration.
#![cfg(windows)]

use super::{
    native_protocol as protocol,
    native_reader::{self as native, GitCommand, GitOperation, GitRunner, NativeReaderError},
    windows_reader::{MAX_RESPONSE_HEADER_BYTES, ReaderError, ReaderRequest},
};
use crate::collect::{
    PinnedWindowsExecutable, ProcessStop, StandaloneWindowsEnvironment, current_windows_process_identity, run_owned_standalone_windows_bounded,
    run_standalone_windows_bounded,
};
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const CLEANUP_RESERVE: Duration = Duration::from_millis(3500);

fn validate_body(request: &ReaderRequest, exact_body: &[u8], deadline: Instant) -> Result<(), ReaderError> {
    if Instant::now() >= deadline || exact_body.len() > super::windows_reader::MAX_REQUEST_BYTES || exact_body.len() > request.limits.file_bytes {
        return Err(ReaderError);
    }
    request.validate()?;
    let parsed: ReaderRequest = super::windows_reader::strict_json(exact_body)?;
    if request.schema != protocol::REQUEST_SCHEMA
        || Instant::now() >= deadline
        || serde_json::to_value(parsed).map_err(|_| ReaderError)? != serde_json::to_value(request).map_err(|_| ReaderError)?
    {
        return Err(ReaderError);
    }
    Ok(())
}

struct FixedGitRunner {
    runtime: tokio::runtime::Runtime,
}
impl GitRunner for FixedGitRunner {
    fn run(&self, request: &GitCommand) -> Result<Vec<u8>, NativeReaderError> {
        let mut command = Command::new(request.program());
        command.current_dir(request.directory()).args(request.arguments());
        let result = self.runtime.block_on(run_standalone_windows_bounded(
            &command,
            StandaloneWindowsEnvironment::new(request.environment()),
            b"",
            request.deadline(),
            request.stdout_limit(),
            request.stderr_limit(),
        ));
        if result.timed_out {
            return Err(NativeReaderError::Timeout);
        }
        if result.output_limited {
            return Err(NativeReaderError::Limit);
        }
        if result.spawn_failed || !result.process_receipt.as_ref().is_some_and(|receipt| receipt.stop == ProcessStop::Completed && receipt.cleanup_confirmed())
        {
            return Err(NativeReaderError::Git);
        }
        // A missing local origin is the sole nonzero metadata result admitted.
        if request.operation() == GitOperation::Origin && result.code == Some(1) && result.stdout.is_empty() && result.stderr.is_empty() {
            return Ok(vec![]);
        }
        if result.code != Some(0) {
            return Err(NativeReaderError::Git);
        }
        Ok(result.stdout)
    }
}

pub fn execute_reader(request: &ReaderRequest, exact_body: &[u8], deadline: Instant) -> Result<Vec<u8>, ReaderError> {
    validate_body(request, exact_body, deadline)?;
    let source = native::windows::WindowsSourceIo::default();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| ReaderError)?;
    let git = FixedGitRunner { runtime };
    let capture = native::collect_with(&request.remote_request(), deadline, &source, &git).map_err(|_| ReaderError)?;
    let mut capture = serde_json::to_value(capture).map_err(|_| ReaderError)?;
    capture.as_object_mut().ok_or(ReaderError)?.insert("runtime".into(), serde_json::json!({"kind":"native","platform":"windows","kernel_wsl":false}));
    let bytes = serde_json::to_vec(&capture).map_err(|_| ReaderError)?;
    if bytes.len() > request.limits.stdout_bytes || Instant::now() >= deadline {
        return Err(ReaderError);
    }
    let identity = current_windows_process_identity().map_err(|_| ReaderError)?;
    let result = protocol::encode_processing_response(request, exact_body, &identity, &bytes)?;
    if Instant::now() >= deadline {
        return Err(ReaderError);
    }
    Ok(result)
}

fn child_budget(deadline: Instant) -> Result<(Instant, u64), ReaderError> {
    let end = deadline.checked_sub(CLEANUP_RESERVE).ok_or(ReaderError)?;
    let remaining = end.checked_duration_since(Instant::now()).ok_or(ReaderError)?;
    let millis = u64::try_from(remaining.as_millis()).map_err(|_| ReaderError)?;
    if millis == 0 {
        return Err(ReaderError);
    }
    Ok((end, millis))
}

fn pinned_self(program: &Path, artifact: &str, deadline: Instant) -> Result<PinnedWindowsExecutable, ReaderError> {
    let pin = PinnedWindowsExecutable::open(program, artifact, deadline).map_err(|_| ReaderError)?;
    // SAFETY: pseudo-handle is borrowed only for a read-only image query. This
    // early standalone process has not launched application/background work.
    #[allow(unsafe_code)]
    pin.verify_process(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() }).map_err(|_| ReaderError)?;
    Ok(pin)
}

/// Real inner observer. Its response describes inner exit; it cannot assert its own exit.
pub fn supervise(request: &ReaderRequest, exact_body: &[u8], deadline: Instant, program: &Path, artifact: &str) -> Result<Vec<u8>, ReaderError> {
    validate_body(request, exact_body, deadline)?;
    let (work_end, budget) = child_budget(deadline)?;
    let nonce = super::windows_reader::fresh_nonce()?;
    let (derived, frame) = protocol::derive_inner(request, nonce, budget.min(request.budget_ms.saturating_sub(1)))?;
    let pin = pinned_self(program, artifact, deadline)?;
    let mut command = Command::new(pin.executable());
    command.args(["topology-native-reader", "--budget-ms", &derived.budget_ms.to_string()]);
    let source = native::windows::WindowsSourceIo::default();
    let environment = native::environment(&source).map_err(|_| ReaderError)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| ReaderError)?;
    let limit = request.limits.stdout_bytes.saturating_add(MAX_RESPONSE_HEADER_BYTES + 16);
    let completion = runtime
        .block_on(run_owned_standalone_windows_bounded(
            pin,
            &command,
            StandaloneWindowsEnvironment::new(&environment),
            &frame,
            work_end,
            limit,
            request.limits.stderr_bytes,
        ))
        .map_err(|_| ReaderError)?;
    let receipt = completion.diagnostic().process_receipt.clone().ok_or(ReaderError)?;
    let inner = completion.into_verified_output(&frame, artifact).map_err(|_| ReaderError)?;
    // Native code validates the exact original and derived request bindings;
    // ProcessReceipt here remains wire diagnostics, never a capability issuer.
    let response = protocol::encode_outer_response(request, exact_body, &frame, &inner, &receipt, artifact)?;
    if Instant::now() >= deadline {
        return Err(ReaderError);
    }
    Ok(response)
}

/// Independent actual outer observer for native acceptance. The opaque completion
/// originates from this OS backend, then the wire codecs validate bound data.
pub fn verify(request: &ReaderRequest, exact_body: &[u8], deadline: Instant, program: &Path, artifact: &str) -> Result<Vec<u8>, ReaderError> {
    validate_body(request, exact_body, deadline)?;
    let (work_end, budget) = child_budget(deadline)?;
    // The native acceptance observer derives its supervised attempt explicitly,
    // changing nonce/budget only. The outer OS deadline remains authoritative;
    // a child's startup-relative budget cannot extend it.
    let (supervised, frame) = protocol::derive_inner(request, super::windows_reader::fresh_nonce()?, budget.min(request.budget_ms.saturating_sub(1)))?;
    let pin = pinned_self(program, artifact, deadline)?;
    let mut command = Command::new(pin.executable());
    command.args([
        "topology-supervise",
        "--budget-ms",
        &supervised.budget_ms.to_string(),
        "--reader-program",
        program.to_str().ok_or(ReaderError)?,
        "--helper-sha256",
        artifact,
    ]);
    let source = native::windows::WindowsSourceIo::default();
    let environment = native::environment(&source).map_err(|_| ReaderError)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| ReaderError)?;
    let limit = request.limits.stdout_bytes.saturating_add(2 * MAX_RESPONSE_HEADER_BYTES + 32);
    let completion = runtime
        .block_on(run_owned_standalone_windows_bounded(
            pin,
            &command,
            StandaloneWindowsEnvironment::new(&environment),
            &frame,
            work_end,
            limit,
            request.limits.stderr_bytes,
        ))
        .map_err(|_| ReaderError)?;
    let identity = completion.diagnostic().process_receipt.as_ref().and_then(|receipt| receipt.root.clone()).ok_or(ReaderError)?;
    let bytes = completion.into_verified_output(&frame, artifact).map_err(|_| ReaderError)?;
    let capture = protocol::decode_outer_response(&bytes, &supervised, &frame[16..], artifact, &identity)?.to_vec();
    if Instant::now() >= deadline {
        return Err(ReaderError);
    }
    Ok(capture)
}
