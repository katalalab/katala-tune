//! Fixed finite Windows probe helper, dispatched before Tokio or application state.
use std::{process::ExitCode, time::Instant};

#[cfg(windows)]
const BUDGET_MS: u64 = 120_000;

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
    let mut command = Command::new(shell);
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &payload]);
    let environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    if environment.is_empty() {
        return false;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => return false,
    };
    let result =
        runtime.block_on(run_standalone_windows_bounded(&command, StandaloneWindowsEnvironment::new(&environment), b"", backend_end, 1_048_576, 65_536));
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

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the separately approved PS5/7 native helper stage"]
    fn payload_parses_and_emits_the_fixed_benchmark_marker() {
        // Native execution belongs to the Windows canary stage.  Keep this
        // positive parser/output contract beside the source constructor.
        let payload = powershell_payload(true, false, true).unwrap();
        assert!(payload.contains("$NoNetwork = $true"));
        assert!(payload.contains(tune_core::collect::BENCH_MARK));
    }
}
