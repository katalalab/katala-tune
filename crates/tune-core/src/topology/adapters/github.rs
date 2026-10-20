//! Bounded GraphQL reads through the operator's existing gh entrypoint.
use super::super::model::{CaptureCounters, RawCapture, RawGitMetadata, RawManifest, RawRepository};
use super::super::{Failure, FailureCode, TopologyLimits, TopologySnapshot};
use super::remote::{oid, safe_relative};
use super::{CommandRunner, SystemRunner, failure_from_process};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GithubRepository {
    pub owner: String,
    pub name: String,
    pub archived: bool,
    pub fork: bool,
    pub empty: bool,
    pub default_branch: Option<String>,
    pub head: Option<String>,
}
/// Only normalized graphs, safe metadata and object IDs may be persisted.
#[derive(Clone, Serialize, Deserialize)]
pub struct GithubCacheEntry {
    pub repository: GithubRepository,
    pub blobs: BTreeMap<String, String>,
    pub actual_fetched_at_ms: i64,
    pub snapshot: Option<TopologySnapshot>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct GithubCache {
    pub owners: Vec<String>,
    pub catalog_fetched_at_ms: i64,
    pub catalog_complete: bool,
    pub repositories: Vec<GithubCacheEntry>,
}
pub struct GithubRequest {
    pub scope_id: String,
    pub owners: Vec<String>,
    pub allowed_owners: Vec<String>,
    pub observed_at_ms: i64,
    pub cache_minutes: u64,
    pub limits: TopologyLimits,
    /// Optional explicit existing wrapper. Production defaults to ~/bin/gh before PATH.
    pub gh_program: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct GithubCachedScope {
    pub scope_id: String,
    pub repository: GithubRepository,
    pub actual_fetched_at_ms: i64,
    pub snapshot: TopologySnapshot,
    pub reused: bool,
}
/// Unchanged content can be reused from the caller's normalized per-source graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GithubReusedBlob {
    pub scope_id: String,
    pub relative_path: String,
    pub oid: String,
}
pub struct GithubCollection {
    pub captures: Vec<RawCapture>,
    pub cached_scopes: Vec<GithubCachedScope>,
    pub reused_blobs: Vec<GithubReusedBlob>,
    pub cache_update: GithubCache,
    pub requests_attempted: usize,
    pub requests_succeeded: usize,
}

pub fn repository_scope(base: &str, owner: &str, name: &str) -> String {
    format!("{base}:repo:{:x}", Sha256::digest(format!("{}/{}", owner.to_ascii_lowercase(), name.to_ascii_lowercase()).as_bytes()))
}
fn owner_valid(owner: &str) -> bool {
    !owner.is_empty() && owner.len() <= 39 && !owner.starts_with('-') && !owner.ends_with('-') && owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn name_valid(name: &str) -> bool {
    !name.is_empty() && name.len() <= 100 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn fresh(at: i64, now: i64, minutes: u64) -> bool {
    at > 0 && now >= at && (now - at) < i64::try_from(minutes.saturating_mul(60_000)).unwrap_or(i64::MAX)
}
fn normalized_owners(owners: &[String]) -> Vec<String> {
    let mut owners = owners.iter().map(|o| o.to_ascii_lowercase()).collect::<Vec<_>>();
    owners.sort();
    owners.dedup();
    owners
}
/// A new commit can retain every relevant blob. Reuse the normalized graph only
/// after carrying forward the fresh HEAD observation; blob fetch time remains
/// the old cache receipt in `actual_fetched_at_ms`.
fn refresh_snapshot_metadata(mut snapshot: TopologySnapshot, old: &GithubRepository, current: &GithubRepository, observed_at_ms: i64) -> TopologySnapshot {
    snapshot.observed_at_ms = observed_at_ms;
    let refresh = |attributes: &mut Value| {
        let Some(attributes) = attributes.as_object_mut() else { return };
        if attributes.get("observed_at_ms").is_some() {
            attributes.insert("observed_at_ms".into(), json!(observed_at_ms));
        }
        for key in ["revision", "head"] {
            if let (Some(previous), Some(next)) = (old.head.as_deref(), current.head.as_deref())
                && attributes.get(key).and_then(Value::as_str) == Some(previous)
            {
                attributes.insert(key.into(), Value::String(next.into()));
            }
        }
    };
    for entity in &mut snapshot.entities {
        refresh(&mut entity.attributes);
    }
    for edge in &mut snapshot.edges {
        refresh(&mut edge.attributes);
    }
    snapshot
}

fn blank(request: &GithubRequest, scope: String) -> RawCapture {
    RawCapture {
        scope_id: scope,
        adapter: "github".into(),
        target_identity: None,
        observed_at_ms: request.observed_at_ms,
        limits: request.limits.clone(),
        repositories: vec![],
        failures: vec![],
        counters: CaptureCounters::default(),
        progress: None,
    }
}
fn fail_capture(request: &GithubRequest, failure: Failure) -> RawCapture {
    let mut capture = blank(request, request.scope_id.clone());
    capture.failures.push(failure);
    capture
}

struct Api<'a> {
    runner: &'a dyn CommandRunner,
    request: &'a GithubRequest,
    program: String,
    attempted: usize,
    succeeded: usize,
    started: Instant,
}
impl Api<'_> {
    async fn query(&mut self, query: String, variables: Value) -> Result<Value, Failure> {
        if self.attempted >= self.request.limits.github_requests_per_run {
            return Err(Failure::new(FailureCode::LimitReached, "GitHub request budget exceeded"));
        }
        let timeout = Duration::from_secs(self.request.limits.remote_timeout_seconds);
        let remaining = timeout
            .checked_sub(self.started.elapsed())
            .filter(|time| !time.is_zero())
            .ok_or_else(|| Failure::new(FailureCode::Timeout, "GitHub run timed out"))?;
        self.attempted += 1;
        let input = serde_json::to_vec(&json!({"query":query,"variables":variables})).unwrap_or_default();
        let output = self
            .runner
            .run(
                &self.program,
                &["api".into(), "graphql".into(), "--input".into(), "-".into()],
                &input,
                remaining,
                self.request.limits.stdout_bytes,
                self.request.limits.stderr_bytes,
            )
            .await;
        if let Some(failure) = failure_from_process(&output) {
            return Err(failure);
        }
        if output.stdout.contains(&0) {
            return Err(Failure::new(FailureCode::ParseError, "GitHub report contains invalid NUL"));
        }
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| Failure::new(FailureCode::ParseError, "GitHub response invalid"))?;
        if let Some(errors) = value.get("errors").and_then(Value::as_array).filter(|errors| !errors.is_empty()) {
            let limited = errors.iter().any(|e| {
                e.get("type").and_then(Value::as_str).is_some_and(|t| t.contains("RATE_LIMIT"))
                    || e.get("message").and_then(Value::as_str).is_some_and(|m| m.to_ascii_lowercase().contains("rate limit"))
            });
            return Err(Failure::new(if limited { FailureCode::RateLimited } else { FailureCode::PermissionDenied }, "GitHub returned partial errors"));
        }
        let data = value.get("data").filter(|v| v.is_object()).cloned().ok_or_else(|| Failure::new(FailureCode::ParseError, "GitHub data unavailable"))?;
        if data.get("rateLimit").and_then(|r| r.get("remaining")).and_then(Value::as_u64) == Some(0) {
            return Err(Failure::new(FailureCode::RateLimited, "GitHub rate budget exhausted"));
        }
        self.succeeded += 1;
        Ok(data)
    }
}

