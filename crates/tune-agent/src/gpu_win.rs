//! Keep NVML initialized between reads. A stalled driver call can be killed without
//! blocking the sampler or leaving an unjoinable thread inside it.

use crate::sample::{GpuQuery, parse_gpu_query};
use serde_json::{Value, json};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::runtime::Runtime;

type Pipes = (ChildStdin, BufReader<ChildStdout>);
type Read = Pin<Box<dyn Future<Output = Option<(Pipes, String)>> + Send>>;

struct Native {
    rt: Runtime,
    child: Child,
    _job: std::os::windows::io::OwnedHandle,
    pipes: Option<Pipes>,
    read: Option<Read>,
    started: Option<Instant>,
    rows: Option<Value>,
    sampled: Option<Instant>,
}

/// Killing the sampler also kills a helper wedged inside the driver.
#[allow(unsafe_code)]
fn own_worker(child: &Child) -> Option<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    // SAFETY: unnamed non-inheritable job, owned by this sampler alone.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return None;
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let process = child.raw_handle()?;
    // SAFETY: handles remain valid throughout; info has the required size and layout.
    unsafe {
        if SetInformationJobObject(job.as_raw_handle(), JobObjectExtendedLimitInformation, (&raw const info).cast(), std::mem::size_of_val(&info) as u32) == 0
            || AssignProcessToJobObject(job.as_raw_handle(), process) == 0
        {
            return None;
        }
    }
    Some(job)
}

impl Native {
    fn open() -> Option<Self> {
        let mut command = tokio::process::Command::new(std::env::current_exe().ok()?);
        command.arg("gpu-query-nvml");
        Self::spawn(command)
    }

    fn spawn(mut command: tokio::process::Command) -> Option<Self> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
        let mut child = {
            let _enter = rt.enter();
            command
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x0800_0000)
                .kill_on_drop(true)
                .spawn()
                .ok()?
        };
        let job = own_worker(&child)?;
        let pipes = (child.stdin.take()?, BufReader::new(child.stdout.take()?));
        Some(Self { rt, child, _job: job, pipes: Some(pipes), read: None, started: None, rows: None, sampled: None })
    }

    fn poll(&mut self, interval: Duration) -> Option<()> {
        if self.read.is_none() && self.started.is_none_or(|t| t.elapsed() >= interval) {
            let (mut input, mut output) = self.pipes.take()?;
            self.started = Some(Instant::now());
            self.read = Some(Box::pin(async move {
                input.write_all(&[1]).await.ok()?;
                let mut line = Vec::new();
                (&mut output).take(65537).read_until(b'\n', &mut line).await.ok()?;
                if line.len() > 65536 || line.last() != Some(&b'\n') {
                    return None;
                }
                Some(((input, output), serde_json::from_slice(&line).ok()?))
            }));
        }
        if let Some(read) = self.read.as_mut() {
            match self.rt.block_on(async { tokio::time::timeout(Duration::from_millis(1), read).await }) {
                Ok(result) => {
                    let (pipes, text) = result?;
                    let mut rows = parse_gpu_query(&text, tune_link::now_ms());
                    if rows.is_empty() {
                        return None;
                    }
                    for row in &mut rows {
                        row["source"] = "NVML (MiB)".into();
                    }
                    self.rows = Some(Value::Array(rows));
                    self.sampled = Some(Instant::now());
                    self.pipes = Some(pipes);
                    self.read = None;
                }
                Err(_) if self.started?.elapsed() >= Duration::from_secs(4) => return None,
                Err(_) => {}
            }
        }
        Some(())
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        self.read = None;
        self.pipes = None;
        let _ = self.child.start_kill();
        let _ = self.rt.block_on(async { tokio::time::timeout(Duration::from_millis(200), self.child.wait()).await });
    }
}

pub struct WindowsGpu {
    native: Option<Native>,
    smi_path: PathBuf,
    fallback: Option<GpuQuery>,
    every: f64,
}

impl WindowsGpu {
    pub fn open(smi_path: PathBuf, every: f64) -> Self {
        Self { native: Native::open(), smi_path, fallback: None, every }
    }

    pub fn poll(&mut self) {
        let interval = Duration::from_secs_f64(self.every.max(5.0));
        if let Some(native) = self.native.as_mut() {
            if native.poll(interval).is_some() {
                return;
            }
            self.native = None;
        }
        self.fallback.get_or_insert_with(|| GpuQuery::open(self.smi_path.clone(), self.every)).poll();
    }

    pub fn json(&self) -> Option<Value> {
        if let Some(legacy) = &self.fallback {
            return legacy.json();
        }
        if let Some(native) = &self.native
            && native.sampled.is_some_and(|t| t.elapsed().as_secs_f64() <= self.every.max(5.0) + 4.5)
        {
            return native.rows.clone();
        }
        Some(json!([{"name":"GPU","source":"NVML","memory_kind":"unknown","memory_status":"unavailable","memory_reason":"query-pending","available":false}]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    #[test]
    #[allow(unsafe_code)]
    fn stalled_worker_is_reaped_and_uses_the_legacy_provider() {
        let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = tokio::process::Command::new(powershell);
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 30"]);
        let native = Native::spawn(command).unwrap();
        // SAFETY: duplicate the live handle before moving ownership into the sampler.
        let process = unsafe { BorrowedHandle::borrow_raw(native.child.raw_handle().unwrap()) }.try_clone_to_owned().unwrap();
        let mut gpu = WindowsGpu { native: Some(native), smi_path: "missing-sensor.exe".into(), every: 5.0, fallback: None };
        gpu.poll();
        let native = gpu.native.as_mut().unwrap();
        assert!(native.read.is_some());
        native.started = Some(Instant::now() - Duration::from_secs(5));
        gpu.poll();
        assert!(gpu.native.is_none() && gpu.fallback.is_some());
        assert_eq!(gpu.json().unwrap()[0]["available"], false);
        // SAFETY: process is an owned handle to the fixture, with bounded wait.
        assert_eq!(unsafe { WaitForSingleObject(process.as_handle().as_raw_handle(), 2000) }, WAIT_OBJECT_0);
    }
}
