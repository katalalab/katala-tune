//! Dedicated one-shot Windows action receiver. It runs before application setup.
use std::{process::ExitCode, time::Instant};

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
    if Instant::now() >= backend_end {
        return Err(ReceiverError);
    }
    let shell = powershell_path()?;
    let mut command = Command::new(shell);
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", POWERSHELL_BOOTSTRAP]);
    let environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    if environment.is_empty() {
        return Err(ReceiverError);
    }
    let encoded = base64(&request.body);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| ReceiverError)?;
    let result = runtime.block_on(run_standalone_windows_bounded(
        &command,
        StandaloneWindowsEnvironment::new(&environment),
        &encoded,
        backend_end,
        MAX_BODY_BYTES,
        MAX_BODY_BYTES,
    ));
    let code = result.code.ok_or(ReceiverError)?;
    if Instant::now() >= backend_end
        || result.timed_out
        || result.output_limited
        || result.spawn_failed
        || !matches!(code, 0 | 3 | 4 | 5 | 6 | 7 | 8 | 9)
        || !result.process_receipt.as_ref().is_some_and(|receipt| receipt.stop == ProcessStop::Completed && receipt.cleanup_confirmed())
    {
        return Err(ReceiverError);
    }
    writeln!(output, "KT2 RESULT {} {} {} {} {}", nonce, request.sha256, code, result.stdout.len(), result.stderr.len()).map_err(|_| ReceiverError)?;
    output.write_all(&result.stdout).map_err(|_| ReceiverError)?;
    output.write_all(&result.stderr).map_err(|_| ReceiverError)?;
    output.flush().map_err(|_| ReceiverError)
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
