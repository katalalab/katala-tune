//! Local-only, finite Docker topology collection. No mutating Docker verbs exist here.
use super::{CommandFuture, CommandRunner, failure_from_process};
use crate::{
    collect::{BoundedEnvironment, run_bounded_with_env_clean},
    topology::{CaptureCounters, Edge, EdgeKind, Entity, EntityKind, Failure, FailureCode, ScopeStatus, TopologyLimits, TopologySnapshot},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

const LOG_DRIVERS: &[&str] = &["json-file", "local", "journald", "syslog", "none", "fluentd", "awslogs", "splunk", "gelf", "etwlogs", "gcplogs"];
const MAX_CONTEXTS: usize = 8;
const MAX_OBJECTS: usize = 512;
const MAX_MEMBERS: usize = 128;
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_TOTAL: usize = 8 * 1024 * 1024;
const CONTAINER_FORMAT: &str = r#"{"id":{{json .Id}},"state":{{json .State.Status}},"health":{{with (index .State "Health")}}{{json (index . "Status")}}{{else}}null{{end}},"memory":{{json .HostConfig.Memory}},"nano_cpus":{{json .HostConfig.NanoCpus}},"cpu_period":{{json .HostConfig.CpuPeriod}},"cpu_quota":{{json .HostConfig.CpuQuota}},"cpu_shares":{{json .HostConfig.CpuShares}},"cpuset":{{json .HostConfig.CpusetCpus}},"restart":{{json .HostConfig.RestartPolicy.Name}},"restart_max":{{json .HostConfig.RestartPolicy.MaximumRetryCount}},"log_driver":{{json .HostConfig.LogConfig.Type}},"max_size":{{with (index .HostConfig.LogConfig "Config")}}{{json (index . "max-size")}}{{else}}null{{end}},"max_file":{{with (index .HostConfig.LogConfig "Config")}}{{json (index . "max-file")}}{{else}}null{{end}},"mounts":[{{range .Mounts}}{"kind":{{json .Type}},"source":{{json .Source}},"target":{{json .Destination}},"rw":{{json .RW}},"name":{{json (index . "Name")}}},{{end}}null],"networks":[{{range .NetworkSettings.Networks}}{{json .NetworkID}},{{end}}null],"ports":[{{range $p,$v := .NetworkSettings.Ports}}{{range $v}}{"container":{{json $p}},"host":{{json .HostPort}}},{{end}}{{end}}null]}"#;
const NETWORK_FORMAT: &str = r#"{"id":{{json .ID}},"driver":{{json .Driver}},"internal":{{json .Internal}}}"#;
const VOLUME_FORMAT: &str = r#"{"id":{{json .Name}},"driver":{{json .Driver}}}"#;

fn hash(value: &str) -> String {
    crate::db::sha256_hex(value.as_bytes())
}
fn bounded_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        && !value.starts_with('-')
        && ![".", ".."].contains(&value)
}
fn opaque(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
pub fn local_endpoint(value: &str) -> bool {
    bounded_text(value)
        && (value.strip_prefix("unix://").is_some_and(|path| path.starts_with('/') && !path.starts_with("//") && path.len() > 1)
            || value.strip_prefix("npipe:////./pipe/").is_some_and(token))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub kind: String,
    pub source_hash: Option<String>,
    pub target_hash: String,
    pub volume_hash: Option<String>,
    pub writable: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedPort {
    pub container_port: u16,
    pub protocol: String,
    pub host_port: u16,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub id: String,
    pub state: Option<String>,
    pub health: Option<String>,
    pub memory_bytes: Option<u64>,
    pub nano_cpus: Option<u64>,
    pub cpu_period: Option<i64>,
    pub cpu_quota: Option<i64>,
    pub cpu_shares: Option<u64>,
    pub cpuset_count: Option<usize>,
    pub restart_policy: Option<String>,
    pub restart_max: Option<u64>,
    pub log_driver: Option<String>,
    pub log_max_size: Option<String>,
    pub log_max_file: Option<u64>,
    pub current_unhealthy: bool,
    pub log_unbounded_candidate: bool,
    pub mounts: Vec<Mount>,
    pub networks: Vec<String>,
    pub ports: Vec<PublishedPort>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub id: String,
    pub driver: Option<String>,
    pub internal: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volume {
    pub id: String,
    pub driver: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerEngine {
    pub id: String,
    pub containers: Vec<Container>,
    pub networks: Vec<Network>,
    pub volumes: Vec<Volume>,
}
/// Sanitized import boundary. Raw inspect/config/log payloads are never accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerPacket {
    pub engines: Vec<DockerEngine>,
}

fn choice(value: &Value, choices: &[&str]) -> Option<String> {
    value.as_str().filter(|s| choices.contains(s)).map(str::to_owned)
}
/// Restricted Go numeric literal parsing, bounded by the caller's 24-byte cap.
/// Decimal and hexadecimal literals allow underscores only at digit boundaries
/// (or directly after 0x), never arbitrary option text.
fn size_amount(number: &str) -> Option<f64> {
    let unsigned = number.strip_prefix('+').unwrap_or(number);
    let hex = unsigned.starts_with("0x") || unsigned.starts_with("0X");
    let bytes = unsigned.as_bytes();
    for (index, _) in bytes.iter().enumerate().filter(|(_, b)| **b == b'_') {
        let digit = |b: &u8| if hex { b.is_ascii_hexdigit() } else { b.is_ascii_digit() };
        let left = index.checked_sub(1).and_then(|i| bytes.get(i));
        let right = bytes.get(index + 1);
        if !right.is_some_and(digit) || !(left.is_some_and(digit) || (hex && index == 2)) {
            return None;
        }
    }
    let clean = unsigned.replace('_', "");
    if !hex {
        let clean = number.replace('_', "");
        return clean.bytes().all(|b| b"0123456789.+-eE".contains(&b)).then(|| clean.parse::<f64>().ok()).flatten();
    }
    let (mantissa, exponent) = clean.get(2..)?.split_once(['p', 'P'])?;
    let exponent = exponent.parse::<i32>().ok()?;
    let mut amount = 0.0;
    let mut fractional = false;
    let mut factor = 1.0;
    let mut digits = 0;
    for character in mantissa.chars() {
        if character == '.' {
            if fractional {
                return None;
            }
            fractional = true;
        } else {
            let digit = f64::from(character.to_digit(16)?);
            digits += 1;
            if fractional {
                factor /= 16.0;
                amount += digit * factor;
            } else {
                amount = amount * 16.0 + digit;
            }
        }
    }
    (digits > 0).then(|| amount * 2_f64.powi(exponent))
}

fn size(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    // Retain a previously observed/default sentinel; current Moby rejects it
    // when explicitly configuring max-size. It is not a missing-value signal.
    if text == "-1" {
        return Some(text.into());
    }
    if text.is_empty() || text.len() > 24 || !text.is_ascii() {
        return None;
    }
    // go-units splits at the first space, or after the final digit/dot.
    let (number, suffix) = text.split_once(' ').or_else(|| {
        let split = text.rfind(|c: char| c.is_ascii_digit() || c == '.')? + 1;
        Some(text.split_at(split))
    })?;
    let amount = size_amount(number)?;
    let suffix = suffix.to_ascii_lowercase();
    let exponent = match suffix.as_str() {
        "" | "b" => 0,
        "k" | "kb" | "kib" => 1,
        "m" | "mb" | "mib" => 2,
        "g" | "gb" | "gib" => 3,
        "t" | "tb" | "tib" => 4,
        "p" | "pb" | "pib" => 5,
        _ => return None,
    };
    // json-file uses FromHumanSize (decimal multipliers), not RAMInBytes.
    let bytes = amount * 1000_f64.powi(exponent);
    (bytes.is_finite() && (0.0..9_223_372_036_854_775_808.0).contains(&bytes)).then(|| text.into())
}
fn numeric(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
fn array(value: &Value) -> Result<Vec<&Value>, &'static str> {
    let items = value.as_array().ok_or("missing projected array")?;
    if items.len() > MAX_MEMBERS + 1 {
        return Err("docker member budget exceeded");
    }
    let entries = items.iter().filter(|v| !v.is_null()).collect::<Vec<_>>();
    if entries.len() > MAX_MEMBERS {
        return Err("docker member budget exceeded");
    }
    Ok(entries)
}
fn private_hash(value: &Value) -> Result<String, &'static str> {
    value.as_str().filter(|s| bounded_text(s)).map(hash).ok_or("invalid docker identity")
}
pub fn parse_container(row: &Value) -> Result<Container, &'static str> {
    let id = private_hash(&row["id"])?;
    let state = choice(&row["state"], &["created", "running", "paused", "restarting", "removing", "exited", "dead"]);
    let health = choice(&row["health"], &["none", "starting", "healthy", "unhealthy"]);
    let log_driver = choice(&row["log_driver"], LOG_DRIVERS);
    let log_max_size = size(&row["max_size"]);
    if !row["max_size"].is_null() && row["max_size"].as_str() != Some("") && log_max_size.is_none() {
        return Err("invalid log size");
    }
    let log_unbounded_candidate = state.as_deref() == Some("running")
        && log_driver.as_deref() == Some("json-file")
        && (row["max_size"].is_null() || row["max_size"].as_str() == Some(""));
    let mut mounts = vec![];
    for m in array(&row["mounts"])? {
        let kind = choice(&m["kind"], &["bind", "volume", "tmpfs", "npipe", "cluster", "image"]).ok_or("invalid mount type")?;
        let volume_hash = if kind == "volume" { m["name"].as_str().filter(|s| !s.is_empty()).map(hash) } else { None };
        mounts.push(Mount {
            kind,
            source_hash: m["source"].as_str().filter(|s| !s.is_empty()).map(hash),
            target_hash: private_hash(&m["target"])?,
            volume_hash,
            writable: m["rw"].as_bool(),
        });
    }
    let networks = array(&row["networks"])?.into_iter().map(private_hash).collect::<Result<Vec<_>, _>>()?;
    let mut ports = vec![];
    for p in array(&row["ports"])? {
        let (number, protocol) = p["container"].as_str().and_then(|s| s.split_once('/')).ok_or("invalid port")?;
        if !["tcp", "udp", "sctp"].contains(&protocol) {
            return Err("invalid protocol");
        }
        ports.push(PublishedPort {
            container_port: number.parse().map_err(|_| "invalid port")?,
            protocol: protocol.into(),
            host_port: numeric(&p["host"]).and_then(|n| u16::try_from(n).ok()).ok_or("invalid port")?,
        });
    }
    let cpuset_count = row["cpuset"].as_str().filter(|s| s.len() <= 1024).and_then(|s| {
        if s.is_empty() {
            return Some(0);
        }
        s.split(',').try_fold(0usize, |count, part| {
            let (a, b) = part.split_once('-').unwrap_or((part, part));
            let a = a.parse::<usize>().ok()?;
            let b = b.parse::<usize>().ok()?;
            (b >= a && b <= 1_000_000).then(|| count.saturating_add(b - a + 1))
        })
    });
    Ok(Container {
        id,
        current_unhealthy: state.as_deref() == Some("running") && health.as_deref() == Some("unhealthy"),
        state,
        health,
        memory_bytes: row["memory"].as_u64(),
        nano_cpus: row["nano_cpus"].as_u64(),
        cpu_period: row["cpu_period"].as_i64(),
        cpu_quota: row["cpu_quota"].as_i64(),
        cpu_shares: row["cpu_shares"].as_u64(),
        cpuset_count,
        restart_policy: choice(&row["restart"], &["no", "always", "unless-stopped", "on-failure"]),
        restart_max: row["restart_max"].as_u64(),
        log_driver,
        log_max_size,
        log_max_file: numeric(&row["max_file"]),
        log_unbounded_candidate,
        mounts,
        networks,
        ports,
    })
}

impl DockerPacket {
    /// Validate sanitized DTOs before graph storage. Identities are already hashes.
    pub fn snapshot(&self, scope_id: String, observed_at_ms: i64) -> Result<TopologySnapshot, &'static str> {
        if serde_json::to_vec(self).map_err(|_| "invalid Docker DTO")?.len() > MAX_TOTAL {
            return Err("Docker DTO payload budget exceeded");
        }
        if self.engines.len() > MAX_CONTEXTS {
            return Err("docker engine budget exceeded");
        }
        let mut snapshot = TopologySnapshot {
            scope_id,
            observed_at_ms,
            status: ScopeStatus::Complete,
            entities: vec![],
            edges: vec![],
            failures: vec![],
            counters: CaptureCounters::default(),
        };
        let mut seen = HashSet::new();
        for engine in &self.engines {
            if !opaque(&engine.id) || engine.containers.len() > MAX_OBJECTS || engine.networks.len() > MAX_OBJECTS || engine.volumes.len() > MAX_OBJECTS {
                return Err("invalid docker engine");
            }
            if [
                &engine.containers.iter().map(|c| &c.id).collect::<Vec<_>>(),
                &engine.networks.iter().map(|n| &n.id).collect::<Vec<_>>(),
                &engine.volumes.iter().map(|v| &v.id).collect::<Vec<_>>(),
            ]
            .iter()
            .any(|ids| ids.iter().collect::<HashSet<_>>().len() != ids.len())
            {
                return Err("duplicate Docker object identity");
            }
            if !seen.insert(&engine.id) {
                continue;
            }
            let e = Entity::with_key(EntityKind::DockerEngine, "Docker engine", &engine.id);
            for network in &engine.networks {
                if !opaque(&network.id) || network.driver.as_deref().is_some_and(|d| !["bridge", "host", "null", "overlay", "macvlan", "ipvlan"].contains(&d)) {
                    return Err("invalid network DTO");
                }
                let mut n = Entity::with_key(EntityKind::DockerNetwork, "Docker network", &format!("{}:{}", engine.id, network.id));
                n.attributes = serde_json::to_value(network).map_err(|_| "invalid DTO")?;
                snapshot.edges.push(Edge::new(EdgeKind::DockerContains, &e.id, &n.id));
                snapshot.entities.push(n);
            }
            for volume in &engine.volumes {
                if !opaque(&volume.id) || volume.driver.as_deref().is_some_and(|d| d != "local") {
                    return Err("invalid volume DTO");
                }
                let mut v = Entity::with_key(EntityKind::DockerVolume, "Docker volume", &format!("{}:{}", engine.id, volume.id));
                v.attributes = serde_json::to_value(volume).map_err(|_| "invalid DTO")?;
                snapshot.edges.push(Edge::new(EdgeKind::DockerContains, &e.id, &v.id));
                snapshot.entities.push(v);
            }
            for c in &engine.containers {
                if !opaque(&c.id) || c.mounts.len() > MAX_MEMBERS || c.networks.len() > MAX_MEMBERS || c.ports.len() > MAX_MEMBERS {
                    return Err("invalid container DTO");
                }
                // Revalidate all externally imported strings/derived flags.
                if c.state.as_deref().is_some_and(|s| !["created", "running", "paused", "restarting", "removing", "exited", "dead"].contains(&s))
                    || c.health.as_deref().is_some_and(|s| !["none", "starting", "healthy", "unhealthy"].contains(&s))
                    || c.log_driver.as_deref().is_some_and(|s| !LOG_DRIVERS.contains(&s))
                    || c.log_max_size.as_ref().is_some_and(|s| size(&json!(s)).as_ref() != Some(s))
                    || c.restart_policy.as_deref().is_some_and(|s| !["no", "always", "unless-stopped", "on-failure"].contains(&s))
                {
                    return Err("invalid container policy");
                }
                if c.log_unbounded_candidate
                    != (c.state.as_deref() == Some("running") && c.log_driver.as_deref() == Some("json-file") && c.log_max_size.is_none())
                {
                    return Err("invalid log assertion");
                }
                if c.current_unhealthy != (c.state.as_deref() == Some("running") && c.health.as_deref() == Some("unhealthy")) {
                    return Err("invalid health assertion");
                }
                let mut entity = Entity::with_key(EntityKind::DockerContainer, "Docker container", &format!("{}:{}", engine.id, c.id));
                entity.attributes = serde_json::to_value(c).map_err(|_| "invalid DTO")?;
                snapshot.edges.push(Edge::new(EdgeKind::DockerContains, &e.id, &entity.id));
                for network in &c.networks {
                    if !opaque(network) {
                        return Err("invalid network hash");
                    }
                    if engine.networks.iter().any(|n| &n.id == network) {
                        let n = Entity::with_key(EntityKind::DockerNetwork, "Docker network", &format!("{}:{}", engine.id, network));
                        snapshot.edges.push(Edge::new(EdgeKind::DockerNetworkMember, &entity.id, &n.id));
                    }
                }
                for (i, m) in c.mounts.iter().enumerate() {
                    if !["bind", "volume", "tmpfs", "npipe", "cluster", "image"].contains(&m.kind.as_str())
                        || !opaque(&m.target_hash)
                        || m.source_hash.as_deref().is_some_and(|s| !opaque(s))
                        || m.volume_hash.as_deref().is_some_and(|s| !opaque(s))
                    {
                        return Err("invalid mount DTO");
                    }
                    let mut mount = Entity::with_key(EntityKind::DockerMount, "Docker mount", &format!("{}:{i}", entity.id));
                    mount.attributes = serde_json::to_value(m).map_err(|_| "invalid DTO")?;
                    snapshot.edges.push(Edge::new(EdgeKind::DockerMounts, &entity.id, &mount.id));
                    if m.kind == "volume" && !m.volume_hash.as_ref().is_some_and(|id| engine.volumes.iter().any(|v| &v.id == id)) {
                        snapshot.failures.push(Failure::new(FailureCode::Truncated, "volume mount target not observed"));
                    }
                    if let Some(volume) = m.volume_hash.as_ref().filter(|id| m.kind == "volume" && engine.volumes.iter().any(|v| &v.id == *id)) {
                        let v = Entity::with_key(EntityKind::DockerVolume, "Docker volume", &format!("{}:{}", engine.id, volume));
                        snapshot.edges.push(Edge::new(EdgeKind::DockerMounts, &mount.id, &v.id));
                    }
                    snapshot.entities.push(mount);
                }
                if c.ports.iter().any(|p| !["tcp", "udp", "sctp"].contains(&p.protocol.as_str())) {
                    return Err("invalid port DTO");
                }
                snapshot.entities.push(entity);
            }
            snapshot.entities.push(e);
        }
        snapshot.counters.entries = snapshot.entities.len();
        if let Some(failure) = unresolved_network_diagnostic(&snapshot.entities, &snapshot.edges) {
            snapshot.failures.push(failure);
        }
        if !snapshot.failures.is_empty() {
            snapshot.status = ScopeStatus::Partial;
        }
        Ok(snapshot)
    }
}

