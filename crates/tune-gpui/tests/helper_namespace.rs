//! Exercise the production startup boundary in fresh processes without opening
//! an Engine, launching a sampler, or touching the desktop.
use std::{path::PathBuf, process::Command};

#[allow(dead_code)]
#[path = "../src/startup.rs"]
mod startup;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn startup_helper_namespace_is_isolated_from_ambient_and_legacy_paths() {
    let root = std::env::temp_dir().join(format!("kt-helper-namespace-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let _fixture = Fixture(root.clone());
    let app = root.join("owned-app");
    let home = root.join("home");
    let stale = root.join("stale-data");
    let chosen = root.join("chosen-data");
    std::fs::write(root.join("blocked-data"), b"inert file, not a data directory").unwrap();
    for dir in [&app, &home, &stale.join("bin"), &chosen.join("bin"), &home.join(".katala-tune/bin")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // These are inert bytes: launch() only builds a command, never executes it.
    let agent = tune_core::helpers::exe("tune-agent");
    for dir in [&app, &stale.join("bin"), &chosen.join("bin"), &home.join(".katala-tune/bin")] {
        std::fs::write(dir.join(&agent), b"inert public sampler fixture").unwrap();
    }
    let executable = app.join(tune_core::helpers::exe("namespace-test"));
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&chosen, root.join("data-alias")).unwrap();
    let cases = [
        "ordinary",
        "resident",
        "hidden",
        "diagnostic",
        "parse-only",
        "default-env",
        // Windows Known Folders ignore HOME/APPDATA overrides. Never consult
        // the real profile just to test the no-env legacy/default branch.
        #[cfg(not(windows))]
        "default-home",
        "relative",
        "missing",
        "immutable",
        "invalid",
        "legacy-env",
        #[cfg(not(windows))]
        "legacy-home",
    ];
    #[cfg(unix)]
    let cases = cases.into_iter().chain(["symlink"]).collect::<Vec<_>>();
    for case in cases {
        let mut command = Command::new(&executable);
        command
            .args(["--exact", "namespace_child", "--ignored", "--nocapture", "--test-threads=1"])
            .current_dir(&root)
            .env("KT_NAMESPACE_FIXTURE", &root)
            .env("KT_NAMESPACE_CASE", case)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("KATALA_TUNE_CONFIG", root.join("unused-ledger.json"))
            .env("KATALA_TUNE_DATA_DIR", &stale);
        if matches!(case, "default-home" | "legacy-home") {
            command.env_remove("KATALA_TUNE_DATA_DIR");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "case={case}\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"), "exact child must run");
        eprintln!("helper namespace case={case}: passed");
    }
    assert_eq!(std::fs::read_dir(&chosen).unwrap().count(), 1, "startup must leave only the preinstalled bin directory");
    assert!(!root.join("missing-data").exists());
    assert!(!stale.join("gpui-preview").exists());
}

#[test]
#[ignore = "only the owning parent may start this isolated fixture"]
fn namespace_child() {
    let root = PathBuf::from(std::env::var_os("KT_NAMESPACE_FIXTURE").unwrap());
    let case = std::env::var("KT_NAMESPACE_CASE").unwrap();
    let stale = root.join("stale-data");
    let inherited = std::env::var_os("KATALA_TUNE_DATA_DIR");
    if case == "parse-only" {
        let blocked = root.join("blocked-data/child");
        let options = startup::parse(["--config".into(), "unresolved-ledger.json".into(), "--data".into(), blocked.clone().into_os_string()]).unwrap().unwrap();
        assert_eq!(options.config, PathBuf::from("unresolved-ledger.json"), "pure validation must not resolve paths before the platform guard");
        assert_eq!(options.data, blocked);
        assert!(tune_core::helpers::bound_bin_dir().is_none(), "pure validation must not bind helpers");
        assert_eq!(std::fs::read(root.join("blocked-data")).unwrap(), b"inert file, not a data directory");
        return;
    }
    if case == "invalid" {
        assert!(tune_core::helpers::bind_process_data_dir(std::path::Path::new("relative-data")).is_err());
        // PathBuf::join removes .. for verbatim Windows paths; append OS text
        // so this direct binding fixture actually supplies unresolved input.
        let mut unresolved = root.as_os_str().to_os_string();
        let separator = std::path::MAIN_SEPARATOR;
        unresolved.push(format!("{separator}parent{separator}..{separator}other-data"));
        let unresolved = PathBuf::from(unresolved);
        assert!(unresolved.is_absolute());
        assert!(unresolved.components().any(|part| matches!(part, std::path::Component::ParentDir)));
        assert!(tune_core::helpers::bind_process_data_dir(&unresolved).is_err());
        assert!(startup::parse(["--hidden".into()]).is_err());
        assert!(startup::parse(["--selftest".into()]).is_err());
        assert!(startup::parse(["--help".into()]).unwrap().is_none());
        assert_eq!(tune_core::helpers::bin_dir(), stale.join("bin"));
        return;
    }
    if case.starts_with("legacy-") {
        let expected = if case == "legacy-env" { stale.join("bin") } else { root.join("home/.katala-tune/bin") };
        assert_eq!(tune_core::helpers::bin_dir(), expected);
        let launch = local_windows_launch();
        assert_eq!(PathBuf::from(launch.program), root.join("owned-app").join(tune_core::helpers::exe("tune-agent")), "legacy sibling priority");
        return;
    }
    let expected = match case.as_str() {
        "default-env" => stale.join("gpui-preview"),
        #[cfg(not(windows))]
        "default-home" => {
            #[cfg(target_os = "macos")]
            let base = root.join("home/Library/Application Support");
            #[cfg(not(target_os = "macos"))]
            let base = root.join("config");
            base.join("Katala Tune/data/gpui-preview")
        }
        "missing" => root.join("missing-data"),
        _ => root.join("chosen-data"),
    };
    let mut args = Vec::new();
    if !case.starts_with("default-") {
        let data = match case.as_str() {
            "relative" => "chosen-data".into(),
            "symlink" => root.join("data-alias").into_os_string(),
            _ => expected.clone().into_os_string(),
        };
        args.extend(["--data".into(), data]);
    }
    match case.as_str() {
        "resident" => args.push("--resident".into()),
        "hidden" => args.extend(["--resident".into(), "--hidden".into()]),
        "diagnostic" => args.extend(["--selftest".into(), "--config".into(), root.join("unused-ledger.json").into_os_string()]),
        _ => {}
    }
    let options = startup::prepare(startup::parse(args).unwrap().unwrap()).unwrap();
    assert_eq!(options.data, expected, "the DB and helper must share the resolved data path");
    assert_eq!(tune_core::helpers::bin_dir(), expected.join("bin"), "startup must override inherited/canonical helper selection");
    assert_eq!(std::env::var_os("KATALA_TUNE_DATA_DIR"), inherited, "binding must not mutate ambient environment");
    assert!(!options.config.exists(), "startup must not create or load a ledger");
    let launch = local_windows_launch();
    if matches!(case.as_str(), "ordinary" | "resident" | "hidden" | "diagnostic" | "relative" | "immutable" | "symlink") {
        assert_eq!(PathBuf::from(launch.program), expected.join("bin").join(tune_core::helpers::exe("tune-agent")), "bound Live ignores the bundled sibling");
    } else {
        assert_eq!(launch.program, "powershell.exe", "missing bound sampler must not use sibling or canonical sampler");
    }
    if case == "immutable" {
        let prepare = |data: PathBuf| startup::prepare(startup::parse(["--data".into(), data.into_os_string()]).unwrap().unwrap());
        assert!(prepare(expected.clone()).is_ok(), "same binding is idempotent");
        assert!(prepare(root.join("other-data")).is_err(), "a different binding must fail");
        assert_eq!(tune_core::helpers::bin_dir(), expected.join("bin"));
    }
    if case == "relative" {
        std::env::set_current_dir(root.join("home")).unwrap();
        assert_eq!(tune_core::helpers::bin_dir(), expected.join("bin"), "later cwd must not shift the binding");
    }
}

fn local_windows_launch() -> tune_core::live::Launch {
    let node = tune_core::nodes::Node::from_value(&serde_json::json!({"id":"fixture", "os":"windows", "local_hostname":"fixture"}), "fixture");
    tune_core::live::launch(&node, &tune_core::live::Opts::default()).unwrap()
}
