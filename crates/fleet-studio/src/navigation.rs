/// Convert an observed repository origin to a safe browser URL.
pub fn repository_url(origin: &str) -> Option<String> {
    if origin.len() > 2_048 || origin.chars().any(char::is_control) {
        return None;
    }
    let github = origin
        .strip_prefix("git")
        .and_then(|rest| rest.strip_prefix('@'))
        .and_then(|rest| rest.strip_prefix("github.com:"))
        .or_else(|| origin.strip_prefix("github.com:"));
    let candidate = if let Some(path) = github {
        format!("https://github.com/{path}")
    } else if !origin.contains("://") {
        let (host, path) = origin.split_once('/')?;
        if !host.contains('.') || path.is_empty() {
            return None;
        }
        format!("https://{origin}")
    } else {
        origin.to_owned()
    };
    let mut url = url::Url::parse(&candidate).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") || url.port().is_some() {
        return None;
    }
    url.set_password(None).ok()?;
    url.set_username("").ok()?;
    url.set_query(None);
    url.set_fragment(None);
    let without_git = url.path().strip_suffix(".git").map(str::to_owned);
    if let Some(path) = without_git {
        url.set_path(&path);
    }
    Some(url.into())
}

/// Build a local URL without doing filesystem I/O on the UI thread.
pub fn local_directory(path: &str) -> Option<String> {
    let path = std::path::PathBuf::from(tune_core::nodes::expand(path));
    if !path.is_absolute() {
        return None;
    }
    url::Url::from_directory_path(path).ok().map(String::from)
}

/// Keep keyboard activation aligned with the visible, filtered rows.
pub fn selected_repository_url(repositories: &[crate::source::Repository], search: &str, selected: usize) -> Option<String> {
    let search = search.to_lowercase();
    repositories
        .iter()
        .filter(|repo| search.is_empty() || repo.name.to_lowercase().contains(&search))
        .take(200)
        .nth(selected)?
        .origin
        .as_deref()
        .and_then(repository_url)
}

pub struct OpenTarget(Target);

enum Target {
    Github(String),
    Directory(std::path::PathBuf),
}

impl OpenTarget {
    /// Describe the same validated target passed to the OS.
    pub fn description(&self) -> String {
        match &self.0 {
            Target::Github(url) => format!("GitHub を開く: {url}"),
            Target::Directory(path) => format!("場所を表示: {}", path.display()),
        }
    }

    /// Accept an already-sanitized URL, never credentials or query parameters.
    pub fn github(value: &str) -> Option<Self> {
        if value.len() > 2_048 || value.chars().any(char::is_control) {
            return None;
        }
        let url = url::Url::parse(value).ok()?;
        if url.scheme() != "https"
            || url.host_str() != Some("github.com")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        Some(Self(Target::Github(url.into())))
    }

    /// Parse a path without blocking on the filesystem; validation happens before launch.
    pub fn directory(value: &str) -> Option<Self> {
        if value.chars().any(char::is_control) {
            return None;
        }
        let path = std::path::PathBuf::from(tune_core::nodes::expand(value));
        if !path.is_absolute() || path.components().any(|part| matches!(part, std::path::Component::ParentDir)) {
            return None;
        }
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            if !matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))) {
                return None;
            }
        }
        Some(Self(Target::Directory(path)))
    }

    /// Call on a dedicated worker thread; never include targets or OS errors in output.
    pub fn open(self) -> OpenResult {
        self.open_guarded(|| true)
    }

    /// Check authorization both before path validation and just before OS handoff.
    pub fn open_guarded(self, authorized: impl Fn() -> bool) -> OpenResult {
        if !authorized() {
            return OpenResult::SelectionChanged;
        }
        #[cfg(windows)]
        {
            let result = match self.0 {
                Target::Github(value) => windows_open::github(&value, &authorized),
                Target::Directory(path) => windows_open::directory(&path, &authorized),
            };
            result.unwrap_or(OpenResult::Failed)
        }
        #[cfg(not(windows))]
        {
            match self.0 {
                Target::Github(value) => drop(value),
                Target::Directory(path) => drop(path),
            }
            OpenResult::Unavailable
        }
    }
}

// The generated Windows bindings require unsafe calls. Keep the boundary narrow;
// all inputs are validated again before an OS launch is attempted.
#[cfg(windows)]
#[allow(unsafe_code)]
#[path = "navigation_windows.rs"]
mod windows_open;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenResult {
    Accepted,
    Failed,
    Unavailable,
    SelectionChanged,
    Sample,
}

