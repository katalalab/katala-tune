use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub struct Options {
    pub config: PathBuf,
    pub data: PathBuf,
    pub selftest: bool,
    pub resident: bool,
    pub hidden: bool,
    pub open: Option<String>,
    pub live_id: Option<String>,
    pub logs: bool,
    /// --selftest 用: 確認欄を出して「やめる」で終える（何も実行しない）
    pub confirm_demo: bool,
    /// 起動時に読む完了台帳（task-completion.v1）。画面へのドラッグでも読める
    pub review: Option<PathBuf>,
    /// Explicit fixture-only shutdown diagnostic; parent controls data/shutdown-check.
    pub shutdown_check: Option<String>,
}

pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let Some(mut options) = parse_options(arguments)? else { return Ok(None) };
    // Freeze paths before IPC/runtime threads. Every normal/diagnostic entry
    // shares these exact paths with Engine, OS integration and helper lookup.
    options.config = resolved(&options.config)?;
    options.data = resolved(&options.data)?;
    tune_core::helpers::bind_process_data_dir(&options.data)?;
    Ok(Some(options))
}

/// Canonicalize existing ancestors too, so missing and later-created paths
/// have the same identity. Resolve symlinks before `..`; never create state.
pub(super) fn resolved(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let mut result = PathBuf::new();
    for part in absolute.components() {
        match part {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            part => result.push(part.as_os_str()),
        }
        // Missing tails are valid; other filesystem errors must fail closed.
        if result.try_exists().map_err(|e| e.to_string())? {
            result = result.canonicalize().map_err(|e| e.to_string())?;
        }
    }
    Ok(result)
}

fn parse_options(arguments: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let values: Vec<_> = arguments.into_iter().collect();
    if values.len() == 1 && matches!(values[0].to_str(), Some("--help" | "-h")) {
        return Ok(None);
    }
    let (mut config, mut data, mut selftest) = (None, None, false);
    let (mut open, mut live_id, mut logs, mut confirm_demo) = (None, None, false, false);
    let (mut resident, mut hidden) = (false, false);
    let mut review: Option<PathBuf> = None;
    let mut shutdown_check = None;
    let mut args = values.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--selftest") if !selftest => selftest = true,
            Some("--resident") if !resident => resident = true,
            Some("--hidden") if !hidden => hidden = true,
            Some("--shutdown-check") => {
                let mode = args
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .filter(|v| matches!(v.as_str(), "close" | "quit" | "native-quit"))
                    .ok_or("--shutdown-check requires close, quit or native-quit")?;
                if shutdown_check.replace(mode).is_some() {
                    return Err("duplicate option: --shutdown-check".into());
                }
            }
            Some("--logs") if !logs => logs = true,
            Some("--confirm-demo") if !confirm_demo => confirm_demo = true,
            Some("--open" | "--live") => {
                let name = arg.to_str().unwrap();
                let value = args
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .filter(|v| !v.is_empty() && !v.starts_with('-'))
                    .ok_or_else(|| format!("{name} requires a target"))?;
                let target = if name == "--open" { &mut open } else { &mut live_id };
                if target.replace(value).is_some() {
                    return Err(format!("duplicate option: {name}"));
                }
            }
            Some("--review") => {
                let path = args.next().filter(|v| !v.is_empty() && !v.to_string_lossy().starts_with('-')).ok_or("--review requires a path")?;
                if review.replace(PathBuf::from(path)).is_some() {
                    return Err("duplicate option: --review".into());
                }
            }
            Some("--config" | "--data") => {
                let name = arg.to_str().unwrap();
                let path = args.next().filter(|v| !v.is_empty() && !v.to_string_lossy().starts_with('-')).ok_or_else(|| format!("{name} requires a path"))?;
                let target = if name == "--config" { &mut config } else { &mut data };
                if target.replace(PathBuf::from(path)).is_some() {
                    return Err(format!("duplicate option: {name}"));
                }
            }
            _ => return Err("unknown option; use --help".into()),
        }
    }
    if hidden && !resident {
        return Err("--hidden requires --resident".into());
    }
    if (resident || hidden) && (selftest || shutdown_check.is_some() || live_id.is_some() || logs || confirm_demo) {
        return Err("--resident/--hidden cannot be combined with diagnostics".into());
    }
    if open.as_deref().is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control)) {
        return Err("--open must be at most 256 bytes without control characters".into());
    }
    if selftest && (config.is_none() || data.is_none()) {
        return Err("--selftest requires --config and --data".into());
    }
    if shutdown_check.is_some() && !selftest {
        return Err("--shutdown-check requires --selftest with explicit --config and --data".into());
    }
    if (live_id.is_some() || logs || confirm_demo) && !selftest {
        return Err("--live, --logs and --confirm-demo require --selftest".into());
    }
    Ok(Some(Options {
        config: config.unwrap_or_else(tune_core::nodes::user_config_path),
        // Preview data stays separate from the existing application by default.
        data: data.unwrap_or_else(|| tune_core::nodes::data_dir().join("gpui-preview")),
        selftest,
        resident,
        hidden,
        open,
        live_id,
        logs,
        confirm_demo,
        review,
        shutdown_check,
    }))
}

