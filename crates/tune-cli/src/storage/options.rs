use super::volume;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};

pub(super) struct Options<'a> {
    pub job: String,
    pub target: PathBuf,
    pub report: PathBuf,
    pub os: u64,
    pub other: u64,
    pub expected: u64,
    pub required: u64,
    pub interval: u64,
    pub categories: BTreeMap<String, PathBuf>,
    pub command: &'a [OsString],
}
fn within(child: &Path, parent: &Path) -> bool {
    // canonicalize resolves aliases. Fold case for the supported case-insensitive filesystems.
    #[cfg(any(windows, target_os = "macos"))]
    if let (Some(a), Some(b)) = (child.to_str(), parent.to_str()) {
        return Path::new(&a.to_lowercase()).starts_with(b.to_lowercase());
    }
    child.starts_with(parent)
}

pub(super) fn parse(args: &[OsString]) -> Result<Options<'_>, String> {
    let (flags, command) = args.split_at(args.iter().position(|s| s == "--").ok_or("expected -- program [args]")?);
    let command = &command[1..];
    if command.is_empty() || command[0].is_empty() {
        return Err("expected a program after --".into());
    }
    let mut values = BTreeMap::new();
    let mut categories = BTreeMap::new();
    for pair in flags.chunks(2) {
        let [k, v] = pair else {
            return Err("option without value".into());
        };
        let k = k.to_str().unwrap_or("");
        match k {
            "--category" => {
                let (kind, path) = v.to_str().and_then(|s| s.split_once('=')).ok_or("expected --category kind=directory")?;
                if !matches!(kind, "original" | "artifact" | "intermediate" | "log")
                    || path.is_empty()
                    || Path::new(path).is_absolute()
                    || Path::new(path).components().any(|p| !matches!(p, std::path::Component::Normal(_) | std::path::Component::CurDir))
                    || categories.insert(kind.to_owned(), PathBuf::from(path)).is_some()
                {
                    return Err("invalid or duplicate category".into());
                }
            }
            "--job-id" | "--target" | "--report" | "--expected-peak-bytes" | "--os-reserve-bytes" | "--other-reserved-bytes" | "--interval-ms" => {
                if v.is_empty() || values.insert(k, v.as_os_str()).is_some() {
                    return Err(format!("empty or duplicate {k}"));
                }
            }
            _ => return Err("unknown storage-run option".into()),
        }
    }
    let required = |k| values.get(k).copied().ok_or_else(|| format!("missing {k}"));
    let number = |k| required(k)?.to_str().and_then(|s| s.parse::<u64>().ok()).ok_or_else(|| format!("invalid byte count: {k}"));
    let os = number("--os-reserve-bytes")?;
    let other = number("--other-reserved-bytes")?;
    let expected = number("--expected-peak-bytes")?;
    let required_bytes = os
        .checked_add(other)
        .and_then(|x| x.checked_add(expected))
        .filter(|n| *n <= super::MAX_BYTES)
        .ok_or("combined byte budget exceeds exact JSON range")?;
    let job = required("--job-id")?.to_str().unwrap_or("");
    if job.is_empty() || job.len() > 100 || !job.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
        return Err("use a short, non-sensitive ASCII job id".into());
    }
    let interval = values
        .get("--interval-ms")
        .copied()
        .unwrap_or(OsStr::new("1000"))
        .to_str()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| (10..=60000).contains(n))
        .ok_or("interval must be 10..60000 ms")?;
    let target = fs::canonicalize(required("--target")?).map_err(|_| "target directory inaccessible")?;
    if !target.is_dir() {
        return Err("target must be an existing directory".into());
    }
    let report = PathBuf::from(required("--report")?);
    let parent = report.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).canonicalize().map_err(|_| "report parent inaccessible")?;
    let report = parent.join(report.file_name().ok_or("invalid report filename")?);
    let mut roots = Vec::<PathBuf>::new();
    for root in categories.values_mut() {
        // Reject redirected components: a category cannot escape its declared subtree.
        let mut path = target.clone();
        for segment in root.components() {
            path.push(segment);
            let meta = path.symlink_metadata().map_err(|_| "category directory inaccessible")?;
            if volume::link(&meta) {
                return Err("category path contains a link or reparse point".into());
            }
        }
        *root = path.canonicalize().map_err(|_| "category directory inaccessible")?;
        if !root.is_dir() || !within(root, &target) || within(&report, root) || roots.iter().any(|r| within(r, root) || within(root, r)) {
            return Err("category directories must be disjoint, inside target, and exclude the report".into());
        }
        roots.push(root.clone());
    }
    Ok(Options { job: job.into(), target, report, os, other, expected, required: required_bytes, interval, categories, command })
}