/// The final permission check must happen after preparation and before dispatch.
pub fn finish_if_authorized(authorized: impl FnOnce() -> bool, dispatch: impl FnOnce() -> bool) -> OpenResult {
    if !authorized() {
        OpenResult::SelectionChanged
    } else if dispatch() {
        OpenResult::Accepted
    } else {
        OpenResult::Failed
    }
}

pub fn sample_target_result(sample: bool) -> Option<OpenResult> {
    sample.then_some(OpenResult::Sample)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenStatus {
    #[default]
    Idle,
    Pending,
    Waiting,
    Accepted,
    Failed,
    Unavailable,
    SelectionChanged,
    Sample,
}

pub fn open_status_text(status: OpenStatus) -> Option<&'static str> {
    match status {
        OpenStatus::Idle => None,
        OpenStatus::Pending => Some("リンクを開いています。"),
        OpenStatus::Waiting => Some("リンクを開く処理の結果は未確認です（終了待ち）。"),
        OpenStatus::Accepted => Some("リンクを OS に引き渡しました。表示や接続は未確認です。"),
        OpenStatus::Failed => Some("リンクを開けませんでした。対象と OS の設定を確認してください。"),
        OpenStatus::Unavailable => Some("この OS ではリンクを開く操作に未対応です。"),
        OpenStatus::SelectionChanged => Some("台帳または対象が変わったため開けません。更新して確認してください。"),
        OpenStatus::Sample => Some("サンプルの対象は実際には開きません。"),
    }
}

#[derive(Default)]
pub struct OpenState {
    sequence: u64,
    status: OpenStatus,
}

impl OpenState {
    pub fn start(&mut self) -> Option<u64> {
        if matches!(self.status, OpenStatus::Pending | OpenStatus::Waiting) {
            return None;
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.status = OpenStatus::Pending;
        Some(self.sequence)
    }
    pub fn status(&self) -> OpenStatus {
        self.status
    }
    pub fn timed_out(&mut self, id: u64) {
        if self.sequence == id && self.status == OpenStatus::Pending {
            self.status = OpenStatus::Waiting;
        }
    }
    pub fn finish(&mut self, id: u64, result: OpenResult) {
        if self.sequence != id || !matches!(self.status, OpenStatus::Pending | OpenStatus::Waiting) {
            return;
        }
        self.status = match result {
            OpenResult::Accepted => OpenStatus::Accepted,
            OpenResult::Failed => OpenStatus::Failed,
            OpenResult::Unavailable => OpenStatus::Unavailable,
            OpenResult::SelectionChanged => OpenStatus::SelectionChanged,
            OpenResult::Sample => OpenStatus::Sample,
        };
    }
}

/// An asynchronous list replacement must not silently select a different link.
#[derive(Default)]
pub struct Selection(Option<usize>);

impl Selection {
    pub fn index(&self) -> Option<usize> {
        self.0
    }

    pub fn invalidate(&mut self) {
        self.0 = None;
    }

    pub fn next(&mut self, count: usize) {
        self.0 = if count == 0 { None } else { Some(self.0.map_or(0, |index| index.saturating_add(1).min(count - 1))) };
    }

