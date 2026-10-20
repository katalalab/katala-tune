use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::{Value, json};

use crate::db::now_ms;
use crate::engine::Engine;
use crate::topology::{
    Failure, FailureCode, ScopeStatus, TopologySnapshot,
    adapters::{
        assemble_capture_cached, capture_local_with_progress, capture_source_cache_keys,
        docker::{DockerRequest, collect_docker, engine_snapshots},
        github::{GithubRequest, collect_github},
        remote::{RemotePlatform, RemoteRequest, RemoteRuntimeKind, collect_remote},
    },
};

static NEXT_TOPOLOGY_RUN: AtomicU64 = AtomicU64::new(1);

fn remote_generation(request: &RemoteRequest) -> String {
    crate::db::sha256_hex(
        &serde_json::to_vec(&json!({
            "roots": request.roots,
            "configs": request.configs,
            "limits": request.limits,
            "platform": match request.platform {
                RemotePlatform::Posix => "posix",
                RemotePlatform::Windows => "windows",
            },
            "runtime": request.expected_runtime,
            "expected_distro": request.expected_distro,
            "alias": request.alias,
            "expected_identity_hash": request.expected_identity_hash,
            "python_program": request.python_program,
            "git_program": request.git_program,
        }))
        .unwrap_or_default(),
    )
}

struct TopologyRefreshGuard<'a> {
    engine: &'a Engine,
    lease: crate::db::TopologyLease,
}

impl Drop for TopologyRefreshGuard<'_> {
    fn drop(&mut self) {
        let _ = self.engine.with_db(|db| db.release_topology_lease_fenced(&self.lease));
        self.engine.topology_refreshing.store(false, Ordering::SeqCst);
    }
}

impl Engine {
    pub fn topology_view(&self) -> Result<Value, String> {
        self.with_db(|db| db.topology_view())
    }
    /// Refresh topology from explicit local roots. Remote environments remain a
    /// visible unsupported scope until their existing read-only transport is used.
    pub async fn topology_refresh(&self, trigger: &str) -> Value {
        self.topology_refresh_nodes(trigger, &[]).await
    }

