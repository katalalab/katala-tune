use super::validate_package;
use crate::topology::{DependencyKind, DependencySource, Failure, FailureCode, ParsedDependency, ParsedPackage, normalize_git_origin, sanitized_path_spec};
use serde_json::Value;

fn safe_registry_spec(raw: &str) -> Option<String> {
    (!raw.is_empty()
        && raw.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'^' | b'~' | b'*' | b'<' | b'>' | b'=' | b'|' | b' ')))
    .then(|| raw.to_owned())
}

fn safe_ref(raw: &str) -> Option<String> {
    let reference = raw.split_once('#')?.1;
    (!reference.is_empty() && reference.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/')))
        .then(|| format!("ref:{reference}"))
}

fn dependency(name: &str, raw: &str, kind: DependencyKind) -> Result<ParsedDependency, Failure> {
    let (source, spec, origin) = if let Some(path) = raw.strip_prefix("file:") {
        (DependencySource::Path, Some(sanitized_path_spec(path)), None)
    } else if raw.starts_with("workspace:") {
        let spec = raw
            .strip_prefix("workspace:")
            .filter(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'^' | b'~' | b'*'))
            })
            .ok_or_else(|| Failure::new(FailureCode::Unsupported, "node workspace dependency is unsupported"))?;
        (DependencySource::Workspace, Some(format!("workspace:{spec}")), None)
    } else if let Some(alias) = raw.strip_prefix("npm:") {
        if alias.is_empty() || alias.contains("://") || alias.contains("file:") {
            return Err(Failure::new(FailureCode::Unsupported, "node npm alias is unsupported"));
        }
        (DependencySource::NpmAlias, Some(format!("npm:{alias}")), None)
    } else if raw.starts_with("git+") || raw.contains("://") || raw.starts_with("git@") {
        let origin = normalize_git_origin(raw).ok_or_else(|| Failure::new(FailureCode::Unsupported, "node git dependency origin is unsupported"))?;
        (DependencySource::Git, safe_ref(raw), Some(origin))
    } else {
        let spec = safe_registry_spec(raw).ok_or_else(|| Failure::new(FailureCode::Unsupported, "node dependency spec is unsupported"))?;
        (DependencySource::Registry, Some(spec), None)
    };
    Ok(ParsedDependency {
        name: name.to_owned(),
        alias: (source == DependencySource::NpmAlias).then(|| name.to_owned()),
        kind,
        local: matches!(source, DependencySource::Path | DependencySource::Workspace),
        source,
        spec,
        resolved: None,
        extra: None,
        origin,
        target: None,
    })
}

pub fn parse_node(text: &str) -> Result<ParsedPackage, Failure> {
    let value: Value = serde_json::from_str(text).map_err(|_| Failure::new(FailureCode::ParseError, "invalid package manifest"))?;
    let name = value.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut dependencies = Vec::new();
    for (section, kind) in [
        ("dependencies", DependencyKind::Normal),
        ("devDependencies", DependencyKind::Dev),
        ("peerDependencies", DependencyKind::Peer),
        ("optionalDependencies", DependencyKind::Optional),
    ] {
        if let Some(table) = value.get(section).and_then(Value::as_object) {
            for (key, value) in table {
                if key == &name {
                    continue;
                }
                let raw = value.as_str().ok_or_else(|| Failure::new(FailureCode::Unsupported, "node dependency declaration is unsupported"))?;
                dependencies.push(dependency(key, raw, kind.clone())?);
            }
        }
    }
    validate_package(ParsedPackage { name, dependencies, workspace_dependencies: Vec::new() })
}
