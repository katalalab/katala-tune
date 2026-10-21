//! Fixed finite Windows probe helper, dispatched before Tokio or application state.
use std::{process::ExitCode, time::Instant};

#[cfg(windows)]
const BUDGET_MS: u64 = 120_000;
#[cfg(any(windows, test))]
const MAX_PROBE_PAYLOAD_BYTES: usize = 128 * 1024;
#[cfg(any(windows, test))]
const WINDOWS_COMMAND_LINE_LIMIT: usize = 32_767;
/// The probe accepts partial snapshots from its permissive observations.  Do
/// not use the action bootstrap here: it turns a tolerated native observation
/// failure left in `$LASTEXITCODE` into a failure of the whole probe.
#[cfg(any(windows, test))]
const PROBE_POWERSHELL_BOOTSTRAP: &str = "$ErrorActionPreference='Stop';try{$r=New-Object System.IO.StreamReader([Console]::OpenStandardInput(),[System.Text.Encoding]::ASCII,$false,1024,$true);$b=[Convert]::FromBase64String($r.ReadToEnd());$s=(New-Object System.Text.UTF8Encoding($false,$true)).GetString($b);$sb=[ScriptBlock]::Create($s)}catch{exit 9};try{&$sb;exit 0}catch{exit 7}";

fn arguments(args: &[String]) -> Option<(String, bool, bool, bool)> {
    let [hash_flag, hash, benchmark_flag, benchmark, network_flag, network, wsl_flag, wsl, budget_flag, budget] = args else { return None };
    if hash_flag != "--helper-sha256"
        || benchmark_flag != "--benchmark"
        || network_flag != "--network"
        || wsl_flag != "--observe-wsl"
        || budget_flag != "--budget-ms"
        || budget != "120000"
    {
        return None;
    }
    let boolean = |value: &str| match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    };
    (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))).then_some((
        hash.clone(),
        boolean(benchmark)?,
        boolean(network)?,
        boolean(wsl)?,
    ))
}

/// Keep the embedded script's one parameter declaration.  The fixed helper
/// receives closed argv booleans, then assigns the probe switches inside that
/// declaration's scope; wrapping the script in another `param` block makes a
/// PowerShell payload invalid.
#[cfg_attr(not(windows), allow(dead_code))]
fn powershell_payload(benchmark: bool, network: bool, observe_wsl: bool) -> Option<String> {
    let script = std::str::from_utf8(tune_core::collect::WIN_PROBE).ok()?;
    let boundary = "param([switch]$NoNetwork, [switch]$ObserveWsl)";
    let switches =
        format!("{boundary}\n$NoNetwork = ${}\n$ObserveWsl = ${}", if network { "false" } else { "true" }, if observe_wsl { "true" } else { "false" },);
    let script = script.replacen(boundary, &switches, 1);
    if script == std::str::from_utf8(tune_core::collect::WIN_PROBE).ok()? {
        return None;
    }
    if !benchmark {
        return Some(script);
    }
    // The benchmark is fixed at build time.  Quote it as PowerShell text so
    // neither argv nor the ambient environment can alter the Python program.
    let python = tune_core::collect::BENCH_PY.replace('\'', "''");
    Some(format!("{script}\nWrite-Output '{}'\n$katalaTuneBenchmark = '{}'\n& python -c $katalaTuneBenchmark\n", tune_core::collect::BENCH_MARK, python,))
}

#[cfg(any(windows, test))]
fn powershell_arguments() -> [&'static str; 5] {
    ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", PROBE_POWERSHELL_BOOTSTRAP]
}

/// Build the only helper-to-PowerShell transport: a small fixed argv and an
/// ASCII stdin body containing base64 of the compile-time payload.  This is
/// distinct from controller-to-helper stdin, which remains EOF-only.
#[cfg(any(windows, test))]
fn powershell_stdin(payload: &str) -> Option<Vec<u8>> {
    (payload.len() <= MAX_PROBE_PAYLOAD_BYTES).then(|| super::windows_action::base64(payload.as_bytes()))
}

#[cfg(any(windows, test))]
fn windows_argv_units(executable: &str, args: &[&str]) -> usize {
    executable.encode_utf16().count() + 3 + args.iter().map(|arg| arg.encode_utf16().count() + 3).sum::<usize>()
}

#[cfg(not(windows))]
pub fn main(args: &[String], _started: Instant) -> ExitCode {
    if arguments(args).is_some() { ExitCode::from(2) } else { ExitCode::from(9) }
}