// Counts describe this candidate scope, not a graph write or a deletion cause.
// Only fixed state labels and numbers enter the persisted observation receipt.
fn unresolved_network_diagnostic(entities: &[Entity], edges: &[Edge]) -> Option<Failure> {
    const STATES: [&str; 8] = ["created", "running", "paused", "restarting", "removing", "exited", "dead", "unknown"];
    let mut memberships = HashMap::<&str, usize>::new();
    for edge in edges.iter().filter(|edge| edge.kind == EdgeKind::DockerNetworkMember) {
        *memberships.entry(edge.from.as_str()).or_default() += 1;
    }
    let mut counts = [0usize; 8];
    for container in entities.iter().filter(|entity| entity.kind == EntityKind::DockerContainer) {
        let expected = container.attributes["networks"].as_array().map_or(0, Vec::len);
        let observed = memberships.get(container.id.as_str()).copied().unwrap_or(0);
        let state = container.attributes["state"].as_str().and_then(|state| STATES.iter().position(|label| *label == state)).unwrap_or(7);
        counts[state] += expected.saturating_sub(observed);
    }
    let unresolved = counts.iter().sum::<usize>();
    (unresolved > 0).then(|| {
        let states = STATES.iter().zip(counts).map(|(state, count)| format!("{state}={count}")).collect::<Vec<_>>().join(",");
        Failure::new(
            FailureCode::Truncated,
            format!(
                "network membership target not observed; observed_entities={}; observed_edges={}; unresolved_references={unresolved}; {states}; scope_graph_applied=false",
                entities.len(),
                edges.len()
            ),
        )
    })
}

