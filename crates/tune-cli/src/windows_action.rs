//! Dedicated one-shot Windows action receiver. It runs before application setup.
use std::{process::ExitCode, time::Instant};

#[cfg(any(windows, test))]
struct Diagnostic<'a> {
    stage: &'a str,
    outcome: &'a str,
    timed_out: bool,
    output_limited: bool,
    spawn_failed: bool,
    receipt_stop: &'a str,
    cleanup_confirmed: bool,
    elapsed: std::time::Duration,
}

#[cfg(any(windows, test))]
fn diagnostic_line(diagnostic: Diagnostic<'_>) -> String {
    let elapsed = match diagnostic.elapsed.as_secs() {
        0..=4 => "0_5s",
        5..=7 => "5_8s",
        _ => "8s_plus",
    };
    format!(
        "KT2 DIAG stage={} outcome={} timed_out={} output_limited={} spawn_failed={} receipt_stop={} cleanup_confirmed={} elapsed={elapsed}\n",
        diagnostic.stage,
        diagnostic.outcome,
        u8::from(diagnostic.timed_out),
        u8::from(diagnostic.output_limited),
        u8::from(diagnostic.spawn_failed),
        diagnostic.receipt_stop,
        u8::from(diagnostic.cleanup_confirmed),
    )
}

#[cfg(not(windows))]
pub fn main(_args: &[String], _started: Instant) -> ExitCode {
    ExitCode::from(2)
}

#[cfg(windows)]
pub fn main(args: &[String], started: Instant) -> ExitCode {
    use std::{sync::mpsc, time::Duration};

    let Some((expected_sha256, budget)) = parse_arguments(args) else {
        super::topology_reader::terminate_self(9);
    };
    let Some(hard_end) = started.checked_add(Duration::from_millis(budget)) else {
        super::topology_reader::terminate_self(9);
    };
    let input_end = hard_end.min(started + Duration::from_secs(5));
    // CleanupState may start its fixed three-second observation after execution
    // ends. Reserve it and the final protocol flush inside the outer deadline.
    let Some(backend_end) = hard_end.checked_sub(Duration::from_millis(3250)) else {
        super::topology_reader::terminate_self(9);
    };
    if backend_end <= input_end {
        super::topology_reader::terminate_self(9);
    }

    let (tx, rx) = mpsc::channel();
    let (ack_tx, ack_rx) = mpsc::channel();
    let spawn = std::thread::Builder::new().name("windows-action-receiver".into()).spawn(move || {
        let result = receive_once(&expected_sha256, input_end, backend_end, &tx, ack_rx);
        let _ = tx.send(Event::Finished(result));
    });
    if spawn.is_err() {
        super::topology_reader::terminate_self(9);
    }

    let mut end = input_end;
    let mut admitted = false;
    loop {
        let Some(remaining) = end.checked_duration_since(Instant::now()) else {
            super::topology_reader::terminate_self(if admitted { 7 } else { 9 });
        };
        match rx.recv_timeout(remaining) {
            Ok(Event::Admitted) if Instant::now() < end => {
                admitted = true;
                end = hard_end;
                // This unbounded send cannot block the watchdog. The worker is
                // forbidden from spawning PowerShell until this record exists.
                if ack_tx.send(()).is_err() {
                    super::topology_reader::terminate_self(7);
                }
            }
            Ok(Event::Finished(Ok(()))) if Instant::now() < hard_end => super::topology_reader::terminate_self(0),
            Ok(Event::Finished(Err(_))) if Instant::now() < hard_end => super::topology_reader::terminate_self(if admitted { 7 } else { 9 }),
            _ => super::topology_reader::terminate_self(if admitted { 7 } else { 9 }),
        }
    }
}

