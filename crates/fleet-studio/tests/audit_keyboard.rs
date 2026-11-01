use fleet_studio::{
    audit::{self, Audit, AuditIdentity, RepoAudit},
    navigation::{self, OpenResult, Selection, WorkRow, WorkRows},
    recent::{PrState, RecentEntry},
    scope::AuditGate,
    source::Repository,
};

fn repository(name: &str) -> Repository {
    Repository { name: name.into(), origin: Some(format!("https://github.com/example/{name}")), observed_at: None, previous: false }
}

fn recent(name: &str, cwd: Option<&str>) -> RecentEntry {
    RecentEntry {
        node: "fixture-node".into(),
        local: true,
        tool: name.into(),
        cwd: cwd.map(str::to_owned),
        activity_at: None,
        directory_url: cwd.map(|_| "synthetic-directory".into()),
        prs: Vec::new(),
        pr_state: PrState::Empty,
    }
}

fn audit_row(path: &str) -> RepoAudit {
    RepoAudit {
        node: "fixture-node".into(),
        path: path.into(),
        branch: None,
        dirty: None,
        unpushed: None,
        behind: None,
        local_only: None,
        previous: false,
        local: true,
        at: None,
        error: None,
    }
}

fn local_path() -> &'static str {
    if cfg!(windows) { r"C:\fixture\project" } else { "/fixture/project" }
}

fn observed(rows: Vec<RepoAudit>) -> Audit {
    Audit { repos: rows, identity: Some(AuditIdentity { node: "fixture-node".into(), fingerprint: "synthetic-fingerprint".into() }), ..Audit::default() }
}

#[test]
fn navigation_orders_visible_repositories_recent_work_then_audits() {
    let repos = [repository("alpha"), repository("beta")];
    let recent = [recent("editor", Some(local_path()))];
    let audits = [audit_row(local_path())];
    let rows = WorkRows::new(&repos, &recent, &audits, "");
    assert_eq!(rows.len(), 4);
    assert!(matches!(rows.row(Some(0)), Some(WorkRow::Repository(row)) if row.name == "alpha"));
    assert!(matches!(rows.row(Some(1)), Some(WorkRow::Repository(row)) if row.name == "beta"));
    assert!(matches!(rows.row(Some(2)), Some(WorkRow::Recent(row)) if row.tool == "editor"));
    assert!(matches!(rows.row(Some(3)), Some(WorkRow::Audit(row)) if row.path == local_path()));
    assert!(rows.row(Some(4)).is_none());
    assert!(rows.row(None).is_none());
}

#[test]
fn navigation_filter_and_limit_match_display_count_and_enter_target() {
    let repos = [repository("needle-repo"), repository("other")];
    let recent = [recent("needle-editor", None), recent("other", None)];
    let mut audits: Vec<_> = (0..205).map(|index| audit_row(&format!("{}/needle-{index:03}", local_path()))).collect();
    audits.push(audit_row(&format!("{}/excluded", local_path())));
    let rows = WorkRows::new(&repos, &recent, &audits, "NEEDLE");
    assert_eq!(rows.repositories.len(), 1);
    assert_eq!(rows.recent.len(), 1);
    assert_eq!(rows.audit_total, 205);
    assert_eq!(rows.audits.len(), 200);
    assert_eq!(rows.len(), 202);
    assert!(matches!(rows.row(Some(2)), Some(WorkRow::Audit(row)) if row.path.ends_with("needle-000")));
    assert!(matches!(rows.row(Some(201)), Some(WorkRow::Audit(row)) if row.path.ends_with("needle-199")));
    assert!(rows.row(Some(202)).is_none());
}

#[test]
fn repository_limit_never_makes_an_undisplayed_row_selectable() {
    let repos: Vec<_> = (0..201).map(|index| repository(&format!("repo-{index:03}"))).collect();
    let rows = WorkRows::new(&repos, &[], &[], "");
    assert_eq!(rows.repositories.len(), 200);
    assert!(matches!(rows.row(Some(199)), Some(WorkRow::Repository(row)) if row.name == "repo-199"));
    assert!(rows.row(Some(200)).is_none());
}

#[test]
fn audit_selection_caption_names_the_same_open_target() {
    let row = audit_row(local_path());
    let data = observed(vec![row.clone()]);
    let gate = AuditGate::default();
    let ticket = gate.ticket();
    let rows = WorkRows::new(&[], &[], &data.repos, "");
    let caption = navigation::selection_text(rows.row(Some(0)), false, None, Some(&data), &ticket).unwrap();
    assert!(caption.contains(local_path()));
    assert!(caption.contains("場所を表示"));
    let prepared = audit::prepare_open(false, true, &ticket, Some(&data), Some(&row));
    assert!(prepared.is_ok());
    assert!(matches!(audit::prepare_open(false, false, &ticket, Some(&data), Some(&row)), Err(OpenResult::SelectionChanged)));
}

