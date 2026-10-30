//! Direct local IPC Docker collection; no environment discovery or subprocesses.
//! Explicit opt-in is `docker.enabled=true` plus a `docker.direct_api` object:
//! `{"endpoints":[{"kind":"unix","endpoint":"unix:///approved/docker.sock",
//! "uid":0,"gid":0,"socket_uid":0,"socket_gid":1001}]}` (synthetic values).
//! Socket ownership and peer uid/gid are distinct policies. Windows additionally
//! requires pid, full created_filetime, session, owner_sid and image_path_sha256.
//! The latter hashes the lowercase queried image path, not executable contents or signature.
//! No real policy is discovered, bootstrapped, persisted or authorized by this module.
mod http;
mod peer;
mod projection;
use super::docker::engine_snapshots;
use crate::topology::TopologyLimits;
use crate::topology::{
    Failure, FailureCode, ScopeStatus, TopologySnapshot,
    candidates::{CandidateAttempt, DockerCandidatePayload, DockerSemanticGap},
};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectConfig {
    endpoints: Vec<peer::Policy>,
}
async fn observe(config: &DirectConfig, limits: &TopologyLimits, deadline: Instant) -> Result<http::Observation, &'static str> {
    http::observe(config, limits, deadline).await
}
pub(crate) struct DirectBatch {
    pub(crate) snapshots: Vec<crate::topology::TopologySnapshot>,
    pub(crate) admissions: std::collections::HashMap<String, crate::topology::candidates::AdmittedDockerPartial>,
}
/// Unserializable owner of actual local IPC evidence. No constructor accepts caller-provided facts.
pub(crate) struct ApiObservation {
    observation: http::Observation,
    scope: String,
    source: String,
    settings: String,
    started: i64,
    finished: i64,
    deadline: Instant,
    duration_ms: u64,
}
impl ApiObservation {
    pub(crate) fn check(&self) -> Result<(), &'static str> {
        self.observation.check()?;
        if Instant::now() >= self.deadline { Err("Docker batch deadline exceeded") } else { Ok(()) }
    }
    pub(crate) fn project(&self) -> Result<(Vec<TopologySnapshot>, Vec<DockerCandidatePayload>, String), &'static str> {
        self.observation.check()?;
        if Instant::now() >= self.deadline || self.started < 0 || self.finished < self.started {
            return Err("Docker observation interval invalid");
        }
        let packet = &self.observation.packet;
        let mut coverage = packet.snapshot(self.scope.clone(), self.finished)?;
        coverage.counters.bytes = self.observation.bytes;
        coverage.counters.duration_ms = self.duration_ms;
        let snapshots = engine_snapshots(coverage);
        let mut payloads = vec![];
        for (engine, snapshot) in packet.engines.iter().zip(snapshots.iter().skip(1)) {
            if snapshot.status == ScopeStatus::Complete {
                continue;
            }
            let networks = engine.networks.iter().map(|n| n.id.as_str()).collect::<std::collections::HashSet<_>>();
            let volumes = engine.volumes.iter().map(|v| v.id.as_str()).collect::<std::collections::HashSet<_>>();
            let mut gaps = vec![];
            if engine.containers.iter().any(|c| c.networks.iter().any(|n| !networks.contains(n.as_str()))) {
                gaps.push(DockerSemanticGap::UnobservedNetworkMembership);
            }
            if engine.containers.iter().any(|c| c.mounts.iter().any(|m| m.kind == "volume" && !m.volume_hash.as_deref().is_some_and(|v| volumes.contains(v)))) {
                gaps.push(DockerSemanticGap::UnobservedVolumeTarget);
            }
            let payload = DockerCandidatePayload {
                version: 1,
                scope_id: snapshot.scope_id.clone(),
                engine_identity: engine.id.clone(),
                source_generation: self.source.clone(),
                settings_generation: self.settings.clone(),
                facts_started_at_ms: self.started,
                facts_observed_at_ms: self.finished,
                attempt_started_at_ms: self.started,
                attempt_finished_at_ms: self.finished,
                gaps,
                entities: snapshot.entities.clone(),
                edges: snapshot.edges.clone(),
            };
            crate::topology::candidates::validate(&payload, crate::db::now_ms())?;
            payloads.push(payload);
        }
        // Bind the complete request/response + endpoint/peer transcript to its exact projected facts and interval.
        let digest = crate::db::sha256_hex(
            &serde_json::to_vec(&(&self.observation.digest, &self.source, &self.settings, self.started, self.finished, &snapshots))
                .map_err(|_| "invalid Docker batch binding")?,
        );
        self.observation.check()?;
        if Instant::now() >= self.deadline {
            return Err("Docker batch deadline exceeded");
        }
        Ok((snapshots, payloads, digest))
    }
}
fn source_generation() -> String {
    // Fixed compiled implementation bytes, not a caller claim or binary/daemon attestation.
    let mut bytes = b"docker-local-ipc-issuer.v2".to_vec();
    for source in [
        include_bytes!("docker_api.rs").as_slice(),
        include_bytes!("docker_api/http.rs").as_slice(),
        include_bytes!("docker_api/peer.rs").as_slice(),
        include_bytes!("docker_api/peer/windows.rs").as_slice(),
        include_bytes!("docker_api/projection.rs").as_slice(),
        include_bytes!("docker.rs").as_slice(),
        include_bytes!("../candidates.rs").as_slice(),
    ] {
        bytes.extend_from_slice(&(source.len() as u64).to_le_bytes());
        bytes.extend_from_slice(source);
    }
    crate::db::sha256_hex(&bytes)
}
pub(crate) fn batch_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let mut nonce = std::collections::hash_map::RandomState::new().build_hasher();
    nonce.write_u64(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    nonce.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    format!("docker-api-{:016x}", nonce.finish())
}
pub(crate) fn failed(scope: &str, code: FailureCode) -> DirectBatch {
    DirectBatch {
        snapshots: vec![TopologySnapshot {
            scope_id: scope.into(),
            status: ScopeStatus::Failed,
            entities: vec![],
            edges: vec![],
            failures: vec![Failure::new(code, "Docker direct local IPC collection did not complete")],
            counters: Default::default(),
            observed_at_ms: crate::db::now_ms(),
        }],
        admissions: Default::default(),
    }
}
fn native_platform() -> Result<(), &'static str> {
    if std::env::var_os("WSL_INTEROP").is_some() || std::env::var_os("WSL_DISTRO_NAME").is_some() {
        return Err("unsupported WSL runtime");
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        let mut release = Vec::new();
        std::fs::File::open("/proc/sys/kernel/osrelease")
            .map_err(|_| "native runtime unavailable")?
            .take(4097)
            .read_to_end(&mut release)
            .map_err(|_| "native runtime unavailable")?;
        if release.is_empty() || release.len() > 4096 {
            return Err("native runtime unavailable");
        }
        let release = std::str::from_utf8(&release).map_err(|_| "native runtime unavailable")?.to_ascii_lowercase();
        if release.contains("microsoft") || release.contains("wsl") {
            return Err("unsupported WSL runtime");
        }
    }
    if !cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        return Err("unsupported native platform");
    }
    Ok(())
}
pub(crate) async fn collect(config: &serde_json::Value, scope: &str, limits: &TopologyLimits, attempt: CandidateAttempt) -> DirectBatch {
    let result = async {
        let began = Instant::now();
        let started = crate::db::now_ms();
        let deadline = began + std::time::Duration::from_secs(limits.local_timeout_seconds.min(60));
        native_platform()?;
        if !scope.ends_with(":docker") || scope.len() > 400 || scope.chars().any(char::is_control) {
            return Err("invalid Docker scope");
        }
        let config: DirectConfig = serde_json::from_value(config.clone()).map_err(|_| "invalid explicit Docker policy")?;
        let settings = crate::db::sha256_hex(&serde_json::to_vec(&(&config, limits, scope)).map_err(|_| "invalid Docker settings")?);
        let observation = observe(&config, limits, deadline).await?;
        let finished = crate::db::now_ms();
        let actual = ApiObservation {
            observation,
            scope: scope.into(),
            source: source_generation(),
            settings,
            started,
            finished,
            deadline,
            duration_ms: began.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        };
        crate::topology::candidates::admit_api(actual, attempt)
    }
    .await;
    result.unwrap_or_else(|reason| {
        failed(
            scope,
            if reason.contains("deadline") {
                FailureCode::Timeout
            } else if reason.contains("budget") || reason.contains("count") {
                FailureCode::LimitReached
            } else if reason.contains("peer") || reason.contains("policy") {
                FailureCode::PermissionDenied
            } else {
                FailureCode::ParseError
            },
        )
    })
}
#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    trait FixtureIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
    impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> FixtureIo for T {}
    #[cfg(unix)]
    type Listener = tokio::net::UnixListener;
    #[cfg(windows)]
    type Listener = tokio::net::windows::named_pipe::NamedPipeServer;
    fn bind_fixture() -> (std::path::PathBuf, DirectConfig, Listener) {
        let name = format!("docker-api-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        #[cfg(unix)]
        {
            let path = std::env::temp_dir().join(format!("{name}.sock"));
            let listener = Listener::bind(&path).unwrap();
            let m = std::fs::symlink_metadata(&path).unwrap();
            (
                path.clone(),
                DirectConfig {
                    endpoints: vec![peer::Policy::Unix {
                        endpoint: format!("unix://{}", path.display()),
                        uid: m.uid(),
                        gid: m.gid(),
                        socket_uid: m.uid(),
                        socket_gid: m.gid(),
                    }],
                },
                listener,
            )
        }
        #[cfg(windows)]
        {
            let listener = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(format!(r"\\.\pipe\{name}"))
                .unwrap();
            (std::path::PathBuf::from(&name), DirectConfig { endpoints: vec![peer::fixture_policy(format!("npipe:////./pipe/{name}"))] }, listener)
        }
    }
    async fn accept(listener: Listener) -> Box<dyn FixtureIo> {
        #[cfg(unix)]
        {
            Box::new(listener.accept().await.unwrap().0)
        }
        #[cfg(windows)]
        {
            listener.connect().await.unwrap();
            Box::new(listener)
        }
    }
    fn cleanup(path: std::path::PathBuf) {
        #[cfg(unix)]
        std::fs::remove_file(path).unwrap();
        #[cfg(windows)]
        let _ = path;
    }
    async fn fixture(responses: Vec<String>) -> (std::path::PathBuf, DirectConfig, tokio::task::JoinHandle<()>) {
        let (path, config, listener) = bind_fixture();
        let worker = tokio::spawn(async move {
            let mut stream = accept(listener).await;
            let inspect_id = responses
                .get(2)
                .and_then(|r| r.split_once("\r\n\r\n"))
                .and_then(|(_, b)| serde_json::from_str::<serde_json::Value>(b).ok())
                .and_then(|v| v[0]["Id"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "a".repeat(64));
            let paths = [
                "/version".to_owned(),
                "/v1.45/info".to_owned(),
                "/v1.45/containers/json?all=1".to_owned(),
                format!("/v1.45/containers/{inspect_id}/json"),
                "/v1.45/networks".to_owned(),
                "/v1.45/volumes".to_owned(),
            ];
            for (index, response) in responses.into_iter().enumerate() {
                let mut request = vec![];
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    request.push(byte[0]);
                    assert!(request.len() < 2048);
                }
                let text = std::str::from_utf8(&request).unwrap();
                assert_eq!(text.split("\r\n").next().unwrap(), format!("GET {} HTTP/1.1", paths[index]));
                let headers = text.to_ascii_lowercase();
                assert!(!headers.contains("authorization:") && !headers.contains("x-registry-auth:"));
                if stream.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
            }
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap_or(0), 0, "no additional requests or retries");
        });
        (path, config, worker)
    }
    fn response(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}", body.len(), body)
    }
    fn sample_responses() -> Vec<String> {
        vec![
            response(r#"{"ApiVersion":"1.45","MinAPIVersion":"1.24"}"#),
            response(r#"{"ID":"fixture-engine","Env":"secret-canary"}"#),
            response(&format!(r#"[{{"Id":"{}"}}]"#, "a".repeat(64))),
            response(&format!(
                r#"{{"Id":"{}","State":{{"Status":"exited"}},"HostConfig":{{"Memory":0,"NanoCpus":0,"LogConfig":null}},"Mounts":[{{"Type":"volume","Destination":"/fixture","Source":"secret-canary","RW":true}}],"NetworkSettings":{{"Networks":{{"fixture":{{"NetworkID":"{}"}}}},"Ports":{{}}}},"Config":{{"Env":["secret-canary"],"Labels":{{"secret-canary":"secret-canary"}}}}}}"#,
                "a".repeat(64),
                "b".repeat(64)
            )),
            response("[]"),
            response(r#"{"Volumes":[]}"#),
        ]
    }
    #[tokio::test]
    async fn actual_local_ipc_http_projects_without_secrets_and_preserves_zero_and_semantic_gap() {
        let responses = sample_responses();
        let (path, config, worker) = fixture(responses).await;
        let result = observe(&config, &TopologyLimits::default(), Instant::now() + std::time::Duration::from_secs(2)).await;
        worker.abort();
        let _ = worker.await;
        cleanup(path);
        let observation = result.expect("actual IPC GET batch must be projected");
        let packet = observation.packet;
        assert_eq!(packet.engines.len(), 1);
        let c = &packet.engines[0].containers[0];
        assert_eq!(c.memory_bytes, Some(0));
        assert_eq!(c.nano_cpus, Some(0));
        assert_eq!(c.health, None);
        assert!(!c.current_unhealthy);
        let snapshot = packet.snapshot("fixture:native:docker".into(), crate::db::now_ms()).unwrap();
        assert_eq!(snapshot.status, crate::topology::ScopeStatus::Partial);
        assert!(!serde_json::to_string(&packet).unwrap().contains("secret-canary"));
    }
    #[tokio::test]
    async fn actual_http_limits_eof_encoding_and_deadline_fail_closed_and_close_owned_stream() {
        let cases = vec![
            ("oversize-content-length", format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", 1024 * 1024 + 1), false),
            ("header", format!("HTTP/1.1 200 OK\r\nX-Fixed: {}\r\nContent-Length: 2\r\n\r\n{{}}", "x".repeat(17000)), false),
            (
                "header-count",
                format!("HTTP/1.1 200 OK\r\n{}Content-Length: 2\r\n\r\n{{}}", (0..40).map(|n| format!("X-{n}: x\r\n")).collect::<String>()),
                false,
            ),
            ("chunk-wire", format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{}0\r\n\r\n", "1\r\nx\r\n".repeat(800)), false),
            ("chunked-body", format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n100\r\n{}\r\n0\r\n\r\n", "x".repeat(256)), false),
            (
                "information-wire",
                format!("{}HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{{}}", "HTTP/1.1 100 Continue\r\nX-Fixed: x\r\n\r\n".repeat(100)),
                false,
            ),
            ("compression", "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\n{}".into(), false),
            ("redirect", "HTTP/1.1 302 Found\r\nLocation: http://untrusted.invalid/\r\nContent-Length: 2\r\n\r\n{}".into(), false),
            ("error-body", "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 99999999\r\n\r\n".into(), false),
            ("delayed-body", "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{".into(), true),
            ("malformed-json", response(r#"{"ApiVersion":false,"Message":"secret-canary"}"#), false),
        ];
        for (label, response, deadline_expected) in cases {
            let (path, config, worker) = fixture(vec![response]).await;
            let mut limits = TopologyLimits::default();
            if label.ends_with("wire") {
                limits.total_bytes_per_environment = 2048;
            }
            if label == "chunked-body" {
                limits.file_bytes = 32;
            }
            let start = Instant::now();
            let result = observe(&config, &limits, start + std::time::Duration::from_millis(100)).await;
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("{label}: invalid batch admitted"),
            };
            if deadline_expected {
                assert_eq!(error, "Docker batch deadline exceeded");
            }
            if label == "chunked-body" {
                assert_eq!(error, "Docker body incomplete or exceeded budget");
            }
            assert!(!error.contains("secret-canary"));
            assert!(start.elapsed() < std::time::Duration::from_secs(1));
            // Reader completion proves the collector dropped both the driver and retained peer stream.
            tokio::time::timeout(std::time::Duration::from_secs(1), worker).await.expect("fixture must observe EOF after failure").unwrap();
            cleanup(path);
        }
        let (path, config, listener) = bind_fixture();
        let worker = tokio::spawn(async move {
            let mut stream = accept(listener).await;
            let mut b = [0; 1024];
            let _ = stream.read(&mut b).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{").await.unwrap();
        });
        assert!(observe(&config, &TopologyLimits::default(), Instant::now() + std::time::Duration::from_secs(1)).await.is_err());
        worker.await.unwrap();
        cleanup(path);
    }
    #[tokio::test]
    async fn external_cancellation_drops_driver_and_retained_peer_without_a_detached_task() {
        let (path, config, worker) = fixture(vec!["HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{".into()]).await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                observe(&config, &TopologyLimits::default(), Instant::now() + std::time::Duration::from_secs(10))
            )
            .await
            .is_err()
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), worker).await.expect("cancel must close every owned stream").unwrap();
        cleanup(path);
    }

    #[tokio::test]
    async fn actual_io_issuer_reaches_fenced_candidate_store_after_its_collection_receipts() {
        let db = crate::db::Store::open_in_memory().unwrap();
        let now = crate::db::now_ms();
        let lease = db.acquire_topology_lease_fenced("fixture-issuer", now, 10000).unwrap().unwrap();
        let attempt = db.reserve_docker_attempt_fenced(&lease, "fixture-batch", now).unwrap();
        let (path, config, worker) = fixture(sample_responses()).await;
        let mut batch = collect(&serde_json::to_value(config).unwrap(), "fixture:native:docker", &TopologyLimits::default(), attempt).await;
        worker.abort();
        let _ = worker.await;
        cleanup(path);
        assert_eq!(batch.admissions.len(), 1, "actual semantic Partial must produce one owned authority");
        assert_eq!(batch.snapshots.len(), 2);
        for snapshot in &batch.snapshots {
            db.record_topology_fenced(&lease, "fixture-batch", snapshot, crate::db::now_ms()).unwrap();
            if let Some(cap) = batch.admissions.remove(&snapshot.scope_id) {
                db.record_docker_candidate_fenced(&lease, cap, crate::db::now_ms()).unwrap();
            }
        }
        let view = db.topology_view().unwrap();
        assert_eq!(view["candidates"].as_array().unwrap().len(), 1);
        assert!(view["entities"].as_array().unwrap().is_empty());
        assert!(!view.to_string().contains("secret-canary"));
    }

    #[tokio::test]
    async fn missing_volume_catalog_and_nonempty_warnings_are_hard_failures() {
        for (body, valid) in
            [("{}", false), (r#"{"Volumes":[],"Warnings":["secret-canary"]}"#, false), (r#"{"Volumes":[]}"#, true), (r#"{"Volumes":null,"Warnings":[]}"#, true)]
        {
            let mut responses = sample_responses();
            *responses.last_mut().unwrap() = response(body);
            let (path, config, worker) = fixture(responses).await;
            let result = observe(&config, &TopologyLimits::default(), Instant::now() + std::time::Duration::from_secs(2)).await;
            worker.abort();
            let _ = worker.await;
            cleanup(path);
            assert_eq!(result.is_ok(), valid, "catalog presence and warning status must be explicit");
        }
    }

    #[tokio::test]
    async fn engine_direct_opt_in_uses_unique_batch_reservations_for_two_environments() {
        struct Host;
        impl crate::engine::Host for Host {}
        let (path_a, config_a, worker_a) = fixture(sample_responses()).await;
        let (path_b, config_b, worker_b) = fixture(sample_responses()).await;
        let directory =
            std::env::temp_dir().join(format!("docker-api-engine-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir(&directory).unwrap();
        let config_path = directory.join("fixture.json");
        let env = |id: &str, api: DirectConfig| serde_json::json!({"id":id,"transport":"node","roots":[],"configs":[],"docker":{"enabled":true,"program":"/fixture/not-trusted","direct_api":api}});
        let config = serde_json::json!({"topology":{"enabled":true,"github":{"enabled":false},"owners":[]},"schedule":{"enabled":false},"nodes":[{"id":"fixture","alias":"fixture","os":std::env::consts::OS,"local_hostname":crate::nodes::local_host(),"topology":{"environments":[env("native-one",config_a),env("native-two",config_b)]}}]});
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        let engine = crate::engine::Engine::open(config_path, directory.join("data"), std::sync::Arc::new(Host)).unwrap();
        let _receipt = engine.topology_refresh("fixture").await;
        worker_a.abort();
        let _ = worker_a.await;
        worker_b.abort();
        let _ = worker_b.await;
        cleanup(path_a);
        cleanup(path_b);
        let view = engine.topology_view().unwrap();
        let candidates = view["candidates"].as_array().unwrap();
        let count = candidates.len();
        let attempts = candidates.iter().filter_map(|c| c["attempt_id"].as_str()).collect::<std::collections::HashSet<_>>().len();
        drop(engine);
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(count, 2, "each explicit environment needs its actual direct IPC candidate");
        assert_eq!(attempts, 2, "global refresh run id must not alias collection reservations");
    }

    fn open_fixture_engine() -> (std::sync::Arc<crate::engine::Engine>, std::path::PathBuf, std::path::PathBuf) {
        struct Host;
        impl crate::engine::Host for Host {}
        let directory =
            std::env::temp_dir().join(format!("docker-api-state-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir(&directory).unwrap();
        let config_path = directory.join("fixture.json");
        std::fs::write(&config_path, b"{}").unwrap();
        let engine = crate::engine::Engine::open(config_path.clone(), directory.join("data"), std::sync::Arc::new(Host)).unwrap();
        (engine, directory, config_path)
    }
    async fn refresh_fixture(
        engine: &crate::engine::Engine,
        config_path: &std::path::Path,
        responses: Vec<String>,
        kind: Option<&str>,
    ) -> (serde_json::Value, serde_json::Value) {
        let (path, config, worker) = fixture(responses).await;
        let mut config = serde_json::json!({"topology":{"enabled":true,"github":{"enabled":false},"owners":[]},"schedule":{"enabled":false},"nodes":[{"id":"fixture","alias":"fixture","os":std::env::consts::OS,"local_hostname":crate::nodes::local_host(),"topology":{"environments":[{"id":"native","kind":kind,"transport":"node","roots":[],"configs":[],"docker":{"enabled":true,"program":"/fixture/not-trusted","direct_api":config}}]}}]});
        if kind.is_none() {
            config["nodes"][0]["topology"]["environments"][0].as_object_mut().unwrap().remove("kind");
        }
        std::fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        let receipt = engine.topology_refresh("fixture").await;
        worker.abort();
        let _ = worker.await;
        cleanup(path);
        (engine.topology_view().unwrap(), receipt)
    }
    #[tokio::test]
    async fn explicitly_wsl_environment_cannot_issue_even_with_a_valid_local_pipe() {
        let (engine, directory, config) = open_fixture_engine();
        let (view, receipt) = refresh_fixture(&engine, &config, sample_responses(), Some("wsl")).await;
        let count = view["candidates"].as_array().unwrap().len();
        let excluded =
            receipt["scopes"].as_array().unwrap().iter().any(|s| s["scope_id"].as_str().is_some_and(|s| s.ends_with(":docker")) && s["status"] == "excluded");
        drop(engine);
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(count, 0, "unverified WSL environment must never issue");
        assert!(excluded);
    }
    fn complete_responses() -> Vec<String> {
        let mut rows = sample_responses();
        let mut inspect: serde_json::Value = serde_json::from_str(rows[3].split_once("\r\n\r\n").unwrap().1).unwrap();
        inspect["Mounts"][0]["Name"] = serde_json::json!("fixture-volume");
        rows[3] = response(&inspect.to_string());
        rows[4] = response(&format!(r#"[{{"Id":"{}","Driver":"bridge","Internal":false}}]"#, "b".repeat(64)));
        rows[5] = response(r#"{"Volumes":[{"Name":"fixture-volume","Driver":"local"}],"Warnings":[]}"#);
        rows
    }
    #[tokio::test]
    async fn engine_receipt_failure_and_late_hard_failure_preserve_facts_then_complete_clears_candidate() {
        let (engine, directory, config) = open_fixture_engine();
        let (good, _) = refresh_fixture(&engine, &config, complete_responses(), None).await;
        let accepted = good["entities"].clone();
        assert!(!accepted.as_array().unwrap().is_empty());
        assert!(good["candidates"].as_array().unwrap().is_empty());
        let (partial, _) = refresh_fixture(&engine, &config, sample_responses(), None).await;
        assert_eq!(partial["entities"], accepted);
        assert_eq!(partial["candidates"].as_array().unwrap().len(), 1);
        let previous_attempt = partial["candidates"][0]["attempt_id"].clone();
        engine.with_db(|db|db.conn().execute_batch("CREATE TRIGGER fixture_receipt_abort BEFORE INSERT ON topology_scopes WHEN NEW.scope_id LIKE '%:docker:engine:%' BEGIN SELECT RAISE(ABORT,'fixture receipt failed');END;")).unwrap();
        let (failed, receipt) = refresh_fixture(&engine, &config, sample_responses(), None).await;
        assert_eq!(failed["entities"], accepted);
        assert_eq!(failed["candidates"][0]["attempt_id"], previous_attempt);
        assert!(receipt["scopes"].as_array().unwrap().iter().any(|s| s["failure"] == "internal"));
        engine.with_db(|db| db.conn().execute_batch("DROP TRIGGER fixture_receipt_abort")).unwrap();
        let mut hard = sample_responses();
        hard[5] = response(r#"{"Volumes":[],"Warnings":["secret-canary"]}"#);
        let (failed, receipt) = refresh_fixture(&engine, &config, hard, None).await;
        assert_eq!(failed["entities"], accepted);
        assert_eq!(failed["candidates"][0]["attempt_id"], previous_attempt);
        assert!(
            receipt["scopes"].as_array().unwrap().iter().any(|s| s["scope_id"].as_str().is_some_and(|s| s.ends_with(":docker")) && s["status"] == "failed")
        );
        let (complete, _) = refresh_fixture(&engine, &config, complete_responses(), None).await;
        assert!(complete["candidates"].as_array().unwrap().is_empty());
        assert!(!complete["entities"].as_array().unwrap().is_empty());
        assert!(!complete.to_string().contains("secret-canary"));
        drop(engine);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn actual_batch_fans_out_one_reservation_and_rejects_duplicate_or_late_failure_for_all_engines() {
        for mode in ["distinct", "duplicate", "late-error"] {
            let db = crate::db::Store::open_in_memory().unwrap();
            let now = crate::db::now_ms();
            let lease = db.acquire_topology_lease_fenced("fixture-batch-holder", now, 10000).unwrap().unwrap();
            let first = sample_responses();
            let mut second = sample_responses();
            if mode != "duplicate" {
                second = second
                    .into_iter()
                    .map(|r| {
                        response(
                            &r.split_once("\r\n\r\n")
                                .unwrap()
                                .1
                                .replace("fixture-engine", "fixture-engine-two")
                                .replace(&"a".repeat(64), &"c".repeat(64))
                                .replace(&"b".repeat(64), &"d".repeat(64)),
                        )
                    })
                    .collect();
            }
            if mode == "late-error" {
                second[5] = response(r#"{"Volumes":[],"Warnings":["secret-canary"]}"#);
            }
            let (path_a, mut config, worker_a) = fixture(first).await;
            let (path_b, other, worker_b) = fixture(second).await;
            config.endpoints.extend(other.endpoints);
            let batch_id = batch_id();
            let attempt = db.reserve_docker_attempt_fenced(&lease, &batch_id, now).unwrap();
            let mut batch = collect(&serde_json::to_value(config).unwrap(), "fixture:native:docker", &TopologyLimits::default(), attempt).await;
            worker_a.abort();
            let _ = worker_a.await;
            worker_b.abort();
            let _ = worker_b.await;
            cleanup(path_a);
            cleanup(path_b);
            if mode != "distinct" {
                assert!(batch.admissions.is_empty());
                assert_eq!(batch.snapshots.len(), 1);
                assert_eq!(batch.snapshots[0].status, ScopeStatus::Failed);
                continue;
            }
            assert_eq!(batch.admissions.len(), 2);
            assert_eq!(batch.snapshots.len(), 3);
            for snapshot in &batch.snapshots {
                db.record_topology_fenced(&lease, &batch_id, snapshot, crate::db::now_ms()).unwrap();
                if let Some(cap) = batch.admissions.remove(&snapshot.scope_id) {
                    db.record_docker_candidate_fenced(&lease, cap, crate::db::now_ms()).unwrap();
                }
            }
            let view = db.topology_view().unwrap();
            let candidates = view["candidates"].as_array().unwrap();
            assert_eq!(candidates.len(), 2);
            assert!(candidates.iter().all(|c| c["attempt_id"] == batch_id));
        }
    }
    #[tokio::test]
    async fn actual_capability_cannot_cross_a_writer_generation() {
        let db = crate::db::Store::open_in_memory().unwrap();
        let now = crate::db::now_ms();
        let old = db.acquire_topology_lease_fenced("fixture-old", now, 10000).unwrap().unwrap();
        let attempt = db.reserve_docker_attempt_fenced(&old, "fixture-old-batch", now).unwrap();
        let (path, config, worker) = fixture(sample_responses()).await;
        let mut batch = collect(&serde_json::to_value(config).unwrap(), "fixture:native:docker", &TopologyLimits::default(), attempt).await;
        worker.abort();
        let _ = worker.await;
        cleanup(path);
        let (_, cap) = batch.admissions.drain().next().unwrap();
        db.release_topology_lease_fenced(&old).unwrap();
        let new = db.acquire_topology_lease_fenced("fixture-new", crate::db::now_ms(), 10000).unwrap().unwrap();
        assert!(db.record_docker_candidate_fenced(&new, cap, crate::db::now_ms()).is_err());
        assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn diagnostic_receipts_and_claimed_generations_cannot_issue_authority() {
        let db = crate::db::Store::open_in_memory().unwrap();
        let now = crate::db::now_ms();
        let lease = db.acquire_topology_lease_fenced("fixture-denied", now, 10000).unwrap().unwrap();
        for (index, claim) in [
            serde_json::json!({"NoProcessExecution":true,"status":"partial","entities":[]}),
            serde_json::json!({"endpoints":[],"source_generation":"0".repeat(64),"settings_generation":"0".repeat(64)}),
        ]
        .iter()
        .enumerate()
        {
            let id = format!("fixture-denied-{index}");
            let token = db.reserve_docker_attempt_fenced(&lease, &id, now).unwrap();
            let batch = collect(claim, "fixture:native:docker", &TopologyLimits::default(), token).await;
            assert!(batch.admissions.is_empty());
            assert_eq!(batch.snapshots[0].status, ScopeStatus::Failed);
        }
    }
    #[tokio::test]
    async fn missing_inspect_catalogs_are_unknown_but_explicit_null_is_an_empty_catalog() {
        for missing in ["Mounts", "Networks", "Ports"] {
            let mut rows = sample_responses();
            let mut inspect: serde_json::Value = serde_json::from_str(rows[3].split_once("\r\n\r\n").unwrap().1).unwrap();
            if missing == "Mounts" {
                inspect.as_object_mut().unwrap().remove(missing);
            } else {
                inspect["NetworkSettings"].as_object_mut().unwrap().remove(missing);
            }
            rows[3] = response(&inspect.to_string());
            let (path, config, worker) = fixture(rows).await;
            let result = observe(&config, &TopologyLimits::default(), Instant::now() + std::time::Duration::from_secs(2)).await;
            worker.abort();
            let _ = worker.await;
            cleanup(path);
            assert!(result.is_err(), "missing {missing} cannot become empty observed facts");
        }
        let mut rows = sample_responses();
        let mut inspect: serde_json::Value = serde_json::from_str(rows[3].split_once("\r\n\r\n").unwrap().1).unwrap();
        inspect["Mounts"] = serde_json::Value::Null;
        inspect["NetworkSettings"]["Networks"] = serde_json::Value::Null;
        inspect["NetworkSettings"]["Ports"] = serde_json::Value::Null;
        rows[3] = response(&inspect.to_string());
        let (path, config, worker) = fixture(rows).await;
        let result = observe(&config, &TopologyLimits::default(), Instant::now() + std::time::Duration::from_secs(2)).await;
        worker.abort();
        let _ = worker.await;
        cleanup(path);
        let packet = result.expect("explicit null catalogs are known empty").packet;
        assert_eq!(packet.snapshot("fixture:native:docker".into(), crate::db::now_ms()).unwrap().status, ScopeStatus::Complete);
    }
    #[tokio::test]
    async fn actual_ipc_rich_data_matches_existing_cli_sanitizer_and_canonical_projection() {
        use crate::topology::adapters::docker::{DockerEngine, DockerPacket, Network, Volume, parse_container};
        let id = "a".repeat(64);
        let network = "b".repeat(64);
        let inspect = serde_json::json!({"Id":id,"State":{"Status":"exited","Health":{"Status":"unhealthy","Log":[{"Output":"secret-canary"}]}},"HostConfig":{"Memory":1048576,"NanoCpus":0,"CpuPeriod":100000,"CpuQuota":-1,"CpuShares":1024,"CpusetCpus":"0-2,4","RestartPolicy":{"Name":"on-failure","MaximumRetryCount":3},"LogConfig":{"Type":"json-file","Config":{"max-size":"10mb","max-file":"3","secret-canary":"secret-canary"}}},"Mounts":[{"Type":"volume","Name":"fixture-volume","Source":"/fixture/source","Destination":"/fixture/target","RW":false},{"Type":"bind","Source":"/fixture/bind","Destination":"/fixture/bound","RW":true}],"NetworkSettings":{"Networks":{"fixture":{"NetworkID":network}},"Ports":{"8080/tcp":[{"HostPort":"18080","HostIp":"secret-canary"}]}},"Config":{"Env":["secret-canary"],"Cmd":["secret-canary"],"Labels":{"secret-canary":"secret-canary"}}});
        let projected = serde_json::json!({"id":id,"state":"exited","health":"unhealthy","memory":1048576,"nano_cpus":0,"cpu_period":100000,"cpu_quota":-1,"cpu_shares":1024,"cpuset":"0-2,4","restart":"on-failure","restart_max":3,"log_driver":"json-file","max_size":"10mb","max_file":"3","mounts":[{"kind":"volume","name":"fixture-volume","source":"/fixture/source","target":"/fixture/target","rw":false},{"kind":"bind","source":"/fixture/bind","target":"/fixture/bound","rw":true}],"networks":[network],"ports":[{"container":"8080/tcp","host":"18080"}]});
        let mut rows = complete_responses();
        rows[3] = response(&inspect.to_string());
        let (path, config, worker) = fixture(rows).await;
        let result = observe(&config, &TopologyLimits::default(), Instant::now() + std::time::Duration::from_secs(2)).await;
        worker.abort();
        let _ = worker.await;
        cleanup(path);
        let actual = result.unwrap().packet;
        let expected = DockerPacket {
            engines: vec![DockerEngine {
                id: crate::db::sha256_hex(b"fixture-engine"),
                containers: vec![parse_container(&projected).unwrap()],
                networks: vec![Network { id: crate::db::sha256_hex(network.as_bytes()), driver: Some("bridge".into()), internal: Some(false) }],
                volumes: vec![Volume { id: crate::db::sha256_hex(b"fixture-volume"), driver: Some("local".into()) }],
            }],
        };
        assert_eq!(serde_json::to_value(&actual).unwrap(), serde_json::to_value(&expected).unwrap());
        let now = crate::db::now_ms();
        assert_eq!(actual.snapshot("fixture:native:docker".into(), now).unwrap(), expected.snapshot("fixture:native:docker".into(), now).unwrap());
        assert!(!serde_json::to_string(&actual).unwrap().contains("secret-canary"));
    }
}