#[cfg(windows)]
const MAX_BODY_BYTES: usize = 65_536;
#[cfg(windows)]
const MAX_HEADER_BYTES: usize = 256;
#[cfg(windows)]
const MAX_BUDGET_MS: u64 = 25_000;
#[cfg(windows)]
const MIN_BUDGET_MS: u64 = 9_000;
#[cfg(windows)]
const DEFAULT_BUDGET_MS: u64 = 25_000;
#[cfg(windows)]
const POWERSHELL_BOOTSTRAP: &str = "$ErrorActionPreference='Stop';try{$r=New-Object System.IO.StreamReader([Console]::OpenStandardInput(),[System.Text.Encoding]::ASCII,$false,1024,$true);$b=[Convert]::FromBase64String($r.ReadToEnd());$s=(New-Object System.Text.UTF8Encoding($false,$true)).GetString($b);$sb=[ScriptBlock]::Create($s)}catch{exit 9};try{&$sb;if($null -eq $LASTEXITCODE){exit 0};exit [int]$LASTEXITCODE}catch{exit 7}";

#[cfg(windows)]
enum Event {
    Admitted,
    Finished(Result<(), ReceiverError>),
}

#[cfg(windows)]
#[derive(Debug)]
struct ReceiverError;

#[cfg(windows)]
fn parse_arguments(args: &[String]) -> Option<(String, u64)> {
    let (hash, budget) = match args {
        [hash_flag, hash] if hash_flag == "--helper-sha256" => (hash, DEFAULT_BUDGET_MS),
        [hash_flag, hash, budget_flag, value] if hash_flag == "--helper-sha256" && budget_flag == "--budget-ms" => (hash, canonical_budget(value)?),
        _ => return None,
    };
    (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))).then(|| (hash.clone(), budget))
}

#[cfg(windows)]
fn canonical_budget(value: &str) -> Option<u64> {
    (value.len() <= 5 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<u64>().ok())
        .flatten()
        .filter(|budget| *budget >= MIN_BUDGET_MS && *budget <= MAX_BUDGET_MS && budget.to_string() == value)
}

#[cfg(windows)]
fn receive_once(
    expected_sha256: &str,
    input_end: Instant,
    backend_end: Instant,
    events: &std::sync::mpsc::Sender<Event>,
    ack: std::sync::mpsc::Receiver<()>,
) -> Result<(), ReceiverError> {
    use std::{ffi::OsString, io::Write, process::Command};
    use tune_core::collect::{PinnedWindowsExecutable, ProcessStop, StandaloneWindowsEnvironment, run_standalone_windows_bounded};

    if Instant::now() >= input_end {
        return Err(ReceiverError);
    }
    let image = std::env::current_exe().map_err(|_| ReceiverError)?;
    let _lease = PinnedWindowsExecutable::open(&image, expected_sha256, input_end).map_err(|_| ReceiverError)?;
    let nonce = nonce()?;
    let mut output = std::io::stdout().lock();
    writeln!(output, "KT2 READY {nonce}").map_err(|_| ReceiverError)?;
    output.flush().map_err(|_| ReceiverError)?;

    let request = read_request(&mut std::io::stdin().lock(), &nonce, input_end)?;
    std::str::from_utf8(&request.body).map_err(|_| ReceiverError)?;
    if Instant::now() >= input_end || backend_end <= Instant::now() {
        return Err(ReceiverError);
    }
    // The worker cannot start an action until the outer watchdog has durably
    // recorded admission and returned this acknowledgement.
    events.send(Event::Admitted).map_err(|_| ReceiverError)?;
    ack.recv().map_err(|_| ReceiverError)?;
    let admitted_at = Instant::now();
    if Instant::now() >= backend_end {
        emit_diagnostic("backend", "deadline_before_start", None, admitted_at.elapsed());
        return Err(ReceiverError);
    }
    let shell = match powershell_path() {
        Ok(shell) => shell,
        Err(_) => {
            emit_diagnostic("setup", "powershell_unavailable", None, admitted_at.elapsed());
            return Err(ReceiverError);
        }
    };
    let mut command = Command::new(shell);
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", POWERSHELL_BOOTSTRAP]);
    let environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    if environment.is_empty() {
        emit_diagnostic("setup", "environment_empty", None, admitted_at.elapsed());
        return Err(ReceiverError);
    }
    let encoded = base64(&request.body);
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => {
            emit_diagnostic("setup", "runtime_failed", None, admitted_at.elapsed());
            return Err(ReceiverError);
        }
    };
    let result = runtime.block_on(run_standalone_windows_bounded(
        &command,
        StandaloneWindowsEnvironment::new(&environment),
        &encoded,
        backend_end,
        MAX_BODY_BYTES,
        MAX_BODY_BYTES,
    ));
    let accepted = result.code.is_some_and(|code| matches!(code, 0 | 3 | 4 | 5 | 6 | 7 | 8 | 9))
        && Instant::now() < backend_end
        && !result.timed_out
        && !result.output_limited
        && !result.spawn_failed
        && result.process_receipt.as_ref().is_some_and(|receipt| receipt.stop == ProcessStop::Completed && receipt.cleanup_confirmed());
    if !accepted {
        emit_diagnostic("backend", backend_outcome(&result, backend_end), Some(&result), admitted_at.elapsed());
        return Err(ReceiverError);
    }
    let code = result.code.expect("accepted result has an exit code");
    if writeln!(output, "KT2 RESULT {} {} {} {} {}", nonce, request.sha256, code, result.stdout.len(), result.stderr.len()).is_err() {
        emit_diagnostic("terminal", "stdout_header_write_failed", Some(&result), admitted_at.elapsed());
        return Err(ReceiverError);
    }
    if output.write_all(&result.stdout).is_err() || output.write_all(&result.stderr).is_err() || output.flush().is_err() {
        emit_diagnostic("terminal", "stdout_payload_write_failed", Some(&result), admitted_at.elapsed());
        return Err(ReceiverError);
    }
    Ok(())
}

