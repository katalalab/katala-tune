use super::validate_package;
use toml::{Table, Value};

use crate::topology::{DependencyKind, DependencySource, Failure, FailureCode, ParsedDependency, ParsedPackage, normalize_git_origin, sanitized_path_spec};

fn git_spec(table: &Table) -> Option<String> {
    for (key, prefix) in [("rev", "rev"), ("tag", "tag"), ("branch", "branch")] {
        if let Some(value) = table.get(key).and_then(Value::as_str).filter(|value| !value.is_empty()) {
            return Some(format!("{prefix}:{value}"));
        }
    }
    None
}

fn dependency(key: &str, value: &Value, kind: DependencyKind, target: Option<&str>) -> Result<ParsedDependency, Failure> {
    let table = value.as_table();
    let package = table.and_then(|table| table.get("package")).and_then(Value::as_str).unwrap_or(key);
    let workspace = table.and_then(|table| table.get("workspace")).and_then(Value::as_bool) == Some(true);
    let path = table.and_then(|table| table.get("path")).and_then(Value::as_str);
    let git = table.and_then(|table| table.get("git")).and_then(Value::as_str);
    let version = match value {
        Value::String(value) => Some(value.to_owned()),
        _ => table.and_then(|table| table.get("version")).and_then(Value::as_str).map(str::to_owned),
    };
    let (source, spec, origin) = if workspace {
        (DependencySource::Workspace, version, None)
    } else if let Some(path) = path {
        (DependencySource::Path, Some(sanitized_path_spec(path)), None)
    } else if let Some(git) = git {
        let origin = normalize_git_origin(git).ok_or_else(|| Failure::new(FailureCode::ParseError, "cargo git dependency origin is invalid"))?;
        (DependencySource::Git, table.and_then(git_spec), Some(origin))
    } else if value.is_str() || table.is_some() {
        (DependencySource::Registry, version, None)
    } else {
        return Err(Failure::new(FailureCode::Unsupported, "cargo dependency declaration is unsupported"));
    };
    Ok(ParsedDependency {
        name: package.to_owned(),
        alias: (package != key).then(|| key.to_owned()),
        kind,
        local: matches!(source, DependencySource::Path | DependencySource::Workspace),
        source,
        spec,
        resolved: None,
        extra: None,
        origin,
        target: target.map(str::to_owned),
    })
}

fn dependencies_from_table(
    out: &mut Vec<ParsedDependency>,
    table: Option<&Table>,
    kind: DependencyKind,
    target: Option<&str>,
    package_name: &str,
) -> Result<(), Failure> {
    for (key, value) in table.into_iter().flatten() {
        let dependency = dependency(key, value, kind.clone(), target)?;
        if dependency.name != package_name {
            out.push(dependency);
        }
    }
    Ok(())
}

pub fn parse_cargo(text: &str) -> Result<ParsedPackage, Failure> {
    let value: Value = toml::from_str(text).map_err(|_| Failure::new(FailureCode::ParseError, "invalid cargo manifest"))?;
    let name = value.get("package").and_then(Value::as_table).and_then(|table| table.get("name")).and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut dependencies = Vec::new();
    for (section, kind) in [("dependencies", DependencyKind::Normal), ("dev-dependencies", DependencyKind::Dev), ("build-dependencies", DependencyKind::Build)]
    {
        dependencies_from_table(&mut dependencies, value.get(section).and_then(Value::as_table), kind, None, &name)?;
    }
    if let Some(targets) = value.get("target").and_then(Value::as_table) {
        for (selector, target) in targets {
            let target = target.as_table().ok_or_else(|| Failure::new(FailureCode::Unsupported, "cargo target declaration is unsupported"))?;
            for (section, kind) in
                [("dependencies", DependencyKind::Normal), ("dev-dependencies", DependencyKind::Dev), ("build-dependencies", DependencyKind::Build)]
            {
                dependencies_from_table(&mut dependencies, target.get(section).and_then(Value::as_table), kind, Some(selector), &name)?;
            }
        }
    }
    let mut workspace_dependencies = Vec::new();
    let workspace = value.get("workspace").and_then(Value::as_table);
    dependencies_from_table(
        &mut workspace_dependencies,
        workspace.and_then(|workspace| workspace.get("dependencies")).and_then(Value::as_table),
        DependencyKind::Normal,
        None,
        "",
    )?;
    validate_package(ParsedPackage { name, dependencies, workspace_dependencies })
}
