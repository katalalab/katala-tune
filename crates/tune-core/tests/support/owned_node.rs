//! Test-only capture for an exact Node fixture, never a shell or a host command.
//! One absolute execution+cleanup budget covers spawn, stdio and runtime teardown.
use std::{
    ffi::OsString,
    process::{ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    task::JoinHandle,
};
#[derive(Clone, Copy)]
pub struct Limits {
    pub execution: Duration,
    pub cleanup: Duration,
    pub stdout: usize,
    pub stderr: usize,
}
#[derive(Debug)]
pub struct Failure {
    pub kind: &'static str,
    pub root_exited: bool,
    pub readers_finished: bool,
}
impl Failure {
    pub fn cleanup_resolved(&self) -> bool {
        self.root_exited && self.readers_finished
    }
}
#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
#[derive(Clone, Copy)]
struct Deadlines {
    execute: Instant,
    cleanup: Instant,
}
struct Readers {
    out: Option<JoinHandle<Result<Vec<u8>, &'static str>>>,
    err: Option<JoinHandle<Result<Vec<u8>, &'static str>>>,
}
impl Drop for Readers {
    fn drop(&mut self) {
        if let Some(t) = &self.out {
            t.abort();
        }
        if let Some(t) = &self.err {
            t.abort();
        }
    }
}
fn read<R: AsyncRead + Unpin + Send + 'static>(mut pipe: R, limit: usize) -> JoinHandle<Result<Vec<u8>, &'static str>> {
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = pipe.read(&mut chunk).await.map_err(|_| "reader_error")?;
            if n == 0 {
                return Ok(bytes);
            }
            if n > limit.saturating_sub(bytes.len()) {
                return Err("output_limit");
            }
            bytes.extend_from_slice(&chunk[..n]);
        }
    })
}
async fn harvest<T>(task: &mut Option<JoinHandle<Result<T, &'static str>>>) -> Option<Result<T, &'static str>> {
    if task.as_ref().is_some_and(JoinHandle::is_finished) { Some(task.take().unwrap().await.unwrap_or(Err("task_error"))) } else { None }
}
fn finished<T>(task: &Option<JoinHandle<T>>) -> bool {
    task.as_ref().is_none_or(JoinHandle::is_finished)
}
pub fn capture_node(args: &[OsString], limits: Limits) -> Result<Output, Failure> {
    capture(args, limits, false)
}
struct RuntimeOwner {
    runtime: Option<tokio::runtime::Runtime>,
    deadline: Instant,
}
impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(self.deadline.saturating_duration_since(Instant::now()));
        }
    }
}
fn capture(args: &[OsString], limits: Limits, held_reader: bool) -> Result<Output, Failure> {
    let start = Instant::now();
    let execute = start + limits.execution;
    let deadlines = Deadlines { execute, cleanup: execute + limits.cleanup };
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(Failure { kind: "nested_runtime", root_exited: false, readers_finished: true });
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| Failure {
        kind: "runtime_setup",
        root_exited: false,
        readers_finished: true,
    })?;
    let owner = RuntimeOwner { runtime: Some(runtime), deadline: deadlines.cleanup };
    let result = owner.runtime.as_ref().unwrap().block_on(async {
        let mut command = Command::new("node");
        command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| Failure { kind: "spawn_failed", root_exited: false, readers_finished: true })?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        // Held in-memory writer models missing EOF without spawning a descendant.
        let (reader, held_writer) = tokio::io::duplex(16);
        let out = if held_reader {
            drop(stdout);
            read(Box::new(reader) as Box<dyn AsyncRead + Unpin + Send>, limits.stdout)
        } else {
            drop(reader);
            read(Box::new(stdout) as Box<dyn AsyncRead + Unpin + Send>, limits.stdout)
        };
        let readers = Readers { out: Some(out), err: Some(read(stderr, limits.stderr)) };
        let result = observe(&mut child, readers, deadlines).await;
        drop(held_writer);
        result
    });
    // Windows process pipes may have blocking-pool reads. Never invoke unbounded
    // Runtime::drop; aborting a task is not proof that its underlying OS I/O ended.
    drop(owner);
    result
}
async fn observe(child: &mut Child, mut readers: Readers, deadlines: Deadlines) -> Result<Output, Failure> {
    let (mut status, mut stdout, mut stderr): (Option<ExitStatus>, Option<Vec<u8>>, Option<Vec<u8>>) = (None, None, None);
    let mut failure = None;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => status = Some(s),
            Ok(None) => {}
            Err(_) => failure = Some("child_observation"),
        }
        if let Some(result) = harvest(&mut readers.out).await {
            match result {
                Ok(x) => stdout = Some(x),
                Err(e) => failure = Some(e),
            }
        }
        if let Some(result) = harvest(&mut readers.err).await {
            match result {
                Ok(x) => stderr = Some(x),
                Err(e) => failure = Some(e),
            }
        }
        if status.is_some_and(|s| !s.success()) {
            failure.get_or_insert("child_exit");
        }
        if let Some(kind) = failure {
            break failure_cleanup(child, &readers, status.is_some(), kind, deadlines).await;
        }
        if Instant::now() < deadlines.execute
            && let (Some(_), Some(out), Some(err)) = (&status, &mut stdout, &mut stderr)
        {
            return Ok(Output { stdout: std::mem::take(out), stderr: std::mem::take(err) });
        }
        if Instant::now() >= deadlines.execute {
            let kind = if status.is_none() { "child_timeout" } else { "reader_timeout" };
            break failure_cleanup(child, &readers, status.is_some(), kind, deadlines).await;
        }
        tokio::time::sleep(Duration::from_millis(10).min(deadlines.execute.saturating_duration_since(Instant::now()))).await;
    }
}
async fn failure_cleanup(child: &mut Child, readers: &Readers, mut exited: bool, kind: &'static str, deadlines: Deadlines) -> Result<Output, Failure> {
    if !exited {
        let _ = child.start_kill();
    }
    // Give owned pipes a chance to reach real EOF after kill. Do not count abort
    // completion as EOF, and never wait()/join() on an unconfirmed child/task.
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            exited = true;
        }
        let readers_finished = finished(&readers.out) && finished(&readers.err);
        if (exited && readers_finished) || Instant::now() >= deadlines.cleanup {
            return Err(Failure { kind, root_exited: exited, readers_finished });
        }
        tokio::time::sleep(Duration::from_millis(10).min(deadlines.cleanup.saturating_duration_since(Instant::now()))).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> Limits {
        Limits { execution: Duration::from_secs(2), cleanup: Duration::from_secs(1), stdout: 1024, stderr: 1024 }
    }
    fn args(code: &str) -> Vec<OsString> {
        vec!["-e".into(), code.into()]
    }
    #[test]
    fn fixture_runtime_unwind_does_not_wait_for_unresolved_blocking_work() {
        let start = Instant::now();
        let panic = std::panic::catch_unwind(|| {
            let owner = RuntimeOwner {
                runtime: Some(tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()),
                deadline: Instant::now() + Duration::from_millis(100),
            };
            owner.runtime.as_ref().unwrap().block_on(async {
                let (tx, rx) = tokio::sync::oneshot::channel();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(());
                    std::thread::sleep(Duration::from_millis(800));
                });
                rx.await.unwrap();
                panic!("controlled fixture unwind");
            });
        });
        assert!(panic.is_err());
        assert!(start.elapsed() < Duration::from_millis(500), "runtime Drop waited for blocking work beyond common deadline");
    }
    #[test]
    fn fixture_capture_accepts_complete_output_and_root_exit() {
        let output = capture_node(&args("process.stdout.write('ok');process.stderr.write('diagnostic')"), limits()).unwrap();
        assert_eq!(output.stdout, b"ok");
        assert_eq!(output.stderr, b"diagnostic");
    }
    #[test]
    fn fixture_capture_root_exit_does_not_certify_incomplete_json() {
        let out = capture_node(&args("process.stdout.write('{')"), limits()).unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&out.stdout).is_err());
    }
    #[test]
    fn fixture_capture_hang_and_each_output_limit_fail_with_bounded_cleanup() {
        for (code, reason) in [
            ("setInterval(()=>{},1000)", "child_timeout"),
            ("process.stdout.write('x'.repeat(2048));setInterval(()=>{},1000)", "output_limit"),
            ("process.stderr.write('x'.repeat(2048));setInterval(()=>{},1000)", "output_limit"),
            ("process.exit(7)", "child_exit"),
        ] {
            let start = Instant::now();
            let err = capture_node(&args(code), limits()).unwrap_err();
            assert_eq!(err.kind, reason);
            assert!(err.cleanup_resolved());
            assert!(start.elapsed() < Duration::from_secs(5));
        }
    }
    #[test]
    fn fixture_capture_exited_root_with_unfinished_reader_cannot_pass_or_hang() {
        let start = Instant::now();
        let err = capture(&args(""), limits(), true).unwrap_err();
        assert_eq!(err.kind, "reader_timeout");
        assert!(err.root_exited);
        assert!(!err.readers_finished);
        assert!(!err.cleanup_resolved());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
