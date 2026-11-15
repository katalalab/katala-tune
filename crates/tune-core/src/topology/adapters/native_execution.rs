//! Finite standalone Windows execution. No application engine, store or configuration.
#![cfg(windows)]

use super::{
    native_protocol as protocol,
    native_reader::{self as native, GitCommand, GitOperation, GitRunner, NativeReaderError},
    windows_reader::{MAX_RESPONSE_HEADER_BYTES, ReaderError, ReaderRequest},
};
use crate::collect::{
    PinnedWindowsExecutable, StandaloneWindowsEnvironment, WindowsMetadataExitPolicy, current_windows_process_identity, run_owned_standalone_windows_bounded,
    run_shared_pinned_windows_metadata_bounded,
};
use std::{
    path::Path,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

const CLEANUP_RESERVE: Duration = Duration::from_millis(3500);

macro_rules! private_result {
    ($stage:ident, $result:expr) => {{
        let result = $result;
        #[cfg(katala_ci_diagnostics)]
        if result.is_err() {
            super::native_private_diagnostics::record(super::native_private_diagnostics::Failure::Stage(super::native_private_diagnostics::Stage::$stage));
        }
        result
    }};
}

#[cfg(katala_ci_diagnostics)]
fn private_stop(result: &crate::collect::BoundedResult) -> super::native_private_diagnostics::Stop {
    use super::native_private_diagnostics::Stop;
    use crate::collect::ProcessStop;
    match result.process_receipt.as_ref().map(|receipt| receipt.stop) {
        None | Some(ProcessStop::SetupFailed) => Stop::SetupFailed,
        Some(ProcessStop::SpawnFailed) => Stop::SpawnFailed,
        Some(ProcessStop::Completed) => Stop::Completed,
        Some(ProcessStop::Timeout) => Stop::Timeout,
        Some(ProcessStop::OutputLimit) => Stop::OutputLimit,
        Some(ProcessStop::LingeringDescendants) => Stop::LingeringDescendants,
        Some(ProcessStop::IoFailure) => Stop::IoFailure,
        Some(ProcessStop::CleanupUnconfirmed) => Stop::CleanupUnconfirmed,
    }
}

#[cfg(katala_ci_diagnostics)]
fn private_owned(role: super::native_private_diagnostics::Role, result: &crate::collect::BoundedResult) {
    use super::native_private_diagnostics as diagnostic;
    let receipt = result.process_receipt.as_ref();
    diagnostic::record(diagnostic::child_failure(
        role,
        private_stop(result),
        receipt.is_some_and(crate::collect::ProcessReceipt::cleanup_confirmed),
        !result.timed_out && !result.output_limited && !result.spawn_failed,
        receipt.and_then(|receipt| receipt.root_exit_code),
    ));
}

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
    pin: Arc<PinnedWindowsExecutable>,
    approved_program: String,
}
impl GitRunner for FixedGitRunner {
    fn run(&self, request: &GitCommand) -> Result<Vec<u8>, NativeReaderError> {
        #[cfg(test)]
        let ordinal = diagnostics::begin_git(request.operation());
        if request.program() != self.approved_program {
            #[cfg(katala_ci_diagnostics)]
            super::native_private_diagnostics::record(super::native_private_diagnostics::Failure::Stage(super::native_private_diagnostics::Stage::GitProgram));
            return Err(NativeReaderError::Git);
        }
        // The same lease survives every metadata query and all final HEAD
        // checks. Never reopen an approved path between queries, even for bytes
        // with the same hash: the retained NTFS identity is execution authority.
        let mut command = Command::new(self.pin.executable());
        command.current_dir(request.directory()).args(request.arguments());
        let policy = if request.operation() == GitOperation::Origin { WindowsMetadataExitPolicy::MissingOrigin } else { WindowsMetadataExitPolicy::ZeroOnly };
        let completion = self
            .runtime
            .block_on(run_shared_pinned_windows_metadata_bounded(
                Arc::clone(&self.pin),
                &command,
                StandaloneWindowsEnvironment::new(request.environment()),
                b"",
                request.deadline(),
                request.stdout_limit(),
                request.stderr_limit(),
                policy,
            ))
            .map_err(|result| {
                #[cfg(katala_ci_diagnostics)]
                {
                    use super::native_private_diagnostics as diagnostic;
                    let operation = match request.operation() {
                        GitOperation::GitDir => diagnostic::Operation::GitDir,
                        GitOperation::CommonDir => diagnostic::Operation::CommonDir,
                        GitOperation::Origin => diagnostic::Operation::Origin,
                        GitOperation::Head => diagnostic::Operation::Head,
                    };
                    // Git exit codes are never decoded as helper diagnostics.
                    diagnostic::record(diagnostic::Failure::Git(operation, private_stop(&result)));
                }
                #[cfg(test)]
                diagnostics::git_result(ordinal, request.operation(), "execution_rejected", &result);
                if result.timed_out {
                    NativeReaderError::Timeout
                } else if result.output_limited {
                    NativeReaderError::Limit
                } else {
                    NativeReaderError::Git
                }
            })?;
        #[cfg(test)]
        diagnostics::git_result(ordinal, request.operation(), "execution_completed", completion.diagnostic());
        // The actual OS witness is consumed here. Public receipt fields never
        // authorize source admission, including the empty missing-origin case.
        let output = completion.into_verified_output(b"", self.pin.sha256()).map_err(|result| {
            #[cfg(katala_ci_diagnostics)]
            super::native_private_diagnostics::record(super::native_private_diagnostics::Failure::Stage(super::native_private_diagnostics::Stage::GitConsume));
            #[cfg(test)]
            diagnostics::git_result(ordinal, request.operation(), "consumption_rejected", &result);
            #[cfg(not(test))]
            let _ = result;
            NativeReaderError::Git
        })?;
        #[cfg(test)]
        diagnostics::stage("git_consumed");
        Ok(output)
    }
}

