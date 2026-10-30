use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Command, Output},
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("kt-discovery-{name}-{}-{}", std::process::id(), tune_core::db::now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tune"))
            .args(args)
            .env("KATALA_TUNE_CONFIG", self.0.join("nodes.json"))
            .env("KATALA_TUNE_DATA_DIR", self.0.join("data"))
            .env("KATALA_TUNE_LINK_DIR", self.0.join("link"))
            .output()
            .unwrap()
    }
    fn untouched(&self) {
        assert!(!self.0.join("data").exists(), "must not initialize DB");
        assert!(!self.0.join("link").exists(), "must not initialize link state");
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Dispatching discovery through Engine would fail with deliberately invalid config.
#[test]
fn help_and_version_do_not_read_config_or_initialize_state() {
    let s = Sandbox::new("help");
    std::fs::write(s.0.join("nodes.json"), "invalid config").unwrap();
    for args in [vec!["--help"], vec!["-h"], vec!["specs", "--help"], vec!["specs-local", "-h"], vec!["help", "specs"], vec!["--version"], vec!["-V"]] {
        let out = s.run(&args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
        assert!(out.stderr.is_empty());
    }
    s.untouched();
}

#[test]
fn fleet_discovery_reports_versions_specs_and_side_effects() {
    let s = Sandbox::new("json");
    for args in [["version", "--json"], ["cli-info", "--json"]] {
        let out = s.run(&args);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["cli_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["specs_schema"], "katala_specs.v1");
        assert_eq!(value["platform"], std::env::consts::OS);
        assert_eq!(value["architecture"], std::env::consts::ARCH);
        if args[0] == "version" {
            assert_eq!(value["schema"], "katala_cli_version.v1");
            assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
            assert_eq!(value["executable_sha256"].as_str().unwrap().len(), 64);
        }
        if args[0] == "cli-info" {
            let commands = value["commands"].as_array().unwrap();
            let specs = commands.iter().find(|c| c["name"] == "specs").unwrap();
            assert_eq!(specs["effects"]["writes_db"], true);
            assert!(specs["arguments"].to_string().contains("--saved"));
            let local = commands.iter().find(|c| c["name"] == "specs-local").unwrap();
            assert_eq!(local["effects"]["reads_config"], false);
        }
    }
    assert!(!s.0.join("nodes.json").exists());
    s.untouched();
}

#[test]
fn specs_rejects_unknown_options_and_ids_before_initializing_db() {
    let s = Sandbox::new("invalid");
    let config = r#"{"nodes":[{"id":"sample","os":"windows","alias":"unreachable.invalid"}]}"#;
    std::fs::write(s.0.join("nodes.json"), config).unwrap();
    for args in [
        vec!["specs", "--unknown"],
        vec!["specs", "absent"],
        vec!["specs", "--saved", "absent"],
        vec!["specs-local", "--unknown"],
        vec!["version", "--unknown"],
        vec!["versions", "absent"],
        vec!["versions", "--unknown"],
    ] {
        let out = s.run(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty());
        s.untouched();
    }
    assert_eq!(std::fs::read_to_string(s.0.join("nodes.json")).unwrap(), config);
}

#[test]
fn saved_specs_reads_existing_state_without_creating_a_database() {
    let s = Sandbox::new("saved");
    std::fs::write(s.0.join("nodes.json"), r#"{"nodes":[{"id":"sample","os":"windows","status":"retired"}]}"#).unwrap();
    let out = s.run(&["specs", "--saved", "sample"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["nodes"][0]["node_id"], "sample");
    assert!(value["nodes"][0]["report"].is_null());
    assert_eq!(value["nodes"][0]["eligible"], false);
    assert_eq!(value["read_only"], true);
    s.untouched();
}

#[test]
fn saved_specs_preserves_last_success_and_database_bytes() {
    use serde_json::json;
    let s = Sandbox::new("existing");
    std::fs::write(s.0.join("nodes.json"), r#"{"nodes":[{"id":"sample","os":"windows","status":"retired"}]}"#).unwrap();
    let data = s.0.join("data");
    let node = tune_core::nodes::Node::from_value(&json!({"id":"sample","os":"windows","status":"retired"}), "test");
    {
        let db = tune_core::db::Store::open(&data).unwrap();
        let report = tune_core::specs::normalize("windows", &json!({"cpu":[{"Name":"Example CPU"}]}));
        tune_core::engine_specs::save_attempt(&db, &node, &json!({"ok":true,"report":report})).unwrap();
        tune_core::engine_specs::save_attempt(&db, &node, &json!({"ok":false,"error":"timeout"})).unwrap();
    }
    let path = data.join(tune_core::db::DB_FILE);
    let before = std::fs::read(&path).unwrap();
    let out = s.run(&["specs", "--saved"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = &value["nodes"][0];
    assert_eq!(row["report"]["sections"]["cpu"]["components"][0]["fields"]["model"], "Example CPU");
    assert_eq!(row["last_attempt"]["error"], "timeout");
    assert_eq!(row["stale"], true);
    assert_eq!(row["eligible"], false);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
#[cfg(any(windows, target_os = "macos"))]
fn versions_audits_local_cli_without_initializing_database() {
    let s = Sandbox::new("audit");
    let config = serde_json::json!({"nodes":[{"id":"sample","os":std::env::consts::OS,"local_hostname":tune_core::nodes::local_host()}]});
    std::fs::write(s.0.join("nodes.json"), config.to_string()).unwrap();
    let out = s.run(&["versions", "sample"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["nodes"][0]["ok"], true);
    assert_eq!(value["nodes"][0]["matches_expected"], true);
    assert_eq!(value["nodes"][0]["expected_version"], env!("CARGO_PKG_VERSION"));
    s.untouched();
}

#[test]
fn versions_returns_json_and_failing_exit_for_a_local_platform_mismatch() {
    let s = Sandbox::new("audit-failed");
    let other_os = if cfg!(windows) { "macos" } else { "windows" };
    let config = serde_json::json!({"nodes":[{"id":"sample","os":other_os,"local_hostname":tune_core::nodes::local_host()}]});
    std::fs::write(s.0.join("nodes.json"), config.to_string()).unwrap();
    let out = s.run(&["versions", "sample"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["counts"]["failed"], 1);
    assert!(!out.status.success(), "failed fleet audit must signal failure to CI");
    s.untouched();
}

#[test]
#[cfg(any(windows, target_os = "macos"))]
fn versions_default_includes_explicitly_opted_in_shared_local_node() {
    let s = Sandbox::new("shared");
    let config =
        serde_json::json!({"nodes":[{"id":"sample","os":std::env::consts::OS,"local_hostname":tune_core::nodes::local_host(),"shared":true,"specs":true}]});
    std::fs::write(s.0.join("nodes.json"), config.to_string()).unwrap();
    let out = s.run(&["versions"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["nodes"][0]["node_id"], "sample");
    assert_eq!(value["nodes"][0]["matches_expected"], true);
    s.untouched();
}
