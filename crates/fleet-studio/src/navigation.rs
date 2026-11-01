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
        #[cfg(windows)]
        {
            let result = match self.0 {
                Target::Github(value) => windows_open::github(&value),
                Target::Directory(path) => windows_open::directory(&path),
            };
            if result.is_ok() { OpenResult::Accepted } else { OpenResult::Failed }
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
        };
    }
}
