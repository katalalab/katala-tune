use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tune_core::topology::{
    TopologyLimits,
    adapters::{
        native_reader::{self as native, GitCommand, GitOperation, GitRunner, NativeReaderError as Error, SourceIo},
        remote::{RemotePlatform, RemoteRequest, RemoteRuntimeKind, RemoteSourceCheckout, RemoteSourceManifest},
    },
};

const CONTENT: &[u8] = b"{\"name\":\"approved-fixture\"}";
fn request() -> RemoteRequest {
    RemoteRequest {
        scope_id: "fixture:native".into(),
        alias: String::new(),
        expected_identity_hash: Some("a".repeat(64)),
        platform: RemotePlatform::Windows,
        expected_runtime: RemoteRuntimeKind::Native,
        expected_distro: None,
        cursor: None,
        roots: vec!["C:/fixture".into()],
        configs: vec![],
        source_allowlist: Some(vec![RemoteSourceCheckout {
            root_index: 0,
            relative_path: ".".into(),
            expected_head: "b".repeat(40),
            manifests: vec![RemoteSourceManifest { relative_path: "package.json".into(), bytes: CONTENT.len(), sha256: tune_core::db::sha256_hex(CONTENT) }],
        }]),
        python_program: String::new(),
        windows_reader_program: None,
        git_program: Some("C:/tools/git.exe".into()),
        observed_at_ms: 1000,
        limits: TopologyLimits::default(),
    }
}
struct Source {
    calls: RefCell<Vec<String>>,
    files: BTreeMap<String, Vec<u8>>,
}
impl Source {
    fn new() -> Self {
        Self { calls: RefCell::new(vec![]), files: BTreeMap::from([("package.json".into(), CONTENT.to_vec())]) }
    }
}
impl SourceIo for Source {
    fn identity_hash(&self) -> Result<String, Error> {
        self.calls.borrow_mut().push("identity".into());
        Ok("a".repeat(64))
    }
    fn system_directories(&self) -> Result<(String, String), Error> {
        self.calls.borrow_mut().push("system".into());
        Ok(("C:/Windows".into(), "C:/Windows/System32".into()))
    }
    fn verify_checkout(&self, _: &str, checkout: &str, _: Instant) -> Result<(), Error> {
        self.calls.borrow_mut().push(format!("checkout:{checkout}"));
        Ok(())
    }
    fn read_approved(&self, _: &str, path: &str, maximum: usize, _: Instant) -> Result<Vec<u8>, Error> {
        self.calls.borrow_mut().push(format!("file:{path}"));
        let data = self.files.get(path).ok_or(Error::Source)?.clone();
        assert!(maximum <= request().limits.file_bytes);
        Ok(data)
    }
}
struct Git {
    calls: RefCell<Vec<GitOperation>>,
    drift: bool,
    oversized: bool,
    directory: String,
}
impl Git {
    fn new() -> Self {
        Self { calls: RefCell::new(vec![]), drift: false, oversized: false, directory: "C:/fixture".into() }
    }
}
impl GitRunner for Git {
    fn run(&self, command: &GitCommand) -> Result<Vec<u8>, Error> {
        assert_eq!(command.program(), "C:/tools/git.exe");
        assert_eq!(command.directory(), self.directory);
        assert!(command.deadline() > Instant::now());
        assert!(command.stdout_limit() <= 8192);
        let env = command.environment().iter().map(|(key, value)| (key.to_str().unwrap(), value.to_str().unwrap())).collect::<BTreeMap<_, _>>();
        assert_eq!(env.len(), 9);
        assert_eq!(env["SystemRoot"], "C:/Windows");
        assert_eq!(env["PATH"], "C:/Windows/System32");
        assert_eq!(env["GIT_CONFIG_GLOBAL"], "NUL");
        assert_eq!(env["GIT_TERMINAL_PROMPT"], "0");
        assert!(!env.contains_key("HOME"));
        let args = command.arguments();
        assert_eq!(&args[..5], &["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=NUL"]);
        let mut calls = self.calls.borrow_mut();
        calls.push(command.operation());
        if self.oversized {
            return Ok(vec![b'x'; command.stdout_limit() + 1]);
        }
        Ok(match command.operation() {
            GitOperation::GitDir => b"C:/fixture/.git\n".to_vec(),
            GitOperation::CommonDir => b"C:/common/.git\n".to_vec(),
            GitOperation::Origin => b"https://example.org/team/fixture.git\n".to_vec(),
            GitOperation::Head => {
                if self.drift && calls.len() == 5 {
                    "c".repeat(40).into_bytes()
                } else {
                    "b".repeat(40).into_bytes()
                }
            }
        })
    }
}
fn run(request: &RemoteRequest, source: &Source, git: &Git) -> Result<tune_core::topology::RawCapture, Error> {
    native::collect_with(request, Instant::now() + Duration::from_secs(10), source, git)
}
#[test]
fn approved_atomic_capture_reads_only_selected_bytes_and_uses_five_fixed_git_operations() {
    let source = Source::new();
    let git = Git::new();
    let capture = run(&request(), &source, &git).unwrap();
    assert_eq!(*git.calls.borrow(), [GitOperation::GitDir, GitOperation::CommonDir, GitOperation::Origin, GitOperation::Head, GitOperation::Head]);
    assert_eq!(source.calls.borrow().iter().filter(|call| call.starts_with("file:")).cloned().collect::<Vec<_>>(), ["file:package.json"]);
    assert_eq!(capture.repositories[0].manifests[0].content.as_bytes(), CONTENT);
    assert!(capture.repositories[0].git.is_worktree);
    assert_eq!(capture.repositories[0].git.git_dir, Some(tune_core::db::sha256_hex(b"C:/fixture/.git")));
    assert_eq!(capture.counters.children, 5);
    assert!(capture.progress.is_none());
    assert!(capture.failures.is_empty());
}
#[test]
fn invalid_selection_and_caps_fail_before_any_io() {
    for mode in 0..17 {
        let mut request = request();
        match mode {
            0 => request.cursor = Some(json!({"version":1})),
            1 => request.configs.push(json!({})),
            2 => request.source_allowlist = None,
            3 => request.source_allowlist.as_mut().unwrap().clear(),
            4 => request.source_allowlist.as_mut().unwrap()[0].manifests[0].relative_path = "../package.json".into(),
            5 => request.roots[0] = "C:/fixture/../other".into(),
            6 => {
                let duplicate = request.source_allowlist.as_ref().unwrap()[0].clone();
                request.source_allowlist.as_mut().unwrap().push(duplicate);
            }
            7 => request.git_program = Some("git.exe".into()),
            8 => request.limits.file_bytes = usize::MAX,
            9 => request.limits.repositories_per_environment = 0,
            10 => request.limits.total_bytes_per_environment = CONTENT.len() - 1,
            11 => request.limits.entries_per_environment = 0,
            12 => request.source_allowlist.as_mut().unwrap()[0].manifests[0].relative_path = "package.json:stream".into(),
            13 => request.source_allowlist.as_mut().unwrap()[0].manifests[0].relative_path = "CON/package.json".into(),
            14 => request.expected_identity_hash = None,
            15 => request.platform = RemotePlatform::Posix,
            _ => {
                request.limits.depth = 1;
                request.source_allowlist.as_mut().unwrap()[0].manifests[0].relative_path = "deep/nested/package.json".into();
            }
        }
        let source = Source::new();
        let git = Git::new();
        assert!(run(&request, &source, &git).is_err(), "case {mode}");
        assert!(source.calls.borrow().is_empty(), "case {mode}");
        assert!(git.calls.borrow().is_empty(), "case {mode}");
    }
}
#[test]
fn nested_approved_checkout_and_unicode_file_content_are_preserved() {
    let mut request = request();
    let path = "package.json";
    let content = "{\"name\":\"fixture-\u{1f680}-e\u{301}\"}".as_bytes();
    let checkout = &mut request.source_allowlist.as_mut().unwrap()[0];
    checkout.relative_path = "nested".into();
    checkout.manifests[0] = RemoteSourceManifest { relative_path: path.into(), bytes: content.len(), sha256: tune_core::db::sha256_hex(content) };
    let mut source = Source::new();
    source.files = BTreeMap::from([("nested/package.json".into(), content.to_vec())]);
    let mut git = Git::new();
    git.directory = "C:/fixture/nested".into();
    let capture = run(&request, &source, &git).unwrap();
    assert_eq!(capture.repositories[0].relative_path, "nested");
    assert_eq!(capture.repositories[0].manifests[0].relative_path, "package.json");
    assert_eq!(capture.repositories[0].manifests[0].content.as_bytes(), content);
}