    /// Limit a manual refresh to configured node IDs. A nonempty selection never
    /// triggers the global GitHub catalog collector.
    pub async fn topology_refresh_nodes(&self, trigger: &str, node_ids: &[String]) -> Value {
        let started_at = now_ms();
        let config = self.reload_config();
        let selected = node_ids.iter().cloned().collect::<HashSet<_>>();
        let unknown = selected.iter().filter(|id| !config.nodes.iter().any(|node| node.id.as_str() == id.as_str())).cloned().collect::<Vec<_>>();
        if !unknown.is_empty() {
            return json!({"schema":"katala_topology.v1","status":"failed","trigger":trigger,"started_at":started_at,"ended_at":now_ms(),"scopes":[],"failure":"unknown_node","unknown_node_ids":unknown});
        }
        if !config.topology.enabled {
            return json!({"schema":"katala_topology.v1","status":"disabled","trigger":trigger,"started_at":started_at,"ended_at":now_ms(),"scopes":[]});
        }
        if self.topology_refreshing.swap(true, Ordering::SeqCst) {
            return json!({"schema":"katala_topology.v1","status":"already_running","trigger":trigger,"started_at":started_at,"ended_at":now_ms(),"scopes":[]});
        }
        let sequence = NEXT_TOPOLOGY_RUN.fetch_add(1, Ordering::Relaxed);
        let run_id = format!("topology-{}", crate::db::sha256_hex(format!("{trigger}:{started_at}:{}:{sequence}", std::process::id()).as_bytes()));
        let lease_ttl_ms = i64::try_from(config.topology.limits.local_timeout_seconds.max(config.topology.limits.remote_timeout_seconds))
            .unwrap_or(3_600)
            .saturating_mul(2_000);
        let lease = self.with_db(|db| db.acquire_topology_lease_fenced(&run_id, started_at, lease_ttl_ms));
        let Ok(Some(lease)) = lease else {
            self.topology_refreshing.store(false, Ordering::SeqCst);
            return json!({"schema":"katala_topology.v1","status":"already_running","run_id":run_id,"trigger":trigger,"started_at":started_at,"ended_at":now_ms(),"scopes":[]});
        };
        let _guard = TopologyRefreshGuard { engine: self, lease: lease.clone() };
        let mut scopes = Vec::new();
        let mut entities = 0usize;
        let mut edges = 0usize;
        let mut changes = 0usize;
        for node in config.nodes.into_iter().filter(|node| node.is_active() && (selected.is_empty() || selected.contains(&node.id))) {
            let environments = node.topology().get("environments").and_then(Value::as_array).cloned().unwrap_or_default();
            for env in environments {
                let id = env.get("id").and_then(Value::as_str).unwrap_or("environment");
                let scope_id = format!("{}:{id}", node.id);
                let transport = env.get("transport").and_then(Value::as_str).unwrap_or("node");
                let roots = env
                    .get("roots")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(std::path::PathBuf::from)
                    .collect::<Vec<_>>();
                let configs = env.get("configs").and_then(Value::as_array).cloned().unwrap_or_default();
                let enabled = env.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                let excluded = |reason: &'static str| TopologySnapshot {
                    scope_id: scope_id.clone(),
                    status: ScopeStatus::Excluded,
                    entities: vec![],
                    edges: vec![],
                    failures: vec![Failure::new(FailureCode::Unsupported, reason)],
                    counters: Default::default(),
                    observed_at_ms: now_ms(),
                };
                let mut progress_to_save = None;
                let mut source_cache_saved = true;
                let mut snapshots = if !enabled {
                    vec![excluded("environment is disabled")]
                } else if node.local && transport == "node" {
                    let progress = self.with_db(|db| db.topology_progress(&scope_id)).ok().flatten();
                    let previous_fingerprint = progress.as_ref().and_then(|value| value.get("fingerprint")).and_then(Value::as_str).map(str::to_owned);
                    let previous_complete = progress.as_ref().and_then(|value| value.get("complete")).and_then(Value::as_bool) == Some(true);
                    let local_scope = scope_id.clone();
                    let local_roots = roots.clone();
                    let local_configs = configs.clone();
                    let local_limits = config.topology.limits.clone();
                    let mut scan = tokio::task::spawn_blocking(move || {
                        capture_local_with_progress(local_scope, &local_roots, &local_configs, &local_limits, progress.as_ref())
                    });
                    let heartbeat_every = std::time::Duration::from_millis((lease_ttl_ms / 3).clamp(1_000, 30_000) as u64);
                    let (capture, cursor) = loop {
                        tokio::select! {
                            result = &mut scan => match result {
                                Ok(value) => break value,
                                Err(_) => break (
                                    crate::topology::RawCapture {
                                        scope_id: scope_id.clone(), adapter: "local".into(), target_identity: None,
                                        observed_at_ms: now_ms(), limits: config.topology.limits.clone(), repositories: vec![],
                                        failures: vec![Failure::new(FailureCode::Internal, "local topology task failed")], counters: Default::default(), progress: None,
                                    },
                                    json!({"version":1,"abandoned":true}),
                                ),
                            },
                            _ = tokio::time::sleep(heartbeat_every) => {
                                if !self.with_db(|db| db.heartbeat_topology_lease(&lease, now_ms(), lease_ttl_ms)).unwrap_or(false) {
                                    scan.abort();
                                    break (
                                        crate::topology::RawCapture {
                                            scope_id: scope_id.clone(), adapter: "local".into(), target_identity: None,
                                            observed_at_ms: now_ms(), limits: config.topology.limits.clone(), repositories: vec![],
                                            failures: vec![Failure::new(FailureCode::Internal, "topology lease lost")], counters: Default::default(), progress: None,
                                        },
                                        json!({"version":1,"abandoned":true}),
                                    );
                                }
                            }
                        }
                    };
                    let complete_generation = previous_complete
                        && previous_fingerprint.as_deref() == cursor.get("fingerprint").and_then(Value::as_str)
                        && cursor.get("complete").and_then(Value::as_bool) == Some(true);
                    progress_to_save = Some(cursor);
                    let cached = self
                        .with_db(|db| {
                            Ok::<_, rusqlite::Error>(
                                capture_source_cache_keys(&capture)
                                    .into_iter()
                                    .filter_map(|key| db.source_cache(&key).ok().flatten().map(|entry| (key, entry.facts)))
                                    .collect::<HashMap<_, _>>(),
                            )
                        })
                        .unwrap_or_default();
                    let (mut snapshots, entries) = assemble_capture_cached(&capture, &cached);
                    for entry in entries {
                        if self.with_db(|db| db.save_source_cache_fenced(&lease, &entry, now_ms())).is_err() {
                            source_cache_saved = false;
                            scopes.push(json!({"scope_id":scope_id,"status":"failed","failure":"source_cache_save_failed"}));
                            break;
                        }
                    }
                    if complete_generation {
                        let prefix = format!("{scope_id}:checkout:");
                        let observed = snapshots.iter().map(|snapshot| snapshot.scope_id.clone()).collect::<HashSet<_>>();
                        let stale = self.with_db(|db| db.topology_scope_ids_with_prefix(&prefix)).unwrap_or_default();
                        for stale_scope in stale.into_iter().filter(|stale_scope| !observed.contains(stale_scope)) {
                            snapshots.push(TopologySnapshot {
                                scope_id: stale_scope,
                                status: ScopeStatus::Partial,
                                entities: vec![],
                                edges: vec![],
                                failures: vec![Failure::new(FailureCode::Unsupported, "checkout was not observed after a complete root-set scan")],
                                counters: capture.counters.clone(),
                                observed_at_ms: capture.observed_at_ms,
                            });
                        }
                    }
                    snapshots
                } else if transport == "ssh" {
                    let alias = env.get("alias").and_then(Value::as_str).unwrap_or(&node.alias).to_owned();
                    let platform = match env.get("platform").and_then(Value::as_str) {
                        Some("windows") => Some(RemotePlatform::Windows),
                        Some("posix") | None => Some(RemotePlatform::Posix),
                        Some(_) => None,
                    };
                    match platform {
                        Some(platform) => {
                            let python_program = env.get("python_program").and_then(Value::as_str).unwrap_or_default().to_owned();
                            let git_program = env.get("git_program").and_then(Value::as_str).map(str::to_owned);
                            let expected_identity_hash = env.get("expected_identity_hash").and_then(Value::as_str).map(str::to_owned);
                            let roots = env
                                .get("roots")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect::<Vec<_>>();
                            let stored_remote_progress = self.with_db(|db| db.topology_progress(&scope_id)).ok().flatten();
                            let expected_runtime =
                                if env.get("kind").and_then(Value::as_str) == Some("wsl") { RemoteRuntimeKind::Wsl } else { RemoteRuntimeKind::Native };
                            let expected_distro = env.get("expected_distro").and_then(Value::as_str).map(str::to_owned);
                            let mut request = RemoteRequest {
                                scope_id: scope_id.clone(),
                                alias,
                                expected_identity_hash,
                                platform,
                                expected_runtime,
                                expected_distro,
                                cursor: None,
                                roots,
                                configs: configs.clone(),
                                python_program,
                                git_program,
                                observed_at_ms: now_ms(),
                                limits: config.topology.limits.clone(),
                            };
                            let remote_generation = remote_generation(&request);
                            let remote_previous_complete = stored_remote_progress.as_ref().is_some_and(|value| {
                                value.get("complete").and_then(Value::as_bool) == Some(true)
                                    && value.get("engine_fingerprint").and_then(Value::as_str) == Some(remote_generation.as_str())
                            });
                            request.cursor = stored_remote_progress.filter(|value| value.get("complete").and_then(Value::as_bool) != Some(true));
                            let mut collection = std::pin::pin!(collect_remote(&request));
                            let heartbeat_every = std::time::Duration::from_millis((lease_ttl_ms / 3).clamp(1_000, 30_000) as u64);
                            let mut lease_lost = false;
                            let capture = loop {
                                tokio::select! {
                                    capture = &mut collection => break capture,
                                    _ = tokio::time::sleep(heartbeat_every) => {
                                        if !self.with_db(|db| db.heartbeat_topology_lease(&lease, now_ms(), lease_ttl_ms)).unwrap_or(false) {
                                            lease_lost = true;
                                            break crate::topology::RawCapture {
                                                scope_id: scope_id.clone(), adapter: "remote".into(), target_identity: None,
                                                observed_at_ms: now_ms(), limits: config.topology.limits.clone(), repositories: vec![],
                                                failures: vec![Failure::new(FailureCode::Internal, "topology lease lost")], counters: Default::default(), progress: None,
                                            };
                                        }
                                    }
                                }
                            };
                            if lease_lost {
                                vec![TopologySnapshot {
                                    scope_id: scope_id.clone(),
                                    status: ScopeStatus::Failed,
                                    entities: vec![],
                                    edges: vec![],
                                    failures: capture.failures,
                                    counters: capture.counters,
                                    observed_at_ms: capture.observed_at_ms,
                                }]
                            } else {
                                let current_complete = capture.failures.is_empty() && capture.progress.is_none();
                                progress_to_save = capture
                                    .progress
                                    .clone()
                                    .or_else(|| current_complete.then(|| json!({"version":1,"complete":true,"engine_fingerprint":remote_generation})));
                                let cached = self
                                    .with_db(|db| {
                                        Ok::<_, rusqlite::Error>(
                                            capture_source_cache_keys(&capture)
                                                .into_iter()
                                                .filter_map(|key| db.source_cache(&key).ok().flatten().map(|entry| (key, entry.facts)))
                                                .collect::<HashMap<_, _>>(),
                                        )
                                    })
                                    .unwrap_or_default();
                                let (mut snapshots, entries) = assemble_capture_cached(&capture, &cached);
                                for entry in entries {
                                    if self.with_db(|db| db.save_source_cache_fenced(&lease, &entry, now_ms())).is_err() {
                                        source_cache_saved = false;
                                        scopes.push(json!({"scope_id":scope_id,"status":"failed","failure":"source_cache_save_failed"}));
                                        break;
                                    }
                                }
                                if remote_previous_complete && current_complete {
                                    let prefix = format!("{scope_id}:checkout:");
                                    let observed = snapshots.iter().map(|snapshot| snapshot.scope_id.clone()).collect::<HashSet<_>>();
                                    let stale = self.with_db(|db| db.topology_scope_ids_with_prefix(&prefix)).unwrap_or_default();
                                    for stale_scope in stale.into_iter().filter(|stale_scope| !observed.contains(stale_scope)) {
                                        snapshots.push(TopologySnapshot {
                                            scope_id: stale_scope,
                                            status: ScopeStatus::Partial,
                                            entities: vec![],
                                            edges: vec![],
                                            failures: vec![Failure::new(FailureCode::Unsupported, "checkout was not observed after a complete root-set scan")],
                                            counters: capture.counters.clone(),
                                            observed_at_ms: capture.observed_at_ms,
                                        });
                                    }
                                }
                                snapshots
                            }
                        }
                        None => vec![excluded("remote platform is unsupported")],
                    }
                } else {
                    vec![excluded("transport is unsupported")]
                };
                let docker = env.get("docker").unwrap_or(&Value::Null);
                if enabled && docker.get("enabled").and_then(Value::as_bool) == Some(true) {
                    let docker_scope = format!("{scope_id}:docker");
                    let snapshot = if node.local && transport == "node" {
                        let contexts = docker
                            .get("contexts")
                            .and_then(Value::as_array)
                            .and_then(|rows| rows.iter().map(|v| v.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>())
                            .unwrap_or_default();
                        let request = DockerRequest {
                            scope_id: docker_scope.clone(),
                            contexts,
                            program: docker.get("program").map(|value| value.as_str().unwrap_or("").to_owned()),
                            observed_at_ms: now_ms(),
                            limits: config.topology.limits.clone(),
                        };
                        collect_docker(&request).await
                    } else {
                        let mut snapshot = excluded("Docker collection requires an explicit local native environment");
                        snapshot.scope_id = docker_scope;
                        snapshot
                    };
                    let mut docker_snapshots = engine_snapshots(snapshot);
                    let prefix = format!("{scope_id}:docker:engine:");
                    let observed = docker_snapshots.iter().map(|s| s.scope_id.clone()).collect::<HashSet<_>>();
                    let previous = self.with_db(|db| db.topology_scope_ids_with_prefix(&prefix)).unwrap_or_default();
                    for missing in previous.into_iter().filter(|s| !observed.contains(s)) {
                        docker_snapshots.push(TopologySnapshot {
                            scope_id: missing,
                            status: ScopeStatus::Partial,
                            entities: vec![],
                            edges: vec![],
                            failures: vec![Failure::new(FailureCode::Unsupported, "Docker engine was not observed in this collection")],
                            counters: Default::default(),
                            observed_at_ms: now_ms(),
                        });
                    }
                    snapshots.extend(docker_snapshots);
                }
                let mut snapshots_saved = true;
                for snapshot in snapshots {
                    entities += snapshot.entities.len();
                    edges += snapshot.edges.len();
                    let status = match snapshot.status {
                        ScopeStatus::Complete => "complete",
                        ScopeStatus::Partial => "partial",
                        ScopeStatus::Failed => "failed",
                        ScopeStatus::Excluded => "excluded",
                    };
                    let renewed = self.with_db(|db| db.heartbeat_topology_lease(&lease, now_ms(), lease_ttl_ms)).unwrap_or(false);
                    if !renewed {
                        scopes.push(json!({"scope_id":snapshot.scope_id,"status":"failed","failure":"lease_lost"}));
                        continue;
                    }
                    match self.with_db(|db| db.record_topology_fenced(&lease, &run_id, &snapshot, now_ms())) {
                        Ok(cs) => {
                            changes += cs.len();
                            scopes.push(json!({"scope_id":snapshot.scope_id,"status":status,"changes":cs.len(),"counters":snapshot.counters,"failures":snapshot.failures.iter().map(|failure| failure.code).collect::<Vec<_>>() }));
                        }
                        Err(_) => {
                            snapshots_saved = false;
                            scopes.push(json!({"scope_id":snapshot.scope_id,"status":"failed","failure":"internal"}));
                        }
                    }
                }
                if let Some(cursor) = progress_to_save {
                    if !source_cache_saved || !snapshots_saved {
                        scopes.push(json!({"scope_id":scope_id,"status":"partial","failure":"progress_withheld_after_fenced_save_failure"}));
                    } else if self.with_db(|db| db.save_topology_progress_fenced(&lease, &scope_id, &cursor, now_ms())).is_err() {
                        scopes.push(json!({"scope_id":scope_id,"status":"failed","failure":"progress_save_failed"}));
                    }
                }
            }
        }
        if selected.is_empty() && config.topology.github_enabled {
            let mut owner_key = config.topology.owners.iter().map(|owner| owner.to_ascii_lowercase()).collect::<Vec<_>>();
            owner_key.sort();
            owner_key.dedup();
            let cache_key = format!("github.v1.{}", crate::db::sha256_hex(owner_key.join("\n").as_bytes()));
            let cache = self.with_db(|db| db.github_cache(&cache_key)).ok().flatten().unwrap_or_default();
            let request = GithubRequest {
                scope_id: "github".into(),
                owners: config.topology.owners.clone(),
                allowed_owners: config.topology.owners.clone(),
                observed_at_ms: now_ms(),
                cache_minutes: config.topology.github_cache_minutes,
                limits: config.topology.limits.clone(),
                gh_program: None,
            };
            let mut collection = std::pin::pin!(collect_github(&request, &cache));
            let heartbeat_every = std::time::Duration::from_millis((lease_ttl_ms / 3).clamp(1_000, 30_000) as u64);
            let mut lease_lost = false;
            let mut collection = loop {
                tokio::select! {
                    collected = &mut collection => break collected,
                    _ = tokio::time::sleep(heartbeat_every) => {
                        if !self.with_db(|db| db.heartbeat_topology_lease(&lease, now_ms(), lease_ttl_ms)).unwrap_or(false) {
                            lease_lost = true;
                            break crate::topology::adapters::github::GithubCollection {
                                captures: vec![], cached_scopes: vec![], reused_blobs: vec![], cache_update: cache.clone(),
                                requests_attempted: 0, requests_succeeded: 0,
                            };
                        }
                    }
                }
            };
            if lease_lost {
                scopes.push(json!({"scope_id":"github","status":"failed","failure":"lease_lost"}));
            } else {
                let mut github_saved = true;
                for cached in collection.cached_scopes.drain(..) {
                    let renewed = self.with_db(|db| db.heartbeat_topology_lease(&lease, now_ms(), lease_ttl_ms)).unwrap_or(false);
                    if !renewed {
                        github_saved = false;
                        scopes.push(json!({"scope_id":cached.snapshot.scope_id,"status":"failed","failure":"lease_lost"}));
                        continue;
                    }
                    match self.with_db(|db| db.record_topology_fenced(&lease, &run_id, &cached.snapshot, now_ms())) {
                        Ok(cs) => {
                            entities += cached.snapshot.entities.len();
                            edges += cached.snapshot.edges.len();
                            changes += cs.len();
                            scopes.push(json!({"scope_id":cached.snapshot.scope_id,"status":"complete","changes":cs.len(),"cache":"reused","actual_fetched_at_ms":cached.actual_fetched_at_ms}));
                        }
                        Err(_) => {
                            github_saved = false;
                            scopes.push(json!({"scope_id":cached.snapshot.scope_id,"status":"failed","failure":"internal"}));
                        }
                    }
                }
                for capture in collection.captures {
                    let cached = self
                        .with_db(|db| {
                            Ok::<_, rusqlite::Error>(
                                capture_source_cache_keys(&capture)
                                    .into_iter()
                                    .filter_map(|key| db.source_cache(&key).ok().flatten().map(|entry| (key, entry.facts)))
                                    .collect::<HashMap<_, _>>(),
                            )
                        })
                        .unwrap_or_default();
                    let (snapshots, entries_to_save) = assemble_capture_cached(&capture, &cached);
                    for entry in entries_to_save {
                        if self.with_db(|db| db.save_source_cache_fenced(&lease, &entry, now_ms())).is_err() {
                            github_saved = false;
                            scopes.push(json!({"scope_id":capture.scope_id,"status":"failed","failure":"source_cache_save_failed"}));
                            break;
                        }
                    }
                    let mut complete_snapshot = None;
                    for snapshot in snapshots {
                        let status = match snapshot.status {
                            ScopeStatus::Complete => "complete",
                            ScopeStatus::Partial => "partial",
                            ScopeStatus::Failed => "failed",
                            ScopeStatus::Excluded => "excluded",
                        };
                        let renewed = self.with_db(|db| db.heartbeat_topology_lease(&lease, now_ms(), lease_ttl_ms)).unwrap_or(false);
                        if !renewed {
                            github_saved = false;
                            scopes.push(json!({"scope_id":snapshot.scope_id,"status":"failed","failure":"lease_lost"}));
                            continue;
                        }
                        match self.with_db(|db| db.record_topology_fenced(&lease, &run_id, &snapshot, now_ms())) {
                            Ok(cs) => {
                                entities += snapshot.entities.len();
                                edges += snapshot.edges.len();
                                changes += cs.len();
                                scopes.push(json!({"scope_id":snapshot.scope_id,"status":status,"changes":cs.len(),"counters":snapshot.counters,"failures":snapshot.failures.iter().map(|failure| failure.code).collect::<Vec<_>>() }));
                                if snapshot.status == ScopeStatus::Complete {
                                    complete_snapshot = Some(snapshot);
                                }
                            }
                            Err(_) => {
                                github_saved = false;
                                scopes.push(json!({"scope_id":snapshot.scope_id,"status":"failed","failure":"internal"}));
                            }
                        }
                    }
                    if capture.failures.is_empty()
                        && let Some(snapshot) = complete_snapshot
                        && let Some(entry) = collection.cache_update.repositories.iter_mut().find(|entry| {
                            crate::topology::adapters::github::repository_scope("github", &entry.repository.owner, &entry.repository.name) == capture.scope_id
                        })
                    {
                        entry.snapshot = Some(snapshot);
                    }
                }
                if github_saved
                    && self
                        .with_db(|db| {
                            db.save_github_cache_fenced(&lease, &cache_key, &collection.cache_update, collection.cache_update.catalog_fetched_at_ms, now_ms())
                        })
                        .is_err()
                {
                    scopes.push(json!({"scope_id":"github","status":"failed","failure":"github_cache_save_failed"}));
                }
            }
        }
        let completed = scopes.iter().filter(|scope| scope.get("status").and_then(Value::as_str) == Some("complete")).count();
        let incomplete = scopes.iter().any(|scope| matches!(scope.get("status").and_then(Value::as_str), Some("partial" | "failed" | "excluded")));
        let status = if completed > 0 && !incomplete { "complete" } else { "partial" };
        json!({"schema":"katala_topology.v1","status":status,"run_id":run_id,"trigger":trigger,"started_at":started_at,"ended_at":now_ms(),"scopes":scopes,"entities":entities,"edges":edges,"changes":changes})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_generation_fences_transport_identity_and_reader_programs() {
        let request = |alias: &str, identity: &str, python: &str, git: &str| RemoteRequest {
            scope_id: "fixture".into(),
            alias: alias.into(),
            expected_identity_hash: Some(identity.into()),
            platform: RemotePlatform::Posix,
            expected_runtime: RemoteRuntimeKind::Wsl,
            expected_distro: Some("ubuntu".into()),
            cursor: None,
            roots: vec!["/allowed".into()],
            configs: vec![json!({"path":"/selected.json","keys":[]})],
            python_program: python.into(),
            git_program: Some(git.into()),
            observed_at_ms: 1,
            limits: crate::topology::TopologyLimits::default(),
        };
        let baseline = remote_generation(&request("direct-ssh", "identity", "/usr/bin/python3", "/usr/bin/git"));
        for changed in [
            remote_generation(&request("other-ssh", "identity", "/usr/bin/python3", "/usr/bin/git")),
            remote_generation(&request("direct-ssh", "other", "/usr/bin/python3", "/usr/bin/git")),
            remote_generation(&request("direct-ssh", "identity", "/other/python", "/usr/bin/git")),
            remote_generation(&request("direct-ssh", "identity", "/usr/bin/python3", "/other/git")),
        ] {
            assert_ne!(baseline, changed);
        }
    }
}
