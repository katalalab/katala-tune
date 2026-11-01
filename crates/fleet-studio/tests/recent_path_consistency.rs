use std::path::Path;

use fleet_studio::{
    navigation::{self, RecentOpen, WorkRow},
    recent::{self, PrState, RecentEntry, RecentWork},
    scope::AuditGate,
};

fn synthetic_path(name: &str) -> String {
    if cfg!(windows) { format!(r"C:\fixture\{name}") } else { format!("/fixture/{name}") }
}

fn synthetic_entry(node: &str, cwd: &str) -> RecentEntry {
    let home = synthetic_path("home");
    RecentEntry {
        node: node.into(),
        local: true,
        tool: "fixture-tool".into(),
        cwd: Some(cwd.into()),
        activity_at: None,
        directory_url: recent::local_cwd_candidate_with_home(node, true, cwd, Path::new(&home)),
        prs: vec!["https://github.com/example/fixture/pull/1".into()],
        pr_state: PrState::Complete,
    }
}

#[test]
fn ambiguous_home_prefix_never_produces_a_directory_candidate() {
    let home = synthetic_path("home");
    for cwd in ["~//project", "~\\\\project", "~/\\project"] {
        assert!(recent::local_cwd_url_with_home(true, cwd, Path::new(&home)).is_none());
    }
}

#[test]
fn recent_candidate_with_changed_cwd_is_rejected_and_pr_falls_back() {
    let original = synthetic_path("candidate");
    let mut entry = synthetic_entry("fixture-node", &original);
    assert!(entry.directory_url.is_some());
    entry.cwd = Some(synthetic_path("other"));
    assert!(navigation::recent_target(&entry, RecentOpen::Directory).is_none());
    assert!(matches!(navigation::preferred_recent(&entry), Some(RecentOpen::PullRequest(0))));
    let work = RecentWork { entries: vec![entry.clone()], selection_fingerprint: Some("fixture-fingerprint".into()), ..Default::default() };
    let ticket = AuditGate::default().ticket();
    let caption = navigation::selection_text_for_platform(Some(WorkRow::Recent(&entry)), false, Some(&work), None, &ticket, true).unwrap();
    assert!(caption.contains("https://github.com/example/fixture/pull/1"));
    assert!(!caption.contains(&original));
}

#[test]
fn recent_candidate_with_changed_node_is_rejected_and_pr_falls_back() {
    let cwd = synthetic_path("candidate");
    let mut entry = synthetic_entry("fixture-original", &cwd);
    assert!(entry.directory_url.is_some());
    entry.node = "fixture-other".into();
    assert!(navigation::recent_target(&entry, RecentOpen::Directory).is_none());
    assert!(matches!(navigation::preferred_recent(&entry), Some(RecentOpen::PullRequest(0))));
}

#[test]
fn matching_recent_candidate_caption_and_open_target_agree() {
    let cwd = synthetic_path("candidate");
    let entry = synthetic_entry("fixture-node", &cwd);
    let work = RecentWork { entries: vec![entry.clone()], selection_fingerprint: Some("fixture-fingerprint".into()), ..Default::default() };
    let ticket = AuditGate::default().ticket();
    let description = navigation::recent_target(&entry, RecentOpen::Directory).unwrap().description();
    assert_eq!(description, format!("場所を表示: {cwd}"));
    let caption = navigation::selection_text_for_platform(Some(WorkRow::Recent(&entry)), false, Some(&work), None, &ticket, true).unwrap();
    assert!(caption.contains(&description));
}

#[test]
fn unsafe_paths_never_create_candidates_and_safe_paths_remain_available() {
    let home = synthetic_path("home");
    let absolute = synthetic_path("project");
    for good in ["~", "~/project", absolute.as_str()] {
        assert!(recent::local_cwd_url_with_home(true, good, Path::new(&home)).is_some());
    }
    for bad in ["../outside", "~/../outside", "~//project", "~/\\project", "file://fixture-server/share", "relative", "bad\u{0007}path"] {
        assert!(recent::local_cwd_url_with_home(true, bad, Path::new(&home)).is_none());
    }
    if cfg!(windows) {
        for bad in [
            r"\\fixture-server\share\repo",
            r"C:\fixture\..\other",
            r"~\\project",
            r"~\/project",
            "~/C:/other",
            r"C:\fixture\project.",
            "C:\\fixture\\project ",
            r"C:\fixture\project:stream",
            r"C:\fixture\CON\child",
            r"C:\fixture\nul.txt\child",
            r"C:\fixture\com¹\child",
            r"C:\fixture\LPT³.data\child",
            r"C:\fixture\COM0\child",
            r"C:\fixture\CONIN$\child",
            r"C:\fixture\conout$.txt\child",
            r"C:\fixture\ leading\child",
        ] {
            assert!(recent::local_cwd_url_with_home(true, bad, Path::new(&home)).is_none());
        }
        for good in [r"~\project", r"C:\fixture\COM10\child", "C:\\fixture\\　leading\\child"] {
            assert!(recent::local_cwd_url_with_home(true, good, Path::new(&home)).is_some());
        }
    } else {
        for bad in ["//fixture-server/share/repo", "/fixture/../other", r"~\project"] {
            assert!(recent::local_cwd_url_with_home(true, bad, Path::new(&home)).is_none());
        }
    }
    assert!(recent::local_cwd_url_with_home(false, &absolute, Path::new(&home)).is_none());
}

