pub mod docker;
pub(crate) mod docker_api;

pub mod github;
pub mod local;
pub mod native_execution;
pub mod native_protocol;
pub mod native_reader;
pub mod remote;
pub mod windows_reader;

pub use local::{assemble_capture, assemble_capture_cached, capture_local, capture_local_with_progress, capture_source_cache_keys};

use std::{future::Future, pin::Pin, time::Duration};

use crate::collect::{BoundedResult, run_bounded};

pub type CommandFuture<'a> = Pin<Box<dyn Future<Output = BoundedResult> + Send + 'a>>;

pub trait CommandRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        program: &'a str,
        args: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
        stdout_limit: usize,
        stderr_limit: usize,
    ) -> CommandFuture<'a>;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run<'a>(
        &'a self,
        program: &'a str,
        args: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
        stdout_limit: usize,
        stderr_limit: usize,
    ) -> CommandFuture<'a> {
        Box::pin(run_bounded(program, args, stdin, timeout, stdout_limit, stderr_limit))
    }
}

pub(crate) fn failure_from_process(output: &BoundedResult) -> Option<super::Failure> {
    use super::{Failure, FailureCode};
    if output.spawn_failed {
        return Some(Failure::new(FailureCode::ToolMissing, "transport executable unavailable"));
    }
    if output.timed_out {
        return Some(Failure::new(FailureCode::Timeout, "transport timed out"));
    }
    if output.output_limited {
        return Some(Failure::new(FailureCode::LimitReached, "transport output budget exceeded"));
    }
    if output.code == Some(0) {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    let code = if stderr.contains("rate limit") || stderr.contains("rate_limit") {
        FailureCode::RateLimited
    } else if stderr.contains("authentication")
        || stderr.contains("bad credentials")
        || stderr.contains("gh auth login")
        || stderr.contains("host key verification")
        || stderr.contains("permission denied (publickey")
    {
        FailureCode::AuthUnavailable
    } else if stderr.contains("permission denied") || stderr.contains("forbidden") || stderr.contains("not found") {
        FailureCode::PermissionDenied
    } else if stderr.contains("could not resolve")
        || stderr.contains("connection refused")
        || stderr.contains("no route")
        || stderr.contains("connection timed out")
    {
        FailureCode::Unreachable
    } else {
        FailureCode::Internal
    };
    Some(Failure::new(code, "transport did not complete"))
}