#[test]
fn all_checkout_heads_are_rechecked_after_all_sources_and_same_named_repo_files_are_allowed() {
    struct MultiGit {
        calls: RefCell<Vec<(String, GitOperation)>>,
        drift: bool,
    }
    impl GitRunner for MultiGit {
        fn run(&self, command: &GitCommand) -> Result<Vec<u8>, Error> {
            let mut calls = self.calls.borrow_mut();
            calls.push((command.directory().into(), command.operation()));
            Ok(match command.operation() {
                GitOperation::GitDir | GitOperation::CommonDir => format!("{}/.git", command.directory()).into_bytes(),
                GitOperation::Origin => vec![],
                GitOperation::Head => {
                    if self.drift && calls.len() > 8 && command.directory().ends_with("/first") {
                        "c".repeat(40).into_bytes()
                    } else {
                        "b".repeat(40).into_bytes()
                    }
                }
            })
        }
    }
    let mut request = request();
    let mut first = request.source_allowlist.take().unwrap().remove(0);
    first.relative_path = "first".into();
    let mut second = first.clone();
    second.relative_path = "second".into();
    request.source_allowlist = Some(vec![first, second]);
    let mut source = Source::new();
    source.files = BTreeMap::from([("first/package.json".into(), CONTENT.to_vec()), ("second/package.json".into(), CONTENT.to_vec())]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let git = MultiGit { calls: RefCell::new(vec![]), drift: false };
    let capture = native::collect_with(&request, deadline, &source, &git).unwrap();
    assert_eq!(capture.repositories.len(), 2);
    assert_eq!(capture.counters.children, 10);
    assert_eq!(&git.calls.borrow()[8..], &[("C:/fixture/first".into(), GitOperation::Head), ("C:/fixture/second".into(), GitOperation::Head)]);
    let git = MultiGit { calls: RefCell::new(vec![]), drift: true };
    assert_eq!(native::collect_with(&request, deadline, &source, &git).err(), Some(Error::Source));
}
#[test]
fn raw_length_and_sha_are_checked_before_strict_utf8() {
    let mut source = Source::new();
    source.files.insert("package.json".into(), vec![0xff]);
    let mut request = request();
    assert_eq!(run(&request, &source, &Git::new()).err(), Some(Error::Source));
    request.source_allowlist.as_mut().unwrap()[0].manifests[0].bytes = 1;
    assert_eq!(run(&request, &source, &Git::new()).err(), Some(Error::Source));
    request.source_allowlist.as_mut().unwrap()[0].manifests[0].sha256 = tune_core::db::sha256_hex(&[0xff]);
    assert_eq!(run(&request, &source, &Git::new()).err(), Some(Error::Utf8));
}
#[test]
fn final_head_drift_and_oversized_git_suppress_the_entire_capture() {
    let mut git = Git::new();
    git.drift = true;
    assert_eq!(run(&request(), &Source::new(), &git).err(), Some(Error::Source));
    let mut git = Git::new();
    git.oversized = true;
    assert_eq!(run(&request(), &Source::new(), &git).err(), Some(Error::Limit));
}
#[test]
fn expired_absolute_budget_performs_no_io_and_backend_remains_unsupported() {
    let source = Source::new();
    let git = Git::new();
    assert_eq!(native::collect_with(&request(), Instant::now(), &source, &git).err(), Some(Error::Timeout));
    assert!(source.calls.borrow().is_empty());
    assert_eq!(
        native::collect_with(&request(), Instant::now() + Duration::from_secs(10), &source, &native::UnsupportedGitRunner).err(),
        Some(Error::Unsupported)
    );
}

#[test]
fn selection_collision_workflow_and_output_caps_are_admitted_before_io() {
    for mode in 0..5 {
        let mut request = request();
        match mode {
            0 => {
                let files = &mut request.source_allowlist.as_mut().unwrap()[0].manifests;
                files[0].relative_path = "nested/package.json".into();
                let mut duplicate = files[0].clone();
                duplicate.relative_path = "Nested/package.json".into();
                files.push(duplicate);
            }
            1 => {
                request.source_allowlist.as_mut().unwrap()[0].manifests[0].relative_path = ".github/workflows/fixture.yml".into();
                request.limits.workflow_bytes = CONTENT.len() - 1;
            }
            2 => {
                request.limits.stdout_bytes = 2048;
                request.source_allowlist.as_mut().unwrap()[0].manifests[0].bytes = 2049;
            }
            3 => {
                request.limits.entries_per_environment = 1;
            }
            _ => {
                request.source_allowlist.as_mut().unwrap()[0].manifests[0].relative_path = "checkout./package.json".into();
            }
        }
        let source = Source::new();
        let git = Git::new();
        assert!(run(&request, &source, &git).is_err(), "case {mode}");
        assert!(source.calls.borrow().is_empty(), "case {mode}");
        assert!(git.calls.borrow().is_empty(), "case {mode}");
    }
}

#[test]
fn native_atomic_checkout_projects_complete_without_settings_or_database() {
    let capture = run(&request(), &Source::new(), &Git::new()).unwrap();
    let snapshots = tune_core::topology::adapters::assemble_capture(&capture);
    assert!(snapshots.iter().any(|snapshot| snapshot.scope_id.contains(":checkout:") && snapshot.status == tune_core::topology::ScopeStatus::Complete));
    assert!(capture.repositories.iter().all(|repo| repo.configs.is_empty() && !repo.config_file_read));
}

#[cfg(windows)]
mod windows_files {
    use super::*;
    use native::windows::WindowsSourceIo;
    use std::{
        fs,
        os::windows::fs::{symlink_dir, symlink_file},
    };
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "native-reader-fixture-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            fs::create_dir_all(root.join("checkout/.git")).unwrap();
            fs::write(root.join("checkout/package.json"), CONTENT).unwrap();
            Self(root)
        }
        fn path(&self) -> String {
            self.0.to_str().unwrap().replace('\\', "/")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn regular_read_uses_held_objects_and_rejects_an_existing_writer() {
        let fixture = Fixture::new();
        let io = WindowsSourceIo::default();
        let end = Instant::now() + Duration::from_secs(10);
        io.verify_checkout(&fixture.path(), "checkout", end).unwrap();
        assert_eq!(io.read_approved(&fixture.path(), "checkout/package.json", CONTENT.len(), end).unwrap(), CONTENT);
        assert!(io.read_approved(&fixture.path(), "checkout/package.json", CONTENT.len() - 1, end).is_err());
        let _writer = fs::OpenOptions::new().write(true).open(fixture.0.join("checkout/package.json")).unwrap();
        assert_eq!(io.read_approved(&fixture.path(), "checkout/package.json", CONTENT.len(), end).err(), Some(Error::Source));
    }
    #[test]
    fn reparse_leaf_checkout_and_root_ancestor_are_rejected() {
        let fixture = Fixture::new();
        let io = WindowsSourceIo::default();
        let end = Instant::now() + Duration::from_secs(10);
        // Native acceptance must provide Windows symlink privilege; failure to
        // create the fixture is a failed gate, never a passing skipped assertion.
        symlink_file(fixture.0.join("checkout/package.json"), fixture.0.join("leaf.json")).unwrap();
        assert_eq!(io.read_approved(&fixture.path(), "leaf.json", CONTENT.len(), end).err(), Some(Error::Source));
        symlink_dir(fixture.0.join("checkout"), fixture.0.join("linked")).unwrap();
        assert_eq!(io.verify_checkout(&fixture.path(), "linked", end).err(), Some(Error::Source));
        let linked_root = format!("{}/linked", fixture.path());
        assert_eq!(io.read_approved(&linked_root, "package.json", CONTENT.len(), end).err(), Some(Error::Source));
    }
}
