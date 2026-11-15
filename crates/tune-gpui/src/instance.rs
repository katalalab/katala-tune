//! Bounded local activation IPC. Ownership is acquired before Engine/DB startup.
//! This protocol can only show a window; it cannot authorize collection or OS effects.
use crate::startup::resolved;
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    io::{Read, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[cfg(target_os = "macos")]
#[path = "instance/unix.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "instance/windows.rs"]
mod platform;

#[cfg(any(target_os = "windows", test))]
mod pipe_accept;

const MAX_MESSAGE: usize = 1024;
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Request {
    pub open: Option<String>,
    pub reply: mpsc::Sender<Result<(), String>>,
}
pub struct Guard {
    pub requests: mpsc::Receiver<Request>,
    stop: Arc<AtomicBool>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        // The worker retains OS ownership until bounded IO completes and cleans
        // its own endpoint. Never wait on GPUI/main-thread callbacks in Drop.
        self.stop.store(true, Ordering::Release);
    }
}
pub enum Claim {
    Primary(Guard),
    Forwarded,
}

fn namespace(config: &Path, data: &Path) -> Result<String, String> {
    let mut hash = Sha256::new();
    let user = platform::user()?;
    let executable = resolved(&std::env::current_exe().map_err(|e| e.to_string())?)?;
    let config = resolved(config)?;
    let data = resolved(data)?;
    for value in [
        b"katala-tune-gpui-preview-v1".as_slice(),
        user.as_bytes(),
        executable.as_os_str().as_encoded_bytes(),
        config.as_os_str().as_encoded_bytes(),
        data.as_os_str().as_encoded_bytes(),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
fn validate(open: Option<&str>) -> Result<(), String> {
    if open.is_some_and(|v| v.is_empty() || v.len() > 256 || v.starts_with('-') || v.chars().any(char::is_control)) {
        return Err("invalid activation route".into());
    }
    Ok(())
}
fn encode(namespace: &str, open: Option<&str>) -> Result<Vec<u8>, String> {
    validate(open)?;
    Ok(serde_json::to_vec(&serde_json::json!({"version":1,"namespace":namespace,"open":open})).unwrap())
}
fn decode(bytes: &[u8], namespace: &str) -> Result<Option<String>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| "malformed activation request")?;
    let object = value.as_object().ok_or("activation object required")?;
    if object.len() != 3 || value["version"] != 1 || value["namespace"].as_str() != Some(namespace) || !object.contains_key("open") {
        return Err("foreign or unsupported activation request".into());
    }
    let open = match &value["open"] {
        serde_json::Value::Null => None,
        serde_json::Value::String(v) => Some(v.clone()),
        _ => return Err("invalid activation route".into()),
    };
    validate(open.as_deref())?;
    Ok(open)
}
fn read_frame(stream: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).map_err(|_| "activation read failed")?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE {
        return Err("activation frame exceeds limit".into());
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).map_err(|_| "incomplete activation frame")?;
    Ok(bytes)
}
fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    stream.write_all(&(bytes.len() as u32).to_le_bytes()).and_then(|()| stream.write_all(bytes)).map_err(|_| "activation write failed".into())
}

pub fn claim(config: &Path, data: &Path, open: Option<&str>, review: bool) -> Result<Claim, String> {
    claim_named(namespace(config, data)?, open, review)
}
fn claim_named(namespace: String, open: Option<&str>, review: bool) -> Result<Claim, String> {
    let request = encode(&namespace, open)?;
    match platform::claim(&namespace)? {
        platform::Claim::Secondary => {
            if review {
                return Err("--review is not supported on a secondary launch; open the review in the existing window or use a distinct --data namespace".into());
            }
            let mut stream = platform::connect(&namespace)?;
            write_frame(&mut stream, &request)?;
            let answer = read_frame(&mut stream)?;
            let result: Result<(), String> = serde_json::from_slice(&answer).map_err(|_| "invalid activation acknowledgement")?;
            let _ = stream.write_all(&[1]);
            result?;
            Ok(Claim::Forwarded)
        }
        platform::Claim::Primary(listener) => {
            let (send, requests) = mpsc::sync_channel(8);
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            std::thread::Builder::new()
                .name("preview-activation".into())
                .spawn(move || {
                    while !stopping.load(Ordering::Acquire) {
                        let mut stream = match listener.accept() {
                            Ok(Some(s)) => s,
                            Ok(None) => {
                                std::thread::sleep(Duration::from_millis(25));
                                continue;
                            }
                            // Keep ownership even if activation becomes unavailable.
                            // A transport error must never permit a second Engine.
                            Err(_) => {
                                std::thread::sleep(Duration::from_millis(100));
                                continue;
                            }
                        };
                        let result = read_frame(&mut stream).and_then(|bytes| decode(&bytes, &namespace)).and_then(|open| {
                            let (reply, response) = mpsc::channel();
                            send.try_send(Request { open, reply }).map_err(|_| "activation unavailable")?;
                            let deadline = std::time::Instant::now() + ACK_TIMEOUT;
                            loop {
                                if stopping.load(Ordering::Acquire) {
                                    return Err("application is exiting".into());
                                }
                                match response.recv_timeout(Duration::from_millis(25)) {
                                    Ok(result) => return result,
                                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err("activation cancelled".into()),
                                    Err(_) if std::time::Instant::now() >= deadline => return Err("activation acknowledgement timed out".into()),
                                    Err(_) => {}
                                }
                            }
                        });
                        if write_frame(&mut stream, &serde_json::to_vec(&result).unwrap()).is_ok() {
                            // Windows DisconnectNamedPipe discards unread bytes; client
                            // confirms consuming the bounded reply before disconnection.
                            stream.read_budget(IO_TIMEOUT);
                            for _ in 0..MAX_MESSAGE {
                                let mut ack = [0];
                                if stream.read_exact(&mut ack).is_err() || ack[0] == 1 {
                                    break;
                                }
                            }
                        }
                    }
                    // Listener drops its own socket/pipe before releasing the lock.
                })
                .map_err(|e| e.to_string())?;
            Ok(Claim::Primary(Guard { requests, stop }))
        }
    }
}

#[cfg(test)]
#[path = "instance/tests.rs"]
mod tests;