    pub fn previous(&mut self, count: usize) {
        self.0 = if count == 0 { None } else { Some(self.0.map_or(0, |index| index.saturating_sub(1).min(count - 1))) };
    }
}

/// Visible resume rows in the same order as the dashboard.
pub struct WorkRows<'a> {
    pub repositories: Vec<&'a crate::source::Repository>,
    pub recent: Vec<&'a crate::recent::RecentEntry>,
    pub audits: Vec<&'a crate::audit::RepoAudit>,
    pub audit_total: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkRow<'a> {
    Repository(&'a crate::source::Repository),
    Recent(&'a crate::recent::RecentEntry),
    Audit(&'a crate::audit::RepoAudit),
}

#[derive(Clone, Copy)]
pub enum RecentOpen {
    Directory,
    PullRequest(usize),
}

/// Resolve the same target for keyboard activation and its fixed caption.
pub fn recent_target(entry: &crate::recent::RecentEntry, kind: RecentOpen) -> Option<OpenTarget> {
    if !entry.local {
        return None;
    }
    match kind {
        RecentOpen::Directory if entry.directory_url.is_some() => entry.cwd.as_deref().and_then(OpenTarget::directory),
        RecentOpen::PullRequest(index) => entry.prs.get(index).and_then(|url| crate::recent::github_pr_url(url)).and_then(|url| OpenTarget::github(&url)),
        _ => None,
    }
}

pub fn preferred_recent(entry: &crate::recent::RecentEntry) -> Option<RecentOpen> {
    if !entry.local {
        return None;
    }
    Some(if entry.directory_url.is_some() { RecentOpen::Directory } else { RecentOpen::PullRequest(0) })
}

impl<'a> WorkRows<'a> {
    pub fn new(
        repositories: &'a [crate::source::Repository],
        recent: &'a [crate::recent::RecentEntry],
        audits: &'a [crate::audit::RepoAudit],
        search: &str,
    ) -> Self {
        let search = search.to_lowercase();
        let matches = |value: &str| search.is_empty() || value.to_lowercase().contains(&search);
        let mut audit_rows: Vec<_> = audits.iter().filter(|row| matches(&format!("{} {}", row.node, row.path))).collect();
        let audit_total = audit_rows.len();
        audit_rows.truncate(200);
        Self {
            repositories: repositories.iter().filter(|repo| matches(&repo.name)).take(200).collect(),
            recent: recent.iter().filter(|entry| matches(&format!("{} {} {}", entry.node, entry.tool, entry.cwd.as_deref().unwrap_or("")))).collect(),
            audits: audit_rows,
            audit_total,
        }
    }

    pub fn len(&self) -> usize {
        self.repositories.len() + self.recent.len() + self.audits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.repositories.is_empty() && self.recent.is_empty() && self.audits.is_empty()
    }

    pub fn row(&self, selected: Option<usize>) -> Option<WorkRow<'a>> {
        let index = selected?;
        if let Some(repo) = self.repositories.get(index) {
            return Some(WorkRow::Repository(repo));
        }
        let index = index.checked_sub(self.repositories.len())?;
        if let Some(entry) = self.recent.get(index) {
            return Some(WorkRow::Recent(entry));
        }
        let index = index.checked_sub(self.recent.len())?;
        self.audits.get(index).map(|row| WorkRow::Audit(row))
    }
}

pub fn selection_text(
    selected: Option<WorkRow<'_>>,
    sample: bool,
    recent: Option<&crate::recent::RecentWork>,
    audit: Option<&crate::audit::Audit>,
    ticket: &crate::scope::AuditTicket,
) -> Option<String> {
    selection_text_for_platform(selected, sample, recent, audit, ticket, cfg!(windows))
}

pub fn selection_text_for_platform(
    selected: Option<WorkRow<'_>>,
    sample: bool,
    recent: Option<&crate::recent::RecentWork>,
    audit: Option<&crate::audit::Audit>,
    ticket: &crate::scope::AuditTicket,
    supported: bool,
) -> Option<String> {
    let selected = selected?;
    let (label, target) = match selected {
        WorkRow::Repository(repo) => {
            let target = repo.origin.as_deref().and_then(repository_url).and_then(|url| OpenTarget::github(&url));
            (format!("構成リポジトリ {}", repo.name), target.map(|target| target.description()))
        }
        WorkRow::Recent(entry) => {
            let authorized = sample || recent.is_some_and(|work| work.selection_fingerprint.is_some() && work.entries.contains(entry));
            let target = if authorized { preferred_recent(entry).and_then(|kind| recent_target(entry, kind)).map(|target| target.description()) } else { None };
            (format!("最近の作業 {} · {}", entry.node, entry.tool), target)
        }
        WorkRow::Audit(row) => {
            let target = match crate::audit::prepare_open(sample, true, ticket, audit, Some(row)) {
                Ok((target, _)) => Some(target.description()),
                Err(OpenResult::Sample) => Some(format!("サンプルの場所 {}（実際には開きません）", row.path)),
                Err(_) => None,
            };
            (format!("衛生監査 {} · {}", row.node, row.path), target)
        }
    };
    let target = match target {
        Some(target) if !sample && !supported => format!("この OS では未対応（対象: {target}）"),
        Some(target) => target,
        None => "開けません".into(),
    };
    let note = if sample && !matches!(selected, WorkRow::Audit(_)) { "（サンプルは実際には開きません）" } else { "" };
    Some(format!("選択: {label} / Enter: {target}{note}"))
}
