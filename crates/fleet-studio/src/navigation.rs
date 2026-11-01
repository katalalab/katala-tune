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
    if url.scheme() != "https" || url.host_str().is_none() {
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
