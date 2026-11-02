use std::cell::Cell;
use std::rc::Rc;

use fleet_studio::navigation::{
    OpenResult,
    macos_policy::{DirectoryFs, FsError, Workspace, open_directory, open_github},
};

#[derive(Default)]
struct FakeFs {
    remote: Option<String>,
    inaccessible: Option<String>,
    non_directory_or_link: Option<String>,
    changed_identity: bool,
    identity_unknown: bool,
    changed_ancestor: Option<String>,
    unknown_ancestor: Option<String>,
    revoke_after_final: Option<Rc<Cell<bool>>>,
    visited: Vec<String>,
    compared: Vec<String>,
}

impl DirectoryFs for FakeFs {
    type Directory = String;

    fn root(&mut self) -> Result<Self::Directory, FsError> {
        self.visited.push("/".into());
        Ok("/".into())
    }

    fn child(&mut self, parent: &Self::Directory, name: &str) -> Result<Self::Directory, FsError> {
        let path = if parent == "/" { format!("/{name}") } else { format!("{parent}/{name}") };
        self.visited.push(path.clone());
        if self.non_directory_or_link.as_deref() == Some(&path) { Err(FsError::Failed) } else { Ok(path) }
    }

    fn local(&mut self, directory: &Self::Directory) -> Result<bool, FsError> {
        if self.inaccessible.as_deref() == Some(directory) { Err(FsError::Failed) } else { Ok(self.remote.as_deref() != Some(directory)) }
    }

    fn same(&mut self, first: &Self::Directory, second: &Self::Directory, _: &impl Fn() -> bool) -> Result<bool, FsError> {
        self.compared.push(first.clone());
        if first.ends_with("/child") {
            if let Some(allowed) = &self.revoke_after_final {
                allowed.set(false);
            }
        }
        if self.unknown_ancestor.as_deref() == Some(first) || (self.identity_unknown && first.ends_with("/child")) {
            Err(FsError::Failed)
        } else {
            Ok(self.changed_ancestor.as_deref() != Some(first) && !(self.changed_identity && first.ends_with("/child")) && first == second)
        }
    }
}

#[derive(Default)]
struct FakeWorkspace {
    accepted: bool,
    opened: Vec<(bool, String)>,
}

impl Workspace for FakeWorkspace {
    fn github(&mut self, url: &str, authorized: &impl Fn() -> bool) -> OpenResult {
        fleet_studio::navigation::finish_if_authorized(authorized, || {
            self.opened.push((true, url.into()));
            self.accepted
        })
    }

    fn directory(&mut self, path: &str, authorized: &impl Fn() -> bool) -> OpenResult {
        fleet_studio::navigation::finish_if_authorized(authorized, || {
            self.opened.push((false, path.into()));
            self.accepted
        })
    }
}