pub async fn collect_github(request: &GithubRequest, cache: &GithubCache) -> GithubCollection {
    collect_github_with(&SystemRunner, request, cache).await
}

pub async fn collect_github_with(runner: &dyn CommandRunner, request: &GithubRequest, cache: &GithubCache) -> GithubCollection {
    let mut result = GithubCollection {
        captures: vec![],
        cached_scopes: vec![],
        reused_blobs: vec![],
        cache_update: cache.clone(),
        requests_attempted: 0,
        requests_succeeded: 0,
    };
    let owners = normalized_owners(&request.owners);
    let allowed = normalized_owners(&request.allowed_owners);
    if owners.is_empty() || request.owners.iter().any(|o| !owner_valid(o)) || owners.iter().any(|o| !allowed.contains(o)) {
        result.captures.push(fail_capture(request, Failure::new(FailureCode::PermissionDenied, "GitHub owner outside allowlist")));
        return result;
    }
    let cache_matches = normalized_owners(&cache.owners) == owners && cache.catalog_complete;
    if cache_matches
        && fresh(cache.catalog_fetched_at_ms, request.observed_at_ms, request.cache_minutes)
        && cache.repositories.iter().all(|entry| entry.snapshot.is_some())
    {
        for entry in &cache.repositories {
            result.cached_scopes.push(GithubCachedScope {
                scope_id: repository_scope(&request.scope_id, &entry.repository.owner, &entry.repository.name),
                repository: entry.repository.clone(),
                actual_fetched_at_ms: entry.actual_fetched_at_ms,
                snapshot: entry.snapshot.clone().unwrap(),
                reused: true,
            });
        }
        return result;
    }
    let wrapper = crate::nodes::home().join("bin/gh");
    let program = request.gh_program.clone().unwrap_or_else(|| if wrapper.is_file() { wrapper.to_string_lossy().into_owned() } else { "gh".into() });
    let mut api = Api { runner, request, program, attempted: 0, succeeded: 0, started: Instant::now() };
    let mut catalog = Vec::new();
    let mut catalog_complete = true;
    for owner in &owners {
        let mut cursor = Value::Null;
        let mut cursors = BTreeSet::new();
        loop {
            let query = "query($owner:String!,$cursor:String){repositoryOwner(login:$owner){repositories(first:100,after:$cursor,ownerAffiliations:[OWNER],orderBy:{field:NAME,direction:ASC}){nodes{name nameWithOwner isArchived isFork isEmpty defaultBranchRef{name target{oid}}}pageInfo{hasNextPage endCursor}}}rateLimit{remaining resetAt}}".to_string();
            let data = match api.query(query, json!({"owner":owner,"cursor":cursor})).await {
                Ok(data) => data,
                Err(failure) => {
                    result.captures.push(fail_capture(request, failure));
                    catalog_complete = false;
                    break;
                }
            };
            let connection = &data["repositoryOwner"]["repositories"];
            let Some(nodes) = connection["nodes"].as_array().filter(|nodes| nodes.len() <= 100) else {
                result.captures.push(fail_capture(request, Failure::new(FailureCode::PermissionDenied, "GitHub catalog unavailable")));
                catalog_complete = false;
                break;
            };
            let mut repository_limit_hit = false;
            for node in nodes {
                let Some(name) = node["name"].as_str().filter(|name| name_valid(name)) else {
                    catalog_complete = false;
                    continue;
                };
                if !node["nameWithOwner"].as_str().is_some_and(|full| full.eq_ignore_ascii_case(&format!("{owner}/{name}"))) {
                    catalog_complete = false;
                    continue;
                }
                let empty = node["isEmpty"].as_bool();
                let archived = node["isArchived"].as_bool();
                let fork = node["isFork"].as_bool();
                if empty.is_none() || archived.is_none() || fork.is_none() {
                    catalog_complete = false;
                    continue;
                }
                let head = node["defaultBranchRef"]["target"]["oid"].as_str().map(str::to_owned);
                if !empty.unwrap() && !head.as_deref().is_some_and(oid) {
                    catalog_complete = false;
                }
                if catalog.len() >= request.limits.repositories_per_environment {
                    repository_limit_hit = true;
                    break;
                }
                catalog.push(GithubRepository {
                    owner: owner.clone(),
                    name: name.to_owned(),
                    empty: empty.unwrap(),
                    archived: archived.unwrap(),
                    fork: fork.unwrap(),
                    default_branch: node["defaultBranchRef"]["name"].as_str().map(str::to_owned),
                    head,
                });
            }
            if repository_limit_hit || (catalog.len() == request.limits.repositories_per_environment && connection["pageInfo"]["hasNextPage"] == true) {
                catalog_complete = false;
                result.captures.push(fail_capture(request, Failure::new(FailureCode::LimitReached, "GitHub repository budget exceeded")));
                break;
            }
            match connection["pageInfo"]["hasNextPage"].as_bool() {
                Some(false) => break,
                Some(true) => {
                    let Some(next) = connection["pageInfo"]["endCursor"].as_str().filter(|c| c.len() <= 4096) else {
                        catalog_complete = false;
                        break;
                    };
                    if !cursors.insert(next.to_owned()) {
                        catalog_complete = false;
                        break;
                    }
                    cursor = json!(next);
                }
                None => {
                    catalog_complete = false;
                    break;
                }
            }
        }
    }
    catalog.sort_by(|a, b| (&a.owner, &a.name).cmp(&(&b.owner, &b.name)));
    catalog.dedup_by(|a, b| a.owner == b.owner && a.name == b.name);
    if catalog.len() > request.limits.repositories_per_environment {
        catalog.truncate(request.limits.repositories_per_environment);
        catalog_complete = false;
        result.captures.push(fail_capture(request, Failure::new(FailureCode::LimitReached, "GitHub repository budget exceeded")));
    }
    let mut captures = Vec::new();
    let mut updates = Vec::new();
    let mut queue = VecDeque::new();
    for repository in catalog {
        let scope = repository_scope(&request.scope_id, &repository.owner, &repository.name);
        let old = cache
            .repositories
            .iter()
            .find(|old| old.repository.owner.eq_ignore_ascii_case(&repository.owner) && old.repository.name.eq_ignore_ascii_case(&repository.name));
        if let Some(old) = old.filter(|old| old.repository.head == repository.head && old.snapshot.is_some()) {
            result.cached_scopes.push(GithubCachedScope {
                scope_id: scope,
                repository: repository.clone(),
                actual_fetched_at_ms: old.actual_fetched_at_ms,
                snapshot: old.snapshot.clone().unwrap(),
                reused: true,
            });
            let mut update = old.clone();
            update.repository = repository;
            updates.push(update);
            continue;
        }
        let index = captures.len();
        let mut capture = blank(request, scope);
        capture.repositories.push(RawRepository {
            relative_path: ".".into(),
            root_fingerprint: format!("{:x}", Sha256::digest(format!("github/{}/{}", repository.owner, repository.name).as_bytes())),
            git: RawGitMetadata {
                origin: Some(format!("github.com/{}/{}", repository.owner, repository.name)),
                head: repository.head.clone(),
                ..Default::default()
            },
            manifests: vec![],
            configs: vec![],
            config_file_read: false,
            failures: vec![],
        });
        if let Some(head) = &repository.head {
            queue.push_back(TreeTask { index, path: String::new(), depth: 0, oid: None, head: head.clone() });
        } else if !repository.empty {
            capture.failures.push(Failure::new(FailureCode::PermissionDenied, "GitHub HEAD unavailable"));
        }
        updates.push(GithubCacheEntry { repository, blobs: BTreeMap::new(), actual_fetched_at_ms: request.observed_at_ms, snapshot: None });
        captures.push(capture);
    }
    // Map each active capture to metadata without relying on catalog/cache ordering.
    let metas = captures
        .iter()
        .map(|capture| {
            updates
                .iter()
                .find(|entry| repository_scope(&request.scope_id, &entry.repository.owner, &entry.repository.name) == capture.scope_id)
                .unwrap()
                .repository
                .clone()
        })
        .collect::<Vec<_>>();
    let root_tasks = queue;
    let mut entries = 0usize;
    let mut manifest_count = 0usize;
    let mut bytes = 0usize;
    let mut reused = BTreeSet::new();
    // Finish each small repository batch before spending budget on the next.
    // Completed cache scopes therefore make durable progress across limited runs.
    for batch_start in (0..captures.len()).step_by(16) {
        let batch_end = (batch_start + 16).min(captures.len());
        let mut queue = root_tasks.iter().filter(|task| task.index >= batch_start && task.index < batch_end).cloned().collect::<VecDeque<_>>();
        let mut blobs = Vec::new();
        if api.attempted >= request.limits.github_requests_per_run || entries >= request.limits.entries_per_environment {
            for capture in &mut captures[batch_start..batch_end] {
                if capture.repositories[0].git.head.is_some() {
                    capture.failures.push(Failure::new(FailureCode::LimitReached, "GitHub scope deferred by run budget"));
                }
            }
            continue;
        }
        while !queue.is_empty() {
            // Keep room to finish blobs already discovered in this batch. A
            // large sibling tree must not starve repositories whose trees ended.
            let blob_requests = blobs.len().div_ceil(16).max(1);
            if api.attempted.saturating_add(1).saturating_add(blob_requests) > request.limits.github_requests_per_run {
                for task in &queue {
                    captures[task.index].failures.push(Failure::new(FailureCode::LimitReached, "GitHub subtree deferred by run budget"));
                }
                break;
            }
            let tasks = (0..16).filter_map(|_| queue.pop_front()).collect::<Vec<_>>();
            let query = tree_query(&tasks, &metas);
            let data = match api.query(query, json!({})).await {
                Ok(data) => data,
                Err(failure) => {
                    for capture in &mut captures[batch_start..batch_end] {
                        capture.failures.push(failure.clone());
                    }
                    break;
                }
            };
            for (slot, task) in tasks.iter().enumerate() {
                let object = &data[format!("r{slot}")]["object"];
                if task.oid.as_deref().is_some_and(|expected| object["oid"].as_str() != Some(expected)) {
                    captures[task.index].failures.push(Failure::new(FailureCode::ParseError, "GitHub tree OID mismatch"));
                    continue;
                }
                let Some(children) = object["entries"].as_array() else {
                    captures[task.index].failures.push(Failure::new(FailureCode::PermissionDenied, "GitHub tree unavailable"));
                    continue;
                };
                for child in children {
                    entries = entries.saturating_add(1);
                    captures[task.index].counters.entries += 1;
                    if entries > request.limits.entries_per_environment {
                        for capture in &mut captures[batch_start..batch_end] {
                            capture.failures.push(Failure::new(FailureCode::LimitReached, "GitHub tree entry budget exceeded"));
                        }
                        queue.clear();
                        break;
                    }
                    let Some(name) = child["name"].as_str().filter(|n| !n.contains('/') && safe_relative(n)) else {
                        captures[task.index].failures.push(Failure::new(FailureCode::ParseError, "GitHub tree entry invalid"));
                        continue;
                    };
                    let path = if task.path.is_empty() { name.to_owned() } else { format!("{}/{name}", task.path) };
                    let Some(object_oid) = child["oid"].as_str().filter(|value| oid(value)) else {
                        captures[task.index].failures.push(Failure::new(FailureCode::ParseError, "GitHub object OID invalid"));
                        continue;
                    };
                    if child["type"] == "tree"
                        && ![".git", "target", "node_modules", ".cache", "__pycache__", ".venv", "vendor", "dist", "build"].contains(&name)
                    {
                        if task.depth >= request.limits.depth {
                            captures[task.index].failures.push(Failure::new(FailureCode::LimitReached, "GitHub tree depth budget exceeded"));
                        } else {
                            queue.push_back(TreeTask { index: task.index, path, depth: task.depth + 1, oid: Some(object_oid.into()), head: task.head.clone() });
                        }
                    } else if child["type"] == "blob"
                        && let Some(kind) = manifest_kind(&path)
                    {
                        let size_limit =
                            if kind == "workflow" { request.limits.file_bytes.min(request.limits.workflow_bytes) } else { request.limits.file_bytes };
                        let metadata = &child["object"];
                        if metadata.get("byteSize").and_then(Value::as_u64).is_none_or(|size| size > size_limit as u64) {
                            captures[task.index].failures.push(Failure::new(FailureCode::LimitReached, "GitHub blob byte budget exceeded"));
                        } else if metadata.get("isBinary").and_then(Value::as_bool) != Some(false) {
                            captures[task.index].failures.push(Failure::new(FailureCode::Unsupported, "GitHub binary blob unsupported"));
                        } else {
                            blobs.push(BlobTask { index: task.index, path, oid: object_oid.into(), kind: kind.into() });
                        }
                    }
                }
            }
            if entries > request.limits.entries_per_environment {
                break;
            }
        }
        let remaining_manifests = request.limits.manifests_per_environment.saturating_sub(manifest_count);
        if blobs.len() > remaining_manifests {
            blobs.truncate(remaining_manifests);
            for capture in &mut captures[batch_start..batch_end] {
                capture.failures.push(Failure::new(FailureCode::LimitReached, "GitHub manifest budget exceeded"));
            }
        }
        manifest_count += blobs.len();
        for blob in &blobs {
            if let Some(update) = updates.iter_mut().find(|entry| entry.repository == metas[blob.index]) {
                update.blobs.insert(blob.path.clone(), blob.oid.clone());
            }
        }
        // If every observed blob is unchanged, the caller can reuse the complete graph.
        for (index, capture) in captures.iter().enumerate().take(batch_end).skip(batch_start) {
            let update = updates.iter().find(|entry| entry.repository == metas[index]).unwrap();
            if let Some(old) = cache.repositories.iter().find(|entry| {
                entry.repository.owner.eq_ignore_ascii_case(&metas[index].owner)
                    && entry.repository.name == metas[index].name
                    && entry.blobs == update.blobs
                    && entry.snapshot.is_some()
            }) && capture.failures.is_empty()
            {
                result.cached_scopes.push(GithubCachedScope {
                    scope_id: capture.scope_id.clone(),
                    repository: metas[index].clone(),
                    actual_fetched_at_ms: old.actual_fetched_at_ms,
                    snapshot: refresh_snapshot_metadata(old.snapshot.clone().unwrap(), &old.repository, &metas[index], request.observed_at_ms),
                    reused: true,
                });
                reused.insert(index);
            }
        }
        for update in &mut updates {
            if let Some(receipt) = result.cached_scopes.iter().find(|receipt| receipt.repository == update.repository) {
                update.snapshot = Some(receipt.snapshot.clone());
                update.actual_fetched_at_ms = receipt.actual_fetched_at_ms;
            }
        }
        blobs.retain(|blob| !reused.contains(&blob.index));
        // A changed repository may retain normalized graphs for unchanged sources.
        // Until the caller merges those sources, it must remain partial.
        blobs.retain(|blob| {
            let meta = &metas[blob.index];
            let unchanged = cache.repositories.iter().any(|old| {
                old.repository.owner.eq_ignore_ascii_case(&meta.owner)
                    && old.repository.name == meta.name
                    && old.blobs.get(&blob.path) == Some(&blob.oid)
                    && old.snapshot.is_some()
            });
            if unchanged {
                result.reused_blobs.push(GithubReusedBlob {
                    scope_id: captures[blob.index].scope_id.clone(),
                    relative_path: blob.path.clone(),
                    oid: blob.oid.clone(),
                });
                captures[blob.index].failures.push(Failure::new(FailureCode::Unsupported, "unchanged source requires normalized graph merge"));
            }
            !unchanged
        });
        for chunk in blobs.chunks(16) {
            let query = blob_query(chunk, &metas);
            let data = match api.query(query, json!({})).await {
                Ok(data) => data,
                Err(failure) => {
                    for blob in chunk {
                        captures[blob.index].failures.push(failure.clone());
                    }
                    for (index, capture) in captures.iter_mut().enumerate().take(batch_end).skip(batch_start) {
                        if !reused.contains(&index) {
                            capture.failures.push(Failure::new(FailureCode::LimitReached, "GitHub manifest collection incomplete"));
                        }
                    }
                    break;
                }
            };
            for (slot, blob) in chunk.iter().enumerate() {
                let object = &data[format!("r{slot}")]["object"];
                let limit = if blob.kind == "workflow" { request.limits.file_bytes.min(request.limits.workflow_bytes) } else { request.limits.file_bytes };
                let failure = if object["oid"].as_str() != Some(&blob.oid) {
                    Some(Failure::new(FailureCode::ParseError, "GitHub blob OID mismatch"))
                } else if object["isBinary"].as_bool() != Some(false) {
                    Some(Failure::new(FailureCode::Unsupported, "GitHub binary or unavailable blob"))
                } else if object["isTruncated"].as_bool() != Some(false) {
                    Some(Failure::new(FailureCode::Truncated, "GitHub blob truncated or unavailable"))
                } else if object["byteSize"].as_u64().is_none_or(|size| size > limit as u64) {
                    Some(Failure::new(FailureCode::LimitReached, "GitHub blob byte budget exceeded"))
                } else {
                    None
                };
                if let Some(failure) = failure {
                    captures[blob.index].failures.push(failure);
                    continue;
                }
                let Some(content) = object["text"].as_str() else {
                    captures[blob.index].failures.push(Failure::new(FailureCode::PermissionDenied, "GitHub blob text unavailable"));
                    continue;
                };
                if object["byteSize"].as_u64() != Some(content.len() as u64) {
                    captures[blob.index].failures.push(Failure::new(FailureCode::Truncated, "GitHub blob length mismatch"));
                    continue;
                }
                if content.len() > limit || bytes.saturating_add(content.len()) > request.limits.total_bytes_per_environment {
                    captures[blob.index].failures.push(Failure::new(FailureCode::LimitReached, "GitHub total byte budget exceeded"));
                    continue;
                }
                bytes += content.len();
                let capture = &mut captures[blob.index];
                capture.counters.bytes += content.len();
                capture.counters.files += 1;
                capture.counters.manifests += 1;
                capture.repositories[0].manifests.push(RawManifest {
                    relative_path: blob.path.clone(),
                    kind: blob.kind.clone(),
                    content: content.into(),
                    content_sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
                    revision: metas[blob.index].head.clone(),
                });
            }
        }
    }
    let elapsed = api.started.elapsed().as_millis() as u64;
    for (index, mut capture) in captures.into_iter().enumerate() {
        if !reused.contains(&index) {
            capture.counters.duration_ms = elapsed;
            capture.counters.repositories = 1;
            result.captures.push(capture);
        }
    }
    if !catalog_complete {
        result.captures.push(fail_capture(request, Failure::new(FailureCode::PermissionDenied, "GitHub catalog incomplete; absence is not removal")));
    }
    if cache_matches {
        for old in &cache.repositories {
            if !updates.iter().any(|entry| entry.repository.owner.eq_ignore_ascii_case(&old.repository.owner) && entry.repository.name == old.repository.name) {
                let scope = repository_scope(&request.scope_id, &old.repository.owner, &old.repository.name);
                let mut missing = blank(request, scope);
                missing.failures.push(Failure::new(FailureCode::PermissionDenied, "GitHub repository not visible; absence is not removal"));
                result.captures.push(missing);
                updates.push(old.clone());
                catalog_complete = false;
            }
        }
    }
    // Failed repositories retain old cache data. Never persist incomplete replacements.
    for update in &mut updates {
        let scope = repository_scope(&request.scope_id, &update.repository.owner, &update.repository.name);
        if result.captures.iter().any(|capture| capture.scope_id == scope && !capture.failures.is_empty())
            && let Some(old) =
                cache.repositories.iter().find(|old| old.repository.owner == update.repository.owner && old.repository.name == update.repository.name)
        {
            *update = old.clone();
        }
    }
    if !catalog_complete {
        for old in &cache.repositories {
            if !updates.iter().any(|entry| entry.repository.owner == old.repository.owner && entry.repository.name == old.repository.name) {
                updates.push(old.clone());
            }
        }
    }
    result.cache_update = GithubCache {
        owners,
        catalog_fetched_at_ms: if catalog_complete { request.observed_at_ms } else { cache.catalog_fetched_at_ms },
        catalog_complete,
        repositories: updates,
    };
    result.requests_attempted = api.attempted;
    result.requests_succeeded = api.succeeded;
    result
}

