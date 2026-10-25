//! Native, fake-only coverage of the production Windows launcher and terminal.
//! The supervisor is a separate single-spawner process, as required by the owned
//! Windows backend. No parent-process environment or installed CLI is changed.

use super::{Agent, PROMPT_FILE, launcher, open_terminal};
use crate::collect::{ProcessStop, StandaloneWindowsEnvironment, run_standalone_windows_bounded};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MODE: &str = "KT_NATIVE_LAUNCHER_MODE";
const ROOT: &str = "KT_NATIVE_LAUNCHER_ROOT";
const RECEIPT: &str = "KT_WINDOWS_NATIVE_LAUNCHER_V1\n";
const KICKOFF: &str = "Read PROMPT.md in this folder and follow it.";
const PROMPT: &str = "KT_PROMPT_DATA_SENTINEL\r\n日本語 'quoted' \"double\" %KT_NATIVE_EXPAND% !KT_NATIVE_BANG!\r\n& echo PROMPT_EXECUTED>prompt-was-executed.txt\r\n| echo pipe >>prompt-was-executed.txt\r\n$(echo substitution) `echo backtick`\r\nexit /b 93\r\n";

#[test]
fn windows_production_launcher_native_four_cases() {
    match std::env::var(MODE).as_deref() {
        Ok("supervisor") => {
            // Only this dedicated process uses the single-spawner backend.
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(supervise());
        }
        Ok("driver") => drive(),
        Ok(_) => panic!("unknown native launcher fixture mode"),
        Err(_) => {
            let root = FixtureDirectory::new();
            let mut environment = compiler_environment();
            environment.extend([(MODE.into(), "supervisor".into()), (ROOT.into(), root.0.as_os_str().to_owned())]);
            let mut command = invocation();
            command.env_clear().envs(environment).stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
            let mut child = OwnedChild(Some(command.spawn().expect("start native fixture supervisor")));
            let end = Instant::now() + Duration::from_secs(180);
            loop {
                if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
                    child.0.take();
                    assert!(status.success(), "native launcher supervisor failed: {status}");
                    break;
                }
                assert!(Instant::now() < end, "native launcher supervisor deadline exceeded");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn invocation() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    let test = format!("{}::windows_production_launcher_native_four_cases", module_path!().split_once("::").unwrap().1);
    command.args(["--exact", &test, "--nocapture", "--test-threads=1"]);
    command
}

// Only explicit OS/toolchain locations, never the full inherited environment.
// These values are used for compilation/supervision, never for the CLI driver.
fn compiler_environment() -> Vec<(OsString, OsString)> {
    let mut values: Vec<_> = [
        "SystemRoot",
        "WINDIR",
        // rustc's MSVC discovery uses these to locate VS Installer/vswhere.
        // Without them it can fall back to an unrelated PATH link.exe.
        "ProgramFiles(x86)",
        "ProgramFiles",
        // Preserve VS Setup's instance-cache root in this cleared environment.
        "ProgramData",
        "TEMP",
        "TMP",
        "PATH",
        "USERPROFILE",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "RUSTC",
        "INCLUDE",
        "LIB",
        "LIBPATH",
        "VSINSTALLDIR",
        "VCINSTALLDIR",
        "VCToolsInstallDir",
        "WindowsSdkDir",
        "WindowsSDKVersion",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (key.into(), value)))
    .collect();
    values.extend([("RUSTUP_AUTO_INSTALL".into(), "0".into()), ("RUSTUP_NO_UPDATE_CHECK".into(), "1".into())]);
    values
}

async fn compiler_program(environment: &[(OsString, OsString)]) -> OsString {
    if let Some(rustc) = std::env::var_os("RUSTC") {
        return rustc;
    }
    // The backend canonicalizes executable paths. A rustup rustc symlink would
    // then become rustup.exe, whose argv[0] selects the wrong CLI mode. Ask the
    // installed, auto-install-disabled rustup for the active real compiler.
    let mut query = Command::new("rustup");
    query.args(["which", "rustc"]);
    let result = run_standalone_windows_bounded(
        &query,
        StandaloneWindowsEnvironment::new(environment),
        b"",
        Instant::now() + Duration::from_secs(15),
        16 * 1024,
        16 * 1024,
    )
    .await;
    assert_compiler_success(&result, "locate real rustc");
    let output = std::str::from_utf8(&result.stdout).expect("rustup compiler path must be UTF-8");
    let path = PathBuf::from(output.trim_end_matches(['\r', '\n']));
    assert!(
        path.is_absolute() && path.is_file() && path.file_name().is_some_and(|name| name.eq_ignore_ascii_case("rustc.exe")),
        "rustup must locate an existing absolute rustc.exe"
    );
    path.into_os_string()
}

async fn supervise() {
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let fakebin = root.join("fakebin");
    std::fs::create_dir(&fakebin).unwrap();
    let source = root.join("fake_agent.rs");
    // Rust source data, passed as a file to rustc, never interpolated into cmd.
    std::fs::write(&source, format!("const EXPECTED_PROMPT: &str = {PROMPT:?};\nconst RECEIPT: &str = {RECEIPT:?};\n{FAKE_AGENT}")).unwrap();
    let environment = compiler_environment();
    let mut compiler = Command::new(compiler_program(&environment).await);
    compiler.args(["--edition=2024", "--crate-name", "katala_fake_agent"]).arg(&source).arg("-o").arg(fakebin.join("claude.exe"));
    let result = run_standalone_windows_bounded(
        &compiler,
        StandaloneWindowsEnvironment::new(&environment),
        b"",
        Instant::now() + Duration::from_secs(90),
        16 * 1024,
        16 * 1024,
    )
    .await;
    assert_compiler_success(&result, "compile fake-only agent");
    std::fs::copy(fakebin.join("claude.exe"), fakebin.join("codex.exe")).unwrap();

    let system_root = PathBuf::from(std::env::var_os("SystemRoot").expect("Windows SystemRoot"));
    let system32 = system_root.join("System32");
    // The only search locations for agent commands are our empty case directory,
    // fakebin and System32. Refuse an unexpected system-installed namesake.
    for agent in ["claude", "codex"] {
        for extension in ["exe", "com", "cmd", "bat"] {
            assert!(!system32.join(format!("{agent}.{extension}")).exists(), "unexpected agent in System32");
        }
    }
    let path = std::env::join_paths([&fakebin, &system32]).unwrap();
    let driver_cwd = root.join("driver-cwd");
    std::fs::create_dir(&driver_cwd).unwrap();
    for agent in [Agent::Claude, Agent::Codex] {
        for suffix in ["simple", "space & 日本語 %KT_NATIVE_EXPAND% !KT_NATIVE_BANG!"] {
            // Both cases deliberately use slash-form paths at the production API.
            let dir = PathBuf::from(root.join(format!("{}-{suffix}", agent.key())).to_str().unwrap().replace('\\', "/"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join(PROMPT_FILE), PROMPT).unwrap();
            let (script, mut body) = launcher(agent, true);
            // All production bytes execute unchanged. Only this test-only tail
            // closes cmd /K after the fake returns; /K persistence is not tested.
            body.push_str("exit\r\n");
            std::fs::write(dir.join(script), body).unwrap();
            let environment = vec![
                ("SystemRoot".into(), system_root.as_os_str().to_owned()),
                ("WINDIR".into(), system_root.as_os_str().to_owned()),
                ("COMSPEC".into(), system32.join("cmd.exe").into_os_string()),
                ("PATH".into(), path.clone()),
                ("PATHEXT".into(), ".EXE".into()),
                (MODE.into(), "driver".into()),
                (ROOT.into(), dir.as_os_str().to_owned()),
                ("KT_NATIVE_EXPECTED_DIR".into(), dir.as_os_str().to_owned()),
                ("KT_NATIVE_EXPAND".into(), "EXPANSION_MUST_NOT_HAPPEN".into()),
                ("KT_NATIVE_BANG".into(), "BANG_MUST_NOT_HAPPEN".into()),
            ];
            let mut command = invocation();
            // A missing production current_dir must fail to find launch.cmd;
            // inheriting the handoff directory here would conceal that bug.
            command.current_dir(&driver_cwd);
            let result = run_standalone_windows_bounded(
                &command,
                StandaloneWindowsEnvironment::new(&environment),
                b"",
                Instant::now() + Duration::from_secs(12),
                16 * 1024,
                16 * 1024,
            )
            .await;
            assert_owned_cleanup(&result);
            let receipt = result.process_receipt.as_ref().unwrap();
            // The driver may return just before the console's appended exit.
            // In that race the existing owner reaps only this Job's descendants.
            assert!(matches!(receipt.stop, ProcessStop::Completed | ProcessStop::LingeringDescendants), "{receipt:?}");
            assert_eq!(
                receipt.root_exit_code,
                Some(0),
                "driver failed: stdout={} stderr={}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            verify_receipt(&dir, agent);
        }
    }
}

fn assert_compiler_success(result: &crate::collect::BoundedResult, stage: &str) {
    assert_owned_cleanup(result);
    assert_eq!(
        result.code,
        Some(0),
        "{stage} failed: stop={:?} timeout={} limited={} spawn={} stdout={} stderr={}",
        result.process_receipt.as_ref().map(|receipt| receipt.stop),
        result.timed_out,
        result.output_limited,
        result.spawn_failed,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn assert_owned_cleanup(result: &crate::collect::BoundedResult) {
    let receipt = result.process_receipt.as_ref().expect("owned Windows Job receipt");
    assert!(receipt.cleanup_confirmed(), "owned fixture cleanup was not confirmed: {receipt:?}");
    assert_eq!(receipt.owner.as_ref().unwrap().pid, std::process::id());
}

fn drive() {
    let dir = PathBuf::from(std::env::var_os(ROOT).unwrap());
    open_terminal(&dir, "launch.cmd").expect("production open_terminal");
    let end = Instant::now() + Duration::from_secs(8);
    while !dir.join("receipt.bin").exists() {
        assert!(Instant::now() < end, "fake agent receipt deadline exceeded");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!dir.join("prompt-was-executed.txt").exists(), "PROMPT became a command");
}

fn verify_receipt(dir: &Path, agent: Agent) {
    let bytes = std::fs::read(dir.join("receipt.bin")).expect("fixed fake agent receipt");
    let mut remaining = bytes.as_slice();
    assert!(remaining.starts_with(RECEIPT.as_bytes()), "unexpected receipt token");
    remaining = &remaining[RECEIPT.len()..];
    let mut fields = Vec::new();
    while !remaining.is_empty() {
        assert!(remaining.len() >= 4, "truncated field length");
        let len = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
        remaining = &remaining[4..];
        assert!(len <= remaining.len(), "truncated receipt field");
        fields.push(String::from_utf8(remaining[..len].to_vec()).unwrap());
        remaining = &remaining[len..];
    }
    let args: &[&str] = match agent {
        Agent::Claude => &["--permission-mode", "plan", KICKOFF],
        Agent::Codex => &["--sandbox", "read-only", "--ask-for-approval", "on-request", KICKOFF],
    };
    assert_eq!(fields.len(), 4 + args.len(), "extra/missing argv or receipt data");
    assert_eq!(fields[0], agent.key());
    assert_eq!(Path::new(&fields[1]), dir, "literal working directory changed");
    assert_eq!(fields[2], "111", "stdin/stdout/stderr must each be a console");
    assert_eq!(fields[3], PROMPT, "PROMPT must remain exact file data");
    assert_eq!(&fields[4..], args, "production launcher changed literal argv");
    assert_eq!(std::fs::read(dir.join(PROMPT_FILE)).unwrap(), PROMPT.as_bytes());
    assert!(!dir.join("prompt-was-executed.txt").exists(), "PROMPT became a command");
}

struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            // Child retains the exact supervisor handle. Killing it also closes
            // its non-inherited KILL_ON_JOB_CLOSE handle: no PID/name lookup,
            // taskkill, shared Job mutation, or unrelated process is involved.
            let _ = child.kill();
            let end = Instant::now() + Duration::from_secs(3);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

struct FixtureDirectory(PathBuf);
impl FixtureDirectory {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kt-native-launcher-{}-{}", std::process::id(), crate::db::now_ms()));
        std::fs::create_dir(&root).expect("new isolated fixture directory");
        Self(root)
    }
}
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
                Err(_) if std::thread::panicking() => break,
                Err(error) => panic!("owned fixture directory cleanup failed: {error}"),
            }
        }
    }
}

