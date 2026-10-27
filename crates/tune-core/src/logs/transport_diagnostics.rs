//! Test-only closed observations for the log runner.
use std::future::Future;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Stage {
    Unknown = 0,
    Exit = 1,
    StdoutEncoding = 2,
    Deadline = 3,
    Spawn = 4,
    Write = 5,
    Shutdown = 6,
    StdoutRead = 7,
    StderrRead = 8,
    Wait = 9,
    StderrEncoding = 10,
    Late = 11,
}
impl Stage {
    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Exit,
            2 => Self::StdoutEncoding,
            3 => Self::Deadline,
            4 => Self::Spawn,
            5 => Self::Write,
            6 => Self::Shutdown,
            7 => Self::StdoutRead,
            8 => Self::StderrRead,
            9 => Self::Wait,
            10 => Self::StderrEncoding,
            11 => Self::Late,
            _ => Self::Unknown,
        }
    }
}
#[derive(Default)]
pub(super) struct Trace {
    stage: AtomicU8,
    completed: AtomicU8,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub stage: Stage,
    pub completed: u8,
}
tokio::task_local! { static TRACE: Trace; }
pub(super) fn fail(stage: Stage) {
    let _ = TRACE.try_with(|trace| trace.stage.compare_exchange(0, stage as u8, Ordering::Relaxed, Ordering::Relaxed));
}
pub(super) fn completed(bit: u8) {
    let _ = TRACE.try_with(|trace| trace.completed.fetch_or(bit, Ordering::Relaxed));
}
pub(super) async fn capture<F: Future>(future: F) -> (F::Output, Snapshot) {
    TRACE
        .scope(Trace::default(), async {
            let result = future.await;
            let snapshot = TRACE
                .with(|trace| Snapshot { stage: Stage::from_code(trace.stage.load(Ordering::Relaxed)), completed: trace.completed.load(Ordering::Relaxed) });
            (result, snapshot)
        })
        .await
}

#[tokio::test]
async fn controlled_nonzero_exit_is_reported_without_output() {
    let mut cmd = crate::collect::command("node");
    cmd.args(["-e", "process.stdin.resume();process.stdin.on('end',()=>{process.stdout.write('{}');process.exitCode=7;});"]);
    let (result, snapshot) = capture(super::transport::run(cmd, b"synthetic", std::time::Duration::from_secs(5))).await;
    assert!(result.code.is_none() && result.out.is_empty());
    assert_eq!(snapshot.stage, Stage::Exit);
    assert_eq!(snapshot.completed, 15);
}

#[tokio::test]
async fn controlled_invalid_utf8_and_deadline_are_separate() {
    let mut invalid = crate::collect::command("node");
    invalid.args(["-e", "process.stdin.resume();process.stdin.on('end',()=>process.stdout.write(Buffer.from([255])));"]);
    let (result, snapshot) = capture(super::transport::run(invalid, b"synthetic", std::time::Duration::from_secs(5))).await;
    assert!(result.code.is_none() && result.out.is_empty());
    assert_eq!(snapshot.stage, Stage::StdoutEncoding);
    let mut stalled = crate::collect::command("node");
    stalled.args(["-e", "process.stdin.resume();setInterval(()=>{},1000);"]);
    let (result, snapshot) = capture(super::transport::run(stalled, b"synthetic", std::time::Duration::from_millis(500))).await;
    assert!(result.code.is_none() && result.out.is_empty());
    assert_eq!(snapshot.stage, Stage::Deadline);
}

#[tokio::test]
async fn controlled_output_caps_reject_without_payload() {
    for (stream, size, stage) in [("stdout", 4 * 1024 * 1024 + 1, Stage::StdoutRead), ("stderr", 64 * 1024 + 1, Stage::StderrRead)] {
        let mut cmd = crate::collect::command("node");
        let script = format!("process.stdin.resume();process.stdin.on('end',()=>process.{stream}.write(Buffer.alloc({size}))); ");
        cmd.args(["-e", &script]);
        let (result, snapshot) = capture(super::transport::run(cmd, b"synthetic", std::time::Duration::from_secs(5))).await;
        assert!(result.code.is_none() && result.out.is_empty());
        assert_eq!(snapshot.stage, stage);
    }
}

#[tokio::test]
async fn task_scopes_keep_different_exchanges_separate() {
    let mut success = crate::collect::command("node");
    success.args(["-e", "process.stdin.resume();process.stdin.on('end',()=>process.stdout.write('{}')); "]);
    let mut failed = crate::collect::command("node");
    failed.args(["-e", "process.stdin.resume();process.stdin.on('end',()=>process.exitCode=7);"]);
    let (good, bad) = tokio::join!(
        capture(super::transport::run(success, b"one", std::time::Duration::from_secs(5))),
        capture(super::transport::run(failed, b"two", std::time::Duration::from_secs(5)))
    );
    assert_eq!(good.0.code, Some(0));
    assert_eq!(good.1.stage, Stage::Unknown);
    assert_eq!(good.1.completed, 15);
    assert!(bad.0.code.is_none() && bad.0.out.is_empty());
    assert_eq!(bad.1.stage, Stage::Exit);
    assert_eq!(bad.1.completed, 15);
}
