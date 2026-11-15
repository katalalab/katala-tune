use std::ffi::OsString;
use std::path::PathBuf;

pub struct Options {
    pub config: PathBuf,
    pub data: PathBuf,
    pub selftest: bool,
}

pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let values: Vec<_> = arguments.into_iter().collect();
    if values.len() == 1 && matches!(values[0].to_str(), Some("--help" | "-h")) {
        return Ok(None);
    }
    let (mut config, mut data, mut selftest) = (None, None, false);
    let mut args = values.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--selftest") if !selftest => selftest = true,
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
    if selftest && (config.is_none() || data.is_none()) {
        return Err("--selftest requires --config and --data".into());
    }
    Ok(Some(Options {
        config: config.unwrap_or_else(tune_core::nodes::user_config_path),
        // Preview data stays separate from the existing application by default.
        data: data.unwrap_or_else(|| tune_core::nodes::data_dir().join("gpui-preview")),
        selftest,
    }))
}
