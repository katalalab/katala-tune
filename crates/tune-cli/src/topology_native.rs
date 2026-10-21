//! Dedicated finite entries. No application runtime, configuration or database.
use std::{process::ExitCode, time::Instant};

#[cfg(not(windows))]
pub fn main(_: &str, _: &[String], _: Instant) -> ExitCode {
    ExitCode::from(2)
}

#[cfg(windows)]
pub fn main(entry: &str, arguments: &[String], started: Instant) -> ExitCode {
    use std::{sync::mpsc, time::Duration};
    use tune_core::topology::adapters::{
        native_execution,
        windows_reader::{self, MAX_BUDGET_MS, ReaderError},
    };
    let parsed = (|| {
        let [flag, value, rest @ ..] = arguments else {
            return None;
        };
        if flag != "--budget-ms" || value.len() > 7 || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let budget = value.parse::<u64>().ok().filter(|n| *n > 0 && *n <= MAX_BUDGET_MS && n.to_string() == *value)?;
        let artifact = if entry == "topology-native-reader" {
            if !rest.is_empty() {
                return None;
            }
            None
        } else {
            let [program_flag, program, hash_flag, hash] = rest else {
                return None;
            };
            if program_flag != "--reader-program"
                || hash_flag != "--helper-sha256"
                || !windows_reader::valid_program_path(program)
                || hash.len() != 64
                || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            Some((program.clone(), hash.clone()))
        };
        Some((budget, artifact))
    })();
    let Some((budget, artifact)) = parsed else {
        super::topology_reader::terminate_self(2);
    };
    let Some(hard_end) = started.checked_add(Duration::from_millis(budget)) else {
        super::topology_reader::terminate_self(2);
    };
    let input_end = hard_end.min(started + Duration::from_secs(2));
    enum Event {
        Admitted(Instant),
        Finished(Result<(), ReaderError>),
    }
    let (tx, rx) = mpsc::channel();
    let entry = entry.to_owned();
    let spawn = std::thread::Builder::new().name("native-reader".into()).spawn(move || {
        let work = || -> Result<(), ReaderError> {
            let (request, body) = windows_reader::read_request(&mut super::topology_reader::unbuffered_stdin()?)?;
            if Instant::now() >= input_end || request.budget_ms > budget {
                return Err(ReaderError);
            }
            let end = started.checked_add(Duration::from_millis(request.budget_ms)).ok_or(ReaderError)?.min(hard_end);
            tx.send(Event::Admitted(end)).map_err(|_| ReaderError)?;
            let bytes = match entry.as_str() {
                "topology-native-reader" => native_execution::execute_reader(&request, &body, end)?,
                "topology-supervise" => {
                    let (program, hash) = artifact.as_ref().ok_or(ReaderError)?;
                    native_execution::supervise(&request, &body, end, std::path::Path::new(program), hash)?
                }
                "topology-verify" => {
                    let (program, hash) = artifact.as_ref().ok_or(ReaderError)?;
                    native_execution::verify(&request, &body, end, std::path::Path::new(program), hash)?
                }
                _ => return Err(ReaderError),
            };
            windows_reader::write_response(&mut std::io::stdout().lock(), &bytes)?;
            Ok(())
        };
        let _ = tx.send(Event::Finished(work()));
    });
    if spawn.is_err() {
        super::topology_reader::terminate_self(2);
    }
    let mut end = input_end;
    loop {
        let Some(remaining) = end.checked_duration_since(Instant::now()) else {
            super::topology_reader::terminate_self(124);
        };
        match rx.recv_timeout(remaining) {
            Ok(Event::Admitted(proposed)) if Instant::now() < end && Instant::now() < proposed => end = proposed.min(hard_end),
            Ok(Event::Finished(result)) if Instant::now() < end => super::topology_reader::terminate_self(if result.is_ok() { 0 } else { 2 }),
            _ => super::topology_reader::terminate_self(124),
        }
    }
}
