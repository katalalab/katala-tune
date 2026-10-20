use serde_json::json;
use tune_core::topology::adapters::docker::{local_endpoint, parse_container};
#[test]
fn endpoint_rejects_remote_and_injection() {
    assert!(local_endpoint("unix:///var/run/docker.sock"));
    assert!(local_endpoint("npipe:////./pipe/docker_engine"));
    for value in ["tcp://127.0.0.1:2375", "ssh://example", "npipe:////remote/pipe/x", "unix://remote/x", "unix:///x\n"] {
        assert!(!local_endpoint(value), "{value:?}");
    }
}
#[test]
fn projection_preserves_null_zero_and_only_current_health() {
    let row = json!({"id":"a".repeat(64),"state":"exited","health":"unhealthy","memory":0,"nano_cpus":null,"restart":"no","log_driver":"local","max_size":null,"max_file":null,"mounts":[],"networks":[],"ports":[]});
    let dto = parse_container(&row).unwrap();
    assert_eq!(dto.memory_bytes, Some(0));
    assert_eq!(dto.nano_cpus, None);
    assert!(!dto.current_unhealthy);
    assert!(!dto.log_unbounded_candidate);
    let mut row = row;
    row["health"] = json!(null);
    row["state"] = json!("running");
    row["log_driver"] = json!("json-file");
    let dto = parse_container(&row).unwrap();
    assert_eq!(dto.health, None);
    assert!(dto.log_unbounded_candidate);
    let serialized = serde_json::to_string(&dto).unwrap();
    assert!(!serialized.contains(&"a".repeat(64)));
}