pub fn execute_reader(request: &ReaderRequest, exact_body: &[u8], deadline: Instant) -> Result<Vec<u8>, ReaderError> {
    #[cfg(test)]
    diagnostics::stage("validate_body");
    private_result!(ReaderBody, validate_body(request, exact_body, deadline))?;
    #[cfg(test)]
    diagnostics::stage("pin_open");
    let artifact = private_result!(ReaderPin, request.git_artifact_sha256.as_deref().ok_or(ReaderError))?;
    let pin = private_result!(ReaderPin, PinnedWindowsExecutable::open(Path::new(&request.git_program), artifact, deadline).map_err(|_| ReaderError))?;
    let source = native::windows::WindowsSourceIo::default();
    #[cfg(test)]
    diagnostics::stage("runtime_build");
    let runtime = private_result!(ReaderRuntime, tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| ReaderError))?;
    let git = FixedGitRunner { runtime, pin: Arc::new(pin), approved_program: request.git_program.clone() };
    #[cfg(test)]
    diagnostics::stage("collect");
    let capture = native::collect_with(&request.remote_request(), deadline, &source, &git).map_err(|error| {
        #[cfg(katala_ci_diagnostics)]
        {
            use super::native_private_diagnostics as diagnostic;
            let error = match error {
                NativeReaderError::Unsupported => diagnostic::Source::Unsupported,
                NativeReaderError::Policy => diagnostic::Source::Policy,
                NativeReaderError::Limit => diagnostic::Source::Limit,
                NativeReaderError::Timeout => diagnostic::Source::Timeout,
                NativeReaderError::Source => diagnostic::Source::Source,
                NativeReaderError::Utf8 => diagnostic::Source::Utf8,
                NativeReaderError::Git => diagnostic::Source::Git,
                NativeReaderError::Identity => diagnostic::Source::Identity,
            };
            diagnostic::record(diagnostic::Failure::Source(error));
        }
        #[cfg(test)]
        diagnostics::source_failure(error);
        #[cfg(not(test))]
        let _ = error;
        ReaderError
    })?;
    #[cfg(test)]
    diagnostics::stage("capture_serialize");
    let mut capture = private_result!(ReaderSerialize, serde_json::to_value(capture).map_err(|_| ReaderError))?;
    private_result!(ReaderSerialize, capture.as_object_mut().ok_or(ReaderError))?
        .insert("runtime".into(), serde_json::json!({"kind":"native","platform":"windows","kernel_wsl":false}));
    let bytes = private_result!(ReaderSerialize, serde_json::to_vec(&capture).map_err(|_| ReaderError))?;
    #[cfg(test)]
    diagnostics::stage("capture_bounds");
    if bytes.len() > request.limits.stdout_bytes || Instant::now() >= deadline {
        return private_result!(ReaderBounds, Err(ReaderError));
    }
    #[cfg(test)]
    diagnostics::stage("reader_identity");
    let identity = private_result!(ReaderIdentity, current_windows_process_identity().map_err(|_| ReaderError))?;
    #[cfg(test)]
    diagnostics::stage("processing_encode");
    let result = private_result!(ReaderProtocol, protocol::encode_processing_response(request, exact_body, &identity, &bytes))?;
    #[cfg(test)]
    diagnostics::stage("response_deadline");
    if Instant::now() >= deadline {
        return private_result!(ReaderDeadline, Err(ReaderError));
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
    private_result!(SuperviseBody, validate_body(request, exact_body, deadline))?;
    let (work_end, budget) = private_result!(SuperviseBudget, child_budget(deadline))?;
    let nonce = private_result!(SuperviseNonce, super::windows_reader::fresh_nonce())?;
    let (derived, frame) = private_result!(SuperviseDerive, protocol::derive_inner(request, nonce, budget.min(request.budget_ms.saturating_sub(1))))?;
    let pin = private_result!(SupervisePin, pinned_self(program, artifact, deadline))?;
    let mut command = Command::new(pin.executable());
    command.args(["topology-native-reader", "--budget-ms", &derived.budget_ms.to_string()]);
    let source = native::windows::WindowsSourceIo::default();
    let environment = private_result!(SuperviseEnvironment, native::environment(&source).map_err(|_| ReaderError))?;
    let runtime = private_result!(SuperviseRuntime, tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| ReaderError))?;
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
        .map_err(|_result| {
            #[cfg(katala_ci_diagnostics)]
            private_owned(super::native_private_diagnostics::Role::Supervise, &_result);
            ReaderError
        })?;
    let receipt = private_result!(SuperviseReceipt, completion.diagnostic().process_receipt.clone().ok_or(ReaderError))?;
    let inner = private_result!(SuperviseConsume, completion.into_verified_output(&frame, artifact).map_err(|_| ReaderError))?;
    // Native code validates the exact original and derived request bindings;
    // ProcessReceipt here remains wire diagnostics, never a capability issuer.
    let response = private_result!(SuperviseProtocol, protocol::encode_outer_response(request, exact_body, &frame, &inner, &receipt, artifact))?;
    if Instant::now() >= deadline {
        return private_result!(SuperviseDeadline, Err(ReaderError));
    }
    Ok(response)
}

