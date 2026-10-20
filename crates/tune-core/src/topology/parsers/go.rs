use super::validate_package;
use std::collections::HashMap;

use crate::topology::{
    DependencyKind, DependencySource, Failure, FailureCode, ParsedDependency, ParsedPackage, is_absolute_path_any_platform, sanitized_path_spec,
};

#[derive(Clone)]
struct Replacement {
    target: String,
    version: Option<String>,
    local: bool,
}

fn safe_module(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/' | b'-' | b'_'))
}

fn parse_requirement(value: &str) -> Option<(String, Option<String>)> {
    let fields = value.split_whitespace().collect::<Vec<_>>();
    safe_module(fields.first()?).then(|| (fields[0].to_owned(), fields.get(1).map(|value| (*value).to_owned())))
}

fn parse_replacement(value: &str) -> Option<(String, Replacement)> {
    let (left, right) = value.split_once("=>")?;
    let (name, _) = parse_requirement(left.trim())?;
    let right = right.trim();
    let (target, version) = parse_requirement(right)?;
    let local = target.starts_with('.') || is_absolute_path_any_platform(&target);
    Some((name, Replacement { target, version, local }))
}

pub fn parse_go(text: &str) -> Result<ParsedPackage, Failure> {
    let mut name = String::new();
    let mut required = Vec::new();
    let mut replacements = HashMap::new();
    let mut require_block = false;
    let mut replace_block = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(value) = line.strip_prefix("module ") {
            name =
                parse_requirement(value).map(|(name, _)| name).ok_or_else(|| Failure::new(FailureCode::Unsupported, "go module declaration is unsupported"))?;
            continue;
        }
        if line == "require (" {
            require_block = true;
            continue;
        }
        if require_block && line == ")" {
            require_block = false;
            continue;
        }
        if let Some(value) = line.strip_prefix("require ") {
            required.push(parse_requirement(value).ok_or_else(|| Failure::new(FailureCode::Unsupported, "go require declaration is unsupported"))?);
            continue;
        }
        if require_block {
            required.push(parse_requirement(line).ok_or_else(|| Failure::new(FailureCode::Unsupported, "go require declaration is unsupported"))?);
            continue;
        }
        if line == "replace (" {
            replace_block = true;
            continue;
        }
        if replace_block && line == ")" {
            replace_block = false;
            continue;
        }
        if let Some(value) = line.strip_prefix("replace ").or(replace_block.then_some(line)) {
            let (dependency, replacement) =
                parse_replacement(value).ok_or_else(|| Failure::new(FailureCode::Unsupported, "go replace declaration is unsupported"))?;
            replacements.insert(dependency, replacement);
        }
    }
    if name.is_empty() {
        return Err(Failure::new(FailureCode::ParseError, "go.mod module is missing"));
    }
    required.sort_by(|left, right| left.0.cmp(&right.0));
    required.dedup_by(|left, right| left.0 == right.0);
    let dependencies = required
        .into_iter()
        .filter(|(dependency, _)| dependency != &name)
        .map(|(name, version)| {
            let replacement = replacements.get(&name);
            let local = replacement.is_some_and(|replacement| replacement.local);
            let resolved = replacement.map(|replacement| {
                if replacement.local {
                    sanitized_path_spec(&replacement.target)
                } else {
                    format!("module:{}{}", replacement.target, replacement.version.as_ref().map(|version| format!("@{version}")).unwrap_or_default())
                }
            });
            ParsedDependency {
                local,
                source: if local { DependencySource::Path } else { DependencySource::Registry },
                resolved,
                name,
                alias: None,
                kind: DependencyKind::Normal,
                spec: version,
                extra: None,
                origin: None,
                target: None,
            }
        })
        .collect();
    validate_package(ParsedPackage { name, dependencies, workspace_dependencies: Vec::new() })
}
