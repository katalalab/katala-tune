use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PARSER_VERSION: &str = "katala-topology-parser.v2";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    DockerEngine,
    DockerContainer,
    DockerNetwork,
    DockerVolume,
    DockerMount,
    Host,
    ExecutionEnvironment,
    Repository,
    Checkout,
    Worktree,
    Workspace,
    Package,
    Configuration,
    Workflow,
    Job,
    Artifact,
    Deployment,
    InstalledArtifact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    DockerContains,
    DockerNetworkMember,
    DockerMounts,
    ContainsEnvironment,
    ContainsCheckout,
    CheckoutOf,
    WorktreeOf,
    ContainsWorkspace,
    ContainsPackage,
    WorkspaceMember,
    DeclaresDependency,
    ResolvesTo,
    HasWorkflow,
    HasJob,
    NeedsJob,
    UsesAction,
    UsesWorkflow,
    RunsOn,
    ReadsConfig,
    InstalledFrom,
    DeploysTo,
    StoresIn,
    CollectsFrom,
}

impl EdgeKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::DockerContains => "docker_contains",
            Self::DockerNetworkMember => "docker_network_member",
            Self::DockerMounts => "docker_mounts",
            Self::ContainsEnvironment => "contains_environment",
            Self::ContainsCheckout => "contains_checkout",
            Self::CheckoutOf => "checkout_of",
            Self::WorktreeOf => "worktree_of",
            Self::ContainsWorkspace => "contains_workspace",
            Self::ContainsPackage => "contains_package",
            Self::WorkspaceMember => "workspace_member",
            Self::DeclaresDependency => "declares_dependency",
            Self::ResolvesTo => "resolves_to",
            Self::HasWorkflow => "has_workflow",
            Self::HasJob => "has_job",
            Self::NeedsJob => "needs_job",
            Self::UsesAction => "uses_action",
            Self::UsesWorkflow => "uses_workflow",
            Self::RunsOn => "runs_on",
            Self::ReadsConfig => "reads_config",
            Self::InstalledFrom => "installed_from",
            Self::DeploysTo => "deploys_to",
            Self::StoresIn => "stores_in",
            Self::CollectsFrom => "collects_from",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeStatus {
    Complete,
    Partial,
    Failed,
    Excluded,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub kind: EntityKind,
    pub label: String,
    pub attributes: serde_json::Value,
}
impl Entity {
    pub fn new(kind: EntityKind, label: &str) -> Self {
        Self::with_key(kind, label, label)
    }
    /// `key` is private canonical identity material; `label` is display-only.
    pub fn with_key(kind: EntityKind, label: &str, key: &str) -> Self {
        Self { id: canonical_entity_id(kind, key), kind, label: label.into(), attributes: serde_json::Value::Null }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub kind: EdgeKind,
    pub from: String,
    pub to: String,
    pub attributes: serde_json::Value,
}
impl Edge {
    pub fn new(kind: EdgeKind, from: &str, to: &str) -> Self {
        Self { id: canonical_edge_id(kind, from, to), kind, from: from.into(), to: to.into(), attributes: serde_json::Value::Null }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TopologySnapshot {
    pub scope_id: String,
    pub status: ScopeStatus,
    pub entities: Vec<Entity>,
    pub edges: Vec<Edge>,
    pub failures: Vec<Failure>,
    pub counters: CaptureCounters,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CaptureCounters {
    pub entries: usize,
    pub files: usize,
    pub bytes: usize,
    pub repositories: usize,
    pub manifests: usize,
    pub children: usize,
    #[serde(default)]
    pub config_files: usize,
    #[serde(default)]
    pub config_keys: usize,
    pub duration_ms: u64,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawCapture {
    pub scope_id: String,
    pub adapter: String,
    pub target_identity: Option<String>,
    pub observed_at_ms: i64,
    pub limits: TopologyLimits,
    pub repositories: Vec<RawRepository>,
    pub failures: Vec<Failure>,
    pub counters: CaptureCounters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<serde_json::Value>,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawRepository {
    pub relative_path: String,
    pub root_fingerprint: String,
    pub git: RawGitMetadata,
    pub manifests: Vec<RawManifest>,
    pub configs: Vec<RawConfig>,
    /// Only configuration pseudo-repositories set this after a bounded regular
    /// file read. It makes the provider counters independently verifiable.
    #[serde(default)]
    pub config_file_read: bool,
    pub failures: Vec<Failure>,
}

#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawGitMetadata {
    pub origin: Option<String>,
    pub head: Option<String>,
    pub git_dir: Option<String>,
    pub common_dir: Option<String>,
    pub is_worktree: bool,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawManifest {
    pub relative_path: String,
    pub kind: String,
    pub content: String,
    pub content_sha256: String,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RawConfig {
    pub relative_path: String,
    pub key: String,
    pub value_hash: String,
    pub mode: String,
}

impl EntityKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::DockerEngine => "docker_engine",
            Self::DockerContainer => "docker_container",
            Self::DockerNetwork => "docker_network",
            Self::DockerVolume => "docker_volume",
            Self::DockerMount => "docker_mount",
            Self::Host => "host",
            Self::ExecutionEnvironment => "environment",
            Self::Repository => "repository",
            Self::Checkout => "checkout",
            Self::Worktree => "worktree",
            Self::Workspace => "workspace",
            Self::Package => "package",
            Self::Configuration => "configuration",
            Self::Workflow => "workflow",
            Self::Job => "job",
            Self::Artifact => "artifact",
            Self::Deployment => "deployment",
            Self::InstalledArtifact => "installed_artifact",
        }
    }
}

fn id(namespace: &str, parts: &[&str]) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(namespace.as_bytes());
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    let hash = Sha256::digest(bytes);
    format!("kt.topology.v1.{namespace}.{}", hash.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

pub fn canonical_entity_id(kind: EntityKind, key: &str) -> String {
    id(kind.slug(), &[key.trim()])
}
/// Compatibility shorthand. New callers should name their canonical key explicitly.
pub fn canonical_id(kind: EntityKind, input: &str) -> String {
    canonical_entity_id(kind, input)
}
pub fn canonical_edge_id(kind: EdgeKind, from: &str, to: &str) -> String {
    id("edge", &[kind.slug(), from, to])
}

/// Strip credentials, query and fragment and map HTTPS/SSH/scp spellings to host/path.
pub fn normalize_git_origin(input: &str) -> Option<String> {
    let raw = input.trim().strip_prefix("git+").unwrap_or(input.trim());
    let raw = raw.split(['?', '#']).next()?.trim_end_matches('/');
    if raw.is_empty() {
        return None;
    }
    let (host, path) = if let Some((_, rest)) = raw.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit_once('@').map_or(authority, |(_, host)| host), path)
    } else if let Some((authority, path)) = raw.split_once(':').filter(|(authority, _)| authority.contains('@')) {
        (authority.rsplit_once('@').map_or(authority, |(_, host)| host), path)
    } else {
        raw.split_once('/')?
    };
    let path = path.trim_end_matches(".git").trim_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| format!("{}/{}", host.to_ascii_lowercase(), path))
}

pub fn is_absolute_path_any_platform(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\")
        || path.starts_with("//")
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'))
}

/// Preserve only a relative path. Absolute paths become non-reversible hashes
/// so parsed facts and their SQLite cache never contain private filesystem data.
pub fn sanitized_path_spec(path: &str) -> String {
    let windows = path.contains('\\') || (path.as_bytes().get(1) == Some(&b':'));
    let normalized = normalize_path(path, if windows { "windows" } else { "posix" });
    if is_absolute_path_any_platform(path) { format!("path-sha256:{}", crate::db::sha256_hex(normalized.as_bytes())) } else { format!("path:{normalized}") }
}

pub fn normalize_path(path: &str, platform: &str) -> String {
    if platform.eq_ignore_ascii_case("windows") {
        path.replace('\\', "/").split('/').filter(|part| !part.is_empty() && *part != ".").map(|part| part.to_ascii_lowercase()).collect::<Vec<_>>().join("/")
    } else {
        path.replace('\\', "/")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    Timeout,
    Unreachable,
    AuthUnavailable,
    PermissionDenied,
    ToolMissing,
    ParseError,
    RateLimited,
    Truncated,
    LimitReached,
    Unsupported,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub code: FailureCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
}

impl Failure {
    pub fn new(code: FailureCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), line: None, column: None }
    }

