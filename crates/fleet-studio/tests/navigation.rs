use fleet_studio::navigation::repository_url;

#[test]
fn replacement_invalidates_selection_before_a_changed_list_can_open() {
    let mut selected = fleet_studio::navigation::Selection::default();
    selected.next(3);
    selected.next(3);
    selected.next(3);
    assert_eq!(selected.index(), Some(2));
    selected.invalidate();
    assert_eq!(selected.index(), None);
    selected.next(4);
    assert_eq!(selected.index(), Some(0));
}

#[test]
fn github_ssh_origin_can_be_opened_in_the_browser() {
    assert_eq!(repository_url(concat!("git", "@", "github.com:fixture-owner/fixture-repo.git")), Some("https://github.com/fixture-owner/fixture-repo".into()));
}

#[test]
fn normalized_topology_origin_can_be_opened_without_treating_paths_as_urls() {
    let origin = tune_core::topology::normalize_git_origin(concat!("git", "@", "github.com:fixture-owner/fixture-repo.git")).unwrap();
    assert_eq!(repository_url(&origin), Some("https://github.com/fixture-owner/fixture-repo".into()));
    assert_eq!(repository_url("/private/local/path"), None);
}

#[test]
fn browser_links_never_include_credentials_or_other_protocols() {
    assert_eq!(repository_url(concat!("https://user:password", "@", "github.com/fixture/repo?token=secret")), Some("https://github.com/fixture/repo".into()));
    assert_eq!(repository_url("https://example.org/fixture/repo"), None);
    assert_eq!(repository_url("file:///private/data"), None);
    assert_eq!(repository_url(concat!("https://github.com", "@", "evil.invalid/path")), None);
}

#[test]
fn local_home_shorthand_can_open_an_existing_directory() {
    let url = fleet_studio::navigation::local_directory("~").expect("home directory is available");
    assert!(url.starts_with("file:"));
    let path = url::Url::parse(&url).unwrap().to_file_path().expect("URL is a local path");
    assert_eq!(std::fs::canonicalize(path).unwrap(), std::fs::canonicalize(tune_core::nodes::home()).unwrap());
}

#[test]
fn opening_a_link_reports_waiting_without_claiming_a_timeout_is_failure() {
    use fleet_studio::navigation::{OpenResult, OpenState, OpenStatus};
    let mut state = OpenState::default();
    let first = state.start().expect("first request can start");
    assert_eq!(state.status(), OpenStatus::Pending);
    state.timed_out(first);
    assert_eq!(state.status(), OpenStatus::Waiting);
    assert!(state.start().is_none(), "a still-running request cannot be duplicated");
    state.finish(first, OpenResult::Failed);
    assert_eq!(state.status(), OpenStatus::Failed);
}

#[test]
fn changed_ledger_is_reported_distinctly_from_os_failure() {
    use fleet_studio::navigation::{OpenResult, OpenState, OpenStatus};
    let mut state = OpenState::default();
    let request = state.start().unwrap();
    state.finish(request, OpenResult::SelectionChanged);
    assert_eq!(state.status(), OpenStatus::SelectionChanged);
}

#[test]
fn open_status_does_not_claim_the_browser_or_directory_was_displayed() {
    use fleet_studio::navigation::{OpenStatus, open_status_text};
    let accepted = open_status_text(OpenStatus::Accepted).unwrap();
    assert!(accepted.contains("OS") && accepted.contains("未確認"));
    assert!(open_status_text(OpenStatus::Waiting).unwrap().contains("終了待ち"));
    assert!(open_status_text(OpenStatus::SelectionChanged).unwrap().contains("台帳"));
    assert_eq!(open_status_text(OpenStatus::Sample), Some("サンプルの対象は実際には開きません。"));
}

#[test]
fn sample_targets_do_not_use_the_os_open_operation() {
    use fleet_studio::navigation::{OpenResult, sample_target_result};
    assert_eq!(sample_target_result(true), Some(OpenResult::Sample));
    assert_eq!(sample_target_result(false), None);
}

#[test]
fn permission_revoked_during_path_validation_prevents_os_handoff() {
    use fleet_studio::navigation::{OpenResult, finish_if_authorized};
    use std::cell::Cell;
    let allowed = Cell::new(true);
    let calls = Cell::new(0);
    assert!(allowed.get());
    allowed.set(false); // permission changes while the synthetic path is being validated
    let result = finish_if_authorized(
        || allowed.get(),
        || {
            calls.set(calls.get() + 1);
            true
        },
    );
    assert_eq!(result, OpenResult::SelectionChanged);
    assert_eq!(calls.get(), 0);
    allowed.set(true);
    assert_eq!(finish_if_authorized(|| allowed.get(), || true), OpenResult::Accepted);
    assert_eq!(finish_if_authorized(|| allowed.get(), || false), OpenResult::Failed);
}