/// Coverage is separate from each fully observed engine: an excluded context
/// must not prevent a successful engine from reaching the fenced graph store.
pub fn engine_snapshots(mut coverage: TopologySnapshot) -> Vec<TopologySnapshot> {
    let mut result = vec![];
    for engine in coverage.entities.iter().filter(|e| e.kind == EntityKind::DockerEngine) {
        let mut ids = HashSet::from([engine.id.clone()]);
        loop {
            let before = ids.len();
            for edge in &coverage.edges {
                if ids.contains(&edge.from) {
                    ids.insert(edge.to.clone());
                }
            }
            if before == ids.len() {
                break;
            }
        }
        let entities = coverage.entities.iter().filter(|e| ids.contains(&e.id)).cloned().collect::<Vec<_>>();
        let edges = coverage.edges.iter().filter(|e| ids.contains(&e.from) && ids.contains(&e.to)).cloned().collect::<Vec<_>>();
        let mut failures = vec![];
        if entities.iter().filter(|e| e.kind == EntityKind::DockerMount && e.attributes["kind"] == "volume").any(|mount| {
            !mount.attributes["volume_hash"].is_string()
                || !edges.iter().any(|edge| {
                    edge.kind == EdgeKind::DockerMounts
                        && edge.from == mount.id
                        && entities.iter().any(|target| target.id == edge.to && target.kind == EntityKind::DockerVolume)
                })
        }) {
            failures.push(Failure::new(FailureCode::Truncated, "volume mount target not observed"));
        }
        let mut snapshot = TopologySnapshot {
            scope_id: format!("{}:engine:{}", coverage.scope_id, engine.id.rsplit('.').next().unwrap_or_default()),
            status: if failures.is_empty() { ScopeStatus::Complete } else { ScopeStatus::Partial },
            entities,
            edges,
            failures,
            counters: CaptureCounters::default(),
            observed_at_ms: coverage.observed_at_ms,
        };
        let environment =
            Entity::with_key(EntityKind::ExecutionEnvironment, "environment", coverage.scope_id.strip_suffix(":docker").unwrap_or(&coverage.scope_id));
        let host = Entity::with_key(EntityKind::Host, "host", coverage.scope_id.split(':').next().unwrap_or("local"));
        snapshot.edges.push(Edge::new(EdgeKind::ContainsEnvironment, &host.id, &environment.id));
        snapshot.entities.push(host);
        snapshot.edges.push(Edge::new(EdgeKind::DockerContains, &environment.id, &engine.id));
        snapshot.entities.push(environment);
        snapshot.counters.entries = snapshot.entities.len();
        if let Some(failure) = unresolved_network_diagnostic(&snapshot.entities, &snapshot.edges) {
            snapshot.failures.push(failure);
            snapshot.status = ScopeStatus::Partial;
        }
        result.push(snapshot);
    }
    coverage.entities.clear();
    coverage.edges.clear();
    result.insert(0, coverage);
    result
}