#[cfg(windows)]
pub fn main(args: &[String], started: Instant) -> ExitCode {
    use std::{sync::mpsc, time::Duration};

    let Some((sha256, benchmark, network, observe_wsl)) = arguments(args) else { return ExitCode::from(9) };
    let Some(hard_end) = started.checked_add(Duration::from_millis(BUDGET_MS)) else { return ExitCode::from(9) };
    let Some(backend_end) = hard_end.checked_sub(Duration::from_millis(3250)) else { return ExitCode::from(9) };
    // The main thread has no stdio or backend work.  It is the independent
    // watchdog from before pinning through a potentially blocked final flush.
    let (tx, rx) = mpsc::channel();
    if std::thread::Builder::new()
        .name("windows-probe-worker".into())
        .spawn(move || {
            let _ = tx.send(run_once(sha256, benchmark, network, observe_wsl, hard_end, backend_end));
        })
        .is_err()
    {
        super::topology_reader::terminate_self(9);
    }
    let Some(remaining) = hard_end.checked_duration_since(Instant::now()) else { super::topology_reader::terminate_self(9) };
    match rx.recv_timeout(remaining) {
        Ok(true) if Instant::now() < hard_end => super::topology_reader::terminate_self(0),
        _ => super::topology_reader::terminate_self(9),
    }
}

