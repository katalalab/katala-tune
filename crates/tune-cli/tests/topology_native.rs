use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn sandbox() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "native-cli-fixture-{}-{}-{}",
        std::process::id(),
        tune_core::db::now_ms(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}
fn launch(arguments: &[&str], base: &std::path::Path) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_tune"))
        .args(arguments)
        .env("KATALA_TUNE_CONFIG", base.join("config/nodes.json"))
        .env("KATALA_TUNE_DATA_DIR", base.join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}
fn finite(child: &mut std::process::Child, duration: Duration) -> std::process::ExitStatus {
    let end = Instant::now() + duration;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            panic!("standalone native command exceeded its finite exit window");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn native_entries_reject_before_application_runtime_or_private_state() {
    let base = sandbox();
    for entry in ["topology-native-reader", "topology-supervise", "topology-verify"] {
        for rest in [vec![], vec!["--budget-ms", "invalid"], vec!["--budget-ms", "0"], vec!["--budget-ms", "3600001"]] {
            let mut args = vec![entry];
            args.extend(rest);
            let mut child = launch(&args, &base);
            assert!(!finite(&mut child, Duration::from_secs(3)).success());
            let output = child.wait_with_output().unwrap();
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        }
    }
    assert!(!base.exists());
}

#[cfg(windows)]
#[test]
fn native_partial_input_remains_finite_without_eof_or_private_state() {
    use std::io::Write;
    let base = sandbox();
    let program = std::path::Path::new(env!("CARGO_BIN_EXE_tune")).canonicalize().unwrap().to_str().unwrap().trim_start_matches(r"\\?\").replace('\\', "/");
    let digest = tune_core::db::sha256_hex(&std::fs::read(env!("CARGO_BIN_EXE_tune")).unwrap());
    for entry in ["topology-native-reader", "topology-supervise", "topology-verify"] {
        for input in [b"".as_slice(), b"KTRDR001".as_slice(), b"KTRDR001\x20\0\0\0\0\0\0\0{".as_slice()] {
            let mut args = vec![entry, "--budget-ms", "200"];
            if entry != "topology-native-reader" {
                args.extend(["--reader-program", &program, "--helper-sha256", &digest]);
            }
            let mut child = launch(&args, &base);
            if let Err(error) = child.stdin.as_mut().unwrap().write_all(input) {
                assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
            }
            assert!(!finite(&mut child, Duration::from_secs(3)).success());
            let output = child.wait_with_output().unwrap();
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        }
    }
    assert!(!base.exists());
}

#[cfg(windows)]
mod native_windows {
    use super::{finite, launch, sandbox};
    use std::{
        io::{Read, Write},
        os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle},
        path::{Path, PathBuf},
        process::{Child, Command, Output, Stdio},
        time::{Duration, Instant},
    };
    use tune_core::{
        collect::ProcessIdentity,
        topology::{
            RawCapture, TopologyLimits,
            adapters::{
                native_protocol,
                native_reader::{SourceIo, windows::WindowsSourceIo},
                remote::{self, RemotePlatform, RemoteRequest, RemoteRuntimeKind, RemoteSourceCheckout, RemoteSourceManifest},
                windows_reader::{self as reader, ReaderRequest},
            },
        },
    };

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const ENTRIES: [&str; 3] = ["topology-native-reader", "topology-supervise", "topology-verify"];
    const MANIFEST: &[u8] = b"{\"name\":\"native-cli-approved-fixture\",\"version\":\"1.0.0\"}";

    fn normal(path: &Path) -> String {
        path.canonicalize().unwrap().to_str().unwrap().trim_start_matches(r"\\?\").replace('\\', "/")
    }

    struct Fixture {
        base: PathBuf,
        repo: PathBuf,
        git: PathBuf,
        head: String,
        approved: Vec<u8>,
    }
    impl Fixture {
        fn new(approved: Vec<u8>) -> Self {
            // PATH is fixture setup discovery only. The product receives this
            // canonical absolute executable and an explicit complete environment.
            let git = std::env::split_paths(&std::env::var_os("PATH").expect("Windows fixture PATH"))
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| path.join("git.exe"))
                .find(|path| path.is_file())
                .expect("real fixture Git must be installed")
                .canonicalize()
                .unwrap();
            let base = sandbox();
            std::fs::create_dir(&base).unwrap();
            let mut fixture = Self { repo: base.join("repo"), base, git, head: String::new(), approved };
            std::fs::create_dir(&fixture.repo).unwrap();
            fixture.git(&["init", "--quiet"]);
            fixture.git(&["config", "remote.origin.url", "https://example.org/native-fixture.git"]);
            std::fs::write(fixture.repo.join("package.json"), &fixture.approved).unwrap();
            std::fs::write(fixture.repo.join("README.md"), b"committed synthetic fixture\n").unwrap();
            fixture.git(&["add", "--", "package.json", "README.md"]);
            fixture.git(&["commit", "--quiet", "-m", "synthetic native CLI fixture"]);
            fixture.head = String::from_utf8(fixture.git(&["rev-parse", "--verify", "HEAD"])).unwrap().trim().to_owned();
            // Dirty, unapproved content is permitted but must never be read into
            // capture, reset, committed, or otherwise changed by the reader.
            std::fs::write(fixture.repo.join("README.md"), b"unapproved dirty synthetic fixture\n").unwrap();
            fixture
        }

        fn git(&self, arguments: &[&str]) -> Vec<u8> {
            let mut command = Command::new(&self.git);
            command.current_dir(&self.repo).env_clear();
            for name in ["SystemRoot", "WINDIR", "TEMP", "TMP", "COMSPEC"] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_SYSTEM", "NUL")
                .env("GIT_CONFIG_GLOBAL", "NUL")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_OPTIONAL_LOCKS", "0")
                .args([
                    "--no-optional-locks",
                    "-c",
                    "core.hooksPath=NUL",
                    "-c",
                    "core.fsmonitor=false",
                    "-c",
                    "gc.auto=0",
                    "-c",
                    "user.name=Native Fixture",
                    "-c",
                    "user.email=native-fixture@example.org",
                ])
                .args(arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().unwrap();
            assert!(finite(&mut child, Duration::from_secs(10)).success(), "synthetic Git setup/query must succeed");
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            output.stdout
        }

        fn request(&self, budget_ms: u64) -> ReaderRequest {
            let identity = WindowsSourceIo::default().identity_hash().expect("read actual native host identity hash");
            let remote = RemoteRequest {
                scope_id: "fixture:native-cli".into(),
                alias: String::new(),
                expected_identity_hash: Some(identity),
                platform: RemotePlatform::Windows,
                expected_runtime: RemoteRuntimeKind::Native,
                expected_distro: None,
                cursor: None,
                roots: vec![normal(&self.base)],
                configs: vec![],
                source_allowlist: Some(vec![RemoteSourceCheckout {
                    root_index: 0,
                    relative_path: "repo".into(),
                    expected_head: self.head.clone(),
                    manifests: vec![RemoteSourceManifest {
                        relative_path: "package.json".into(),
                        bytes: self.approved.len(),
                        sha256: tune_core::db::sha256_hex(&self.approved),
                    }],
                }]),
                python_program: String::new(),
                windows_reader_program: Some(normal(Path::new(env!("CARGO_BIN_EXE_tune")))),
                git_program: Some(normal(&self.git)),
                observed_at_ms: tune_core::db::now_ms(),
                limits: TopologyLimits { file_bytes: 4 * 1024 * 1024, stdout_bytes: 16 * 1024 * 1024, ..Default::default() },
            };
            ReaderRequest::from_remote_native(&remote, reader::fresh_nonce().unwrap(), budget_ms).expect("native v2 request")
        }

        fn snapshot(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
            (
                self.git(&["rev-parse", "--verify", "HEAD"]),
                std::fs::read(self.repo.join("package.json")).unwrap(),
                std::fs::read(self.repo.join("README.md")).unwrap(),
                self.git(&["status", "--porcelain", "--untracked-files=all"]),
            )
        }

        fn assert_private_state_absent(&self) {
            assert!(!self.base.join("config").exists());
            assert!(!self.base.join("data").exists());
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // This unique directory was created exclusively by this fixture.
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn artifact() -> (String, String) {
        (normal(Path::new(env!("CARGO_BIN_EXE_tune"))), tune_core::db::sha256_hex(&std::fs::read(env!("CARGO_BIN_EXE_tune")).unwrap()))
    }

    fn spawn_entry(fixture: &Fixture, entry: &str, budget: &str, digest: &str) -> Child {
        let (program, _) = artifact();
        let mut args = vec![entry, "--budget-ms", budget];
        if entry != "topology-native-reader" {
            args.extend(["--reader-program", &program, "--helper-sha256", digest]);
        }
        launch(&args, &fixture.base)
    }

    struct RetainedChild {
        _process: OwnedHandle,
        identity: ProcessIdentity,
    }

    #[allow(unsafe_code)]
    fn retained_identity(child: &Child) -> RetainedChild {
        use windows_sys::Win32::System::{RemoteDesktop::ProcessIdToSessionId, Threading::GetProcessTimes};
        let (mut creation, mut exit, mut kernel, mut user) = (Default::default(), Default::default(), Default::default(), Default::default());
        // SAFETY: Child retains the actual process handle; every FILETIME output
        // is writable. No PID lookup, termination, or handle ownership transfer.
        assert_ne!(unsafe { GetProcessTimes(child.as_raw_handle(), &mut creation, &mut exit, &mut kernel, &mut user) }, 0);
        let filetime = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let mut session = 0;
        // SAFETY: child is still retained and has not been reaped by this test.
        assert_ne!(unsafe { ProcessIdToSessionId(child.id(), &mut session) }, 0);
        assert!(filetime > (1u64 << 53), "retain full FILETIME precision");
        // SAFETY: Child owns this checked live handle through duplication. The
        // independent read-only observer remains held through response decoding.
        let process = unsafe { BorrowedHandle::borrow_raw(child.as_raw_handle()) }.try_clone_to_owned().unwrap();
        RetainedChild { _process: process, identity: ProcessIdentity { pid: child.id(), created_filetime: filetime.to_string(), os_session: session } }
    }

    fn run(fixture: &Fixture, entry: &str, request: &ReaderRequest, trailing: bool, digest: &str) -> (Output, RetainedChild, Vec<u8>) {
        let frame = reader::encode_request(request).unwrap();
        let end = Instant::now() + Duration::from_millis(request.budget_ms + 5000);
        let mut child = spawn_entry(fixture, entry, &request.budget_ms.to_string(), digest);
        let identity = retained_identity(&child);
        let stdout = bounded_reader(child.stdout.take().unwrap(), 18 * 1024 * 1024 + reader::MAX_RESPONSE_HEADER_BYTES);
        let stderr = bounded_reader(child.stderr.take().unwrap(), request.limits.stderr_bytes);
        if let Err(error) = child.stdin.as_mut().unwrap().write_all(&frame) {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        if trailing && let Err(error) = child.stdin.as_mut().unwrap().write_all(b"ignored-after-one-complete-frame") {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        // Keep stdin open through process exit: a bounded frame never awaits EOF.
        let status = finite(&mut child, end.saturating_duration_since(Instant::now()));
        let stdout = joined_reader(stdout, end);
        let stderr = joined_reader(stderr, end);
        (Output { status, stdout, stderr }, identity, frame[16..].to_vec())
    }

    fn bounded_reader(pipe: impl Read + Send + 'static, limit: usize) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take((limit + 1) as u64).read_to_end(&mut bytes)?;
            // At most `limit` bytes means Read returned actual EOF before the
            // take boundary. Reaching limit+1 is an overflow, never EOF proof.
            if bytes.len() > limit {
                return Err(std::io::Error::from(std::io::ErrorKind::FileTooLarge));
            }
            Ok(bytes)
        })
    }

    fn joined_reader(thread: std::thread::JoinHandle<std::io::Result<Vec<u8>>>, deadline: Instant) -> Vec<u8> {
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(thread.is_finished(), "actual pipe EOF required within the original finite test window");
        thread.join().expect("bounded reader thread").expect("bounded output and actual EOF")
    }

    fn assert_capture(bytes: &[u8], request: &ReaderRequest, fixture: &Fixture) {
        let value: serde_json::Value = serde_json::from_slice(bytes).expect("native capture JSON");
        assert_eq!(value["runtime"]["kind"], "native");
        assert_eq!(value["runtime"]["platform"], "windows");
        assert_eq!(value["runtime"]["kernel_wsl"], false);
        let capture: RawCapture = serde_json::from_value(value).unwrap();
        assert_eq!(capture.scope_id, request.scope_id);
        assert_eq!(capture.target_identity.as_deref(), Some(request.expected_identity_hash.as_str()));
        assert_eq!(capture.observed_at_ms, request.observed_at_ms);
        assert!(capture.limits == request.limits);
        assert!(capture.failures.is_empty());
        assert_eq!(capture.repositories.len(), 1);
        assert_eq!(capture.counters.children, 5, "actual Git metadata and final HEAD recheck");
        let repo = &capture.repositories[0];
        assert_eq!(repo.relative_path, "repo");
        assert_eq!(repo.root_fingerprint, remote::source_root_fingerprint(&request.roots[0], RemotePlatform::Windows));
        assert_eq!(repo.git.head.as_deref(), Some(fixture.head.as_str()));
        assert_eq!(repo.manifests.len(), 1);
        assert_eq!(repo.manifests[0].relative_path, "package.json");
        assert_eq!(repo.manifests[0].content.as_bytes(), fixture.approved);
        assert_eq!(repo.manifests[0].content_sha256, tune_core::db::sha256_hex(&fixture.approved));
        assert!(!bytes.windows(b"unapproved dirty synthetic fixture".len()).any(|value| value == b"unapproved dirty synthetic fixture"));
    }

    #[test]
    fn native_three_entries_use_real_git_and_process_one_frame_without_eof() {
        let _serial = SERIAL.lock().unwrap();
        let fixture = Fixture::new(MANIFEST.to_vec());
        let before = fixture.snapshot();
        assert!(!before.3.is_empty(), "fixture must actually be dirty");
        let (_, digest) = artifact();
        for entry in ENTRIES {
            let request = fixture.request(30_000);
            let (output, retained, body) = run(&fixture, entry, &request, true, &digest);
            assert!(output.status.success(), "native role must exit zero: role={entry} code={:?}", output.status.code());
            assert!(output.stderr.is_empty());
            let capture = match entry {
                "topology-native-reader" => native_protocol::decode_processing_response(&output.stdout, &request, &body).unwrap(),
                "topology-supervise" => {
                    let mut wrong_identity = retained.identity.clone();
                    wrong_identity.created_filetime = "1".into();
                    assert!(native_protocol::decode_outer_response(&output.stdout, &request, &body, &digest, &wrong_identity).is_err());
                    native_protocol::decode_outer_response(&output.stdout, &request, &body, &digest, &retained.identity).unwrap()
                }
                "topology-verify" => &output.stdout,
                _ => unreachable!(),
            };
            assert_capture(capture, &request, &fixture);
            assert!(fixture.snapshot() == before, "read-only native roles must preserve HEAD, approved bytes and dirty unapproved content");
            fixture.assert_private_state_absent();
        }
    }

    #[test]
    fn native_entries_reject_wrong_helper_hash_source_drift_and_invalid_utf8() {
        let _serial = SERIAL.lock().unwrap();
        let fixture = Fixture::new(MANIFEST.to_vec());
        let (_, digest) = artifact();
        for entry in ["topology-supervise", "topology-verify"] {
            let request = fixture.request(30_000);
            let before = fixture.snapshot();
            let (output, _, _) = run(&fixture, entry, &request, false, &"0".repeat(64));
            assert!(!output.status.success());
            assert!(output.stdout.is_empty() && output.stderr.is_empty());
            assert!(fixture.snapshot() == before);
            fixture.assert_private_state_absent();
        }
        std::fs::write(fixture.repo.join("package.json"), b"{\"name\":\"unapproved-drift\"}").unwrap();
        let before = fixture.snapshot();
        for entry in ENTRIES {
            let request = fixture.request(30_000);
            let (output, _, _) = run(&fixture, entry, &request, false, &digest);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty() && output.stderr.is_empty());
            assert!(fixture.snapshot() == before);
            fixture.assert_private_state_absent();
        }
        let invalid = Fixture::new(vec![0xff, 0xfe]);
        let before = invalid.snapshot();
        for entry in ENTRIES {
            // The invalid UTF-8 buffer itself is approved by exact length/hash:
            // rejection must occur after authenticating bytes, before decoding.
            let request = invalid.request(30_000);
            let (output, _, _) = run(&invalid, entry, &request, false, &digest);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty() && output.stderr.is_empty());
            assert!(invalid.snapshot() == before);
            invalid.assert_private_state_absent();
        }
    }

    #[allow(unsafe_code)]
    fn wait_for_response_prefix(child: &mut Child, deadline: Instant) -> [u8; 16] {
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;
        loop {
            let mut available = 0;
            // SAFETY: borrowed owned stdout pipe; zero buffer performs only a
            // bounded availability query. This test is its sole reader.
            let ok = unsafe {
                PeekNamedPipe(
                    child.stdout.as_ref().unwrap().as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0, "actual response prefix required before blocked-output test");
            if available >= 16 {
                let mut prefix = [0; 16];
                child.stdout.as_mut().unwrap().read_exact(&mut prefix).unwrap();
                return prefix;
            }
            assert!(child.try_wait().unwrap().is_none(), "reader failed before producing the real response");
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("response prefix exceeded original test deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn native_unread_large_response_self_exits_at_hard_deadline() {
        let _serial = SERIAL.lock().unwrap();
        let approved = format!("{{\"name\":\"large-native-fixture\",\"padding\":\"{}\"}}", "x".repeat(2 * 1024 * 1024)).into_bytes();
        let fixture = Fixture::new(approved);
        let request = fixture.request(30_000);
        let before = fixture.snapshot();
        let (_, digest) = artifact();
        let started = Instant::now();
        let mut child = spawn_entry(&fixture, "topology-native-reader", "30000", &digest);
        child.stdin.as_mut().unwrap().write_all(&reader::encode_request(&request).unwrap()).unwrap();
        let prefix = wait_for_response_prefix(&mut child, started + Duration::from_secs(30));
        let body_bytes = u32::from_le_bytes(prefix[12..16].try_into().unwrap());
        assert!(body_bytes > 1024 * 1024, "actual processing response must exceed pipe capacity");
        // Leave the response and stdin unread/open after this proven prefix.
        // A successful flush notification or early setup rejection cannot pass.
        let status = finite(&mut child, Duration::from_secs(35).saturating_sub(started.elapsed()));
        assert_eq!(status.code(), Some(124));
        assert!(started.elapsed() < Duration::from_secs(35));
        assert!(child.wait_with_output().unwrap().stderr.is_empty());
        assert!(fixture.snapshot() == before);
        fixture.assert_private_state_absent();
    }
}