    pub fn at(code: FailureCode, message: impl Into<String>, line: Option<u64>, column: Option<u64>) -> Self {
        Self { code, message: message.into(), line, column }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DependencyKind {
    Normal,
    Dev,
    Build,
    Target,
    Workspace,
    Peer,
    Optional,
    PythonOptional,
}
impl PartialEq<&str> for DependencyKind {
    fn eq(&self, other: &&str) -> bool {
        self.slug() == *other
    }
}
impl DependencyKind {
    pub fn slug(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Dev => "dev",
            Self::Build => "build",
            Self::Target => "target",
            Self::Workspace => "workspace",
            Self::Peer => "peer",
            Self::Optional => "optional",
            Self::PythonOptional => "optional",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencySource {
    Registry,
    Git,
    Path,
    Workspace,
    NpmAlias,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParsedDependency {
    /// Resolved package/module name when the manifest supports an alias.
    pub name: String,
    /// Manifest key that declared this dependency, kept separately from `name`.
    pub alias: Option<String>,
    pub kind: DependencyKind,
    pub local: bool,
    pub source: DependencySource,
    pub spec: Option<String>,
    pub resolved: Option<String>,
    pub extra: Option<String>,
    pub origin: Option<String>,
    /// The Cargo target selector, when this declaration is target-specific.
    pub target: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParsedPackage {
    pub name: String,
    pub dependencies: Vec<ParsedDependency>,
    /// Workspace-level declarations remain declarations until assembly can match
    /// them to an observed member manifest.
    pub workspace_dependencies: Vec<ParsedDependency>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowJob {
    pub id: String,
    pub needs: Vec<String>,
    /// Retained for consumers that need every reference, while the fields below
    /// preserve whether it is a reusable workflow or an action step.
    pub uses: Vec<String>,
    pub job_uses: Option<String>,
    pub step_uses: Vec<String>,
    pub if_condition: Option<String>,
    pub runs_on: Vec<String>,
    pub environment: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub triggers: Vec<String>,
    /// Statically declared branch/tag/path filters. Dynamic expressions are not evaluated.
    pub trigger_filters: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<String>>>,
    /// Parser coverage limits, never interpreted as successful conditions.
    pub coverage: Vec<String>,
    pub jobs: Vec<WorkflowJob>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyLimits {
    pub roots_per_environment: usize,
    pub depth: usize,
    pub entries_per_environment: usize,
    pub repositories_per_environment: usize,
    pub manifests_per_environment: usize,
    pub file_bytes: usize,
    pub workflow_bytes: usize,
    pub total_bytes_per_environment: usize,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub local_timeout_seconds: u64,
    pub remote_timeout_seconds: u64,
    pub github_requests_per_run: usize,
}

impl Default for TopologyLimits {
    fn default() -> Self {
        Self {
            roots_per_environment: 8,
            depth: 8,
            entries_per_environment: 20_000,
            repositories_per_environment: 512,
            manifests_per_environment: 4_096,
            file_bytes: 1_048_576,
            workflow_bytes: 524_288,
            total_bytes_per_environment: 67_108_864,
            stdout_bytes: 16_777_216,
            stderr_bytes: 131_072,
            local_timeout_seconds: 60,
            remote_timeout_seconds: 90,
            github_requests_per_run: 128,
        }
    }
}