/// Independent actual outer observer for native acceptance. The opaque completion
/// originates from this OS backend, then the wire codecs validate bound data.
pub fn verify(request: &ReaderRequest, exact_body: &[u8], deadline: Instant, program: &Path, artifact: &str) -> Result<Vec<u8>, ReaderError> {
    private_result!(VerifyBody, validate_body(request, exact_body, deadline))?;
    let (work_end, budget) = private_result!(VerifyBudget, child_budget(deadline))?;
    // The native acceptance observer derives its supervised attempt explicitly,
    // changing nonce/budget only. The outer OS deadline remains authoritative;
    // a child's startup-relative budget cannot extend it.
    let nonce = private_result!(VerifyDerive, super::windows_reader::fresh_nonce())?;
    let (supervised, frame) = private_result!(VerifyDerive, protocol::derive_inner(request, nonce, budget.min(request.budget_ms.saturating_sub(1))))?;
    let pin = private_result!(VerifyPin, pinned_self(program, artifact, deadline))?;
    let mut command = Command::new(pin.executable());
    command.args([
        "topology-supervise",
        "--budget-ms",
        &supervised.budget_ms.to_string(),
        "--reader-program",
        private_result!(VerifyPin, program.to_str().ok_or(ReaderError))?,
        "--helper-sha256",
        artifact,
    ]);
    let source = native::windows::WindowsSourceIo::default();
    let environment = private_result!(VerifyEnvironment, native::environment(&source).map_err(|_| ReaderError))?;
    let runtime = private_result!(VerifyRuntime, tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| ReaderError))?;
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
        .map_err(|_result| {
            #[cfg(katala_ci_diagnostics)]
            private_owned(super::native_private_diagnostics::Role::Verify, &_result);
            ReaderError
        })?;
    let identity =
        private_result!(VerifyReceipt, completion.diagnostic().process_receipt.as_ref().and_then(|receipt| receipt.root.clone()).ok_or(ReaderError))?;
    let bytes = private_result!(VerifyConsume, completion.into_verified_output(&frame, artifact).map_err(|_| ReaderError))?;
    let capture = private_result!(VerifyProtocol, protocol::decode_outer_response(&bytes, &supervised, &frame[16..], artifact, &identity))?.to_vec();
    if Instant::now() >= deadline {
        return private_result!(VerifyDeadline, Err(ReaderError));
    }
    Ok(capture)
}

#[cfg(test)]
mod diagnostics {
    use super::*;
    use crate::topology::{
        RawCapture, TopologyLimits,
        adapters::{
            native_reader::SourceIo,
            remote::{RemotePlatform, RemoteRequest, RemoteRuntimeKind, RemoteSourceCheckout, RemoteSourceManifest},
            windows_reader as reader,
        },
    };
    use std::{cell::Cell, io::Read, process::Stdio};

