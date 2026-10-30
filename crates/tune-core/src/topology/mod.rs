//! Bounded, read-only topology discovery.  Private roots and identities are
//! supplied by the node ledger and never become public defaults.

pub mod adapters;
pub mod candidates;
pub mod config;
pub mod export;
pub mod model;
pub mod parsers;
pub mod scan;
pub mod source_cache;

pub use model::{
    CaptureCounters, DependencyKind, DependencySource, Edge, EdgeKind, Entity, EntityKind, Failure, FailureCode, ParsedDependency, ParsedPackage, RawCapture,
    RawConfig, RawGitMetadata, RawManifest, RawRepository, ScopeStatus, TopologyLimits, TopologySnapshot, Workflow, WorkflowJob, canonical_edge_id,
    canonical_entity_id, canonical_id, is_absolute_path_any_platform, normalize_git_origin, normalize_path, sanitized_path_spec,
};
pub use source_cache::{SourceCacheEntry, SourceCacheKey, SourceFacts};
