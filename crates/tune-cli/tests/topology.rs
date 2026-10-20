use std::process::Command;

#[test]
fn topology_refresh_and_finite_watch_emit_receipts() {
    let base = std::env::temp_dir().join(format!("katala-topology-cli-{}-{}", std::process::id(), tune_core::db::now_ms()));
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("package.json"), r#"{"name":"fixture-package","dependencies":{"fixture-dependency":"1"}}"#).unwrap();
    let host = tune_core::nodes::local_host();
    let selected = base.join("selected.json");
    std::fs::write(&selected, r#"{"flag":true}"#).unwrap();
    let cfg = format!(
        r#"{{"topology":{{"enabled":true,"owners":[]}},"schedule":{{"topology_minutes":73}},"nodes":[{{"id":"fixture-node","alias":"fixture","os":"macos","local_hostname":"{host}","topology":{{"environments":[{{"id":"native","transport":"node","roots":[{}],"configs":[{{"path":{},"format":"json","keys":[{{"pointer":"/flag","mode":"hash"}}]}}]}}]}}}}]}}"#,
        serde_json::to_string(&root.to_string_lossy()).unwrap(),
        serde_json::to_string(&selected.to_string_lossy()).unwrap()
    );
    let config_path = base.join("nodes.json");
    std::fs::write(&config_path, cfg).unwrap();
    let parsed = tune_core::nodes::load_config(&config_path).unwrap();
    assert_eq!(parsed.schedule.unwrap()["topology_minutes"], 73, "the fixture must not fall back after parsing a Windows path");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tune"))
            .args(args)
            .env("KATALA_TUNE_CONFIG", base.join("nodes.json"))
            .env("KATALA_TUNE_DATA_DIR", base.join("data"))
            .output()
            .unwrap()
    };
    let one = run(&["topology-refresh", "fixture-node"]);
    assert!(one.status.success(), "{}", String::from_utf8_lossy(&one.stderr));
    let receipt: serde_json::Value = serde_json::from_slice(&one.stdout).unwrap();
    assert_eq!(receipt["status"], "complete");
    assert!(receipt["scopes"].as_array().is_some_and(|scopes| !scopes.is_empty() && scopes.iter().all(|scope| scope["scope_id"].as_str().is_some_and(|id| id.starts_with("fixture-node:")))));
    assert!(!receipt["scopes"].as_array().unwrap().iter().any(|scope| scope["scope_id"] == "github"));
    let exported = run(&["topology-export", "--format", "json"]);
    assert!(exported.status.success(), "{}", String::from_utf8_lossy(&exported.stderr));
    let graph: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert!(graph["entities"].as_array().is_some_and(|entities| !entities.is_empty()));
    assert_eq!(graph["schema"], "katala_topology.public.v1");
    assert!(graph["entities"].as_array().unwrap().iter().all(|entity| entity["label"].as_str().is_some_and(|label| label.contains('-'))));
    assert!(!String::from_utf8_lossy(&exported.stdout).contains(root.to_string_lossy().as_ref()));
    let watch = run(&["topology-watch", "--interval-seconds", "1", "--iterations", "2", "fixture-node"]);
    assert!(watch.status.success(), "{}", String::from_utf8_lossy(&watch.stderr));
    assert_eq!(String::from_utf8_lossy(&watch.stdout).lines().count(), 2);
    let unknown = run(&["topology-refresh", "missing-node"]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown topology node id"));
    let _ = std::fs::remove_dir_all(base);
}