#[cfg(test)]
mod tests {
    use super::parse_options as parse;
    use super::*;

    #[test]
    fn resident_hidden_are_explicit_and_diagnostics_are_disjoint() {
        let ordinary = parse([]).unwrap().unwrap();
        assert!(!ordinary.resident && !ordinary.hidden);
        let hidden = parse(["--resident", "--hidden", "--open", "status"].map(OsString::from)).unwrap().unwrap();
        assert!(hidden.resident && hidden.hidden);
        assert_eq!(hidden.open.as_deref(), Some("status"));
        for args in [
            vec!["--hidden"],
            vec!["--resident", "--selftest"],
            vec!["--resident", "--shutdown-check", "close"],
            vec!["--resident", "--logs"],
            vec!["--resident", "--live", "fixture"],
            vec!["--resident", "--confirm-demo"],
            vec!["--resident", "--resident"],
        ] {
            assert!(parse(args.into_iter().map(OsString::from)).is_err());
        }
        assert!(parse(["--open", &"x".repeat(257)].map(OsString::from)).is_err());
    }

    #[test]
    fn preview_and_selftest_modes_keep_all_explicit_options() {
        let options =
            parse(["--config", "ledger.json", "--data", "preview-data", "--open", "logs", "--selftest", "--live", "node", "--logs"].map(OsString::from))
                .unwrap()
                .unwrap();
        assert_eq!(options.config, PathBuf::from("ledger.json"));
        assert_eq!(options.data, PathBuf::from("preview-data"));
        assert_eq!(options.open.as_deref(), Some("logs"));
        assert_eq!(options.live_id.as_deref(), Some("node"));
        assert!(options.selftest && options.logs);
    }

    #[test]
    fn shutdown_diagnostic_requires_selftest_explicit_paths_and_known_mode() {
        for args in [
            vec!["--shutdown-check", "close"],
            vec!["--selftest", "--shutdown-check", "close"],
            vec!["--selftest", "--config", "fixture.json", "--shutdown-check", "close"],
            vec!["--selftest", "--config", "fixture.json", "--data", "fixture", "--shutdown-check", "unknown"],
        ] {
            assert!(parse(args.into_iter().map(OsString::from)).is_err());
        }
        for mode in ["close", "quit", "native-quit"] {
            let options =
                parse(["--selftest", "--config", "fixture.json", "--data", "fixture", "--shutdown-check", mode].map(OsString::from)).unwrap().unwrap();
            assert_eq!(options.shutdown_check.as_deref(), Some(mode));
        }
    }

    #[test]
    fn diagnostics_require_selftest_and_targets_cannot_be_duplicated() {
        for args in [vec!["--logs"], vec!["--live", "node"], vec!["--open"], vec!["--open", "logs", "--open", "node"]] {
            assert!(parse(args.into_iter().map(OsString::from)).is_err());
        }
        let options = parse([]).unwrap().unwrap();
        assert_eq!(options.data, tune_core::nodes::data_dir().join("gpui-preview"));
    }
}
