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

#[cfg(not(windows))]
pub fn main(args: &[String], _started: Instant) -> ExitCode {
    if arguments(args).is_some() { ExitCode::from(2) } else { ExitCode::from(9) }
}

#[cfg(windows)]
pub fn main(args: &[String], started: Instant) -> ExitCode {
    use std::{ffi::OsString, io::Write, process::Command, time::Duration};
    use tune_core::collect::{PinnedWindowsExecutable, ProcessStop, StandaloneWindowsEnvironment, run_standalone_windows_bounded};

    let Some((sha256, benchmark, network, observe_wsl)) = arguments(args) else { return ExitCode::from(9) };
    let Some(hard_end) = started.checked_add(Duration::from_millis(BUDGET_MS)) else { return ExitCode::from(9) };
    let Some(backend_end) = hard_end.checked_sub(Duration::from_millis(3250)) else { return ExitCode::from(9) };
    let image = match std::env::current_exe() {
        Ok(image) => image,
        Err(_) => return ExitCode::from(9),
    };
    let _lease = match PinnedWindowsExecutable::open(&image, &sha256, hard_end) {
        Ok(lease) => lease,
        Err(_) => return ExitCode::from(9),
    };
    let shell = match super::windows_action::powershell_path() {
        Ok(shell) => shell,
        Err(_) => return ExitCode::from(9),
    };
    let script = String::from_utf8_lossy(tune_core::collect::WIN_PROBE);
    let payload = format!(
        "& {{ param([bool]$Benchmark,[bool]$Network,[bool]$ObserveWsl) {} }} -Benchmark:${} -Network:${} -ObserveWsl:${}",
        script, benchmark, network, observe_wsl
    );
    let mut command = Command::new(shell);
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &payload]);
    let environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    if environment.is_empty() {
        return ExitCode::from(9);
    }
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => return ExitCode::from(9),
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
        return ExitCode::from(9);
    }
    let mut output = std::io::stdout().lock();
    if output.write_all(&result.stdout).is_err() || output.flush().is_err() {
        return ExitCode::from(9);
    }
    ExitCode::SUCCESS
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
}