    const MARKER: &str = "--native-reader-diagnostic";
    thread_local! {
        static ORDINAL: Cell<usize> = const { Cell::new(0) };
    }

    fn active() -> bool {
        std::env::args().any(|argument| argument == MARKER)
    }
    pub(super) fn stage(stage: &'static str) {
        if active() {
            eprintln!("native_reader_stage={stage}");
        }
    }
    pub(super) fn source_failure(error: NativeReaderError) {
        if active() {
            eprintln!("native_reader_source_error={error:?}");
        }
    }
    pub(super) fn begin_git(operation: GitOperation) -> usize {
        let ordinal = ORDINAL.with(|value| {
            let next = value.get() + 1;
            value.set(next);
            next
        });
        if active() {
            eprintln!("native_reader_git_begin={ordinal} operation={operation:?}");
        }
        ordinal
    }
    pub(super) fn git_result(ordinal: usize, operation: GitOperation, outcome: &'static str, result: &crate::collect::BoundedResult) {
        if !active() {
            return;
        }
        let receipt = result.process_receipt.as_ref();
        eprintln!(
            "native_reader_git={ordinal} operation={operation:?} outcome={outcome} code={:?} timeout={} limit={} spawn={} stdout_bytes={} stderr_bytes={} receipt={} owner={} root={} root_exit_code={:?} stop={:?} root_exited={} job_empty={} observed_exited={} stdout_eof={} stderr_eof={} writer_finished={} observed_count={} total_processes={} cleanup={}",
            result.code,
            result.timed_out,
            result.output_limited,
            result.spawn_failed,
            result.stdout.len(),
            result.stderr.len(),
            receipt.is_some(),
            receipt.is_some_and(|r| r.owner.is_some()),
            receipt.is_some_and(|r| r.root.is_some()),
            receipt.and_then(|r| r.root_exit_code),
            receipt.map(|r| r.stop),
            receipt.is_some_and(|r| r.root_exited),
            receipt.is_some_and(|r| r.job_empty),
            receipt.is_some_and(|r| r.observed_exited),
            receipt.is_some_and(|r| r.stdout_eof),
            receipt.is_some_and(|r| r.stderr_eof),
            receipt.is_some_and(|r| r.writer_finished),
            receipt.map_or(0, |r| r.observed.len()),
            receipt.map_or(0, |r| r.total_processes),
            receipt.is_some_and(|r| r.cleanup_confirmed()),
        );
    }