#[test]
fn invalid_or_missing_directory_candidate_falls_back_to_the_first_valid_pr() {
    let home = synthetic_path("home");
    for bad in ["not-a-path", "https://github.com/example/fixture", "file://fixture-server/share/", "file:///fixture/other/?query=1"] {
        let entry = synthetic_entry("fixture-node", bad);
        assert!(entry.directory_url.is_none());
        assert!(navigation::recent_target(&entry, RecentOpen::Directory).is_none());
        assert!(matches!(navigation::preferred_recent(&entry), Some(RecentOpen::PullRequest(0))));
        let fallback = navigation::recent_target(&entry, RecentOpen::PullRequest(0)).unwrap();
        assert_eq!(fallback.description(), "GitHub を開く: https://github.com/example/fixture/pull/1");
    }
    let path = synthetic_path("candidate");
    let mut entry = synthetic_entry("fixture-node", &path);
    assert!(entry.directory_url.is_some());
    for changed in [None, Some(""), Some("relative"), Some("bad\u{0007}path")] {
        entry.cwd = changed.map(str::to_owned);
        assert!(navigation::recent_target(&entry, RecentOpen::Directory).is_none());
        assert!(matches!(navigation::preferred_recent(&entry), Some(RecentOpen::PullRequest(0))));
    }
    entry.cwd = Some(path.clone());
    entry.local = false;
    assert!(navigation::recent_target(&entry, RecentOpen::Directory).is_none());
    assert!(navigation::preferred_recent(&entry).is_none());
    assert!(recent::local_cwd_candidate_with_home("fixture-node", false, &path, Path::new(&home)).is_none());
    assert!(recent::local_cwd_candidate_with_home("", true, &path, Path::new(&home)).is_none());
    assert!(recent::local_cwd_candidate_with_home("invalid\u{0007}", true, &path, Path::new(&home)).is_none());
}

#[test]
fn home_candidate_remains_bound_to_its_creation_and_changed_row_is_not_visible() {
    let first_home = synthetic_path("home-one");
    let second_home = synthetic_path("home-two");
    let cwd = "~/project";
    let mut current = synthetic_entry("fixture-node", cwd);
    current.directory_url = recent::local_cwd_candidate_with_home("fixture-node", true, cwd, Path::new(&first_home));
    let target = navigation::recent_target(&current, RecentOpen::Directory).unwrap();
    let expected = Path::new(&first_home).join("project");
    assert_eq!(target.description(), format!("場所を表示: {}", expected.display()));

    let mut other = current.clone();
    other.directory_url = recent::local_cwd_candidate_with_home("fixture-node", true, cwd, Path::new(&second_home));
    assert_ne!(current, other);
    let work = RecentWork { entries: vec![current], selection_fingerprint: Some("fixture-fingerprint".into()), ..Default::default() };
    let ticket = AuditGate::default().ticket();
    let caption = navigation::selection_text_for_platform(Some(WorkRow::Recent(&other)), false, Some(&work), None, &ticket, true).unwrap();
    assert!(caption.contains("開けません"));
}

#[test]
fn encoded_literal_path_is_preserved_in_candidate_caption_and_open_target() {
    let cwd = if cfg!(windows) { r"C:\fixture\日本語 %2F #part" } else { "/fixture/日本語 %2F #part" };
    let entry = synthetic_entry("fixture-node", cwd);
    let candidate = entry.directory_url.as_ref().unwrap();
    assert!(candidate.url().contains("%252F"));
    assert!(candidate.url().contains("%23"));
    assert!(candidate.url().contains("%20"));
    let target = navigation::recent_target(&entry, RecentOpen::Directory).unwrap();
    let description = target.description();
    assert_eq!(description, format!("場所を表示: {cwd}"));
    let work = RecentWork { entries: vec![entry.clone()], selection_fingerprint: Some("fixture-fingerprint".into()), ..Default::default() };
    let ticket = AuditGate::default().ticket();
    let caption = navigation::selection_text_for_platform(Some(WorkRow::Recent(&entry)), false, Some(&work), None, &ticket, true).unwrap();
    assert!(caption.contains(&description));
}