#[test]
fn macos_directory_validates_each_ancestor_and_preserves_special_characters() {
    let mut fs = FakeFs::default();
    let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
    let path = "/fixture/日本語 dir & 100% #?";
    assert_eq!(open_directory(path, &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Accepted);
    assert_eq!(fs.visited, ["/", "/fixture", path, "/", "/fixture", path]);
    assert_eq!(workspace.opened, [(false, path.to_owned())]);
}

#[test]
fn macos_directory_rejects_changed_or_unknown_final_identity() {
    for mut fs in [FakeFs { changed_identity: true, ..Default::default() }, FakeFs { identity_unknown: true, ..Default::default() }] {
        let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
        assert_eq!(open_directory("/fixture/child", &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Failed);
        assert!(workspace.opened.is_empty());
    }
}

#[test]
fn macos_directory_rejects_changed_or_unknown_ancestor_identity() {
    for mut fs in
        [FakeFs { changed_ancestor: Some("/fixture".into()), ..Default::default() }, FakeFs { unknown_ancestor: Some("/fixture".into()), ..Default::default() }]
    {
        let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
        assert_eq!(open_directory("/fixture/child", &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Failed);
        assert_eq!(fs.compared, ["/", "/fixture"]);
        assert!(workspace.opened.is_empty());
    }
}

#[test]
fn macos_directory_rejects_nonlocal_unknown_and_linked_ancestors() {
    for mut fs in [
        FakeFs { remote: Some("/fixture".into()), ..Default::default() },
        FakeFs { inaccessible: Some("/fixture".into()), ..Default::default() },
        FakeFs { non_directory_or_link: Some("/fixture".into()), ..Default::default() },
    ] {
        let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
        assert_eq!(open_directory("/fixture/child", &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Failed);
        assert!(workspace.opened.is_empty());
        assert!(!fs.visited.iter().any(|path| path == "/fixture/child"));
    }
}

#[test]
fn macos_directory_rejects_nonlocal_or_unknown_root_without_descending() {
    for mut fs in [FakeFs { remote: Some("/".into()), ..Default::default() }, FakeFs { inaccessible: Some("/".into()), ..Default::default() }] {
        let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
        assert_eq!(open_directory("/fixture/child", &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Failed);
        assert_eq!(fs.visited, ["/"]);
        assert!(workspace.opened.is_empty());
    }
}

#[test]
fn macos_directory_os_failure_is_not_reported_as_acceptance() {
    let mut fs = FakeFs::default();
    let mut workspace = FakeWorkspace::default();
    assert_eq!(open_directory("/fixture/child", &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Failed);
    assert_eq!(workspace.opened.len(), 1);
}

#[test]
fn macos_directory_keeps_both_ancestor_chains_open_during_handoff() {
    use std::rc::Rc;

    struct Handle(Rc<Cell<usize>>);
    impl Drop for Handle {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }
    struct Fs(Rc<Cell<usize>>);
    impl Fs {
        fn next(&self) -> Handle {
            self.0.set(self.0.get() + 1);
            Handle(self.0.clone())
        }
    }
    impl DirectoryFs for Fs {
        type Directory = Handle;
        fn root(&mut self) -> Result<Handle, FsError> {
            Ok(self.next())
        }
        fn child(&mut self, _: &Handle, _: &str) -> Result<Handle, FsError> {
            Ok(self.next())
        }
        fn local(&mut self, _: &Handle) -> Result<bool, FsError> {
            Ok(true)
        }
        fn same(&mut self, _: &Handle, _: &Handle, _: &impl Fn() -> bool) -> Result<bool, FsError> {
            Ok(true)
        }
    }
    struct Receiver(Rc<Cell<usize>>);
    impl Workspace for Receiver {
        fn github(&mut self, _: &str, _: &impl Fn() -> bool) -> OpenResult {
            unreachable!()
        }
        fn directory(&mut self, _: &str, authorized: &impl Fn() -> bool) -> OpenResult {
            fleet_studio::navigation::finish_if_authorized(authorized, || self.0.get() == 6)
        }
    }
    let live = Rc::new(Cell::new(0));
    assert_eq!(open_directory("/fixture/child", &mut Fs(live.clone()), &mut Receiver(live.clone()), &|| true, &|| true), OpenResult::Accepted);
    assert_eq!(live.get(), 0);
}

#[test]
fn macos_directory_rejects_unsafe_syntax_before_any_lookup() {
    for path in ["", "relative/path", "//remote/share", "/fixture/../elsewhere", "/fixture/./child", "/fixture/line\nbreak"] {
        let mut fs = FakeFs::default();
        let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
        assert_eq!(open_directory(path, &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Failed);
        assert!(fs.visited.is_empty());
        assert!(workspace.opened.is_empty());
    }
}

#[test]
fn macos_directory_rechecks_revoked_permission_before_dispatch() {
    let allowed = Rc::new(Cell::new(true));
    let mut fs = FakeFs { revoke_after_final: Some(allowed.clone()), ..Default::default() };
    let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
    assert_eq!(open_directory("/fixture/child", &mut fs, &mut workspace, &|| true, &|| allowed.get()), OpenResult::SelectionChanged);
    assert_eq!(fs.compared, ["/", "/fixture", "/fixture/child"]);
    assert!(workspace.opened.is_empty());
}

#[test]
fn macos_github_refuses_invalid_urls_and_reports_os_acceptance_only() {
    let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
    for url in ["http://github.com/a/b", "https://example.invalid/a/b", "https://github.com/a/b?token=x", "https://github.com/a/b#fragment"] {
        assert_eq!(open_github(url, &mut workspace, &|| true), OpenResult::Failed);
    }
    assert!(workspace.opened.is_empty());
    assert_eq!(open_github("https://github.com/fixture/repo", &mut workspace, &|| true), OpenResult::Accepted);
    assert_eq!(workspace.opened, [(true, "https://github.com/fixture/repo".to_owned())]);
    workspace.accepted = false;
    assert_eq!(open_github("https://github.com/fixture/repo", &mut workspace, &|| true), OpenResult::Failed);
}

#[test]
fn macos_github_rechecks_permission_at_handoff() {
    let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
    let calls = Cell::new(0);
    let authorized = || {
        let count = calls.get();
        calls.set(count + 1);
        count == 0
    };
    assert_eq!(open_github("https://github.com/fixture/repo", &mut workspace, &authorized), OpenResult::SelectionChanged);
    assert!(workspace.opened.is_empty());
}

#[test]
fn macos_cancellation_during_validation_stops_subsequent_filesystem_calls() {
    use std::rc::Rc;

    struct CancelFs {
        allowed: Rc<Cell<bool>>,
        calls: usize,
        cancel_at: usize,
    }
    impl CancelFs {
        fn record(&mut self) {
            self.calls += 1;
            if self.calls == self.cancel_at {
                self.allowed.set(false);
            }
        }
    }
    impl DirectoryFs for CancelFs {
        type Directory = ();
        fn root(&mut self) -> Result<(), FsError> {
            self.record();
            Ok(())
        }
        fn child(&mut self, _: &(), _: &str) -> Result<(), FsError> {
            self.record();
            Ok(())
        }
        fn local(&mut self, _: &()) -> Result<bool, FsError> {
            self.record();
            Ok(true)
        }
        fn same(&mut self, _: &(), _: &(), authorized: &impl Fn() -> bool) -> Result<bool, FsError> {
            self.record();
            if !authorized() {
                return Err(FsError::Revoked);
            }
            self.record();
            Ok(true)
        }
    }

    for cancel_at in 1..=18 {
        let allowed = Rc::new(Cell::new(true));
        let mut fs = CancelFs { allowed: allowed.clone(), calls: 0, cancel_at };
        let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
        let result = open_directory("/fixture/child", &mut fs, &mut workspace, &|| true, &|| allowed.get());
        assert_eq!(result, OpenResult::SelectionChanged);
        assert_eq!(fs.calls, cancel_at);
        assert!(workspace.opened.is_empty());
    }
}

#[test]
fn macos_directory_limits_components_before_filesystem_access() {
    let mut fs = FakeFs::default();
    let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
    let maximum = format!("/{}", vec!["x"; 48].join("/"));
    assert_eq!(open_directory(&maximum, &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Accepted);

    let mut fs = FakeFs::default();
    let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
    let too_deep = format!("/{}", vec!["x"; 49].join("/"));
    assert_eq!(open_directory(&too_deep, &mut fs, &mut workspace, &|| true, &|| true), OpenResult::Failed);
    assert!(fs.visited.is_empty());
    assert!(workspace.opened.is_empty());
}

#[test]
fn macos_directory_uses_full_authorization_only_at_start_and_handoff() {
    let mut fs = FakeFs::default();
    let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
    let full_checks = Cell::new(0);
    let authorized = || {
        full_checks.set(full_checks.get() + 1);
        true
    };
    assert_eq!(open_directory("/fixture/child", &mut fs, &mut workspace, &authorized, &|| true), OpenResult::Accepted);
    assert_eq!(full_checks.get(), 2);
}

#[test]
fn macos_directory_rechecks_full_authorization_after_validation() {
    let mut fs = FakeFs::default();
    let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
    let full_checks = Cell::new(0);
    let authorized = || {
        let count = full_checks.get();
        full_checks.set(count + 1);
        count == 0
    };
    assert_eq!(open_directory("/fixture/child", &mut fs, &mut workspace, &authorized, &|| true), OpenResult::SelectionChanged);
    assert_eq!(full_checks.get(), 2);
    assert!(workspace.opened.is_empty());
}

#[test]
fn macos_revocation_takes_precedence_when_filesystem_call_also_fails() {
    struct FailFs {
        allowed: Rc<Cell<bool>>,
        calls: usize,
        fail_at: usize,
    }
    impl FailFs {
        fn record(&mut self) -> Result<(), FsError> {
            self.calls += 1;
            if self.calls == self.fail_at {
                self.allowed.set(false);
                Err(FsError::Failed)
            } else {
                Ok(())
            }
        }
    }
    impl DirectoryFs for FailFs {
        type Directory = ();
        fn root(&mut self) -> Result<(), FsError> {
            self.record()
        }
        fn child(&mut self, _: &(), _: &str) -> Result<(), FsError> {
            self.record()
        }
        fn local(&mut self, _: &()) -> Result<bool, FsError> {
            self.record()?;
            Ok(true)
        }
        fn same(&mut self, _: &(), _: &(), current: &impl Fn() -> bool) -> Result<bool, FsError> {
            self.record()?;
            if !current() {
                return Err(FsError::Revoked);
            }
            self.record()?;
            Ok(true)
        }
    }
    for fail_at in 1..=18 {
        let allowed = Rc::new(Cell::new(true));
        let mut fs = FailFs { allowed: allowed.clone(), calls: 0, fail_at };
        let mut workspace = FakeWorkspace { accepted: true, ..Default::default() };
        let result = open_directory("/fixture/child", &mut fs, &mut workspace, &|| true, &|| allowed.get());
        assert_eq!(result, OpenResult::SelectionChanged);
        assert_eq!(fs.calls, fail_at);
        assert!(workspace.opened.is_empty());
    }
}

#[test]
fn macos_home_rejects_ambiguous_shorthand_and_keeps_absolute_backslashes() {
    use fleet_studio::navigation::macos_policy::MacHome;
    use std::path::PathBuf;

    let snapshot = MacHome::default();
    assert!(snapshot.resolve("~\\fixture", || PathBuf::from("/fixture/home")).is_none());
    let direct = snapshot.resolve("/fixture/a\\b", || panic!("absolute paths must not read home")).unwrap();
    assert_eq!(direct.to_str(), Some("/fixture/a\\b"));
}

#[test]
fn macos_home_rejects_relative_unknown_and_non_unicode_home() {
    use fleet_studio::navigation::macos_policy::MacHome;
    use std::path::PathBuf;

    assert!(MacHome::default().resolve("~", || PathBuf::from(".")).is_none());
    assert!(MacHome::default().resolve("~/fixture", || PathBuf::from("")).is_none());
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_wide(&[b'/' as u16, 0xd800]);
        assert!(MacHome::default().resolve("~", || PathBuf::from(invalid)).is_none());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_vec(vec![b'/', 0xff]);
        assert!(MacHome::default().resolve("~", || PathBuf::from(invalid)).is_none());
    }
}

#[test]
fn macos_home_rejects_rooted_and_parent_suffixes_before_home_read() {
    use fleet_studio::navigation::macos_policy::MacHome;

    for value in ["~//other", "~/\\other", "~/../other", "~/./other", "~/one//other"] {
        assert!(MacHome::default().resolve(value, || panic!("invalid shorthand must not read home")).is_none());
    }
}

#[test]
fn macos_home_tilde_and_tilde_slash_share_the_native_snapshot() {
    use fleet_studio::navigation::macos_policy::MacHome;
    use std::path::PathBuf;

    let snapshot = MacHome::default();
    let first = snapshot.resolve("~", || PathBuf::from("/fixture/home"));
    let second = snapshot.resolve("~/", || PathBuf::from("/fixture/home"));
    assert_eq!(first, Some(PathBuf::from("/fixture/home")));
    assert_eq!(second, first);
}

#[test]
fn macos_home_snapshot_keeps_the_first_native_path_for_display_and_open() {
    use fleet_studio::navigation::macos_policy::MacHome;
    use std::path::PathBuf;

    let snapshot = MacHome::default();
    let reads = Cell::new(0);
    let first = snapshot.resolve("~/project", || {
        reads.set(reads.get() + 1);
        PathBuf::from("/fixture/home-one")
    });
    let second = snapshot.resolve("~/project", || {
        reads.set(reads.get() + 1);
        PathBuf::from("/fixture/home-one")
    });
    assert_eq!(first, Some(PathBuf::from("/fixture/home-one").join("project")));
    assert_eq!(second, first);
    assert_eq!(reads.get(), 2);
}

#[test]
fn macos_home_snapshot_rejects_changed_or_missing_current_home() {
    use fleet_studio::navigation::macos_policy::MacHome;
    use std::path::PathBuf;

    let snapshot = MacHome::default();
    assert!(snapshot.resolve("~/project", || PathBuf::from("/fixture/first")).is_some());
    assert!(snapshot.resolve("~/project", || PathBuf::from("/fixture/second")).is_none());
    assert!(snapshot.resolve("~/project", PathBuf::new).is_none());
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_wide(&[b'/' as u16, 0xd800]);
        assert!(snapshot.resolve("~/project", || PathBuf::from(invalid)).is_none());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_vec(vec![b'/', 0xff]);
        assert!(snapshot.resolve("~/project", || PathBuf::from(invalid)).is_none());
    }
}

#[test]
fn macos_audit_snapshot_binds_display_click_and_keyboard_to_one_home() {
    use fleet_studio::audit::{self, Audit, AuditIdentity, RepoAudit};
    use fleet_studio::navigation::macos_policy::MacHome;
    use fleet_studio::scope::AuditGate;
    use std::path::PathBuf;

    let row = RepoAudit {
        node: "fixture-node".into(),
        path: "~/project".into(),
        branch: None,
        dirty: None,
        unpushed: None,
        behind: None,
        local_only: None,
        previous: false,
        local: true,
        at: None,
        error: None,
    };
    let audit = Audit {
        repos: vec![row.clone()],
        identity: Some(AuditIdentity { node: "fixture-node".into(), fingerprint: "fixture-fingerprint".into() }),
        ..Audit::default()
    };
    let gate = AuditGate::default();
    let ticket = gate.ticket();
    let home = MacHome::default();
    let reads = Cell::new(0);
    let resolve = |path: &str, value: &str| {
        home.resolve(path, || {
            reads.set(reads.get() + 1);
            PathBuf::from(value)
        })
    };
    let display = audit::prepare_open_with_target(false, true, &ticket, Some(&audit), Some(&row), |path| resolve(path, "/fixture/first")).unwrap().0;
    let click = audit::prepare_open_with_target(false, true, &ticket, Some(&audit), Some(&row), |path| resolve(path, "/fixture/first")).unwrap().0;
    let keyboard = audit::prepare_open_with_target(false, true, &ticket, Some(&audit), Some(&row), |path| resolve(path, "/fixture/first")).unwrap().0;
    assert_eq!(display, PathBuf::from("/fixture/first").join("project"));
    assert_eq!(click, display);
    assert_eq!(keyboard, display);
    assert_eq!(reads.get(), 3);
    assert!(matches!(
        audit::prepare_open_with_target(false, true, &ticket, Some(&audit), Some(&row), |path| resolve(path, "/fixture/changed")),
        Err(OpenResult::Failed)
    ));

    let mut changed = row.clone();
    changed.path = "~/other".into();
    assert!(matches!(
        audit::prepare_open_with_target(false, true, &ticket, Some(&audit), Some(&changed), |_| -> Option<PathBuf> { panic!("stale row") }),
        Err(OpenResult::SelectionChanged)
    ));
    gate.close();
    assert!(matches!(
        audit::prepare_open_with_target(false, true, &ticket, Some(&audit), Some(&row), |_| -> Option<PathBuf> { panic!("stale ticket") }),
        Err(OpenResult::SelectionChanged)
    ));
}

#[test]
fn macos_sample_audit_never_reads_or_resolves_home() {
    use fleet_studio::audit::{self, RepoAudit};
    use fleet_studio::scope::AuditGate;
    use std::path::PathBuf;

    let row = RepoAudit {
        node: "fixture-node".into(),
        path: "~/project".into(),
        branch: None,
        dirty: None,
        unpushed: None,
        behind: None,
        local_only: None,
        previous: false,
        local: true,
        at: None,
        error: None,
    };
    let ticket = AuditGate::default().ticket();
    assert!(matches!(
        audit::prepare_open_with_target(true, true, &ticket, None, Some(&row), |_| -> Option<PathBuf> { panic!("sample must not resolve home") }),
        Err(OpenResult::Sample)
    ));
}