use std::{collections::VecDeque, sync::Mutex, time::Duration};
use tune_core::{
    collect::BoundedResult,
    topology::{
        ScopeStatus, TopologyLimits,
        adapters::{
            CommandFuture, CommandRunner,
            docker::{DockerRequest, collect_docker_with},
        },
    },
};
struct Fake {
    rows: Mutex<VecDeque<BoundedResult>>,
    calls: Mutex<Vec<Vec<String>>>,
}
impl Fake {
    fn new(rows: Vec<Vec<u8>>) -> Self {
        Self {
            rows: Mutex::new(rows.into_iter().map(|stdout| BoundedResult { stdout, code: Some(0), ..Default::default() }).collect()),
            calls: Mutex::new(vec![]),
        }
    }
}
impl CommandRunner for Fake {
    fn run<'a>(&'a self, _: &'a str, args: &'a [String], _: &'a [u8], timeout: Duration, stdout: usize, stderr: usize) -> CommandFuture<'a> {
        Box::pin(async move {
            assert!(timeout <= Duration::from_secs(5));
            assert!(stdout <= 1024 * 1024);
            assert!(stderr <= 8192);
            self.calls.lock().unwrap().push(args.to_vec());
            self.rows.lock().unwrap().pop_front().expect("unexpected Docker call")
        })
    }
}
fn request(contexts: &[&str]) -> DockerRequest {
    DockerRequest {
        scope_id: "test:docker".into(),
        contexts: contexts.iter().map(|s| (*s).into()).collect(),
        program: None,
        observed_at_ms: 123,
        limits: TopologyLimits::default(),
    }
}
fn bytes(v: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&v).unwrap()
}
#[tokio::test]
async fn remote_context_never_contacts_daemon_and_aliases_dedup() {
    let f = Fake::new(vec![
        bytes(json!("ssh://example")),
        bytes(json!("unix:///var/run/docker.sock")),
        bytes(json!("engine-a")),
        vec![],
        vec![],
        vec![],
        bytes(json!("unix:///var/run/docker.sock")),
        bytes(json!("engine-a")),
    ]);
    let got = collect_docker_with(&f, &request(&["remote", "one", "two"]), "fixture").await;
    assert_eq!(got.status, ScopeStatus::Partial);
    assert_eq!(got.entities.len(), 1);
    let calls = f.calls.lock().unwrap();
    assert_eq!(calls.len(), 8);
    assert!(calls.iter().skip(2).filter(|a| a[0] != "context").all(|a| a[0] == "--host" && a[1] == "unix:///var/run/docker.sock"));
    assert!(calls.iter().all(|a| !a.iter().any(|s| ["start", "stop", "create", "exec", "prune", "rm", "kill"].contains(&s.as_str()))));
}
#[tokio::test]
async fn timeout_and_invalid_selection_have_explicit_receipts() {
    let f = Fake::new(vec![]);
    assert_eq!(collect_docker_with(&f, &request(&["-bad"]), "fixture").await.status, ScopeStatus::Failed);
    assert!(f.calls.lock().unwrap().is_empty());
    f.rows.lock().unwrap().push_back(BoundedResult { timed_out: true, ..Default::default() });
    let got = collect_docker_with(&f, &request(&["one"]), "fixture").await;
    assert_eq!(got.failures[0].code, tune_core::topology::FailureCode::Timeout);
    assert_eq!(got.status, ScopeStatus::Failed);
}
#[test]
fn imported_policy_cannot_lie_or_carry_arbitrary_strings() {
    let row = json!({"id":"c","state":"running","health":null,"log_driver":"local","max_size":null,"mounts":[],"networks":[],"ports":[]});
    let mut c = parse_container(&row).unwrap();
    c.log_unbounded_candidate = true;
    let packet = tune_core::topology::adapters::docker::DockerPacket {
        engines: vec![tune_core::topology::adapters::docker::DockerEngine {
            id: "a".repeat(64),
            containers: vec![c.clone()],
            networks: vec![],
            volumes: vec![],
        }],
    };
    assert!(packet.snapshot("test".into(), 1).is_err());
    let mut packet = packet;
    c.log_unbounded_candidate = false;
    c.log_driver = Some("private-token".into());
    packet.engines[0].containers = vec![c];
    assert!(packet.snapshot("test".into(), 1).is_err());
}
#[test]
fn docker_graph_survives_store_and_public_export_without_private_attributes() {
    use tune_core::{
        db::Store,
        topology::{
            adapters::docker::{DockerEngine, DockerPacket, Network},
            export::{ExportView, public_json},
        },
    };
    let c=parse_container(&json!({"id":"container-private","state":"running","health":null,"memory":0,"nano_cpus":null,"log_driver":"local","mounts":[],"networks":["network-private"],"ports":[{"container":"80/tcp","host":"8080"}]})).unwrap();
    let n = c.networks[0].clone();
    let engine = DockerEngine {
        id: "e".repeat(64),
        containers: vec![c],
        networks: vec![Network { id: n, driver: Some("bridge".into()), internal: Some(false) }],
        volumes: vec![],
    };
    let snapshot = DockerPacket { engines: vec![engine.clone(), engine] }.snapshot("fixture:docker".into(), 123).unwrap();
    assert_eq!(snapshot.entities.len(), 3);
    assert_eq!(snapshot.edges.len(), 3);
    let db = Store::open_in_memory().unwrap();
    db.record_topology("fixture", &snapshot, 123).unwrap();
    let view = db.topology_view().unwrap();
    let container = view["entities"].as_array().unwrap().iter().find(|e| e["kind"] == "docker_container").unwrap();
    assert_eq!(container["attributes"]["memory_bytes"], 0);
    assert!(container["attributes"]["nano_cpus"].is_null());
    let exported = public_json(&view, ExportView::All);
    assert!(exported.contains("docker_container"));
    assert!(exported.contains("docker_network_member"));
    assert!(!exported.contains("container-private"));
    assert!(!exported.contains("memory_bytes"));
}
#[test]
fn trusted_distribution_paths_allow_homebrew_but_not_escape() {
    use tune_core::topology::adapters::docker::trusted_path;
    #[cfg(not(windows))]
    {
        assert!(trusted_path("/opt/homebrew/Cellar/docker/29.9.1/bin/docker"));
        for path in ["/tmp/docker", "/opt/homebrew/Cellar/docker/../../bin/docker", "/opt/homebrew/Cellar/docker/1/bin/other"] {
            assert!(!trusted_path(path));
        }
    }
}
#[test]
fn successful_engine_is_saved_when_another_context_is_unknown() {
    use tune_core::topology::{
        Failure, FailureCode,
        adapters::docker::{DockerEngine, DockerPacket, engine_snapshots},
    };
    let mut s = DockerPacket { engines: vec![DockerEngine { id: "e".repeat(64), containers: vec![], networks: vec![], volumes: vec![] }] }
        .snapshot("fixture:native:docker".into(), 1)
        .unwrap();
    s.status = ScopeStatus::Partial;
    s.failures.push(Failure::new(FailureCode::Unsupported, "excluded"));
    let result = engine_snapshots(s);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].status, ScopeStatus::Partial);
    assert!(result[0].entities.is_empty());
    assert_eq!(result[1].status, ScopeStatus::Complete);
    assert!(result[1].entities.iter().any(|e| e.kind == tune_core::topology::EntityKind::ExecutionEnvironment));
}
#[tokio::test]
async fn projected_collection_drops_private_fields_and_parses_network_boolean_strings() {
    let mut row = json!({"id":"a".repeat(64),"state":"running","health":null,"memory":0,"nano_cpus":null,"log_driver":"json-file","max_size":null,"mounts":[{"kind":"bind","source":"/private/fixture","target":"/app","rw":true,"name":null}],"networks":["net"],"ports":[{"container":"80/tcp","host":"8080"}],"Env":["SECRET=private"],"Labels":{"secret":"private"},"Cmd":["private"]});
    let f = Fake::new(vec![
        bytes(json!("unix:///var/run/docker.sock")),
        bytes(json!("engine")),
        format!("{}\n", "a".repeat(64)).into_bytes(),
        bytes(row.take()),
        bytes(json!({"id":"net","driver":"bridge","internal":"false"})),
        bytes(json!({"id":"volume","driver":"local"})),
    ]);
    let got = collect_docker_with(&f, &request(&["one"]), "fixture").await;
    assert_eq!(got.status, ScopeStatus::Complete);
    let text = serde_json::to_string(&got).unwrap();
    assert!(!text.contains("private"));
    assert!(!text.contains("SECRET"));
    let n = got.entities.iter().find(|e| e.kind == tune_core::topology::EntityKind::DockerNetwork).unwrap();
    assert_eq!(n.attributes["internal"], false);
    let args = f.calls.lock().unwrap();
    let template = &args[3][5];
    for field in ["{{json .Config}}", ".Env", ".Cmd", ".Labels", ".State.Health.Log", "HostIp", ".IPAddress"] {
        assert!(!template.contains(field), "{field}");
    }
    assert!(template.contains(".State.Health.Status"));
}
#[test]
fn malformed_import_and_nested_budget_are_rejected() {
    use tune_core::topology::adapters::docker::{DockerEngine, DockerPacket};
    assert!(serde_json::from_value::<DockerPacket>(json!({"engines":[],"Env":["forbidden"]})).is_err());
    let mut row = json!({"id":"c","state":"exited","health":"unhealthy","log_driver":"json-file","max_size":null,"mounts":[],"networks":[],"ports":[]});
    assert!(!parse_container(&row).unwrap().log_unbounded_candidate);
    row["ports"] = json!((0..129).map(|_| json!({"container":"80/tcp","host":"8080"})).collect::<Vec<_>>());
    assert!(parse_container(&row).is_err());
    row["ports"] = json!([]);
    let c = parse_container(&row).unwrap();
    let packet = DockerPacket { engines: vec![DockerEngine { id: "e".repeat(64), containers: vec![c.clone(), c], networks: vec![], volumes: vec![] }] };
    assert!(packet.snapshot("test".into(), 1).is_err());
}
#[tokio::test]
async fn cumulative_deadline_cannot_be_reset_by_more_contexts() {
    struct Slow(std::sync::atomic::AtomicUsize);
    impl CommandRunner for Slow {
        fn run<'a>(&'a self, _: &'a str, _: &'a [String], _: &'a [u8], timeout: Duration, _: usize, _: usize) -> CommandFuture<'a> {
            Box::pin(async move {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if tokio::time::timeout(timeout, tokio::time::sleep(Duration::from_millis(400))).await.is_err() {
                    BoundedResult { timed_out: true, ..Default::default() }
                } else {
                    BoundedResult { code: Some(0), stdout: bytes(json!("unix:///var/run/docker.sock")), ..Default::default() }
                }
            })
        }
    }
    let mut req = request(&["one", "two", "three", "four"]);
    req.limits.local_timeout_seconds = 1;
    let runner = Slow(std::sync::atomic::AtomicUsize::new(0));
    let begin = std::time::Instant::now();
    let got = collect_docker_with(&runner, &req, "fixture").await;
    assert!(begin.elapsed() < Duration::from_millis(1400));
    assert!(runner.0.load(std::sync::atomic::Ordering::Relaxed) <= 3);
    assert!(got.failures.iter().any(|f| f.code == tune_core::topology::FailureCode::Timeout));
}
#[tokio::test]
async fn stopped_daemon_is_unreachable_and_stderr_is_never_retained() {
    let f = Fake::new(vec![bytes(json!("unix:///var/run/docker.sock"))]);
    f.rows.lock().unwrap().push_back(BoundedResult {
        code: Some(1),
        stderr: b"Cannot connect to the Docker daemon at unix:///private/socket. private".to_vec(),
        ..Default::default()
    });
    let got = collect_docker_with(&f, &request(&["one"]), "fixture").await;
    assert_eq!(got.failures[0].code, tune_core::topology::FailureCode::Unreachable);
    assert!(!serde_json::to_string(&got).unwrap().contains("private"));
}
#[tokio::test]
async fn engine_refresh_opt_in_and_failed_collection_keep_old_engine_unobserved() {
    use std::sync::Arc;
    use tune_core::{
        engine::{Engine, NoHost},
        topology::adapters::docker::{DockerEngine, DockerPacket, engine_snapshots},
    };
    let base = std::env::temp_dir().join(format!("docker-engine-test-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("nodes.json");
    let mut config = json!({"topology":{"enabled":true},"schedule":{"enabled":false},"nodes":[{"id":"fixture","alias":"fixture","os":"macos","local_hostname":tune_core::nodes::local_host(),"topology":{"environments":[{"id":"native","transport":"node","roots":[],"docker":{"enabled":false,"contexts":["one"],"program":"/untrusted/docker"}}]}}]});
    std::fs::write(&path, config.to_string()).unwrap();
    let engine = Engine::open(path.clone(), base.join("data"), Arc::new(NoHost)).unwrap();
    engine.topology_refresh("test").await;
    assert!(!engine.topology_view().unwrap()["scopes"].as_array().unwrap().iter().any(|s| s["scope_id"].as_str().unwrap().contains(":docker")));
    let packet = DockerPacket { engines: vec![DockerEngine { id: "e".repeat(64), containers: vec![], networks: vec![], volumes: vec![] }] };
    let snapshots = engine_snapshots(packet.snapshot("fixture:native:docker".into(), 1).unwrap());
    let scope = snapshots[1].scope_id.clone();
    engine.with_db(|db| db.record_topology("seed", &snapshots[1], 1)).unwrap();
    config["nodes"][0]["topology"]["environments"][0]["docker"]["enabled"] = json!(true);
    std::fs::write(&path, config.to_string()).unwrap();
    let receipt = engine.topology_refresh("test").await;
    assert!(receipt["scopes"].as_array().unwrap().iter().any(|s| s["scope_id"] == "fixture:native:docker" && s["status"] == "failed"));
    let view = engine.topology_view().unwrap();
    assert!(view["entities"].as_array().unwrap().iter().any(|e| e["kind"] == "docker_engine"));
    assert!(view["scopes"].as_array().unwrap().iter().any(|s| s["scope_id"] == scope && s["status"] == "partial"));
    drop(engine);
    std::fs::remove_dir_all(base).unwrap();
}
#[test]
fn explicit_unlimited_log_size_is_preserved_through_packet_validation() {
    use tune_core::topology::adapters::docker::{DockerEngine, DockerPacket};
    let row = json!({"id":"c","state":"running","health":null,"log_driver":"json-file","max_size":"-1","mounts":[],"networks":[],"ports":[]});
    let c = parse_container(&row).expect("Docker accepts explicit -1 log size");
    assert_eq!(c.log_max_size.as_deref(), Some("-1"));
    assert!(!c.log_unbounded_candidate, "only missing max-size is a candidate");
    let packet = DockerPacket { engines: vec![DockerEngine { id: "e".repeat(64), containers: vec![c], networks: vec![], volumes: vec![] }] };
    assert!(packet.snapshot("test".into(), 1).is_ok());
}
#[tokio::test]
async fn missing_socket_wording_from_new_cli_is_unreachable() {
    let f = Fake::new(vec![bytes(json!("unix:///var/run/docker.sock"))]);
    f.rows.lock().unwrap().push_back(BoundedResult {
        code: Some(1),
        stderr: b"failed to connect to the docker API at unix:///fixture/docker.sock: connect: no such file or directory".to_vec(),
        ..Default::default()
    });
    let got = collect_docker_with(&f, &request(&["one"]), "fixture").await;
    assert_eq!(got.failures[0].code, tune_core::topology::FailureCode::Unreachable);
}
