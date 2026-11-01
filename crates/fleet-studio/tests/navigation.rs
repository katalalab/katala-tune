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
    assert_eq!(repository_url("https://user:password@example.org/fixture/repo?token=secret"), Some("https://example.org/fixture/repo".into()));
    assert_eq!(repository_url("file:///private/data"), None);
    assert_eq!(repository_url(concat!("https://example.org", "@", "evil.invalid/path")), Some("https://evil.invalid/path".into()));
}

#[test]
fn local_home_shorthand_can_open_an_existing_directory() {
    let url = fleet_studio::navigation::local_directory("~").expect("home directory is available");
    assert!(url.starts_with("file:"));
    let path = url::Url::parse(&url).unwrap().to_file_path().expect("URL is a local path");
    assert_eq!(std::fs::canonicalize(path).unwrap(), std::fs::canonicalize(tune_core::nodes::home()).unwrap());
}