#[cfg(windows)]
fn run_once(sha256: String, benchmark: bool, network: bool, observe_wsl: bool, hard_end: Instant, backend_end: Instant) -> bool {
    use std::{ffi::OsString, io::Write, process::Command};
    use tune_core::collect::{PinnedWindowsExecutable, ProcessStop, StandaloneWindowsEnvironment, run_standalone_windows_bounded};

    let image = match std::env::current_exe() {
        Ok(image) => image,
        Err(_) => return false,
    };
    let _lease = match PinnedWindowsExecutable::open(&image, &sha256, hard_end) {
        Ok(lease) => lease,
        Err(_) => return false,
    };
    let shell = match super::windows_action::powershell_path() {
        Ok(shell) => shell,
        Err(_) => return false,
    };
    let Some(payload) = powershell_payload(benchmark, network, observe_wsl) else { return false };
    let Some(input) = powershell_stdin(&payload) else { return false };
    let mut command = Command::new(shell);
    command.args(powershell_arguments());
    let environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    if environment.is_empty() {
        return false;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => return false,
    };
    let result =
        runtime.block_on(run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&environment), &input, backend_end, 1_048_576, 65_536));
    let accepted = result.code == Some(0)
        && !result.timed_out
        && !result.output_limited
        && !result.spawn_failed
        && result.process_receipt.as_ref().is_some_and(|receipt| receipt.stop == ProcessStop::Completed && receipt.cleanup_confirmed())
        && Instant::now() < hard_end;
    if !accepted {
        return false;
    }
    let mut output = std::io::stdout().lock();
    if output.write_all(&result.stdout).is_err() || output.flush().is_err() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_closed_and_fixed() {
        let hash = "a".repeat(64);
        assert_eq!(
            arguments(&[
                "--helper-sha256".into(),
                hash.clone(),
                "--benchmark".into(),
                "1".into(),
                "--network".into(),
                "0".into(),
                "--observe-wsl".into(),
                "1".into(),
                "--budget-ms".into(),
                "120000".into()
            ]),
            Some((hash, true, false, true))
        );
        assert!(
            arguments(&[
                "--helper-sha256".into(),
                "a".repeat(64),
                "--benchmark".into(),
                "2".into(),
                "--network".into(),
                "0".into(),
                "--observe-wsl".into(),
                "1".into(),
                "--budget-ms".into(),
                "120000".into()
            ])
            .is_none()
        );
    }

    #[test]
    fn payload_covers_all_closed_boolean_combinations() {
        for benchmark in [false, true] {
            for network in [false, true] {
                for observe_wsl in [false, true] {
                    let payload = powershell_payload(benchmark, network, observe_wsl).unwrap();
                    assert_eq!(payload.matches("param([switch]$NoNetwork, [switch]$ObserveWsl)").count(), 1);
                    assert!(payload.contains(if network { "$NoNetwork = $false" } else { "$NoNetwork = $true" }));
                    assert!(payload.contains(if observe_wsl { "$ObserveWsl = $true" } else { "$ObserveWsl = $false" }));
                    assert_eq!(payload.contains(tune_core::collect::BENCH_MARK), benchmark);
                    assert_eq!(payload.contains(&tune_core::collect::BENCH_PY.replace('\'', "''")), benchmark);
                }
            }
        }
    }

    #[test]
    fn probe_payload_uses_bounded_stdin_and_never_the_windows_command_line() {
        let payload = powershell_payload(true, false, true).unwrap();
        let input = powershell_stdin(&payload).unwrap();
        assert!(payload.len() > 32_767, "counterfactual oversized payload must fail when placed in argv");
        assert!(input.len() <= MAX_PROBE_PAYLOAD_BYTES.div_ceil(3) * 4);
        let shell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        assert!(windows_argv_units(shell, &powershell_arguments()) < WINDOWS_COMMAND_LINE_LIMIT);
        let oversized_argv = ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", payload.as_str()];
        assert!(windows_argv_units(shell, &oversized_argv) >= WINDOWS_COMMAND_LINE_LIMIT);
        let max_payload = "x".repeat(MAX_PROBE_PAYLOAD_BYTES);
        let max_input = powershell_stdin(&max_payload).unwrap();
        assert_eq!(max_input.len(), MAX_PROBE_PAYLOAD_BYTES.div_ceil(3) * 4);
        let max_argv = ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", max_payload.as_str()];
        assert!(windows_argv_units(shell, &max_argv) >= WINDOWS_COMMAND_LINE_LIMIT);
        assert_eq!(input, crate::windows_action::base64(payload.as_bytes()));
        assert!(input.iter().all(u8::is_ascii));
    }

    #[cfg(windows)]
    fn run_bootstrap(bootstrap: &str, input: &[u8]) -> std::process::Output {
        use std::{
            io::Write,
            process::{Command, Stdio},
        };

        let shell = crate::windows_action::powershell_path().expect("fixed System32 PowerShell resolver");
        let mut child = Command::new(shell)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", bootstrap])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start fixed PowerShell fixture");
        let mut stdin = child.stdin.take().expect("fixture stdin");
        stdin.write_all(input).expect("write fixture payload");
        drop(stdin);
        child.wait_with_output().expect("wait for fixed PowerShell fixture")
    }

    #[cfg(windows)]
    fn run_probe_payload(payload: &str) -> std::process::Output {
        let input = powershell_stdin(payload).expect("bounded fixture payload");
        run_bootstrap(PROBE_POWERSHELL_BOOTSTRAP, &input)
    }

    #[cfg(windows)]
    const SYSTEM32_CMD_NONZERO: &str = "& (Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::System) 'cmd.exe') /c exit 23";

    #[cfg(windows)]
    #[test]
    fn probe_bootstrap_ignores_tolerated_native_exit_after_snapshot() {
        let payload = format!("Write-Output '{{\"probe\":\"windows\"}}'\n{SYSTEM32_CMD_NONZERO}\n");
        let input = powershell_stdin(&payload).unwrap();
        let action = run_bootstrap(crate::windows_action::POWERSHELL_BOOTSTRAP, &input);
        assert!(!action.status.success(), "the action bootstrap demonstrates the stale LASTEXITCODE counterfactual");

        let probe = run_probe_payload(&payload);
        assert!(probe.status.success(), "probe completion, not a tolerated observation, sets its exit status: {:?}", probe.status);
        assert_eq!(probe.stdout, b"{\"probe\":\"windows\"}\r\n");
    }

    #[cfg(windows)]
    #[test]
    fn probe_bootstrap_does_not_follow_the_action_bootstrap_error_path() {
        let payload = "Write-Output '{\"probe\":\"windows\"}'\n$global:LASTEXITCODE = 'not-an-exit-code'\n";
        let input = powershell_stdin(payload).unwrap();
        let action = run_bootstrap(crate::windows_action::POWERSHELL_BOOTSTRAP, &input);
        assert_eq!(action.status.code(), Some(7), "the action bootstrap error path remains observable with base64 stdin");

        let probe = run_probe_payload(payload);
        assert!(probe.status.success());
        assert_eq!(probe.stdout, b"{\"probe\":\"windows\"}\r\n");
    }

    #[cfg(windows)]
    #[test]
    fn probe_bootstrap_retains_snapshot_when_benchmark_exit_is_nonzero() {
        let payload = format!("Write-Output '{{\"probe\":\"windows\"}}'\nWrite-Output '{}'\n{SYSTEM32_CMD_NONZERO}\n", tune_core::collect::BENCH_MARK,);
        let probe = run_probe_payload(&payload);
        assert!(probe.status.success(), "a benchmark native exit must not discard an already emitted snapshot: {:?}", probe.status);
        assert_eq!(probe.stdout, format!("{{\"probe\":\"windows\"}}\r\n{}\r\n", tune_core::collect::BENCH_MARK).as_bytes(),);
    }

    #[cfg(windows)]
    #[test]
    fn probe_bootstrap_rejects_decode_parse_and_terminating_payload_failures() {
        let decode = run_bootstrap(PROBE_POWERSHELL_BOOTSTRAP, b"%%%not-base64%%%");
        assert!(!decode.status.success());
        assert!(decode.stdout.is_empty());

        let parse = run_bootstrap(PROBE_POWERSHELL_BOOTSTRAP, &powershell_stdin("if (").unwrap());
        assert!(!parse.status.success());
        assert!(parse.stdout.is_empty());

        let terminating = run_probe_payload("throw 'fixture terminating failure'");
        assert!(!terminating.status.success());
        assert!(terminating.stdout.is_empty());
    }
}
