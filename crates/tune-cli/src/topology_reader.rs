//! Isolated Windows-only reader entry. Never invoked in an application runtime.
//! The supervisor owns no standard IO locks, ledger, database, or child handles.
use std::{process::ExitCode, time::Instant};

#[cfg(not(windows))]
pub fn main(_args: &[String], _started: Instant) -> ExitCode {
    ExitCode::from(2)
}

#[cfg(windows)]
pub fn main(args: &[String], started: Instant) -> ExitCode {
    use std::{io::Write, sync::mpsc, time::Duration};
    use tune_core::{
        collect::ProcessStop,
        topology::adapters::windows_reader::{MAX_BUDGET_MS, ReaderFailure},
    };
    let budget = match args {
        [flag, value] if flag == "--budget-ms" && value.len() <= 7 && value.bytes().all(|b| b.is_ascii_digit()) => {
            value.parse::<u64>().ok().filter(|n| *n > 0 && *n <= MAX_BUDGET_MS && n.to_string() == *value)
        }
        _ => None,
    };
    let Some(budget) = budget else { terminate_self(2) };
    let Some(hard_deadline) = started.checked_add(Duration::from_millis(budget)) else { terminate_self(2) };
    let input_deadline = hard_deadline.min(started + Duration::from_secs(2));
    enum Failure {
        StdinHandle,
        RequestFrame,
        RequestAdmission,
        RuntimeInit,
        Core(ReaderFailure),
        ResponseWrite,
    }
    impl Failure {
        fn stage(&self) -> &'static str {
            match self {
                Self::StdinHandle => "stdin_handle",
                Self::RequestFrame => "request_frame",
                Self::RequestAdmission => "request_admission",
                Self::RuntimeInit => "runtime_init",
                Self::Core(failure) => failure.stage.name(),
                Self::ResponseWrite => "response_write",
            }
        }
    }
    fn stop_name(stop: ProcessStop) -> &'static str {
        match stop {
            ProcessStop::SetupFailed => "setup_failed",
            ProcessStop::SpawnFailed => "spawn_failed",
            ProcessStop::Completed => "completed",
            ProcessStop::Timeout => "timeout",
            ProcessStop::OutputLimit => "output_limit",
            ProcessStop::LingeringDescendants => "lingering_descendants",
            ProcessStop::IoFailure => "io_failure",
            ProcessStop::CleanupUnconfirmed => "cleanup_unconfirmed",
        }
    }
    fn bit(value: bool) -> u8 {
        if value { 1 } else { 0 }
    }
    fn write_failure(failure: &Failure) -> std::io::Result<()> {
        let mut stderr = std::io::stderr().lock();
        match failure {
            Failure::Core(ReaderFailure { backend: Some(backend), .. }) => writeln!(
                stderr,
                "topology-reader failure={} backend={} spawn={} timeout={} limit={} stop={} root0={} cleanup={} receipt={}",
                failure.stage(),
                backend.exit.name(),
                bit(backend.spawn_failed),
                bit(backend.timed_out),
                bit(backend.output_limited),
                backend.stop.map(stop_name).unwrap_or("none"),
                bit(backend.root_exit_zero),
                bit(backend.cleanup_confirmed),
                bit(backend.receipt_accepted),
            ),
            _ => writeln!(stderr, "topology-reader failure={} backend=none", failure.stage()),
        }
    }
    enum Event {
        Admitted(Instant),
        Finished(Result<(), Failure>),
    }
    let (tx, rx) = mpsc::channel();
    let spawn = std::thread::Builder::new().name("topology-reader".into()).spawn(move || {
        let work = || -> Result<(), Failure> {
            let mut stdin = unbuffered_stdin().map_err(|_| Failure::StdinHandle)?;
            let (request, bytes) = tune_core::topology::adapters::windows_reader::read_request(&mut stdin).map_err(|_| Failure::RequestFrame)?;
            if request.budget_ms > budget || Instant::now() >= input_deadline {
                return Err(Failure::RequestAdmission);
            }
            // Request budget shares the CLI entry's time origin and can only
            // shorten the absolute argv cap. Malformed input never updates it.
            let deadline = started.checked_add(Duration::from_millis(request.budget_ms)).ok_or(Failure::RequestAdmission)?.min(hard_deadline);
            tx.send(Event::Admitted(deadline)).map_err(|_| Failure::RequestAdmission)?;
            if Instant::now() >= deadline {
                return Err(Failure::RequestAdmission);
            }
            let response = {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| Failure::RuntimeInit)?;
                runtime.block_on(tune_core::topology::adapters::windows_reader::execute_diagnostic(&request, &bytes, deadline)).map_err(Failure::Core)?
                // All guard resources and this independent runtime are dropped
                // before a blocking response write or successful notification.
            };
            tune_core::topology::adapters::windows_reader::write_response(&mut std::io::stdout().lock(), &response).map_err(|_| Failure::ResponseWrite)?;
            Ok(())
        };
        let result = work();
        if let Err(failure) = &result {
            let _ = write_failure(failure);
        }
        let _ = tx.send(Event::Finished(result));
    });
    if spawn.is_err() {
        terminate_self(2);
    }
    // No join, logging, flush, or Rust/CRT shutdown in an uncertain IO state.
    let mut deadline = input_deadline;
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else { terminate_self(124) };
        match rx.recv_timeout(remaining) {
            Ok(Event::Admitted(end)) => match admit_deadline(deadline, end.min(hard_deadline), Instant::now()) {
                Some(end) => deadline = end,
                None => terminate_self(124),
            },
            Ok(Event::Finished(result)) => {
                if Instant::now() >= deadline {
                    terminate_self(124);
                }
                terminate_self(if result.is_ok() { 0 } else { 2 });
            }
            Err(_) => terminate_self(124),
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn terminate_self(code: u32) -> ! {
    // SAFETY: borrowed pseudo-handle always names only this standalone reader.
    // No PID lookup, inherited parent Job, destructor, or DLL detach is used.
    unsafe {
        windows_sys::Win32::System::Threading::TerminateProcess(windows_sys::Win32::System::Threading::GetCurrentProcess(), code);
    }
    // Self termination does not return on success. Preserve no-return behavior
    // without IO/cleanup if an unexpected platform failure returns instead.
    std::process::abort()
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn unbuffered_stdin() -> Result<std::fs::File, tune_core::topology::adapters::windows_reader::ReaderError> {
    use std::os::windows::io::BorrowedHandle;
    use tune_core::topology::adapters::windows_reader::ReaderError;
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::Console::{GetStdHandle, STD_INPUT_HANDLE},
    };
    // SAFETY: the standalone reader is the sole stdin consumer and does not
    // replace/close its process standard handle while obtaining a duplicate.
    let raw = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        return Err(ReaderError);
    }
    // SAFETY: raw is a checked open process standard handle and stays open
    // until after the independent owned duplicate has been obtained.
    let borrowed = unsafe { BorrowedHandle::borrow_raw(raw) };
    let owned = borrowed.try_clone_to_owned().map_err(|_| ReaderError)?;
    // File reads are unbuffered: even the sixteen-byte prefix read cannot
    // prefetch bytes belonging to a later, deliberately ignored frame.
    Ok(std::fs::File::from(owned))
}

#[cfg(any(windows, test))]
fn admit_deadline(current: Instant, proposed: Instant, observed: Instant) -> Option<Instant> {
    (observed < current && observed < proposed).then_some(proposed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn admission_observed_at_or_after_input_deadline_never_extends_it() {
        let start = Instant::now();
        let input_end = start + Duration::from_secs(2);
        let work_end = start + Duration::from_secs(10);
        assert_eq!(admit_deadline(input_end, work_end, start + Duration::from_secs(1)), Some(work_end));
        assert_eq!(admit_deadline(input_end, work_end, input_end), None);
        assert_eq!(admit_deadline(input_end, work_end, input_end + Duration::from_nanos(1)), None);
        assert_eq!(admit_deadline(input_end, start + Duration::from_secs(1), input_end - Duration::from_millis(1)), None);
    }
}