#[cfg(windows)]
fn backend_outcome(result: &tune_core::collect::BoundedResult, backend_end: Instant) -> &'static str {
    if Instant::now() >= backend_end {
        "deadline_elapsed"
    } else if result.timed_out {
        "timed_out"
    } else if result.output_limited {
        "output_limited"
    } else if result.spawn_failed {
        "spawn_failed"
    } else if result.code.is_none() {
        "missing_exit_code"
    } else if !result.code.is_some_and(|code| matches!(code, 0 | 3 | 4 | 5 | 6 | 7 | 8 | 9)) {
        "unexpected_exit_code"
    } else {
        "receipt_unconfirmed"
    }
}

#[cfg(windows)]
fn emit_diagnostic(stage: &str, outcome: &str, result: Option<&tune_core::collect::BoundedResult>, elapsed: std::time::Duration) {
    use std::io::Write;
    use tune_core::collect::ProcessStop;

    let (timed_out, output_limited, spawn_failed, receipt_stop, cleanup_confirmed) = match result {
        Some(result) => {
            let receipt = result.process_receipt.as_ref();
            (
                result.timed_out,
                result.output_limited,
                result.spawn_failed,
                receipt.map_or("absent", |receipt| match receipt.stop {
                    ProcessStop::SetupFailed => "setup_failed",
                    ProcessStop::SpawnFailed => "spawn_failed",
                    ProcessStop::Completed => "completed",
                    ProcessStop::Timeout => "timeout",
                    ProcessStop::OutputLimit => "output_limit",
                    ProcessStop::LingeringDescendants => "lingering_descendants",
                    ProcessStop::IoFailure => "io_failure",
                    ProcessStop::CleanupUnconfirmed => "cleanup_unconfirmed",
                }),
                receipt.is_some_and(tune_core::collect::ProcessReceipt::cleanup_confirmed),
            )
        }
        None => (false, false, false, "absent", false),
    };
    // This runs only in the worker. A blocked stderr may prevent Finished, so
    // a watchdog deadline can leave no diagnostic line. The main watchdog
    // retains its deadline and terminates independently.
    let _ = std::io::stderr().lock().write_all(
        diagnostic_line(Diagnostic { stage, outcome, timed_out, output_limited, spawn_failed, receipt_stop, cleanup_confirmed, elapsed }).as_bytes(),
    );
}