#[test]
fn late_results_do_not_replace_the_status_of_a_newer_request() {
    use fleet_studio::navigation::{OpenResult, OpenState, OpenStatus};
    let mut state = OpenState::default();
    let first = state.start().unwrap();
    state.finish(first, OpenResult::Accepted);
    let second = state.start().unwrap();
    state.timed_out(first);
    state.finish(first, OpenResult::Failed);
    assert_eq!(state.status(), OpenStatus::Pending);
    state.finish(second, OpenResult::Unavailable);
    assert_eq!(state.status(), OpenStatus::Unavailable);
}

#[test]
fn only_clean_github_https_targets_can_be_dispatched() {
    use fleet_studio::navigation::OpenTarget;
    assert!(OpenTarget::github("https://github.com/fixture-owner/fixture-repo").is_some());
    assert!(OpenTarget::github("https://github.com/fixture-owner/fixture-repo/issues").is_some());
    for target in [
        "http://github.com/fixture/repo",
        "https://github.com.evil.invalid/fixture/repo",
        concat!("https://github.com", "@", "evil.invalid/fixture/repo"),
        concat!("https://user:secret", "@", "github.com/fixture/repo"),
        "https://github.com:8443/fixture/repo",
        "https://github.com/fixture/repo?token=secret",
        "https://github.com/fixture/repo#secret",
    ] {
        assert!(OpenTarget::github(target).is_none());
    }
}

#[cfg(windows)]
#[test]
fn local_open_rejects_missing_paths_and_regular_files_without_launching_them() {
    use fleet_studio::navigation::{OpenResult, OpenTarget};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let missing = root.join("target").join("fixture-never-created-51bce");
    let file = root.join("target").join(format!("fixture-open-{}.txt", std::process::id()));
    std::fs::write(&file, b"fixture").unwrap();
    assert_eq!(OpenTarget::directory(missing.to_str().unwrap()).unwrap().open(), OpenResult::Failed);
    assert_eq!(OpenTarget::directory(file.to_str().unwrap()).unwrap().open(), OpenResult::Failed);
    std::fs::remove_file(file).unwrap();
    assert!(OpenTarget::directory(r"\\fixture-server\fixture-share").is_none());
    assert!(OpenTarget::directory(r"C:\fixture\junction\..\other").is_none());
}

#[cfg(windows)]
#[test]
fn local_open_rejects_a_junction_in_the_path() {
    use fleet_studio::navigation::{OpenResult, OpenTarget};
    use std::process::{Command, Stdio};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("fixture-junction-{}", std::process::id()));
    let destination = root.join("destination");
    let junction = root.join("junction");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&destination).unwrap();
    std::fs::create_dir(destination.join("child")).unwrap();
    let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let created = Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "New-Item -ItemType Junction -Path $env:FS_TEST_LINK -Target $env:FS_TEST_TARGET -ErrorAction Stop | Out-Null",
        ])
        .env("FS_TEST_LINK", &junction)
        .env("FS_TEST_TARGET", &destination)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(created.success(), "failed to create the synthetic junction");
    assert_eq!(OpenTarget::directory(junction.to_str().unwrap()).unwrap().open(), OpenResult::Failed);
    assert_eq!(OpenTarget::directory(junction.join("child").to_str().unwrap()).unwrap().open(), OpenResult::Failed);
    std::fs::remove_dir(junction).unwrap();
    std::fs::remove_dir(destination.join("child")).unwrap();
    std::fs::remove_dir(destination).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[cfg(windows)]
#[test]
#[ignore = "shows a synthetic directory in Explorer"]
fn shows_a_synthetic_local_directory_in_explorer() {
    use fleet_studio::navigation::{OpenResult, OpenTarget};
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("fixture Fleet Studio 日本語 & test-parent").join("fixture-selected");
    std::fs::create_dir_all(&path).unwrap();
    let target = OpenTarget::directory(path.to_str().unwrap()).unwrap();
    assert_eq!(target.open(), OpenResult::Accepted);
}

#[test]
fn keyboard_selection_opens_only_the_visible_matching_repository() {
    use fleet_studio::{navigation::selected_repository_url, source::Repository};
    let repo = |name: &str, origin: &str| Repository { name: name.into(), origin: Some(origin.into()), observed_at: None, previous: false };
    let repositories = vec![
        repo("alpha", "https://github.com/fixture/alpha"),
        repo("beta", "https://github.com/fixture/beta"),
        repo("unavailable", "https://example.org/fixture/repo"),
    ];
    assert_eq!(selected_repository_url(&repositories, "AL", 0).as_deref(), Some("https://github.com/fixture/alpha"));
    assert_eq!(selected_repository_url(&repositories, "", 1).as_deref(), Some("https://github.com/fixture/beta"));
    assert_eq!(selected_repository_url(&repositories, "", 2), None);
    assert_eq!(selected_repository_url(&repositories, "beta", 1), None);
}
