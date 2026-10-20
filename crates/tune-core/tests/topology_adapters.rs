use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::Duration,
};
#[cfg(unix)]
use tune_core::collect::run_bounded;
use tune_core::{
    collect::BoundedResult,
    topology::{
        CaptureCounters, FailureCode, RawCapture, RawGitMetadata, RawManifest, RawRepository, ScopeStatus, TopologyLimits, TopologySnapshot,
        adapters::{
            CommandFuture, CommandRunner,
            github::{GithubCache, GithubCacheEntry, GithubRequest, collect_github_with, repository_scope},
            remote::{POSIX_PROBE, RemotePlatform, RemoteRequest, RemoteRuntimeKind, WINDOWS_PROBE, collect_remote_with, identity_hash},
        },
    },
};
type TransportCall = (String, Vec<String>, Vec<u8>);

struct Fake {
    output: Mutex<VecDeque<BoundedResult>>,
    calls: Mutex<Vec<TransportCall>>,
}
impl Fake {
    fn new(rows: Vec<Value>) -> Self {
        Self {
            output: Mutex::new(
                rows.into_iter().map(|row| BoundedResult { code: Some(0), stdout: serde_json::to_vec(&row).unwrap(), ..Default::default() }).collect(),
            ),
            calls: Mutex::new(vec![]),
        }
    }
}
impl CommandRunner for Fake {
    fn run<'a>(&'a self, program: &'a str, args: &'a [String], stdin: &'a [u8], _: Duration, _: usize, _: usize) -> CommandFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((program.into(), args.to_vec(), stdin.to_vec()));
            self.output.lock().unwrap().pop_front().expect("unexpected transport request")
        })
    }
}
fn limits() -> TopologyLimits {
    TopologyLimits::default()
}
fn remote() -> RemoteRequest {
    RemoteRequest {
        scope_id: "test:environment".into(),
        alias: "example-node".into(),
        expected_identity_hash: Some(identity_hash("example-host")),
        platform: RemotePlatform::Posix,
        expected_runtime: RemoteRuntimeKind::Native,
        expected_distro: None,
        cursor: None,
        roots: vec!["/private/example-root".into()],
        configs: vec![],
        python_program: "/usr/bin/python3".into(),
        git_program: None,
        observed_at_ms: 1000,
        limits: limits(),
    }
}
fn capture(request: &RemoteRequest) -> RawCapture {
    let text = "{\"name\":\"example-package\"}";
    RawCapture {
        scope_id: request.scope_id.clone(),
        adapter: "remote".into(),
        target_identity: request.expected_identity_hash.clone(),
        progress: None,
        observed_at_ms: request.observed_at_ms,
        limits: request.limits.clone(),
        failures: vec![],
        counters: CaptureCounters { repositories: 1, manifests: 1, files: 1, bytes: text.len(), ..Default::default() },
        repositories: vec![RawRepository {
            relative_path: ".".into(),
            root_fingerprint: "a".repeat(64),
            git: RawGitMetadata {
                origin: Some("https://credential@example.org/example/repo.git?secret=value#fragment".into()),
                head: Some("a".repeat(40)),
                git_dir: Some("b".repeat(64)),
                common_dir: Some("b".repeat(64)),
                is_worktree: false,
            },
            manifests: vec![RawManifest {
                relative_path: "package.json".into(),
                kind: "node".into(),
                content: text.into(),
                content_sha256: tune_core::db::sha256_hex(text.as_bytes()),
                revision: Some("a".repeat(40)),
            }],
            configs: vec![],
            config_file_read: false,
            failures: vec![],
        }],
    }
}
fn remote_capture(request: &RemoteRequest) -> Value {
    let mut value = serde_json::to_value(capture(request)).unwrap();
    value["runtime"] = json!({"kind":if request.expected_runtime==RemoteRuntimeKind::Wsl {"wsl"}else{"native"},"platform":if request.platform==RemotePlatform::Windows {"windows"}else{"posix"},"kernel_wsl":request.expected_runtime==RemoteRuntimeKind::Wsl,"distro":request.expected_distro});
    value
}
fn github() -> GithubRequest {
    GithubRequest {
        scope_id: "test:github".into(),
        owners: vec!["example-owner".into()],
        allowed_owners: vec!["example-owner".into()],
        observed_at_ms: 1_000_000,
        cache_minutes: 15,
        limits: limits(),
        gh_program: Some("/private/existing-gh-wrapper".into()),
    }
}
fn catalog(head: &str) -> Value {
    json!({"data":{"repositoryOwner":{"repositories":{"nodes":[{"name":"example-repo","nameWithOwner":"example-owner/example-repo","isArchived":true,"isFork":true,"isEmpty":false,"defaultBranchRef":{"name":"main","target":{"oid":head}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}},"rateLimit":{"remaining":1000}}})
}
fn tree(blob: &str) -> Value {
    json!({"data":{"r0":{"object":{"oid":"b".repeat(40),"entries":[{"name":"package.json","type":"blob","oid":blob,"object":{"byteSize":2,"isBinary":false,"isTruncated":false}}]}},"rateLimit":{"remaining":999}}})
}
fn blob(object_oid: &str, binary: bool, truncated: bool, text: Option<&str>) -> Value {
    json!({"data":{"r0":{"object":{"oid":object_oid,"byteSize":text.map(str::len).unwrap_or(0),"isBinary":binary,"isTruncated":truncated,"text":text}},"rateLimit":{"remaining":998}}})
}
fn snapshot(scope: &str) -> TopologySnapshot {
    TopologySnapshot {
        scope_id: scope.into(),
        status: ScopeStatus::Complete,
        entities: vec![],
        edges: vec![],
        failures: vec![],
        counters: CaptureCounters::default(),
        observed_at_ms: 123,
    }
}

#[tokio::test]
async fn remote_payload_is_stdin_strict_and_redacts_metadata() {
    let request = remote();
    let fake = Fake::new(vec![remote_capture(&request)]);
    let got = collect_remote_with(&fake, &request).await;
    assert!(got.failures.is_empty());
    assert_eq!(got.repositories[0].git.origin.as_deref(), Some("example.org/example/repo"));
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "ssh");
    assert!(calls[0].1.contains(&"StrictHostKeyChecking=yes".into()));
    assert!(calls[0].1.contains(&"BatchMode=yes".into()));
    assert!(calls[0].1.last().unwrap().starts_with("exec /usr/bin/python3 -B -"));
    let args = calls[0].1.join(" ");
    assert!(!args.contains(&request.roots[0]));
    assert!(!args.contains("wsl.exe"));
    let script = String::from_utf8_lossy(&calls[0].2);
    assert!(script.contains("GIT_NO_LAZY_FETCH"));
    assert!(!script.contains("open(path, \"w\")"));
}
#[tokio::test]
async fn remote_rejects_identity_scope_nul_hash_and_unsafe_alias() {
    for mode in 0..4 {
        let request = remote();
        let mut raw = remote_capture(&request);
        match mode {
            0 => raw["target_identity"] = json!("0".repeat(64)),
            1 => raw["scope_id"] = json!("unexpected"),
            2 => raw["repositories"][0]["manifests"][0]["content_sha256"] = json!("0".repeat(64)),
            _ => raw["repositories"][0]["relative_path"] = json!("../escape"),
        }
        let fake = Fake::new(vec![raw]);
        assert!(!collect_remote_with(&fake, &request).await.failures.is_empty());
    }
    let mut request = remote();
    request.alias = "-oProxyCommand=bad".into();
    let fake = Fake::new(vec![]);
    assert!(!collect_remote_with(&fake, &request).await.failures.is_empty());
    assert!(fake.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn windows_script_has_ascii_bom_no_write_and_short_loader() {
    assert!(WINDOWS_PROBE.starts_with('\u{feff}'));
    assert!(WINDOWS_PROBE.trim_start_matches('\u{feff}').is_ascii());
    assert!(!WINDOWS_PROBE.contains('\''));
    for forbidden in ["Set-Content", "Out-File", "New-Item", "WriteAll", "wsl.exe", "git fetch", "Remove-Item", "Start-Process"] {
        assert!(!WINDOWS_PROBE.contains(forbidden), "{forbidden}");
    }
    let mut request = remote();
    request.platform = RemotePlatform::Windows;
    request.roots = vec!["C:\\private\\example-root".into()];
    let fake = Fake::new(vec![remote_capture(&request)]);
    assert!(collect_remote_with(&fake, &request).await.failures.is_empty());
    let calls = fake.calls.lock().unwrap();
    assert!(calls[0].1.last().unwrap().len() < 1000);
    assert!(calls[0].1.last().unwrap().contains("-EncodedCommand"));
    assert!(calls[0].2.starts_with(b"$TopologyRequest"));
    assert!(String::from_utf8(calls[0].2.clone()).unwrap().ends_with(WINDOWS_PROBE.trim_start_matches('\u{feff}')));
    assert!(!POSIX_PROBE.contains("os.mkdir"));
    assert!(!POSIX_PROBE.contains("git fetch"));
}
#[cfg(unix)]
#[tokio::test]
async fn process_budgets_bound_buffers_and_time_and_stdin() {
    let output = run_bounded(
        "/usr/bin/python3",
        &["-c".into(), "import sys;sys.stdout.buffer.write(sys.stdin.buffer.read())".into()],
        b"example",
        Duration::from_secs(2),
        100,
        100,
    )
    .await;
    assert_eq!(output.stdout, b"example");
    assert_eq!(output.code, Some(0));
    let output =
        run_bounded("/usr/bin/python3", &["-c".into(), "import sys;sys.stdout.write('x'*100000)".into()], b"", Duration::from_secs(2), 4096, 100).await;
    assert!(output.output_limited);
    assert_eq!(output.stdout.len(), 4096);
    let output = run_bounded("/usr/bin/python3", &["-c".into(), "import time;time.sleep(2)".into()], b"", Duration::from_millis(20), 100, 100).await;
    assert!(output.timed_out);
}
#[tokio::test]
async fn github_allowlist_and_injection_rejected_before_transport() {
    for owner in ["other-owner", "bad\"owner", "-bad", "bad/owner"] {
        let mut request = github();
        request.owners = vec![owner.into()];
        let fake = Fake::new(vec![]);
        let result = collect_github_with(&fake, &request, &GithubCache::default()).await;
        assert_eq!(result.requests_attempted, 0);
        assert_eq!(result.captures[0].failures[0].code, FailureCode::PermissionDenied);
    }
}
#[tokio::test]
async fn github_reads_exact_oids_and_metadata_with_bounded_batches() {
    let request = github();
    let head = "a".repeat(40);
    let object = "c".repeat(40);
    let content = "{\"name\":\"example-package\"}";
    let fake = Fake::new(vec![catalog(&head), tree(&object), blob(&object, false, false, Some(content))]);
    let result = collect_github_with(&fake, &request, &GithubCache::default()).await;
    assert_eq!(result.requests_attempted, 3);
    assert_eq!(result.requests_succeeded, 3);
    assert!(result.captures[0].failures.is_empty());
    assert_eq!(result.captures[0].repositories[0].manifests[0].revision.as_deref(), Some(head.as_str()));
    assert!(result.cache_update.repositories[0].repository.archived);
    assert!(result.cache_update.repositories[0].repository.fork);
    let calls = fake.calls.lock().unwrap();
    for call in calls.iter() {
        assert_eq!(call.0, "/private/existing-gh-wrapper");
        assert_eq!(call.1, ["api", "graphql", "--input", "-"]);
        let data: Value = serde_json::from_slice(&call.2).unwrap();
        let query = data["query"].as_str().unwrap();
        assert!(!query.contains("mutation"));
    }
    let query: Value = serde_json::from_slice(&calls[1].2).unwrap();
    assert!(query["query"].as_str().unwrap().contains(&format!("{head}:")));
    let query: Value = serde_json::from_slice(&calls[2].2).unwrap();
    assert!(query["query"].as_str().unwrap().contains(&object));
    let cache_serialized = serde_json::to_string(&result.cache_update).unwrap();
    assert!(!cache_serialized.contains("example-package"));
}
#[tokio::test]
async fn github_partial_rate_auth_binary_truncated_and_oid_mismatch_stay_failed() {
    let request = github();
    for error in [
        json!({"errors":[{"type":"RATE_LIMITED","message":"private secret must not escape"}],"data":{}}),
        json!({"errors":[{"type":"FORBIDDEN","message":"secret"}],"data":{"repositoryOwner":null}}),
    ] {
        let fake = Fake::new(vec![error]);
        let result = collect_github_with(&fake, &request, &GithubCache::default()).await;
        assert!(result.captures.iter().any(|c| !c.failures.is_empty()));
        let safe = serde_json::to_string(&result.captures).unwrap();
        assert!(!safe.contains("secret"));
    }
    for (binary, truncated, object_oid, text) in [
        (true, false, "c".repeat(40), None),
        (false, true, "c".repeat(40), Some("{}")),
        (false, false, "d".repeat(40), Some("{}")),
        (false, false, "c".repeat(40), None),
    ] {
        let fake = Fake::new(vec![catalog(&"a".repeat(40)), tree(&"c".repeat(40)), blob(&object_oid, binary, truncated, text)]);
        let result = collect_github_with(&fake, &request, &GithubCache::default()).await;
        assert!(!result.captures[0].failures.is_empty());
        assert!(result.captures[0].repositories[0].manifests.is_empty());
    }
}
#[tokio::test]
async fn github_ttl_cache_does_zero_requests_without_extending_original_time() {
    let request = github();
    let initial = collect_github_with(
        &Fake::new(vec![catalog(&"a".repeat(40)), tree(&"c".repeat(40)), blob(&"c".repeat(40), false, false, Some("{}"))]),
        &request,
        &GithubCache::default(),
    )
    .await;
    let mut cache = initial.cache_update;
    let scope = repository_scope(&request.scope_id, "example-owner", "example-repo");
    cache.repositories[0].snapshot = Some(snapshot(&scope));
    let mut next = github();
    next.observed_at_ms += 1000;
    let fake = Fake::new(vec![]);
    let result = collect_github_with(&fake, &next, &cache).await;
    assert_eq!(result.requests_attempted, 0);
    assert_eq!(result.cached_scopes[0].actual_fetched_at_ms, request.observed_at_ms);
    assert_eq!(result.cache_update.catalog_fetched_at_ms, request.observed_at_ms);
    next.observed_at_ms += 15 * 60_000;
    let fake = Fake::new(vec![catalog(&"a".repeat(40))]);
    let result = collect_github_with(&fake, &next, &cache).await;
    assert_eq!(result.requests_attempted, 1);
    assert_eq!(result.cached_scopes[0].actual_fetched_at_ms, request.observed_at_ms);
}
#[tokio::test]
async fn github_expired_changed_head_same_blob_skips_blob_request() {
    let request = github();
    let mut cache = GithubCache { owners: request.owners.clone(), catalog_fetched_at_ms: 1, catalog_complete: true, repositories: vec![] };
    let scope = repository_scope(&request.scope_id, "example-owner", "example-repo");
    let mut old_snapshot = snapshot(&scope);
    let mut checkout = tune_core::topology::Entity::new(tune_core::topology::EntityKind::Checkout, "example-checkout");
    checkout.attributes = json!({"revision":"a".repeat(40),"head":"a".repeat(40),"observed_at_ms":123});
    old_snapshot.entities.push(checkout);
    cache.repositories.push(GithubCacheEntry {
        repository: serde_json::from_value(
            json!({"owner":"example-owner","name":"example-repo","archived":true,"fork":true,"empty":false,"default_branch":"main","head":"a".repeat(40)}),
        )
        .unwrap(),
        blobs: BTreeMap::from([("package.json".into(), "c".repeat(40))]),
        actual_fetched_at_ms: 10,
        snapshot: Some(old_snapshot),
    });
    let fake = Fake::new(vec![catalog(&"b".repeat(40)), tree(&"c".repeat(40))]);
    let result = collect_github_with(&fake, &request, &cache).await;
    assert_eq!(result.requests_attempted, 2);
    assert_eq!(result.cached_scopes.len(), 1);
    assert!(result.captures.is_empty());
    assert_eq!(result.cached_scopes[0].actual_fetched_at_ms, 10);
    let refreshed = &result.cached_scopes[0].snapshot;
    assert_eq!(refreshed.observed_at_ms, request.observed_at_ms);
    assert_eq!(refreshed.entities[0].attributes["revision"], json!("b".repeat(40)));
    assert_eq!(refreshed.entities[0].attributes["head"], json!("b".repeat(40)));
    assert_eq!(refreshed.entities[0].attributes["observed_at_ms"], json!(request.observed_at_ms));
    assert_eq!(result.cache_update.repositories[0].actual_fetched_at_ms, 10);
    assert_eq!(result.cache_update.repositories[0].snapshot.as_ref().unwrap().entities[0].attributes["revision"], json!("b".repeat(40)));
}
#[tokio::test]
async fn github_budget_reports_uncollected_and_preserves_previous_cache() {
    let mut request = github();
    request.limits.github_requests_per_run = 1;
    let fake = Fake::new(vec![catalog(&"a".repeat(40))]);
    let result = collect_github_with(&fake, &request, &GithubCache::default()).await;
    assert_eq!(result.requests_attempted, 1);
    assert!(result.captures.iter().any(|c| c.failures.iter().any(|f| f.code == FailureCode::LimitReached)));
}

#[cfg(unix)]
#[tokio::test]
async fn posix_probe_reads_two_roots_and_worktree_without_mutation_or_symlink_descent() {
    use std::{fs, os::unix::fs::symlink, process::Command};
    let base = std::env::temp_dir().join(format!("topology-adapter-fixture-{}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    let first = base.join("first");
    let second = base.join("second");
    let outside = base.join("outside");
    for path in [&first, &second, &outside] {
        fs::create_dir_all(path).unwrap();
        assert!(Command::new("/usr/bin/git").args(["init", "--quiet"]).arg(path).status().unwrap().success());
        fs::write(path.join("package.json"), "{\"name\":\"example-package\"}").unwrap();
    }
    let worktree = first.join("linked");
    let linked = Command::new("/usr/bin/git").arg("-C").arg(&first).args(["worktree", "add", "--orphan", "--quiet"]).arg(&worktree).status().unwrap();
    assert!(linked.success());
    fs::write(worktree.join("go.mod"), "module example.invalid/module\n").unwrap();
    symlink(&outside, first.join("ignored-link")).unwrap();
    fs::create_dir_all(first.join(".branch-recovery/objects")).unwrap();
    fs::write(first.join(".branch-recovery/objects/package.json"), "{invalid").unwrap();
    let roots = vec![first.to_string_lossy().into_owned(), second.to_string_lossy().into_owned()];
    let config = json!({"scope_id":"test:posix","observed_at_ms":1000,"roots":roots,"configs":[],"limits":limits()});
    let prefix = format!("import json\nCONFIG=json.loads({})\n", serde_json::to_string(&config.to_string()).unwrap());
    let text = format!("{prefix}{POSIX_PROBE}");
    let before = std::fs::read(first.join(".git/config")).unwrap();
    let output = run_bounded("/usr/bin/python3", &["-".into()], text.as_bytes(), Duration::from_secs(10), 1_000_000, 1000).await;
    assert_eq!(output.code, Some(0));
    let capture: RawCapture = serde_json::from_slice(&output.stdout).unwrap();
    assert!(capture.failures.is_empty());
    assert_eq!(capture.repositories.len(), 3);
    assert_eq!(capture.counters.manifests, 3);
    assert!(capture.repositories.iter().any(|repo| repo.git.is_worktree));
    assert!(capture.repositories.iter().flat_map(|repo| &repo.manifests).all(|manifest| !manifest.relative_path.contains(".branch-recovery")));
    let roots = capture.repositories.iter().map(|repo| &repo.root_fingerprint).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(roots.len(), 2);
    assert_eq!(std::fs::read(first.join(".git/config")).unwrap(), before);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&base.to_string_lossy().to_string()));
    fs::remove_dir_all(base).unwrap();
}

#[tokio::test]
async fn github_subset_change_does_not_refetch_unchanged_blob_or_claim_complete() {
    let request = github();
    let scope = repository_scope(&request.scope_id, "example-owner", "example-repo");
    let repository = serde_json::from_value(
        json!({"owner":"example-owner","name":"example-repo","archived":true,"fork":true,"empty":false,"default_branch":"main","head":"a".repeat(40)}),
    )
    .unwrap();
    let cache = GithubCache {
        owners: request.owners.clone(),
        catalog_fetched_at_ms: 1,
        catalog_complete: true,
        repositories: vec![GithubCacheEntry {
            repository,
            blobs: BTreeMap::from([("package.json".into(), "c".repeat(40)), ("Cargo.toml".into(), "e".repeat(40))]),
            actual_fetched_at_ms: 10,
            snapshot: Some(snapshot(&scope)),
        }],
    };
    let tree = json!({"data":{"r0":{"object":{"oid":"b".repeat(40),"entries":[{"name":"package.json","type":"blob","oid":"c".repeat(40),"object":{"byteSize":2,"isBinary":false}},{"name":"Cargo.toml","type":"blob","oid":"d".repeat(40),"object":{"byteSize":2,"isBinary":false}}]}},"rateLimit":{"remaining":100}}});
    let fake = Fake::new(vec![catalog(&"b".repeat(40)), tree, blob(&"d".repeat(40), false, false, Some("{}"))]);
    let result = collect_github_with(&fake, &request, &cache).await;
    assert_eq!(result.reused_blobs.len(), 1);
    assert_eq!(result.reused_blobs[0].relative_path, "package.json");
    assert!(!result.captures[0].failures.is_empty());
    assert_eq!(result.captures[0].repositories[0].manifests.len(), 1);
    assert_eq!(result.cache_update.repositories[0].repository.head, cache.repositories[0].repository.head);
    let calls = fake.calls.lock().unwrap();
    let query: Value = serde_json::from_slice(&calls[2].2).unwrap();
    let query = query["query"].as_str().unwrap();
    assert!(!query.contains(&"c".repeat(40)));
    assert!(query.contains(&"d".repeat(40)));
}

#[tokio::test]
async fn github_visibility_loss_keeps_previous_repository_as_unknown() {
    let request = github();
    let scope = repository_scope(&request.scope_id, "example-owner", "example-repo");
    let repository = serde_json::from_value(
        json!({"owner":"example-owner","name":"example-repo","archived":false,"fork":false,"empty":false,"default_branch":"main","head":"a".repeat(40)}),
    )
    .unwrap();
    let cache = GithubCache {
        owners: request.owners.clone(),
        catalog_fetched_at_ms: 1,
        catalog_complete: true,
        repositories: vec![GithubCacheEntry { repository, blobs: BTreeMap::new(), actual_fetched_at_ms: 10, snapshot: Some(snapshot(&scope)) }],
    };
    let response =
        json!({"data":{"repositoryOwner":{"repositories":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}},"rateLimit":{"remaining":100}}});
    let result = collect_github_with(&Fake::new(vec![response]), &request, &cache).await;
    assert_eq!(result.cache_update.repositories.len(), 1);
    assert!(!result.cache_update.catalog_complete);
    assert!(result.captures.iter().any(|capture| capture.scope_id == scope && !capture.failures.is_empty()));
}

struct CatalogFake {
    calls: Mutex<usize>,
}
impl CommandRunner for CatalogFake {
    fn run<'a>(&'a self, _: &'a str, _: &'a [String], input: &'a [u8], _: Duration, _: usize, _: usize) -> CommandFuture<'a> {
        Box::pin(async move {
            *self.calls.lock().unwrap() += 1;
            let request: Value = serde_json::from_slice(input).unwrap();
            let query = request["query"].as_str().unwrap();
            let data = if query.contains("repositoryOwner") {
                let offset = request["variables"]["cursor"].as_str().map(|s| s.parse::<usize>().unwrap()).unwrap_or(0);
                let nodes=(offset..(offset+100).min(213)).map(|index|json!({"name":format!("example-repo-{index:03}"),"nameWithOwner":format!("example-owner/example-repo-{index:03}"),"isArchived":false,"isFork":false,"isEmpty":false,"defaultBranchRef":{"name":"main","target":{"oid":"a".repeat(40)}}})).collect::<Vec<_>>();
                json!({"repositoryOwner":{"repositories":{"nodes":nodes,"pageInfo":{"hasNextPage":offset+100<213,"endCursor":(offset+100).to_string()}}},"rateLimit":{"remaining":1000}})
            } else {
                let mut fields = serde_json::Map::new();
                for (slot, _) in regex::Regex::new(r"r\d+:repository").unwrap().find_iter(query).enumerate() {
                    let object = if query.contains("entries") {
                        json!({"oid":"b".repeat(40),"entries":[{"name":"package.json","type":"blob","oid":"c".repeat(40),"object":{"byteSize":2,"isBinary":false}}]})
                    } else {
                        json!({"oid":"c".repeat(40),"byteSize":2,"isBinary":false,"isTruncated":false,"text":"{}"})
                    };
                    fields.insert(format!("r{slot}"), json!({"object":object}));
                }
                fields.insert("rateLimit".into(), json!({"remaining":1000}));
                Value::Object(fields)
            };
            BoundedResult { code: Some(0), stdout: serde_json::to_vec(&json!({"data":data})).unwrap(), ..Default::default() }
        })
    }
}
#[tokio::test]
async fn github_catalog_213_repositories_paginates_and_batches_within_128_requests() {
    let fake = CatalogFake { calls: Mutex::new(0) };
    let result = collect_github_with(&fake, &github(), &GithubCache::default()).await;
    assert_eq!(result.captures.len(), 213);
    assert_eq!(result.cache_update.repositories.len(), 213);
    assert!(result.captures.iter().all(|capture| capture.failures.is_empty()));
    assert_eq!(result.requests_attempted, 31);
    assert_eq!(*fake.calls.lock().unwrap(), 31);
    assert!(result.requests_attempted < 128);
}

#[tokio::test]
async fn github_low_budget_runs_make_progress_after_successful_scopes_are_cached() {
    let mut request = github();
    request.limits.github_requests_per_run = 5;
    let first = collect_github_with(&CatalogFake { calls: Mutex::new(0) }, &request, &GithubCache::default()).await;
    let completed = first.captures.iter().filter(|capture| capture.failures.is_empty()).count();
    assert_eq!(completed, 16);
    assert!(first.captures.iter().filter(|capture| !capture.failures.is_empty()).count() > 0);
    let mut cache = first.cache_update;
    for entry in &mut cache.repositories {
        let scope = repository_scope(&request.scope_id, &entry.repository.owner, &entry.repository.name);
        if first.captures.iter().any(|capture| capture.scope_id == scope && capture.failures.is_empty()) {
            entry.snapshot = Some(snapshot(&scope));
        }
    }
    let second = collect_github_with(&CatalogFake { calls: Mutex::new(0) }, &request, &cache).await;
    assert_eq!(second.cached_scopes.len(), 16);
    assert_eq!(second.captures.iter().filter(|capture| capture.failures.is_empty()).count(), 16);
    let newly_completed = second.captures.iter().filter(|capture| capture.failures.is_empty()).map(|capture| &capture.scope_id).collect::<Vec<_>>();
    assert!(first.captures.iter().filter(|capture| capture.failures.is_empty()).all(|capture| !newly_completed.contains(&&capture.scope_id)));
}

/// This opt-in test consumes an explicitly approved private request file and
/// saves receipts only. No root, alias, hostname or manifest content is printed.
#[tokio::test]
#[ignore = "requires approved private SSH surfaces and request/receipt paths"]
async fn approved_remote_smoke_receipts() {
    use tune_core::topology::adapters::remote::collect_remote;
    let path = std::env::var("KATALA_TOPOLOGY_SMOKE_REQUESTS").expect("private request path");
    let output = std::env::var("KATALA_TOPOLOGY_SMOKE_RECEIPTS").expect("private receipt path");
    let requests: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut receipts = Vec::new();
    for item in requests.as_array().unwrap() {
        let request = RemoteRequest {
            scope_id: item["scope_id"].as_str().unwrap().into(),
            alias: item["alias"].as_str().unwrap().into(),
            expected_identity_hash: Some(item["expected_identity_hash"].as_str().unwrap().into()),
            platform: if item["platform"] == "windows" { RemotePlatform::Windows } else { RemotePlatform::Posix },
            expected_runtime: if item["kind"] == "wsl" { RemoteRuntimeKind::Wsl } else { RemoteRuntimeKind::Native },
            expected_distro: item["expected_distro"].as_str().map(str::to_owned),
            cursor: None,
            roots: item["roots"].as_array().unwrap().iter().map(|value| value.as_str().unwrap().to_owned()).collect(),
            configs: vec![],
            python_program: "/usr/bin/python3".into(),
            git_program: None,
            observed_at_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
            limits: item.get("limits").map(|value| serde_json::from_value(value.clone()).unwrap()).unwrap_or_default(),
        };
        let started = std::time::Instant::now();
        let capture = collect_remote(&request).await;
        receipts.push(json!({"schema":"topology_adapter_live_receipt.v1","scope_id":request.scope_id,"observed_at_ms":request.observed_at_ms,"duration_ms":started.elapsed().as_millis(),"identity_matches":capture.target_identity==request.expected_identity_hash,"target_identity":capture.target_identity,"limits":capture.limits,"repositories":capture.repositories.len(),"manifests":capture.repositories.iter().map(|repo|repo.manifests.len()).sum::<usize>(),"counters":capture.counters,"failures":capture.failures,"probe_sha256":tune_core::db::sha256_hex(if request.platform==RemotePlatform::Windows { WINDOWS_PROBE.as_bytes() } else { POSIX_PROBE.as_bytes() }),"raw_content_persisted":false,"transport":"existing_strict_ssh_stdin"}));
    }
    std::fs::write(output, serde_json::to_vec_pretty(&receipts).unwrap()).unwrap();
    assert!(
        receipts.iter().all(|receipt| receipt["identity_matches"] == true
            && receipt["repositories"].as_u64().unwrap_or(0) > 0
            && receipt["failures"].as_array().unwrap().is_empty()),
        "inspect private receipt failure codes"
    );
}

#[tokio::test]
async fn github_large_sibling_tree_does_not_starve_completed_repository() {
    let mut request = github();
    request.limits.github_requests_per_run = 3;
    let mut catalog = catalog(&"a".repeat(40));
    let mut sibling = catalog["data"]["repositoryOwner"]["repositories"]["nodes"][0].clone();
    sibling["name"] = json!("example-sibling");
    sibling["nameWithOwner"] = json!("example-owner/example-sibling");
    catalog["data"]["repositoryOwner"]["repositories"]["nodes"].as_array_mut().unwrap().push(sibling);
    let mut root = tree(&"c".repeat(40));
    root["data"]["r1"] = root["data"]["r0"].clone();
    root["data"]["r0"]["object"]["entries"].as_array_mut().unwrap().push(json!({"name":"large-subtree","type":"tree","oid":"d".repeat(40)}));
    let mut content = blob(&"c".repeat(40), false, false, Some("{}"));
    content["data"]["r1"] = content["data"]["r0"].clone();
    let result = collect_github_with(&Fake::new(vec![catalog, root, content]), &request, &GithubCache::default()).await;
    assert_eq!(result.requests_attempted, 3);
    assert_eq!(result.captures.iter().filter(|capture| capture.failures.is_empty()).count(), 1);
    assert_eq!(result.captures.iter().filter(|capture| !capture.failures.is_empty()).count(), 1);
}

#[tokio::test]
async fn process_failures_are_static_and_do_not_expose_stderr_secrets() {
    for (output, expected) in [
        (BoundedResult { timed_out: true, ..Default::default() }, FailureCode::Timeout),
        (BoundedResult { output_limited: true, ..Default::default() }, FailureCode::LimitReached),
        (BoundedResult { spawn_failed: true, ..Default::default() }, FailureCode::ToolMissing),
        (BoundedResult { code: Some(255), stderr: b"Permission denied (publickey) secret-value".to_vec(), ..Default::default() }, FailureCode::AuthUnavailable),
    ] {
        let fake = Fake { output: Mutex::new(VecDeque::from([output])), calls: Mutex::new(vec![]) };
        let got = collect_remote_with(&fake, &remote()).await;
        assert_eq!(got.failures[0].code, expected);
        assert!(!serde_json::to_string(&got.failures).unwrap().contains("secret-value"));
    }
}

#[cfg(unix)]
async fn run_posix_probe(config: &Value) -> Value {
    let prefix = format!("import json\nCONFIG=json.loads({})\n", serde_json::to_string(&config.to_string()).unwrap());
    let source = format!("{prefix}{POSIX_PROBE}");
    let output = run_bounded(
        "/usr/bin/python3",
        &["-B".into(), "-".into()],
        source.as_bytes(),
        Duration::from_secs(12),
        config["limits"]["stdout_bytes"].as_u64().unwrap() as usize,
        2000,
    )
    .await;
    assert_eq!(output.code, Some(0), "bounded probe exited without a valid receipt");
    serde_json::from_slice(&output.stdout).expect("bounded probe must return valid JSON")
}

#[cfg(unix)]
#[tokio::test]
async fn posix_cursor_reaches_late_repository_in_three_small_budget_runs() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("topology-progress-fixture-{}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    for name in ["a", "b", "c"] {
        let path = base.join(name);
        fs::create_dir_all(&path).unwrap();
        assert!(Command::new("/usr/bin/git").args(["init", "--quiet"]).arg(&path).status().unwrap().success());
        fs::write(path.join("package.json"), format!("{{\"name\":\"{name}\"}}")).unwrap();
    }
    let mut budget = limits();
    budget.repositories_per_environment = 1;
    budget.entries_per_environment = 8;
    let mut config = json!({"scope_id":"test:progress","observed_at_ms":1000,"roots":[base],"configs":[],"limits":budget});
    let mut observed = std::collections::BTreeSet::new();
    for index in 0..3 {
        let capture = run_posix_probe(&config).await;
        for repo in capture["repositories"].as_array().unwrap() {
            if repo["failures"].as_array().unwrap().is_empty() {
                observed.insert(repo["relative_path"].as_str().unwrap().to_owned());
            }
        }
        if index < 2 {
            assert!(capture["progress"].is_object(), "small repository budget must return its next boundary");
        }
        config["cursor"] = capture["progress"].clone();
    }
    assert_eq!(observed, std::collections::BTreeSet::from(["a".to_owned(), "b".to_owned(), "c".to_owned()]));
    assert!(config["cursor"].is_null());
    fs::remove_dir_all(base).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn posix_skip_policy_change_invalidates_legacy_cursor_and_discovers_normal_checkout() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("topology-legacy-skip-{}", std::process::id()));
    for name in ["normal-a", "normal-b"] {
        let repo = base.join(name);
        fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("/usr/bin/git").args(["init", "--quiet"]).arg(&repo).status().unwrap().success());
        fs::write(repo.join("package.json"), format!(r#"{{"name":"{name}"}}"#)).unwrap();
    }
    fs::create_dir_all(base.join(".branch-recovery/objects")).unwrap();
    fs::write(base.join(".branch-recovery/objects/package.json"), "{ invalid").unwrap();
    let mut budget = limits();
    budget.repositories_per_environment = 1;
    let mut config = json!({"scope_id":"test:legacy-skip","observed_at_ms":1000,"roots":[base],"configs":[],"limits":budget});
    let legacy_probe = POSIX_PROBE.replacen("\".branch-recovery\", ", "", 1);
    let prefix = format!("import json\nCONFIG=json.loads({})\n", serde_json::to_string(&config.to_string()).unwrap());
    let legacy =
        run_bounded("/usr/bin/python3", &["-B".into(), "-".into()], format!("{prefix}{legacy_probe}").as_bytes(), Duration::from_secs(10), 1_000_000, 1000)
            .await;
    assert_eq!(legacy.code, Some(0), "{}", String::from_utf8_lossy(&legacy.stderr));
    let legacy: Value = serde_json::from_slice(&legacy.stdout).unwrap();
    assert!(legacy["progress"].is_object());
    config["cursor"] = legacy["progress"].clone();
    let resumed = run_posix_probe(&config).await;
    assert!(
        resumed["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|failure| failure["code"] == "unsupported" && failure["message"] == "continuation cursor invalidated")
    );
    assert!(resumed["repositories"].as_array().unwrap().iter().any(|repo| repo["relative_path"] == "normal-a" || repo["relative_path"] == "normal-b"));
    assert!(resumed["repositories"].as_array().unwrap().iter().all(|repo| !repo["relative_path"].as_str().unwrap_or_default().contains("branch-recovery")));
    fs::remove_dir_all(base).unwrap();
}

#[tokio::test]
async fn remote_runtime_and_malicious_cursor_fail_without_cache_or_progress() {
    let mut request = remote();
    request.expected_runtime = RemoteRuntimeKind::Wsl;
    request.expected_distro = Some("ubuntu".into());
    let mut raw = remote_capture(&request);
    raw["runtime"]["kernel_wsl"] = json!(false);
    raw["progress"] = json!({"version":1,"fingerprint":"a".repeat(64),"next_root":0,"frames":[{"dir":".","after":""}]});
    let result = collect_remote_with(&Fake::new(vec![raw]), &request).await;
    assert!(result.repositories.is_empty() && result.progress.is_none());
    assert_eq!(result.failures[0].code, FailureCode::PermissionDenied);
    for directory in ["../escape", "C:/escape", "//server/share", "safe/../escape", "bad\npath"] {
        request.cursor = Some(json!({"version":1,"fingerprint":"a".repeat(64),"next_root":0,"frames":[{"dir":directory,"after":""}]}));
        let fake = Fake::new(vec![]);
        let result = collect_remote_with(&fake, &request).await;
        assert!(result.repositories.is_empty() && result.progress.is_none());
        assert!(fake.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn typed_json_hash_has_stable_unicode_order_numbers_and_rejects_large_integers() {
    use tune_core::topology::adapters::remote::hash_json_typed_v1;
    assert_eq!(hash_json_typed_v1(&json!({"b":1,"a":"\u{00e9}"})).unwrap(), hash_json_typed_v1(&json!({"a":"\u{00e9}","b":1.0})).unwrap());
    assert_eq!(hash_json_typed_v1(&json!(-0.0)).unwrap(), hash_json_typed_v1(&json!(0)).unwrap());
    assert!(hash_json_typed_v1(&json!(9007199254740993u64)).is_err());
    assert!(hash_json_typed_v1(&json!(1e30)).is_err());
    assert!(hash_json_typed_v1(&json!(9007199254740992u64)).is_ok());
}

#[test]
fn typed_hash_matches_all_provider_fixture_vectors() {
    use tune_core::topology::adapters::remote::hash_json_typed_v1;
    let vectors = [
        (r#"null"#, "594e519ae499312b29433b7dd8a97ff068defcba9755b6d5d00e84c524d67b06"),
        (r#"false"#, "c02c0b965e023abee808f2b548d8d5193a8b5229be6f3121a6f16e2d41a449b3"),
        (r#"true"#, "7dc96f776c8423e57a2785489a3f9c43fb6e756876d6ad9a9cac4aa4e72ec193"),
        (r#""\u00e9\u65e5\ud83d\ude00""#, "1789205cfc80125c6a35e514397ac90668e4f7ea2e8bde60a45f854f123b316a"),
        (r#"[true,null,1,1.0,-0.0,0.1]"#, "3431f86841b06fe316871400d9c8bb906162fac829b78c049ddfecd1bbcf4145"),
        (r#"{"\ud83d\ude00":"x","\ue000":"y","a":1.25}"#, "4a24b26c851c89e0aeeac90d1c3396f72a00d9fdaf0b9acde6f09ef5142717e6"),
        (r#"0"#, "ccc8f5baaccd9479ed418376a27544490399157ba4fc3f75e635244540c2b713"),
        (r#"1"#, "73af61a19efb83c5922f1e2d07eceae40f55d2c60d0ffe1664f75e66f8f6c8da"),
        (r#"0.1"#, "9a64b3ebfdcfa1031ec6dbd94980c7d0eb8d6d037a8a8756ec73b4eb4996c6ad"),
        (r#"-0.0"#, "ccc8f5baaccd9479ed418376a27544490399157ba4fc3f75e635244540c2b713"),
        (r#"9007199254740992"#, "62789eda7303b08d4fc7a0435c3e45f9af40f6b91097ac0c7b02f269fde1dea3"),
    ];
    for (source, expected) in vectors {
        let value: Value = serde_json::from_str(source).unwrap();
        assert_eq!(hash_json_typed_v1(&value).unwrap(), expected);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn posix_listing_cache_counts_160_directories_once_and_completes_checkout() {
    use std::{fs, process::Command};
    let root = std::env::temp_dir().join(format!("topology-scaling-fixture-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    assert!(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false", "init", "--quiet", "--template="])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    let content = format!("{{\"name\":\"fixture\",\"padding\":\"{}\"}}", "x".repeat(8192));
    for index in 0..160 {
        let directory = root.join(format!("package-{index:03}"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("package.json"), &content).unwrap();
    }
    let config = json!({"scope_id":"test:scaling","observed_at_ms":1000,"roots":[root],"configs":[],"limits":limits()});
    let result = run_posix_probe(&config).await;
    assert!(result["progress"].is_null(), "321 unique entries must fit the default 20000 entry budget");
    assert_eq!(result["counters"]["entries"], json!(321));
    assert_eq!(result["counters"]["manifests"], json!(160));
    assert!(result["repositories"][0]["failures"].as_array().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_transport_sends_bounded_raw_stdin_and_rejects_oversized_before_ssh() {
    let mut request = remote();
    request.platform = RemotePlatform::Windows;
    request.roots = vec!["C:\\fixture".into()];
    request.configs = vec![json!({"fixture_padding":"x".repeat(70000)})];
    let fake = Fake::new(vec![remote_capture(&request)]);
    assert!(collect_remote_with(&fake, &request).await.failures.is_empty());
    {
        let calls = fake.calls.lock().unwrap();
        assert!(calls[0].2.starts_with(b"$TopologyRequest"));
        assert!(calls[0].2.len() > 70000);
        assert!(calls[0].2.len() <= 2 * 1024 * 1024);
        assert!(String::from_utf8(calls[0].2.clone()).unwrap().ends_with(WINDOWS_PROBE.trim_start_matches('\u{feff}')));
    }
    request.limits.file_bytes = 4 * 1024 * 1024;
    request.configs = vec![json!({"fixture_padding":"x".repeat(2*1024*1024)})];
    let fake = Fake::new(vec![]);
    let result = collect_remote_with(&fake, &request).await;
    assert!(fake.calls.lock().unwrap().is_empty());
    assert_eq!(result.failures[0].code, FailureCode::LimitReached);
}

#[cfg(unix)]
#[tokio::test]
async fn posix_probe_configuration_caps_are_partial_and_counted() {
    use std::fs;
    let base = std::env::temp_dir().join(format!("topology-config-cap-{}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    let first = base.join("one.json");
    let second = base.join("two.json");
    fs::write(&first, r#"{"value":1}"#).unwrap();
    fs::write(&second, r#"{"value":2}"#).unwrap();
    let mut budget = limits();
    budget.manifests_per_environment = 1;
    let config = json!({
        "scope_id":"test:config-cap", "observed_at_ms":1000, "roots":[base],
        "configs":[
          {"path":first,"format":"json","keys":[{"pointer":"/value","mode":"hash"}]},
          {"path":second,"format":"json","keys":[{"pointer":"/value","mode":"hash"}]}
        ], "limits":budget
    });
    let prefix = format!("import json\nCONFIG=json.loads({})\n", serde_json::to_string(&config.to_string()).unwrap());
    let output = run_bounded("/usr/bin/python3", &["-".into()], format!("{prefix}{POSIX_PROBE}").as_bytes(), Duration::from_secs(10), 1_000_000, 1000).await;
    assert_eq!(output.code, Some(0));
    let capture: RawCapture = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(capture.counters.config_files, 1);
    assert_eq!(capture.counters.config_keys, 1);
    assert!(capture.failures.iter().any(|failure| failure.code == FailureCode::LimitReached && failure.message == "configuration file budget exceeded"));
    assert!(
        capture.repositories.iter().all(|repo| repo.failures.iter().all(|failure| failure.code != FailureCode::LimitReached)),
        "the unstarted second config must not be represented as complete"
    );
    fs::remove_dir_all(base).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn posix_probe_configuration_key_cap_is_environment_wide() {
    use std::fs;
    let base = std::env::temp_dir().join(format!("topology-config-key-cap-{}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    let first = base.join("one.json");
    let second = base.join("two.json");
    fs::write(&first, r#"{"one":1}"#).unwrap();
    fs::write(&second, r#"{"one":1,"two":2}"#).unwrap();
    let mut budget = limits();
    budget.manifests_per_environment = 2;
    let config = json!({"scope_id":"test:config-key-cap","observed_at_ms":1000,"roots":[base],"configs":[
        {"path":first,"format":"json","keys":[{"pointer":"/one","mode":"hash"}]},
        {"path":second,"format":"json","keys":[{"pointer":"/one","mode":"hash"},{"pointer":"/two","mode":"hash"}]}
    ],"limits":budget});
    let prefix = format!("import json\nCONFIG=json.loads({})\n", serde_json::to_string(&config.to_string()).unwrap());
    let output = run_bounded("/usr/bin/python3", &["-".into()], format!("{prefix}{POSIX_PROBE}").as_bytes(), Duration::from_secs(10), 1_000_000, 1000).await;
    assert_eq!(output.code, Some(0));
    let capture: RawCapture = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(capture.counters.config_files, 2);
    assert_eq!(capture.counters.config_keys, 2);
    assert!(capture.failures.iter().any(|failure| failure.code == FailureCode::LimitReached && failure.message == "configuration key budget exceeded"));
    assert!(capture.repositories.last().unwrap().failures.iter().any(|failure| failure.code == FailureCode::LimitReached));
    fs::remove_dir_all(base).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn posix_probe_missing_configuration_key_keeps_a_partial_receipt() {
    use std::fs;
    let base = std::env::temp_dir().join(format!("topology-config-missing-key-{}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    let selected = base.join("selected.json");
    fs::write(&selected, r#"{"present":true}"#).unwrap();
    let request = remote();
    let config = json!({"scope_id":request.scope_id.clone(),"observed_at_ms":request.observed_at_ms,"roots":[base],"configs":[
        {"path":selected,"format":"json","keys":[{"pointer":"/missing","mode":"hash"}]}
    ],"limits":request.limits});
    let prefix = format!("import json\nCONFIG=json.loads({})\n", serde_json::to_string(&config.to_string()).unwrap());
    let output = run_bounded("/usr/bin/python3", &["-".into()], format!("{prefix}{POSIX_PROBE}").as_bytes(), Duration::from_secs(10), 1_000_000, 1000).await;
    assert_eq!(output.code, Some(0));
    let mut reported: Value = serde_json::from_slice(&output.stdout).unwrap();
    reported["target_identity"] = json!(request.expected_identity_hash.clone());
    reported["runtime"] = json!({"kind":"native","platform":"posix","kernel_wsl":false,"distro":null});
    let capture = collect_remote_with(&Fake::new(vec![reported]), &request).await;
    assert!(capture.failures.is_empty(), "the scoped failure is a valid partial capture");
    assert_eq!(capture.counters.config_files, 1);
    assert_eq!(capture.counters.config_keys, 0);
    assert!(capture.repositories.iter().any(|repo| repo.config_file_read && repo.failures.iter().any(|failure| failure.code == FailureCode::ParseError)));
    fs::remove_dir_all(base).unwrap();
}

#[tokio::test]
async fn remote_rejects_inconsistent_configuration_counters() {
    let request = remote();
    let mut malformed = remote_capture(&request);
    malformed["counters"]["config_files"] = json!(1);
    let result = collect_remote_with(&Fake::new(vec![malformed]), &request).await;
    assert!(result.failures.iter().any(|failure| failure.code == FailureCode::ParseError && failure.message == "remote report violates bounded schema"));
}

#[tokio::test]
async fn remote_keeps_failed_configuration_receipts_without_claiming_a_read() {
    let request = remote();
    let mut reported = remote_capture(&request);
    reported["counters"]["repositories"] = json!(2);
    reported["counters"]["config_files"] = json!(0);
    reported["counters"]["config_keys"] = json!(0);
    reported["repositories"].as_array_mut().unwrap().push(json!({
        "relative_path": format!("configuration-{}", "c".repeat(64)),
        "root_fingerprint": "d".repeat(64),
        "git": {"origin":null,"head":null,"git_dir":null,"common_dir":null,"is_worktree":false},
        "manifests": [], "configs": [], "config_file_read": false,
        "failures": [{"code":"parse_error","message":"selected configuration unavailable"}]
    }));
    let capture = collect_remote_with(&Fake::new(vec![reported]), &request).await;
    assert!(capture.failures.is_empty());
    assert_eq!(capture.counters.config_files, 0);
    assert_eq!(capture.counters.config_keys, 0);
    assert!(capture.repositories.iter().any(|repo| repo.relative_path.starts_with("configuration-") && !repo.failures.is_empty()));
}

#[tokio::test]
async fn remote_rejects_configuration_receipts_that_claim_no_read_without_a_failure() {
    let request = remote();
    let mut reported = remote_capture(&request);
    reported["counters"]["repositories"] = json!(2);
    reported["repositories"].as_array_mut().unwrap().push(json!({
        "relative_path": format!("configuration-{}", "c".repeat(64)),
        "root_fingerprint": "d".repeat(64),
        "git": {"origin":null,"head":null,"git_dir":null,"common_dir":null,"is_worktree":false},
        "manifests": [], "configs": [], "config_file_read": false, "failures": []
    }));
    let result = collect_remote_with(&Fake::new(vec![reported]), &request).await;
    assert!(result.failures.iter().any(|failure| failure.code == FailureCode::ParseError && failure.message == "remote report violates bounded schema"));
}

#[tokio::test]
async fn remote_rejects_mismatched_retained_manifest_count() {
    let request = remote();
    let mut malformed = remote_capture(&request);
    malformed["counters"]["manifests"] = json!(0);
    let result = collect_remote_with(&Fake::new(vec![malformed]), &request).await;
    assert!(result.failures.iter().any(|failure| failure.code == FailureCode::ParseError && failure.message == "remote report violates bounded schema"));
}