#[cfg(windows)]
struct Request {
    sha256: String,
    body: Vec<u8>,
}

#[cfg(windows)]
fn read_request(input: &mut impl std::io::Read, nonce: &str, deadline: Instant) -> Result<Request, ReceiverError> {
    let header = read_header(input, deadline)?;
    let fields: Vec<_> = header.split(' ').collect();
    let [prefix, kind, request_nonce, count, sha256] = fields.as_slice() else {
        return Err(ReceiverError);
    };
    if *prefix != "KT2" || *kind != "REQUEST" || *request_nonce != nonce || !valid_sha256(sha256) {
        return Err(ReceiverError);
    }
    let count = canonical_count(count)?;
    let mut body = vec![0; count];
    input.read_exact(&mut body).map_err(|_| ReceiverError)?;
    let mut trailing = [0];
    if input.read(&mut trailing).map_err(|_| ReceiverError)? != 0 || Instant::now() >= deadline || tune_core::db::sha256_hex(&body) != *sha256 {
        return Err(ReceiverError);
    }
    Ok(Request { sha256: (*sha256).to_owned(), body })
}

#[cfg(windows)]
fn read_header(input: &mut impl std::io::Read, deadline: Instant) -> Result<String, ReceiverError> {
    let mut header = Vec::with_capacity(MAX_HEADER_BYTES);
    while header.len() < MAX_HEADER_BYTES {
        if Instant::now() >= deadline {
            return Err(ReceiverError);
        }
        let mut byte = [0];
        input.read_exact(&mut byte).map_err(|_| ReceiverError)?;
        if byte[0] == b'\n' {
            return String::from_utf8(header).map_err(|_| ReceiverError);
        }
        if !byte[0].is_ascii() || byte[0] == b'\r' {
            return Err(ReceiverError);
        }
        header.push(byte[0]);
    }
    Err(ReceiverError)
}

#[cfg(windows)]
fn canonical_count(value: &str) -> Result<usize, ReceiverError> {
    (value.len() <= 5 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<usize>().ok())
        .flatten()
        .filter(|count| *count > 0 && *count <= MAX_BODY_BYTES && count.to_string() == value)
        .ok_or(ReceiverError)
}

