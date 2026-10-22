use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tune"))
        .args(args)
        .env("KATALA_TUNE_CONFIG", root.join("nodes.json"))
        .env("KATALA_TUNE_DATA_DIR", root.join("data"))
        .env("KATALA_TUNE_LINK_DIR", root.join("link"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}
fn root(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kt-discovery-{tag}-{}-{}", std::process::id(), tune_core::db::now_ms()))
}

#[test]
fn help_and_catalog_work_without_initializing_state() {
    let root = root("missing");
    for args in [vec!["--help"], vec!["help"], vec!["help", "ledger"], vec!["cli-info", "--json"]] {
        let out = run(&root, &args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!out.stdout.is_empty());
        assert!(out.stderr.is_empty());
        assert!(!root.exists(), "discovery must not create config, DB or link state");
    }
}

#[test]
fn catalog_describes_all_public_commands_and_their_effects() {
    let root = root("catalog");
    let out = run(&root, &["cli-info", "--json"]);
    assert!(out.status.success());
    let info: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(info["schema_version"], 1);
    assert_eq!(info["cli_version"], env!("CARGO_PKG_VERSION"));
    let commands = info["commands"].as_array().unwrap();
    // Reviewed against the public main dispatch and agent::run, excluding native protocols.
    let expected = "help cli-info paths probe logs ledger last power capabilities maintenance-plan network status analyze inventory ai ai-summary ai-provenance ai-trace ai-verify ai-limits netsec hygiene export hub-pull monitor schedule metrics dashboard update monitor-run live agent-pair agent-peers agent-probe agent-unpair topology-refresh topology-watch topology-export";
    let names: BTreeSet<_> = commands.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, expected.split_whitespace().collect());
    assert_eq!(commands.len(), names.len());
    for c in commands {
        let name = c["name"].as_str().unwrap();
        assert!(c["arguments"].is_array());
        assert!(!c["summary"].as_str().unwrap().is_empty());
        let out = run(&root, &["help", name]);
        assert!(out.status.success(), "{name}");
        let help = String::from_utf8(out.stdout).unwrap();
        assert!(help.contains(name));
        assert!(help.contains(c["summary"].as_str().unwrap()));
    }
    let entry = |name: &str| commands.iter().find(|c| c["name"] == name).unwrap();
    for name in ["help", "cli-info", "ledger", "analyze", "probe", "live"] {
        assert_eq!(entry(name)["effects"]["writes_db"], false, "{name}");
    }
    for name in ["last", "status", "export", "power", "topology-export", "monitor", "schedule", "update"] {
        assert_eq!(entry(name)["effects"]["writes_db"], true, "initialization is a write: {name}");
    }
    assert_eq!(entry("monitor-run")["effects"]["may_deploy_helper"], true);
    assert_eq!(entry("agent-pair")["effects"]["writes_link_state"], true);
    assert_eq!(entry("probe")["effects"]["connects_to_nodes"], true);
    assert_eq!(entry("logs")["output"]["default"], "none");
    assert_eq!(entry("topology-watch")["output"]["default"], "json-lines");
    assert!(!root.exists());
}

#[test]
fn malformed_discovery_requests_fail_without_state() {
    let root = root("invalid");
    for args in [
        vec!["help", "unknown"],
        vec!["help", "ledger", "extra"],
        vec!["--help", "extra"],
        vec!["cli-info"],
        vec!["cli-info", "--bad"],
        vec!["cli-info", "--json", "extra"],
    ] {
        let out = run(&root, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
    assert!(!root.exists());
}

#[test]
fn discovery_does_not_read_or_migrate_existing_invalid_state() {
    let root = root("invalid-state");
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::create_dir_all(root.join("link")).unwrap();
    let files = [root.join("nodes.json"), root.join("data").join(tune_core::db::DB_FILE), root.join("link").join("peers.json")];
    for file in &files {
        std::fs::write(file, b"invalid fixture - leave untouched").unwrap();
    }
    for args in [vec!["--help"], vec!["help", "status"], vec!["cli-info", "--json"]] {
        assert!(run(&root, &args).status.success());
    }
    for file in &files {
        assert_eq!(std::fs::read(file).unwrap(), b"invalid fixture - leave untouched");
    }
    assert_eq!(std::fs::read_dir(root.join("data")).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(root.join("link")).unwrap().count(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn existing_payloads_and_errors_remain_unwrapped() {
    let root = root("compat");
    std::fs::create_dir_all(&root).unwrap();
    let config = br#"{"nodes":[]}"#;
    std::fs::write(root.join("nodes.json"), config).unwrap();
    let out = run(&root, &["ledger", "--json"]);
    assert!(out.status.success());
    let ledger: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(ledger, json!({"rows":[],"counts":{"target":0,"unobserved":0,"excluded":0,"total":0}}));
    let snapshot = root.join("snapshot.json");
    std::fs::write(&snapshot, "{}").unwrap();
    let out = run(&root, &["analyze", snapshot.to_str().unwrap()]);
    assert!(out.status.success());
    let analyzed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(analyzed["findings"].is_array());
    assert!(analyzed.get("score").is_some());
    assert_eq!(analyzed.as_object().unwrap().len(), 2);
    let out = run(&root, &["probe", "unknown-fixture"]);
    assert!(out.status.success());
    assert_eq!(out.stdout, b"[]\n");
    let out = run(&root, &["unknown-command"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    for args in [vec!["--help"], vec!["cli-info", "--json"]] {
        assert!(run(&root, &args).status.success());
    }
    assert_eq!(std::fs::read(root.join("nodes.json")).unwrap(), config);
    assert!(!root.join("data").exists());
    assert!(!root.join("link").exists());
    std::fs::remove_dir_all(root).unwrap();
}
