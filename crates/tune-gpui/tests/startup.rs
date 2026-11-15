use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root =
            std::env::temp_dir().join(format!("kt-gpui-startup-{}-{}-{}", std::process::id(), tune_core::db::now_ms(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&root).unwrap();
        // The old parser falls back to this path. A regular file makes the DB
        // open fail before a GUI can start, while proving the wrong code path.
        std::fs::write(root.join("production"), b"keep").unwrap();
        std::fs::write(root.join("invalid.json"), b"invalid fixture").unwrap();
        Self(root)
    }

    fn run(&self, args: &[&str]) -> Output {
        let binary = env!("CARGO_BIN_EXE_katala-tune-gpui");
        Command::new(binary)
            .args(args)
            .env("KATALA_TUNE_CONFIG", self.0.join("invalid.json"))
            .env("KATALA_TUNE_DATA_DIR", self.0.join("production"))
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn invalid_startup_arguments_are_rejected_before_opening_user_state() {
    let f = Fixture::new();
    for (args, expected) in [
        (vec!["--selftest"], "--selftest requires --config and --data"),
        (vec!["--hidden"], "--hidden requires --resident"),
        (vec!["--resident", "--selftest"], "cannot be combined with diagnostics"),
        (vec!["--resident", "--hidden", "--shutdown-check", "quit"], "cannot be combined with diagnostics"),
        (vec!["--selftest", "--data"], "--data requires a path"),
        (vec!["--dat"], "unknown option"),
        (vec!["--config", "--data", "unused"], "--config requires a path"),
    ] {
        let out = f.run(&args);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(expected), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(std::fs::read(f.0.join("production")).unwrap(), b"keep");
        assert_eq!(std::fs::read(f.0.join("invalid.json")).unwrap(), b"invalid fixture");
    }
}

#[test]
fn selftest_refuses_invalid_ledger_before_creating_a_database() {
    let f = Fixture::new();
    let data = f.0.join("qa-data");
    let out = f.run(&["--selftest", "--config", f.0.join("invalid.json").to_str().unwrap(), "--data", data.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(!data.exists());
    assert_eq!(std::fs::read(f.0.join("invalid.json")).unwrap(), b"invalid fixture");
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[test]
fn unsupported_desktop_does_not_open_or_create_user_state() {
    let f = Fixture::new();
    let out = f.run(&[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("supports macOS and Windows"));
    assert_eq!(std::fs::read(f.0.join("production")).unwrap(), b"keep");
    assert_eq!(std::fs::read(f.0.join("invalid.json")).unwrap(), b"invalid fixture");
}
