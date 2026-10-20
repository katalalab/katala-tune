//! Typed parse-cache values. Raw manifests and configuration bodies deliberately
//! have no representation here.

use serde::{Deserialize, Serialize};

use super::{ParsedDependency, ParsedPackage, Workflow};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SourceCacheKey {
    pub source_id: String,
    pub content_sha256: String,
    pub parser_version: String,
    pub kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceFacts {
    Package { name: String, dependencies: Vec<ParsedDependency>, workspace_dependencies: Vec<ParsedDependency> },
    Workflow { workflow: Workflow },
}

impl From<ParsedPackage> for SourceFacts {
    fn from(package: ParsedPackage) -> Self {
        Self::Package { name: package.name, dependencies: package.dependencies, workspace_dependencies: package.workspace_dependencies }
    }
}

impl From<Workflow> for SourceFacts {
    fn from(workflow: Workflow) -> Self {
        Self::Workflow { workflow }
    }
}

impl SourceFacts {
    pub fn as_package(&self) -> Option<ParsedPackage> {
        match self {
            Self::Package { name, dependencies, workspace_dependencies } => {
                Some(ParsedPackage { name: name.clone(), dependencies: dependencies.clone(), workspace_dependencies: workspace_dependencies.clone() })
            }
            Self::Workflow { .. } => None,
        }
    }

    pub fn as_workflow(&self) -> Option<Workflow> {
        match self {
            Self::Package { .. } => None,
            Self::Workflow { workflow } => Some(workflow.clone()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceCacheEntry {
    pub key: SourceCacheKey,
    /// Safe source revision/OID, never a local path or raw source text.
    pub revision: Option<String>,
    /// Time of the original successful fetch; cache hits must not change it.
    pub fetched_at_ms: i64,
    pub facts: SourceFacts,
}
