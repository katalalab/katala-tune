//! Platform-independent checks for the macOS navigation adapter.
use super::{OpenResult, OpenTarget, Target};

#[derive(Default)]
pub struct MacHome(std::sync::OnceLock<std::path::PathBuf>);

impl MacHome {
    pub const fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }

    pub fn resolve(&self, value: &str, home: impl FnOnce() -> std::path::PathBuf) -> Option<std::path::PathBuf> {
        if value.starts_with("~\\") || value.chars().any(char::is_control) {
            return None;
        }
        if value == "~" || value.starts_with("~/") {
            let suffix = value.strip_prefix("~/").unwrap_or("");
            if suffix.starts_with('/')
                || suffix.starts_with('\\')
                || (!suffix.is_empty() && suffix.split('/').any(|part| part.is_empty() || part == "." || part == ".."))
            {
                return None;
            }
            let current = home();
            let home = self.0.get_or_init(|| current.clone());
            if *home != current {
                return None;
            }
            components(home.to_str()?)?;
            let path = if suffix.is_empty() { home.clone() } else { home.join(suffix) };
            components(path.to_str()?)?;
            Some(path)
        } else {
            components(value)?;
            Some(std::path::PathBuf::from(value))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    Failed,
    Revoked,
}

pub trait DirectoryFs {
    type Directory;
    fn root(&mut self) -> Result<Self::Directory, FsError>;
    fn child(&mut self, parent: &Self::Directory, name: &str) -> Result<Self::Directory, FsError>;
    fn local(&mut self, directory: &Self::Directory) -> Result<bool, FsError>;
    fn same(&mut self, first: &Self::Directory, second: &Self::Directory, authorized: &impl Fn() -> bool) -> Result<bool, FsError>;
}

pub trait Workspace {
    fn github(&mut self, url: &str, authorized: &impl Fn() -> bool) -> OpenResult;
    fn directory(&mut self, path: &str, authorized: &impl Fn() -> bool) -> OpenResult;
}

fn components(path: &str) -> Option<Vec<&str>> {
    if !path.starts_with('/') || path.starts_with("//") || path.chars().any(char::is_control) {
        return None;
    }
    let rest = path.strip_prefix('/')?;
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let mut components = Vec::new();
    for part in rest.split('/') {
        if part.is_empty() || part == "." || part == ".." || components.len() == 48 {
            return None;
        }
        components.push(part);
    }
    Some(components)
}

fn check(authorized: &impl Fn() -> bool) -> Result<(), FsError> {
    if authorized() { Ok(()) } else { Err(FsError::Revoked) }
}

fn observe<T>(result: Result<T, FsError>, current: &impl Fn() -> bool) -> Result<T, FsError> {
    match result {
        Err(_) if !current() => Err(FsError::Revoked),
        other => other,
    }
}

fn walk<F: DirectoryFs>(fs: &mut F, components: &[&str], authorized: &impl Fn() -> bool) -> Result<Vec<F::Directory>, FsError> {
    check(authorized)?;
    let root = observe(fs.root(), authorized)?;
    check(authorized)?;
    if !observe(fs.local(&root), authorized)? {
        return Err(if authorized() { FsError::Failed } else { FsError::Revoked });
    }
    let mut directories = vec![root];
    for component in components {
        check(authorized)?;
        let current = observe(fs.child(directories.last().ok_or(FsError::Failed)?, component), authorized)?;
        check(authorized)?;
        if !observe(fs.local(&current), authorized)? {
            return Err(if authorized() { FsError::Failed } else { FsError::Revoked });
        }
        directories.push(current);
    }
    Ok(directories)
}

fn walk_result<T>(result: Result<T, FsError>) -> Result<T, OpenResult> {
    result.map_err(|error| match error {
        FsError::Failed => OpenResult::Failed,
        FsError::Revoked => OpenResult::SelectionChanged,
    })
}

pub fn open_directory<F: DirectoryFs, W: Workspace>(
    path: &str,
    fs: &mut F,
    workspace: &mut W,
    authorized: &impl Fn() -> bool,
    current: &impl Fn() -> bool,
) -> OpenResult {
    if !authorized() {
        return OpenResult::SelectionChanged;
    }
    let Some(components) = components(path) else { return OpenResult::Failed };
    let first = match walk_result(walk(fs, &components, current)) {
        Ok(first) => first,
        Err(result) => return result,
    };
    let second = match walk_result(walk(fs, &components, current)) {
        Ok(second) => second,
        Err(result) => return result,
    };
    if first.len() != second.len() {
        return OpenResult::Failed;
    }
    for (first, second) in first.iter().zip(&second) {
        if !current() {
            return OpenResult::SelectionChanged;
        }
        match walk_result(observe(fs.same(first, second, current), current)) {
            Ok(true) => {}
            Ok(false) => return if current() { OpenResult::Failed } else { OpenResult::SelectionChanged },
            Err(result) => return result,
        }
    }
    if !current() {
        return OpenResult::SelectionChanged;
    }
    let result = workspace.directory(path, &|| current() && authorized());
    drop(second);
    drop(first);
    result
}

pub fn open_github<W: Workspace>(value: &str, workspace: &mut W, authorized: &impl Fn() -> bool) -> OpenResult {
    if !authorized() {
        return OpenResult::SelectionChanged;
    }
    let Some(OpenTarget(Target::Github(url))) = OpenTarget::github(value) else { return OpenResult::Failed };
    workspace.github(&url, authorized)
}