#[test]
fn recent_selection_caption_matches_the_preferred_directory_or_first_pr() {
    let mut entry = recent("editor", Some(local_path()));
    entry.prs.push("https://github.com/example/fixture/pull/1".into());
    let ticket = AuditGate::default().ticket();
    let mut work =
        fleet_studio::recent::RecentWork { entries: vec![entry.clone()], selection_fingerprint: Some("synthetic-fingerprint".into()), ..Default::default() };
    let caption = navigation::selection_text(Some(WorkRow::Recent(&entry)), false, Some(&work), None, &ticket).unwrap();
    assert!(caption.contains(local_path()));
    assert!(!caption.contains("/pull/1"));

    entry.directory_url = None;
    work.entries = vec![entry.clone()];
    let caption = navigation::selection_text(Some(WorkRow::Recent(&entry)), false, Some(&work), None, &ticket).unwrap();
    assert!(caption.contains("https://github.com/example/fixture/pull/1"));
    assert!(!caption.contains(local_path()));

    entry.local = false;
    work.entries = vec![entry.clone()];
    let caption = navigation::selection_text(Some(WorkRow::Recent(&entry)), false, Some(&work), None, &ticket).unwrap();
    assert!(caption.contains("開けません"));
    assert!(navigation::preferred_recent(&entry).is_none());
}

#[test]
fn recent_caption_does_not_offer_a_target_without_the_required_fingerprint() {
    let entry = recent("editor", Some(local_path()));
    let work = fleet_studio::recent::RecentWork { entries: vec![entry.clone()], ..Default::default() };
    let ticket = AuditGate::default().ticket();
    let caption = navigation::selection_text(Some(WorkRow::Recent(&entry)), false, Some(&work), None, &ticket).unwrap();
    assert!(caption.contains("開けません"));
    assert!(!caption.contains(local_path()));
}

#[test]
fn unsupported_os_caption_distinguishes_real_targets_from_sample() {
    let repo = repository("alpha");
    let audit_row = audit_row(local_path());
    let audit = observed(vec![audit_row.clone()]);
    let entry = recent("editor", Some(local_path()));
    let recent =
        fleet_studio::recent::RecentWork { entries: vec![entry.clone()], selection_fingerprint: Some("synthetic-fingerprint".into()), ..Default::default() };
    let ticket = AuditGate::default().ticket();
    for row in [WorkRow::Repository(&repo), WorkRow::Recent(&entry), WorkRow::Audit(&audit_row)] {
        let text = navigation::selection_text_for_platform(Some(row), false, Some(&recent), Some(&audit), &ticket, false).unwrap();
        assert!(text.contains("未対応"));
        assert!(text.contains("対象"));
    }
    let sample = navigation::selection_text_for_platform(Some(WorkRow::Audit(&audit_row)), true, None, None, &ticket, false).unwrap();
    assert!(sample.contains("サンプル"));
    assert!(sample.contains("開きません"));
    assert!(!sample.contains("未対応"));
}

#[test]
fn sample_audit_never_needs_a_valid_identity_or_path() {
    let row = audit_row("relative-fixture");
    let ticket = AuditGate::default().ticket();
    assert!(matches!(audit::prepare_open(true, true, &ticket, None, Some(&row)), Err(OpenResult::Sample)));
    assert!(matches!(audit::prepare_open(true, true, &ticket, None, None), Err(OpenResult::SelectionChanged)));
}

#[test]
fn audit_rejects_stale_remote_missing_and_invalid_targets_without_io() {
    let valid = audit_row(local_path());
    let data = observed(vec![valid.clone()]);
    let mut gate = AuditGate::default();
    let old = gate.ticket();
    let mut stored = Some(data.clone());
    let mut selected = Selection::default();
    audit::begin(&mut selected, &mut gate, &mut stored);
    assert!(matches!(audit::prepare_open(false, true, &old, Some(&data), Some(&valid)), Err(OpenResult::SelectionChanged)));

    let ticket = gate.ticket();
    let mut remote = valid.clone();
    remote.local = false;
    let remote_data = observed(vec![remote.clone()]);
    assert!(matches!(audit::prepare_open(false, true, &ticket, Some(&remote_data), Some(&remote)), Err(OpenResult::SelectionChanged)));
    assert!(matches!(audit::prepare_open(false, true, &ticket, Some(&data), None), Err(OpenResult::SelectionChanged)));
    assert!(matches!(audit::prepare_open(false, true, &ticket, None, Some(&valid)), Err(OpenResult::SelectionChanged)));

    let invalid = audit_row("relative-fixture");
    let invalid_data = observed(vec![invalid.clone()]);
    assert!(matches!(audit::prepare_open(false, true, &ticket, Some(&invalid_data), Some(&invalid)), Err(OpenResult::Failed)));
    let mut wrong_identity = data.clone();
    wrong_identity.identity.as_mut().unwrap().node = "different-node".into();
    assert!(matches!(audit::prepare_open(false, true, &ticket, Some(&wrong_identity), Some(&valid)), Err(OpenResult::SelectionChanged)));
}

#[test]
fn audit_start_and_result_replacement_or_failure_clear_selection() {
    let row = audit_row(local_path());
    let mut data = Some(observed(vec![row.clone()]));
    let mut gate = AuditGate::default();
    let old = gate.ticket();
    let mut selected = Selection::default();
    selected.next(3);
    audit::begin(&mut selected, &mut gate, &mut data);
    assert_eq!(selected.index(), None);
    assert!(data.is_none());
    assert!(!old.is_current());

    selected.next(3);
    audit::replace(&mut selected, &mut data, Some(observed(vec![row])));
    assert_eq!(selected.index(), None);
    assert!(data.is_some());

    selected.next(3);
    audit::replace(&mut selected, &mut data, None);
    assert_eq!(selected.index(), None);
    assert!(data.is_none());
}