    fn require<T, E>(result: Result<T, E>, stage: &'static str) -> T {
        result.unwrap_or_else(|_| panic!("native reader diagnostic failed at {stage}"))
    }
    fn normal(path: &Path) -> String {
        require(path.to_str().ok_or(()), "lossless path").trim_start_matches(r"\\?\").replace('\\', "/")
    }
    fn finite(child: &mut std::process::Child, deadline: Instant) -> std::process::ExitStatus {
        loop {
            if let Some(status) = require(child.try_wait(), "process observation") {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("native reader diagnostic exceeded its fixed deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn actual_native_reader_reports_sanitized_stages() {
        let fixture = format!("{}::native_reader_standalone_fixture", require(module_path!().split_once("::").ok_or(()), "test module").1);
        let state = std::env::temp_dir().join(format!("native-reader-diagnostic-state-{}-{}", std::process::id(), crate::db::now_ms()));
        assert!(!state.exists());
        let mut command = Command::new(require(std::env::current_exe(), "test executable"));
        command
            .args(["--exact", &fixture, "--nocapture", "--", MARKER])
            .env("KATALA_TUNE_CONFIG", state.join("config/nodes.json"))
            .env("KATALA_TUNE_DATA_DIR", state.join("data"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let mut child = require(command.spawn(), "isolated fixture spawn");
        let status = finite(&mut child, Instant::now() + Duration::from_secs(45));
        assert!(!state.exists(), "read-only diagnostic must not create private state");
        assert!(status.success(), "isolated native reader diagnostic must succeed: code={:?}", status.code());
    }

    #[test]
    fn native_reader_standalone_fixture() {
        if !active() {
            return;
        }
        stage("fixture_approval");
        // PATH discovery and readonly HEAD/Cargo.toml reads are test approval,
        // never production artifact discovery or a source admission capability.
        let git = require(std::env::var_os("PATH").ok_or(()), "fixture Git search path");
        let git = require(
            std::env::split_paths(&git).filter(|path| !path.as_os_str().is_empty()).map(|path| path.join("git.exe")).find(|path| path.is_file()).ok_or(()),
            "fixture Git executable",
        );
        let git = require(git.canonicalize(), "fixture Git canonical path");
        let checkout = require(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize(), "CI checkout");
        let root = require(checkout.parent().ok_or(()), "registered root");
        let relative = require(require(checkout.file_name().ok_or(()), "checkout name").to_str().ok_or(()), "checkout UTF-8");
        let source = native::windows::WindowsSourceIo::default();
        let environment = require(native::environment(&source), "fixture environment");
        let mut query = Command::new(&git);
        query.current_dir(&checkout).env_clear().envs(environment).args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=NUL",
            "rev-parse",
            "--verify",
            "HEAD",
        ]);
        query.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = require(query.spawn(), "approved HEAD query");
        let stdout = require(child.stdout.take().ok_or(()), "HEAD pipe");
        let read = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(8193).read_to_end(&mut bytes);
            (result, bytes)
        });
        let query_end = Instant::now() + Duration::from_secs(10);
        assert!(finite(&mut child, query_end).success(), "readonly HEAD approval must succeed");
        while !read.is_finished() && Instant::now() < query_end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(read.is_finished(), "readonly HEAD query requires actual pipe EOF");
        let (result, head) = require(read.join(), "HEAD reader");
        require(result, "HEAD bytes");
        assert!(head.len() <= 8192);
        let head = require(String::from_utf8(head), "HEAD UTF-8").trim().to_owned();
        let limits = TopologyLimits::default();
        let mut approved = Vec::new();
        require(
            require(std::fs::File::open(checkout.join("Cargo.toml")), "approved manifest open").take(limits.file_bytes as u64 + 1).read_to_end(&mut approved),
            "approved manifest bytes",
        );
        assert!(approved.len() <= limits.file_bytes);
        let remote = RemoteRequest {
            scope_id: "fixture:native-reader-diagnostic".into(),
            alias: String::new(),
            expected_identity_hash: Some(require(source.identity_hash(), "approved host identity")),
            platform: RemotePlatform::Windows,
            expected_runtime: RemoteRuntimeKind::Native,
            expected_distro: None,
            cursor: None,
            roots: vec![normal(root)],
            configs: vec![],
            source_allowlist: Some(vec![RemoteSourceCheckout {
                root_index: 0,
                relative_path: relative.into(),
                expected_head: head.clone(),
                manifests: vec![RemoteSourceManifest { relative_path: "Cargo.toml".into(), bytes: approved.len(), sha256: crate::db::sha256_hex(&approved) }],
            }]),
            python_program: String::new(),
            windows_reader_program: None,
            git_program: Some(normal(&git)),
            observed_at_ms: crate::db::now_ms(),
            limits,
        };
        let artifact = crate::db::sha256_hex(&require(std::fs::read(&git), "approved Git artifact"));
        let request = require(ReaderRequest::from_remote_native(&remote, require(reader::fresh_nonce(), "request nonce"), 30_000, artifact), "native request");
        let frame = require(reader::encode_request(&request), "request frame");
        // One execution, no retries or restarted budgets. The public response is
        // processing completion only; this test does not assert helper self-exit.
        let deadline = Instant::now() + Duration::from_secs(30);
        let response = require(execute_reader(&request, &frame[16..], deadline), "execute_reader");
        stage("processing_decode");
        let bytes = require(protocol::decode_processing_response(&response, &request, &frame[16..]), "processing response");
        let capture: RawCapture = require(serde_json::from_slice(bytes), "capture JSON");
        assert!(Instant::now() < deadline, "decoding must share the original deadline");
        assert!(capture.failures.is_empty() && capture.progress.is_none());
        assert_eq!(capture.repositories.len(), 1);
        assert_eq!(capture.counters.children, 5);
        let repo = &capture.repositories[0];
        assert!(repo.relative_path == relative && repo.git.head.as_deref() == Some(head.as_str()));
        assert_eq!(repo.manifests.len(), 1);
        let manifest = &repo.manifests[0];
        assert!(manifest.relative_path == "Cargo.toml" && manifest.content.as_bytes() == approved);
        assert!(manifest.content_sha256 == crate::db::sha256_hex(&approved) && manifest.revision.as_deref() == Some(head.as_str()));
        eprintln!(
            "native_reader_stage=complete repositories={} children={} response_bytes={} capture_bytes={}",
            capture.repositories.len(),
            capture.counters.children,
            response.len(),
            bytes.len()
        );
    }
}
