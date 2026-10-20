use super::{Failure, FailureCode, ParsedPackage};

mod cargo;
mod go;
mod node;
mod python;
mod workflow;

pub use cargo::parse_cargo;
pub use go::parse_go;
pub use node::parse_node;
pub use python::parse_python;
pub use workflow::parse_workflow;

const MAX_FACT_FIELD_BYTES: usize = 8 * 1024;
const MAX_PACKAGE_DECLARATIONS: usize = 4_096;

/// The typed parse cache is also a storage boundary. Reject oversized facts
/// rather than silently truncating names, aliases or specifications.
pub(crate) fn validate_package(package: ParsedPackage) -> Result<ParsedPackage, Failure> {
    if package.name.len() > MAX_FACT_FIELD_BYTES {
        return Err(Failure::new(FailureCode::LimitReached, "manifest package name exceeds field budget"));
    }
    let count = package.dependencies.len().saturating_add(package.workspace_dependencies.len());
    if count > MAX_PACKAGE_DECLARATIONS {
        return Err(Failure::new(FailureCode::LimitReached, "manifest declaration budget exceeded"));
    }
    for dependency in package.dependencies.iter().chain(&package.workspace_dependencies) {
        if [
            Some(dependency.name.as_str()),
            dependency.alias.as_deref(),
            dependency.spec.as_deref(),
            dependency.resolved.as_deref(),
            dependency.extra.as_deref(),
            dependency.origin.as_deref(),
            dependency.target.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.len() > MAX_FACT_FIELD_BYTES)
        {
            return Err(Failure::new(FailureCode::LimitReached, "manifest declaration field exceeds budget"));
        }
    }
    Ok(package)
}