// A native .exe is necessary: a .cmd fake would add another cmd parse boundary.
// This helper uses only std, performs no subprocess/network/provider calls, and
// publishes a fixed receipt only after validating argv, cwd, all three console
// handles and the exact adversarial PROMPT bytes.
const FAKE_AGENT: &str = r#"
use std::io::{IsTerminal, Write};
use std::path::Path;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let agent = exe.file_stem().unwrap().to_str().unwrap();
    let expected: Vec<&str> = match agent {
        "claude" => vec!["--permission-mode", "plan", "Read PROMPT.md in this folder and follow it."],
        "codex" => vec!["--sandbox", "read-only", "--ask-for-approval", "on-request", "Read PROMPT.md in this folder and follow it."],
        _ => panic!("only fake agents are permitted"),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(args, expected);
    let cwd = std::env::current_dir()?;
    assert_eq!(cwd, Path::new(&std::env::var_os("KT_NATIVE_EXPECTED_DIR").unwrap()));
    let terminals = [std::io::stdin().is_terminal(), std::io::stdout().is_terminal(), std::io::stderr().is_terminal()];
    assert_eq!(terminals, [true; 3]);
    let prompt = std::fs::read("PROMPT.md")?;
    assert_eq!(prompt, EXPECTED_PROMPT.as_bytes());
    assert!(!Path::new("prompt-was-executed.txt").exists());
    let mut receipt = std::fs::OpenOptions::new().write(true).create_new(true).open("receipt.tmp")?;
    receipt.write_all(RECEIPT.as_bytes())?;
    let cwd = cwd.to_str().unwrap();
    for field in [agent, cwd, "111", EXPECTED_PROMPT].into_iter().chain(args.iter().map(String::as_str)) {
        receipt.write_all(&u32::try_from(field.len())?.to_le_bytes())?;
        receipt.write_all(field.as_bytes())?;
    }
    receipt.sync_all()?;
    drop(receipt);
    std::fs::rename("receipt.tmp", "receipt.bin")?;
    Ok(())
}
"#;