#[cfg(windows)]
fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(windows)]
fn base64(bytes: &[u8]) -> Vec<u8> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        encoded.extend([
            TABLE[(first >> 2) as usize],
            TABLE[((first & 0x03) << 4 | second >> 4) as usize],
            TABLE[((second & 0x0f) << 2 | third >> 6) as usize],
        ]);
        encoded.push(if chunk.len() > 2 { TABLE[(third & 0x3f) as usize] } else { b'=' });
        if chunk.len() == 1 {
            let index = encoded.len() - 2;
            encoded[index] = b'=';
        }
    }
    encoded
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn nonce() -> Result<String, ReceiverError> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};

    let mut bytes = [0u8; 32];
    // SAFETY: CNG writes exactly the fixed local buffer with the system RNG.
    let status = unsafe { BCryptGenRandom(null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    (status >= 0).then(|| bytes.iter().map(|byte| format!("{byte:02x}")).collect()).ok_or(ReceiverError)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn powershell_path() -> Result<std::path::PathBuf, ReceiverError> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = [0u16; 32_768];
    // SAFETY: fixed writable UTF-16 buffer for the Windows system directory.
    let count = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if count == 0 || count >= buffer.len() {
        return Err(ReceiverError);
    }
    let path = std::path::PathBuf::from(OsString::from_wide(&buffer[..count])).join("WindowsPowerShell").join("v1.0").join("powershell.exe");
    path.is_file().then_some(path).ok_or(ReceiverError)
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    use std::{io::Cursor, time::Duration};

    #[test]
    fn arguments_require_the_caller_supplied_lowercase_sha256() {
        let hash = "a".repeat(64);
        assert_eq!(parse_arguments(&["--helper-sha256".into(), hash.clone()]), Some((hash.clone(), DEFAULT_BUDGET_MS)));
        assert_eq!(parse_arguments(&["--helper-sha256".into(), hash.clone(), "--budget-ms".into(), "9000".into()]), Some((hash.clone(), 9_000)));
        assert_eq!(parse_arguments(&["--helper-sha256".into(), hash.clone(), "--budget-ms".into(), "25000".into()]), Some((hash, 25_000)));
        for args in [
            vec!["--helper-sha256".into(), "A".repeat(64)],
            vec!["--helper-sha256".into(), "a".repeat(63)],
            vec!["--helper-sha256".into(), "a".repeat(64), "--budget-ms".into(), "8999".into()],
            vec!["--helper-sha256".into(), "a".repeat(64), "--budget-ms".into(), "025".into()],
            vec!["--helper-sha256".into(), "a".repeat(64), "--budget-ms".into(), "25001".into()],
        ] {
            assert_eq!(parse_arguments(&args), None);
        }
    }

    #[test]
    fn request_accepts_exact_body_and_eof_only() {
        let body = b"exit 3";
        let hash = tune_core::db::sha256_hex(body);
        let frame = format!("KT2 REQUEST nonce {} {hash}\n", body.len());
        let request = read_request(&mut Cursor::new([frame.as_bytes(), body].concat()), "nonce", Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(request.sha256, hash);
        assert_eq!(request.body, body);

        for frame in [
            format!("KT2 REQUEST other {} {}\n", body.len(), tune_core::db::sha256_hex(body)),
            format!("KT2 REQUEST nonce 0 {}\n", tune_core::db::sha256_hex(body)),
            format!("KT2 REQUEST nonce 07 {}\n", tune_core::db::sha256_hex(body)),
            format!("KT2 REQUEST nonce {} {}\n", body.len(), "0".repeat(64)),
        ] {
            assert!(read_request(&mut Cursor::new([frame.as_bytes(), body].concat()), "nonce", Instant::now() + Duration::from_secs(1)).is_err());
        }
        let mut extra = [format!("KT2 REQUEST nonce {} {}\n", body.len(), tune_core::db::sha256_hex(body)).as_bytes(), body, b"x"].concat();
        assert!(read_request(&mut Cursor::new(&mut extra), "nonce", Instant::now() + Duration::from_secs(1)).is_err());
    }

    #[test]
    fn base64_keeps_utf8_body_as_bounded_ascii_stdin() {
        assert_eq!(base64(b"f"), b"Zg==");
        assert_eq!(base64(b"fo"), b"Zm8=");
        assert_eq!(base64(b"foo"), b"Zm9v");
        assert_eq!(base64("日本語".as_bytes()), b"5pel5pys6Kqe");
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn diagnostic_line_uses_only_fixed_fields_and_elapsed_buckets() {
        assert_eq!(
            diagnostic_line(Diagnostic {
                stage: "backend",
                outcome: "output_limited",
                timed_out: false,
                output_limited: true,
                spawn_failed: false,
                receipt_stop: "completed",
                cleanup_confirmed: true,
                elapsed: Duration::from_secs(5),
            }),
            "KT2 DIAG stage=backend outcome=output_limited timed_out=0 output_limited=1 spawn_failed=0 receipt_stop=completed cleanup_confirmed=1 elapsed=5_8s\n"
        );
        assert!(
            diagnostic_line(Diagnostic {
                stage: "backend",
                outcome: "timed_out",
                timed_out: true,
                output_limited: false,
                spawn_failed: false,
                receipt_stop: "timeout",
                cleanup_confirmed: false,
                elapsed: Duration::from_secs(8),
            })
            .ends_with("elapsed=8s_plus\n")
        );
    }
}
