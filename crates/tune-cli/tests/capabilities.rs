use std::process::Command;
#[test]
fn readonly_commands_reject_flags_and_missing_config_without_creating_state() {
    let root = std::env::temp_dir().join(format!("kt-cap-cli-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&root).unwrap();
    let cfg = root.join("missing.json");
    let data = root.join("data");
    for cmd in ["capabilities", "maintenance-plan"] {
        for args in [vec![cmd], vec![cmd, "--all"], vec![cmd, "--probe"]] {
            let out = Command::new(env!("CARGO_BIN_EXE_tune")).args(args).env("KATALA_TUNE_CONFIG", &cfg).env("KATALA_TUNE_DATA_DIR", &data).output().unwrap();
            assert!(!out.status.success());
            assert!(!cfg.exists());
            assert!(!data.exists());
            assert!(!String::from_utf8_lossy(&out.stderr).contains("使い方:"), "new commands must be recognized");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn valid_config_rejects_unknown_targets_and_flags_without_writes_or_transport() {
    let root = std::env::temp_dir().join(format!("kt-cap-valid-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&root).unwrap();
    let cfg = root.join("nodes.json");
    let data = root.join("data");
    let text = r#"{"nodes":[{"id":"sample","os":"windows","alias":"unreachable.invalid"}]}"#;
    std::fs::write(&cfg, text).unwrap();
    for cmd in ["capabilities", "maintenance-plan"] {
        for id in ["absent", "--all", "--probe", "-x"] {
            let out =
                Command::new(env!("CARGO_BIN_EXE_tune")).args([cmd, id]).env("KATALA_TUNE_CONFIG", &cfg).env("KATALA_TUNE_DATA_DIR", &data).output().unwrap();
            assert!(!out.status.success());
            assert!(out.stdout.is_empty());
            assert!(!data.exists());
            assert_eq!(std::fs::read_to_string(&cfg).unwrap(), text);
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
