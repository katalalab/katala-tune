use super::validate_package;
use crate::topology::{DependencyKind, DependencySource, Failure, FailureCode, ParsedDependency, ParsedPackage, normalize_git_origin, sanitized_path_spec};
use toml::Value;

fn dependency_name(requirement: &str) -> String {
    requirement.trim().split(['[', '<', '>', '=', '!', '~', ';', '@', ' ']).next().unwrap_or_default().to_owned()
}

fn safe_registry_spec(raw: &str) -> bool {
    !raw.is_empty()
        && !raw.contains("://")
        && !raw.contains("file:")
        && raw.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'-' | b'_' | b'~' | b'<' | b'>' | b'=' | b'!' | b'[' | b']' | b';' | b',' | b'\'' | b'"' | b'(' | b')' | b' ')
        })
}

fn dependency(requirement: &str, kind: DependencyKind, extra: Option<&str>) -> Result<ParsedDependency, Failure> {
    let trimmed = requirement.trim();
    let (name_source, direct) = trimmed.split_once('@').map(|(name, reference)| (name.trim(), Some(reference.trim()))).unwrap_or((trimmed, None));
    let name = dependency_name(name_source);
    if name.is_empty() {
        return Err(Failure::new(FailureCode::Unsupported, "python dependency name is unsupported"));
    }
    let (source, spec, origin, local) = match direct {
        Some(reference) if reference.starts_with("file:") => {
            (DependencySource::Path, Some(sanitized_path_spec(reference.trim_start_matches("file:"))), None, true)
        }
        Some(reference) => {
            let origin =
                normalize_git_origin(reference).ok_or_else(|| Failure::new(FailureCode::Unsupported, "python direct dependency origin is unsupported"))?;
            (DependencySource::Git, None, Some(origin), false)
        }
        None if safe_registry_spec(trimmed) => (DependencySource::Registry, Some(trimmed.to_owned()), None, false),
        None => return Err(Failure::new(FailureCode::Unsupported, "python dependency spec is unsupported")),
    };
    Ok(ParsedDependency { name, alias: None, kind, local, source, spec, resolved: None, extra: extra.map(str::to_owned), origin, target: None })
}

fn add(out: &mut Vec<ParsedDependency>, values: Option<&Vec<Value>>, kind: DependencyKind, extra: Option<&str>) -> Result<(), Failure> {
    for value in values.into_iter().flatten() {
        let requirement = value.as_str().ok_or_else(|| Failure::new(FailureCode::Unsupported, "python dependency declaration is unsupported"))?;
        out.push(dependency(requirement, kind.clone(), extra)?);
    }
    Ok(())
}

pub fn parse_python(text: &str) -> Result<ParsedPackage, Failure> {
    let value: Value = toml::from_str(text).map_err(|_| Failure::new(FailureCode::ParseError, "invalid python manifest"))?;
    let project = value.get("project").and_then(Value::as_table);
    let name = project.and_then(|p| p.get("name")).and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut dependencies = Vec::new();
    add(&mut dependencies, project.and_then(|p| p.get("dependencies")).and_then(Value::as_array), DependencyKind::Normal, None)?;
    if let Some(groups) = project.and_then(|p| p.get("optional-dependencies")).and_then(Value::as_table) {
        for (group, values) in groups {
            add(&mut dependencies, values.as_array(), DependencyKind::PythonOptional, Some(group))?;
        }
    }
    add(
        &mut dependencies,
        value.get("build-system").and_then(Value::as_table).and_then(|p| p.get("requires")).and_then(Value::as_array),
        DependencyKind::Build,
        None,
    )?;
    dependencies.retain(|dependency| dependency.name != name);
    validate_package(ParsedPackage { name, dependencies, workspace_dependencies: Vec::new() })
}