pub struct DockerRequest {
    pub scope_id: String,
    pub contexts: Vec<String>,
    pub program: Option<String>,
    pub observed_at_ms: i64,
    pub limits: TopologyLimits,
}
struct DockerRunner;
impl CommandRunner for DockerRunner {
    fn run<'a>(&'a self, program: &'a str, args: &'a [String], stdin: &'a [u8], timeout: Duration, stdout: usize, stderr: usize) -> CommandFuture<'a> {
        Box::pin(run_bounded_with_env_clean(
            program,
            args,
            stdin,
            timeout,
            stdout,
            stderr,
            BoundedEnvironment {
                set: &[],
                remove: &[
                    "DOCKER_HOST",
                    "DOCKER_CONTEXT",
                    "DOCKER_TLS",
                    "DOCKER_TLS_VERIFY",
                    "DOCKER_CERT_PATH",
                    "DOCKER_API_VERSION",
                    "DOCKER_CUSTOM_HEADERS",
                ],
            },
        ))
    }
}
pub fn trusted_path(path: &str) -> bool {
    #[cfg(not(windows))]
    let known = ["/usr/bin/docker", "/usr/local/bin/docker", "/opt/homebrew/bin/docker", "/Applications/Docker.app/Contents/Resources/bin/docker"];
    #[cfg(windows)]
    let known = [r"C:\Program Files\Docker\Docker\resources\bin\docker.exe", r"C:\Program Files\Docker\docker.exe"];
    if known.contains(&path) {
        return true;
    }
    #[cfg(not(windows))]
    if let Some(rest) = path.strip_prefix("/opt/homebrew/Cellar/docker/").or_else(|| path.strip_prefix("/usr/local/Cellar/docker/")) {
        return rest.split_once('/').is_some_and(|(version, tail)| token(version) && tail == "bin/docker");
    }
    false
}
pub async fn collect_docker(request: &DockerRequest) -> TopologySnapshot {
    let candidate = request.program.clone().or_else(|| {
        #[cfg(not(windows))]
        let choices = ["/usr/bin/docker", "/usr/local/bin/docker", "/opt/homebrew/bin/docker", "/Applications/Docker.app/Contents/Resources/bin/docker"];
        #[cfg(windows)]
        let choices = [r"C:\Program Files\Docker\Docker\resources\bin\docker.exe", r"C:\Program Files\Docker\docker.exe"];
        choices.iter().find(|p| Path::new(p).is_file()).map(|p| (*p).into())
    });
    let safe =
        candidate.as_deref().filter(|p| trusted_path(p)).and_then(|p| std::fs::canonicalize(p).ok()).and_then(|p| p.to_str().map(str::to_owned)).filter(|p| {
            #[cfg(windows)]
            let p = p.strip_prefix(r"\\?\").unwrap_or(p);
            trusted_path(p)
        });
    match safe {
        Some(program) => collect_docker_with(&DockerRunner, request, &program).await,
        None => failed(request, FailureCode::ToolMissing, "trusted Docker binary unavailable"),
    }
}
fn failed(request: &DockerRequest, code: FailureCode, message: &'static str) -> TopologySnapshot {
    TopologySnapshot {
        scope_id: request.scope_id.clone(),
        status: ScopeStatus::Failed,
        entities: vec![],
        edges: vec![],
        failures: vec![Failure::new(code, message)],
        counters: CaptureCounters::default(),
        observed_at_ms: request.observed_at_ms,
    }
}
struct Session<'a> {
    runner: &'a dyn CommandRunner,
    request: &'a DockerRequest,
    program: &'a str,
    deadline: std::time::Instant,
    bytes: usize,
    children: usize,
}
impl Session<'_> {
    async fn read(&mut self, args: Vec<String>) -> Result<Vec<u8>, Failure> {
        let left = self.deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(Failure::new(FailureCode::Timeout, "Docker collection deadline reached"));
        }
        let limit =
            self.request.limits.stdout_bytes.min(MAX_OUTPUT).min(self.request.limits.total_bytes_per_environment.min(MAX_TOTAL).saturating_sub(self.bytes));
        if limit == 0 {
            return Err(Failure::new(FailureCode::LimitReached, "Docker payload budget reached"));
        }
        self.children += 1;
        let out = self.runner.run(self.program, &args, b"", left.min(Duration::from_secs(5)), limit, self.request.limits.stderr_bytes.min(8192)).await;
        self.bytes += out.stdout.len() + out.stderr.len();
        if self.bytes > self.request.limits.total_bytes_per_environment.min(MAX_TOTAL) {
            return Err(Failure::new(FailureCode::LimitReached, "Docker cumulative payload budget reached"));
        }
        if let Some(mut failure) = failure_from_process(&out) {
            let diagnostic = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
            if failure.code == FailureCode::Internal
                && (diagnostic.contains("cannot connect to the docker daemon")
                    || (diagnostic.contains("failed to connect") && (diagnostic.contains("no such file") || diagnostic.contains("connect:"))))
            {
                failure = Failure::new(FailureCode::Unreachable, "Docker daemon is unavailable");
            }
            return Err(failure);
        }
        Ok(out.stdout)
    }
    async fn json(&mut self, args: Vec<String>) -> Result<Value, Failure> {
        let out = self.read(args).await?;
        serde_json::from_slice(&out).map_err(|_| Failure::new(FailureCode::ParseError, "invalid Docker projection"))
    }
    async fn lines(&mut self, args: Vec<String>) -> Result<Vec<Value>, Failure> {
        let out = self.read(args).await?;
        let text = std::str::from_utf8(&out).map_err(|_| Failure::new(FailureCode::ParseError, "invalid Docker projection"))?;
        let lines = text.lines().filter(|line| !line.is_empty()).collect::<Vec<_>>();
        if lines.len() > MAX_OBJECTS.min(self.request.limits.entries_per_environment) {
            return Err(Failure::new(FailureCode::LimitReached, "Docker object count exceeded"));
        }
        lines.iter().map(|line| serde_json::from_str(line).map_err(|_| Failure::new(FailureCode::ParseError, "invalid Docker projection"))).collect()
    }
}
/// Testable transport boundary. Caller supplies a trusted binary; production resolves it above.
pub async fn collect_docker_with(runner: &dyn CommandRunner, request: &DockerRequest, program: &str) -> TopologySnapshot {
    if request.contexts.is_empty() || request.contexts.len() > MAX_CONTEXTS || request.contexts.iter().any(|s| !token(s)) {
        return failed(request, FailureCode::PermissionDenied, "invalid explicit Docker context selection");
    }
    let start = std::time::Instant::now();
    let mut session =
        Session { runner, request, program, deadline: start + Duration::from_secs(request.limits.local_timeout_seconds.clamp(1, 60)), bytes: 0, children: 0 };
    let mut packet = DockerPacket { engines: vec![] };
    let mut failures = vec![];
    let mut excluded = 0;
    for context in &request.contexts {
        let endpoint =
            match session.json(vec!["context".into(), "inspect".into(), "--format".into(), "{{json .Endpoints.docker.Host}}".into(), context.clone()]).await {
                Ok(Value::String(value)) if local_endpoint(&value) => value,
                Ok(_) => {
                    failures.push(Failure::new(FailureCode::Unsupported, "Docker context excluded: nonlocal or unknown endpoint"));
                    excluded += 1;
                    continue;
                }
                Err(f) => {
                    failures.push(f);
                    continue;
                }
            };
        let prefix = ["--host".into(), endpoint];
        let args = |tail: &[&str]| prefix.iter().cloned().chain(tail.iter().map(|s| (*s).into())).collect::<Vec<_>>();
        let result = async {
            let identity = session.json(args(&["info", "--format", "{{json .ID}}"])).await?;
            let engine_id = private_hash(&identity).map_err(|_| Failure::new(FailureCode::ParseError, "Docker engine identity unknown"))?;
            if packet.engines.iter().any(|e| e.id == engine_id) {
                return Ok(None);
            }
            let raw_ids = session.read(args(&["container", "ls", "--all", "--quiet", "--no-trunc"])).await?;
            let text = std::str::from_utf8(&raw_ids).map_err(|_| Failure::new(FailureCode::ParseError, "invalid Docker container list"))?;
            let ids = text.lines().filter(|s| !s.is_empty()).collect::<Vec<_>>();
            if ids.len() > MAX_OBJECTS.min(request.limits.entries_per_environment)
                || ids.iter().any(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(Failure::new(FailureCode::LimitReached, "invalid or excessive Docker container list"));
            }
            let mut engine = DockerEngine { id: engine_id, containers: vec![], networks: vec![], volumes: vec![] };
            for id in ids {
                let row = session.json(args(&["container", "inspect", "--format", CONTAINER_FORMAT, id])).await?;
                engine.containers.push(parse_container(&row).map_err(|_| Failure::new(FailureCode::ParseError, "invalid Docker container projection"))?);
            }
            for row in session.lines(args(&["network", "ls", "--no-trunc", "--format", NETWORK_FORMAT])).await? {
                engine.networks.push(Network {
                    id: private_hash(&row["id"]).map_err(|_| Failure::new(FailureCode::ParseError, "invalid Docker network identity"))?,
                    driver: choice(&row["driver"], &["bridge", "host", "null", "overlay", "macvlan", "ipvlan"]),
                    internal: row["internal"].as_bool().or_else(|| match row["internal"].as_str() {
                        Some("true") => Some(true),
                        Some("false") => Some(false),
                        _ => None,
                    }),
                });
            }
            for row in session.lines(args(&["volume", "ls", "--format", VOLUME_FORMAT])).await? {
                engine.volumes.push(Volume {
                    id: private_hash(&row["id"]).map_err(|_| Failure::new(FailureCode::ParseError, "invalid Docker volume identity"))?,
                    driver: choice(&row["driver"], &["local"]),
                });
            }
            Ok(Some(engine))
        }
        .await;
        match result {
            Ok(Some(engine)) => packet.engines.push(engine),
            Ok(None) => {}
            Err(f) => failures.push(f),
        }
    }
    let mut snapshot =
        packet.snapshot(request.scope_id.clone(), request.observed_at_ms).unwrap_or_else(|_| failed(request, FailureCode::ParseError, "invalid Docker DTO"));
    snapshot.failures.extend(failures);
    if !snapshot.failures.is_empty() {
        snapshot.status = if excluded == request.contexts.len() {
            ScopeStatus::Excluded
        } else if packet.engines.is_empty() {
            ScopeStatus::Failed
        } else {
            ScopeStatus::Partial
        };
    }
    snapshot.counters.bytes = session.bytes;
    snapshot.counters.children = session.children;
    snapshot.counters.duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    snapshot
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    #[tokio::test]
    async fn bounded_owned_child_is_reaped_and_output_capped() {
        // The PID comes from a shell builtin; exec keeps that owned PID without Python startup.
        let out =
            DockerRunner.run("/bin/sh", &["-c".into(), "printf '%s\\n' \"$$\"; exec /bin/sleep 10".into()], b"", Duration::from_millis(150), 100, 100).await;
        assert!(out.timed_out);
        let pid = std::str::from_utf8(&out.stdout).unwrap().trim();
        assert!(pid.parse::<u32>().is_ok(), "owned PID invalid: stdout={:?}, stderr={:?}, code={:?}", out.stdout, out.stderr, out.code);
        let alive = std::process::Command::new("/bin/kill").args(["-0", pid]).output().unwrap();
        assert!(!alive.status.success(), "owned child must be gone after timeout");
        let out = DockerRunner.run("/bin/sh", &["-c".into(), "printf '%100000s' x".into()], b"", Duration::from_secs(2), 4096, 100).await;
        assert!(out.output_limited);
        assert_eq!(out.stdout.len(), 4096);
    }
}

#[cfg(test)]
mod hash_tests {
    use super::hash;

    #[test]
    fn docker_hash_matches_sha256_vectors_with_zero_padding() {
        for (value, expected) in [
            ("", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            ("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            ("s", "043a718774c572bd8a25adbeb1bfcd5c0256ae11cecf9f9c3f925d0e52beaf89"),
        ] {
            let actual = hash(value);
            assert_eq!(actual, expected);
            assert_eq!(actual.len(), 64);
            assert!(actual.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        }
    }
}