#[derive(Clone)]
struct TreeTask {
    index: usize,
    path: String,
    depth: usize,
    oid: Option<String>,
    head: String,
}
struct BlobTask {
    index: usize,
    path: String,
    oid: String,
    kind: String,
}
fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}
fn tree_query(tasks: &[TreeTask], repos: &[GithubRepository]) -> String {
    let fields = tasks.iter().enumerate().map(|(slot, task)| { let repo = &repos[task.index]; format!("r{slot}:repository(owner:{},name:{}){{object(expression:{}){{... on Tree{{oid entries{{name type oid object{{... on Blob{{byteSize isBinary isTruncated}}}}}}}}}}}}", quoted(&repo.owner), quoted(&repo.name), quoted(&format!("{}:{}", task.head, task.path))) }).collect::<String>();
    format!("query{{{fields}rateLimit{{remaining resetAt}}}}")
}
fn blob_query(tasks: &[BlobTask], repos: &[GithubRepository]) -> String {
    let fields = tasks
        .iter()
        .enumerate()
        .map(|(slot, task)| {
            let repo = &repos[task.index];
            format!(
                "r{slot}:repository(owner:{},name:{}){{object(expression:{}){{... on Blob{{oid byteSize isBinary isTruncated text}}}}}}",
                quoted(&repo.owner),
                quoted(&repo.name),
                quoted(&task.oid)
            )
        })
        .collect::<String>();
    format!("query{{{fields}rateLimit{{remaining resetAt}}}}")
}
fn manifest_kind(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next()?;
    match name {
        "Cargo.toml" => Some("cargo"),
        "package.json" => Some("node"),
        "pyproject.toml" => Some("python"),
        "go.mod" => Some("go"),
        _ if path.starts_with(".github/workflows/") && path.split('/').count() == 3 && (path.ends_with(".yml") || path.ends_with(".yaml")) => Some("workflow"),
        _ => None,
    }
}
