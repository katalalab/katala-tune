//! Synthetic owned children only. The outer thread observes runtime destruction;
//! no private ledger, network transport or system collector is used.
use super::*;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::Instant,
};
use tune_core::{
    db::Store,
    live::{Opts, Route, Source},
};

// Explicit fake ownership for synthetic lifetime tests. Default hosts retain
// the actual OS-user claim; native ownership integration is a separate gate.
pub(super) fn with_fake_owner(mut host: MonitorHost) -> MonitorHost {
    host.collection_owner = Some(Arc::new(|| Ok(())));
    host
}

struct Refused;
impl Route for Refused {
    fn open(&self, _: &tune_core::nodes::Node, _: &Opts) -> Result<Source, String> {
        Err("synthetic route".into())
    }
}
fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "fixture boundary not reached: {}", path.display());
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn mark(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), []).unwrap();
}

// Re-exec just this test as an owned root with bounded synthetic waits.
#[test]
#[ignore = "owned child fixture, launched only by the shutdown harness"]
fn owned_child() {
    let Some(dir) = std::env::var_os("TUNE_SHUTDOWN_FIXTURE") else { return };
    let dir = PathBuf::from(dir);
    mark(&dir, "child-start");
    if dir.join("connect-gate").exists() {
        mark(&dir, "boundary");
        wait_file(&dir.join("release"));
    }
    mark(&dir, "connected");
    if dir.join("stdout-gate").exists() {
        mark(&dir, "boundary");
        wait_file(&dir.join("release"));
    }
    println!("synthetic payload");
    mark(&dir, "child-end");
}

fn close_boundary(phase: &'static str, completed_first: bool) {
    let dir = std::env::temp_dir().join(format!("gpui-owned-close-{}-{phase}-{completed_first}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    let count = if phase == "network" { 2 } else { 1 };
    for i in 0..count {
        let child_dir = dir.join(format!("child-{i}"));
        std::fs::create_dir_all(&child_dir).unwrap();
        if phase == "connect" || (phase == "network" && i == 1) {
            mark(&child_dir, "connect-gate");
        }
        if phase == "stdout" {
            mark(&child_dir, "stdout-gate");
        }
    }
    let (ready_send, ready_recv) = mpsc::channel();
    let (exit_send, exit_recv) = mpsc::channel();
    let (shutdown_send, shutdown_recv) = mpsc::channel();
    let (runtime_send, runtime_recv) = mpsc::channel();
    let worker_dir = dir.clone();
    let outer = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (engine, host, mut job) = rt.block_on(async {
            let engine = Engine::open(worker_dir.join("nodes.json"), worker_dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
            let live = Live::new(Arc::new(Refused), Opts::default(), Box::new(|_| {}));
            let host = with_fake_owner(MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false))));
            let mut job = Job::default();
            let work_engine = engine.clone();
            let work_dir = worker_dir.clone();
            let handle = Handle::current();
            let progress = job.progress.clone();
            host.start_manual_with(&mut job, Kind::Network(false), move || {
                mark(&work_dir, "worker-start");
                let receipt = handle.block_on(collection::run_network((0..count).map(|i| i.to_string()).collect(), false, progress, |id, _| {
                    let child_dir = work_dir.join(format!("child-{id}"));
                    let engine = work_engine.clone();
                    let dir = work_dir.clone();
                    async move {
                        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
                        command
                            .args(["--exact", "monitor_host::shutdown_tests::owned_child", "--ignored", "--nocapture"])
                            .env("TUNE_SHUTDOWN_FIXTURE", &child_dir)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .kill_on_drop(true);
                        let child = command.spawn().unwrap();
                        // wait_with_output reaps this actual root and observes pipe EOF.
                        let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output()).await.unwrap().unwrap();
                        assert!(output.status.success());
                        assert!(String::from_utf8_lossy(&output.stdout).contains("synthetic payload"));
                        mark(&child_dir, "root-waited");
                        if phase == "db" {
                            mark(&dir, "boundary");
                            wait_file(&dir.join("release"));
                        }
                        engine.with_db(|d| d.set_meta(&format!("saved-{id}"), &json!(true))).unwrap();
                        mark(&child_dir, "commit");
                        Ok(json!({"nodes":[{"node_id":id,"ok":true}]}))
                    }
                }));
                mark(&work_dir, "worker-end");
                receipt
            })
            .unwrap();
            (engine, host, job)
        });
        ready_send.send(()).unwrap();
        exit_recv.recv_timeout(Duration::from_secs(15)).unwrap();
        if completed_first {
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !host.state.lock().unwrap().manual.as_ref().unwrap().is_finished() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            });
        }
        host.request_exit();
        let report = rt.block_on(host.shutdown());
        assert_eq!(report.manual_complete, Some(true));
        assert_eq!(report.monitor_complete, None);
        assert_eq!(report.journal_ack, Some(true));
        assert_eq!(rt.block_on(host.shutdown()), report);
        let receipt_ready = job.poll();
        let receipt_success = job.result.as_ref().is_some_and(|r| r.complete == count && r.failed == 0);
        mark(&worker_dir, "shutdown-returned");
        shutdown_send.send((receipt_ready, receipt_success)).unwrap();
        drop((job, host, engine));
        drop(rt);
        mark(&worker_dir, "runtime-ended");
        runtime_send.send(()).unwrap();
    });
    ready_recv.recv_timeout(Duration::from_secs(10)).unwrap();
    let boundary = match phase {
        "db" => dir.join("boundary"),
        "network" => dir.join("child-1/boundary"),
        _ => dir.join("child-0/boundary"),
    };
    wait_file(&boundary);
    assert!(!dir.join("worker-end").exists());
    if phase == "network" {
        assert!(dir.join("child-0/commit").exists());
    }
    let release = || {
        mark(&dir, "release");
        for i in 0..count {
            mark(&dir.join(format!("child-{i}")), "release");
        }
    };
    if completed_first {
        release();
        wait_file(&dir.join("worker-end"));
        // The outer scope also waits for the host's task completion before Exit.
    }
    let began = Instant::now();
    exit_send.send(()).unwrap();
    let early = shutdown_recv.recv_timeout(Duration::from_millis(150)).ok();
    let runtime_early = runtime_recv.try_recv().is_ok();
    release();
    let receipt = early.unwrap_or_else(|| shutdown_recv.recv_timeout(Duration::from_secs(10)).unwrap());
    if !runtime_early {
        runtime_recv.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    outer.join().unwrap();
    let db = Store::open_read_only(&dir.join("data")).unwrap();
    for i in 0..count {
        assert_eq!(db.get_meta(&format!("saved-{i}")).unwrap(), Some(json!(true)));
        assert!(dir.join(format!("child-{i}/child-end")).exists());
        assert!(dir.join(format!("child-{i}/root-waited")).exists());
        assert!(dir.join(format!("child-{i}/commit")).exists());
    }
    drop(db);
    eprintln!(
        "phase={phase} completed_first={completed_first} shutdown_before_release={} receipt_ready={} receipt_success={} runtime_before_release={runtime_early} elapsed_ms={} child_end/root_waited/final_commit/runtime_end=true",
        early.is_some(),
        receipt.0,
        receipt.1,
        began.elapsed().as_millis()
    );
    std::fs::remove_dir_all(dir).unwrap();
    assert!(completed_first || early.is_none(), "shutdown returned before owned manual work ended at {phase}");
    assert!(receipt.0 && receipt.1, "shutdown must follow receipt delivery and the final commit");
}
#[test]
fn close_during_connect() {
    close_boundary("connect", false);
}
#[test]
fn close_during_stdout() {
    close_boundary("stdout", false);
}
#[test]
fn close_before_database_commit() {
    close_boundary("db", false);
}
#[test]
fn close_between_sequential_network_nodes() {
    close_boundary("network", false);
}
#[test]
fn close_after_completed_manual_job() {
    close_boundary("stdout", true);
}

fn host_fixture(label: &str) -> (PathBuf, Arc<Engine>, Arc<MonitorHost>) {
    let dir = std::env::temp_dir().join(format!("gpui-host-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
    let live = Live::new(Arc::new(Refused), Opts::default(), Box::new(|_| {}));
    let host = Arc::new(with_fake_owner(MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false)))));
    (dir, engine, host)
}

#[tokio::test]
async fn exit_rejects_queued_requests_and_drains_without_ui_receiver() {
    let (dir, engine, host) = host_fixture("closing");
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true}))).unwrap();
    assert!(host.ensure_ticker().is_ok());
    let mut job = Job::default();
    let (started_send, started_recv) = tokio::sync::oneshot::channel();
    let (release_send, release_recv) = mpsc::channel();
    let committed = engine.clone();
    host.start_manual_with(&mut job, Kind::Logs, move || {
        started_send.send(()).unwrap();
        release_recv.recv_timeout(Duration::from_secs(5)).unwrap();
        committed.with_db(|d| d.set_meta("manual-saved", &json!(true))).unwrap();
        Receipt::error(Kind::Logs, "synthetic result")
    })
    .unwrap();
    started_recv.await.unwrap();
    // A second UI receiver cannot evade the host's single-worker contract.
    assert!(host.start_manual_with(&mut Job::default(), Kind::Logs, || panic!("duplicate start")).is_err());
    drop(job);
    let queued_host = host.clone();
    let (queue_send, queue_recv) = tokio::sync::oneshot::channel();
    let queued = tokio::spawn(async move {
        queue_recv.await.unwrap();
        queued_host.start_monitor()
    });
    host.request_exit();
    queue_send.send(()).unwrap();
    assert!(queued.await.unwrap().is_err());
    assert!(host.start_manual_with(&mut Job::default(), Kind::Logs, || panic!("late start")).is_err());
    assert!(host.start_monitor().is_err());
    assert!(host.pause_monitor().is_err());
    assert!(host.request_pull().is_err());
    assert!(host.save_monitor_settings(json!({"enabled":false})).is_err());
    assert!(host.ensure_ticker().is_err());
    assert_eq!(host.snapshot().mode, RunMode::Exit);
    let drain_host = host.clone();
    let mut drain = tokio::spawn(async move { drain_host.shutdown().await });
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut drain).await.is_err());
    assert_eq!(engine.with_db(|d| d.get_meta("manual-saved")).unwrap(), None);
    release_send.send(()).unwrap();
    let report = drain.await.unwrap();
    assert_eq!(report.manual_complete, Some(true));
    assert!(report.confirmed());
    assert_eq!(host.shutdown().await, report);
    assert_eq!(engine.with_db(|d| d.get_meta("manual-saved")).unwrap(), Some(json!(true)));
    assert_eq!(engine.with_db(|d| d.get_meta("monitor")).unwrap(), Some(json!({"enabled":true})));
    assert!(host.state.lock().unwrap().ticker.is_none());
    drop((host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn manual_panic_is_an_unconfirmed_completion_with_a_failure_receipt() {
    let (dir, _, host) = host_fixture("panic");
    let mut job = Job::default();
    host.start_manual_with(&mut job, Kind::Logs, || panic!("synthetic manual failure")).unwrap();
    let report = host.shutdown().await;
    assert_eq!(report.manual_complete, Some(false));
    assert!(!report.confirmed());
    assert!(job.poll());
    assert_eq!(job.result.as_ref().unwrap().failed, 1);
    assert_eq!(job.result.as_ref().unwrap().complete, 0);
    assert_eq!(host.shutdown().await, report);
    drop((job, host));
    std::fs::remove_dir_all(dir).unwrap();
}

struct DelayedClose(Arc<tokio::sync::Notify>);
impl Route for DelayedClose {
    fn open(&self, _: &tune_core::nodes::Node, _: &Opts) -> Result<Source, String> {
        let (lines_send, lines_recv) = tokio::sync::mpsc::channel(1);
        let (stop_send, stop_recv) = tokio::sync::oneshot::channel();
        let (done_send, done_recv) = tokio::sync::oneshot::channel();
        self.0.notify_one();
        tokio::spawn(async move {
            let _ = stop_recv.await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(lines_send);
            let _ = done_send.send(tune_core::live::Ended::default());
        });
        Ok(Source::new(lines_recv, stop_send, done_recv))
    }
}

#[test]
fn live_and_journal_share_one_final_budget_and_missing_ack_is_not_success() {
    // One occupied blocking slot holds the real journal writer's final commit.
    // This avoids adding a product test hook or changing SQLite busy handling.
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().max_blocking_threads(1).build().unwrap();
    let dir = rt.block_on(async {
        let (dir, engine, _) = host_fixture("budget");
        let opened = Arc::new(tokio::sync::Notify::new());
        let live = Live::new(Arc::new(DelayedClose(opened.clone())), Opts::default(), Box::new(|_| {}));
        let node = tune_core::nodes::Node::from_value(&json!({"id":"fixture","os":"macos"}), "synthetic-host");
        live.start(&[node], &[]);
        tokio::time::timeout(Duration::from_secs(2), opened.notified()).await.unwrap();
        let host = with_fake_owner(MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false))));
        let (held_send, held_recv) = tokio::sync::oneshot::channel();
        let (release_send, release_recv) = mpsc::channel();
        let held = tokio::task::spawn_blocking(move || {
            held_send.send(()).unwrap();
            release_recv.recv_timeout(Duration::from_secs(10)).unwrap();
        });
        held_recv.await.unwrap();
        engine.start_monitor_journal();
        let start = Instant::now();
        let report = host.shutdown().await;
        let elapsed = start.elapsed();
        release_send.send(()).unwrap();
        held.await.unwrap();
        // A later commit cannot retroactively confirm the missing shutdown ACK.
        tokio::time::timeout(Duration::from_secs(2), async {
            while engine.monitor_journal_health()["running"] == true {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(host.shutdown().await, report);
        assert_eq!(report.journal_ack, Some(false));
        assert!(!report.confirmed());
        assert!(elapsed >= Duration::from_millis(2900));
        assert!(elapsed < Duration::from_millis(3800), "two independent budgets would take five seconds: {elapsed:?}");
        eprintln!("shared_live_journal_budget_ms={} report={report:?}", elapsed.as_millis());
        drop((host, engine));
        dir
    });
    // Release runtime-owned task state before deleting the fixture's SQLite files.
    drop(rt);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn accepted_install_outlives_window_and_three_second_drain_budget() {
    let (dir, engine, host) = host_fixture("install-join");
    let gate = crate::gate::Gate::new(|| {});
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = mpsc::channel();
    host.admit_install_work(&gate, move || {
        entered.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
        Err(tune_core::helpers::native_install::prepare_native_sampler(Path::new("relative-source")).unwrap_err())
    })
    .unwrap();
    // Registration precedes both window close and shutdown, even before polling.
    assert!(host.state.lock().unwrap().install_job.is_some());
    gate.close();
    ready.await.unwrap();
    let draining = host.clone();
    let mut task = tokio::spawn(async move { draining.shutdown().await });
    assert!(tokio::time::timeout(Duration::from_millis(3150), &mut task).await.is_err());
    assert!(host.native_install_snapshot().busy);
    release.send(()).unwrap();
    let report = task.await.unwrap();
    assert_eq!(report.install_complete, Some(true));
    assert!(!report.install_recovery_required);
    assert!(report.confirmed());
    assert_eq!(host.shutdown().await, report);
    assert!(matches!(host.native_install_snapshot().result.as_deref(), Some(NativeInstallResult::Completed(Err(_)))));
    drop((engine, host));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn install_panic_retains_uncertainty_and_blocks_all_reuse() {
    let (dir, engine, host) = host_fixture("install-panic");
    let gate = crate::gate::Gate::new(|| {});
    host.admit_install_work(&gate, || panic!("synthetic blocking transaction loss")).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while host.native_install_snapshot().busy {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(host.native_install_snapshot().recovery_required);
    assert!(host.ensure_ticker().is_err());
    assert!(host.start_diagnostic_journal_from_ui(&gate).is_err());
    assert!(host.admit_install_work(&gate, || panic!("recovery cannot retry")).is_err());
    let report = host.shutdown().await;
    assert_eq!(report.install_complete, Some(false));
    assert!(report.install_recovery_required);
    assert!(!report.confirmed());
    assert!(matches!(host.native_install_snapshot().result.as_deref(), Some(NativeInstallResult::WorkerFailed)));
    assert!(host.admit_install_work(&gate, || panic!("no retry")).is_err());
    assert_eq!(host.shutdown().await, report);
    drop((engine, host));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn registered_install_still_runs_when_quit_precedes_first_worker_poll() {
    let (dir, engine, host) = host_fixture("install-register-first");
    let gate = crate::gate::Gate::new(|| {});
    let called = Arc::new(AtomicBool::new(false));
    let observed = called.clone();
    host.admit_install_work(&gate, move || {
        observed.store(true, Ordering::Release);
        Err(tune_core::helpers::native_install::prepare_native_sampler(Path::new("relative-source")).unwrap_err())
    })
    .unwrap();
    assert!(!called.load(Ordering::Acquire));
    host.request_exit();
    gate.close();
    assert_eq!(host.shutdown().await.install_complete, Some(true));
    assert!(called.load(Ordering::Acquire));
    drop((host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

async fn uncertain_install_completion(failure: NativeInstallFailure, label: &str) {
    let (dir, engine, host) = host_fixture(label);
    let gate = crate::gate::Gate::new(|| {});
    {
        let mut state = host.state.lock().unwrap();
        MonitorHost::record_install(&mut state, NativeInstallResult::Completed(Err(failure)));
    }
    let snapshot = host.native_install_snapshot();
    assert!(snapshot.recovery_required, "uncertain completion must latch before any new request");
    assert!(!snapshot.busy);
    assert!(!snapshot.ever_collection_admitted);
    assert!(snapshot.unavailable.is_some());
    let result = snapshot.result.as_deref().expect("terminal detail must survive without a UI receiver");
    let NativeInstallResult::Completed(Err(retained)) = result else { panic!("core failure facts cannot become worker loss or success") };
    assert_eq!(retained.cause, native_install::Cause::Io);
    match label {
        "completion-recovery" => {
            assert_eq!(retained.outcome, InstallOutcome::RecoveryRequired);
            assert_eq!(retained.operation, Operation::Rollback);
            assert_eq!(retained.rollback_cause, Some(native_install::Cause::PermissionDenied));
            assert_eq!(retained.recovery_paths, [PathBuf::from("synthetic-owned-backup")]);
            assert!(retained.detail().contains("synthetic-owned-backup"));
            assert!(retained.detail().contains("PermissionDenied"));
        }
        "completion-close" => {
            assert_eq!(retained.outcome, InstallOutcome::Rejected);
            assert_eq!(retained.operation, Operation::Close);
            assert_eq!(retained.rollback_cause, None);
            assert!(retained.recovery_paths.is_empty());
            assert!(retained.detail().contains("Close"));
        }
        _ => unreachable!(),
    }
    // None of these refusals may be attributed to exiting, busy, or ever=true.
    assert!(!host.state.lock().unwrap().exiting);
    assert!(host.start_monitor_from_ui(&gate).is_err());
    assert!(host.request_pull_from_ui(&gate).is_err());
    assert!(host.start_schedule_from_ui(&gate).is_err());
    assert!(host.ensure_ticker().is_err());
    assert!(host.start_diagnostic_journal_from_ui(&gate).is_err());
    assert!(host.admit_live_request(&gate, || panic!("recovery cannot admit Live")).is_err());
    assert!(host.start_manual_with(&mut Job::default(), Kind::Network(false), || panic!("recovery cannot admit manual work")).is_err());
    assert!(host.admit_install_work(&gate, || panic!("recovery cannot admit another install")).is_err());

    // A later confirmed completion must not erase the earlier uncertainty.
    // Rejected is a confirmed end of a transaction, even though it is not an install success.
    let definite = native_install::prepare_native_sampler(Path::new("relative-source")).unwrap_err();
    {
        let mut state = host.state.lock().unwrap();
        MonitorHost::record_install(&mut state, NativeInstallResult::Completed(Err(NativeInstallFailure::from(&definite))));
        assert_eq!(state.install_confirmed, Some(false));
    }
    assert!(host.native_install_snapshot().recovery_required);
    assert!(host.admit_install_work(&gate, || panic!("a confirmed later completion cannot clear recovery")).is_err());
    assert!(host.ensure_ticker().is_err());
    let report = host.shutdown().await;
    assert_eq!(report.install_complete, Some(false));
    assert!(report.install_recovery_required);
    assert_eq!(report.journal_ack, Some(true));
    assert!(!report.confirmed());
    assert_eq!(host.shutdown().await, report);
    // The original typed snapshot remains readable after later results and shutdown.
    assert!(matches!(snapshot.result.as_deref(), Some(NativeInstallResult::Completed(Err(_)))));
    drop((snapshot, host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn install_recovery_completion_retains_details_and_refuses_reuse() {
    uncertain_install_completion(
        NativeInstallFailure {
            outcome: InstallOutcome::RecoveryRequired,
            operation: Operation::Rollback,
            cause: native_install::Cause::Io,
            rollback_cause: Some(native_install::Cause::PermissionDenied),
            recovery_paths: vec![PathBuf::from("synthetic-owned-backup")],
        },
        "completion-recovery",
    )
    .await;
}

#[tokio::test]
async fn install_close_completion_retains_uncertainty_even_when_rejected() {
    uncertain_install_completion(
        NativeInstallFailure {
            outcome: InstallOutcome::Rejected,
            operation: Operation::Close,
            cause: native_install::Cause::Io,
            rollback_cause: None,
            recovery_paths: Vec::new(),
        },
        "completion-close",
    )
    .await;
}

use tune_core::helpers::remote_update::{
    RemoteArchitecture, RemoteTargetSnapshot, RemoteUpdateControl, RemoteUpdateFuture, RemoteUpdateOutcome, RemoteUpdatePayload, RemoteUpdateReply,
    RemoteUpdateRequest, RemoteUpdateStage, RemoteUpdateTransport,
};
struct RemoteRoute;
impl Route for RemoteRoute {
    fn open(&self, _: &tune_core::nodes::Node, _: &Opts) -> Result<Source, String> {
        let (send, recv) = tokio::sync::mpsc::channel(4);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            send.send(r#"{"type":"hello","impl":"rust","agent":"0.0.1"}"#.into()).await.unwrap();
            let _ = stopped.await;
            drop(send);
            let _ = done.send(tune_core::live::Ended::default());
        });
        Ok(Source::new(recv, stop, finished))
    }
}
struct FakeRemoteUpdate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    reply: u8,
    calls: std::sync::atomic::AtomicUsize,
}
impl RemoteUpdateTransport for FakeRemoteUpdate {
    fn place<'a>(&'a self, node: &'a tune_core::nodes::Node, payload: RemoteUpdatePayload<'a>) -> RemoteUpdateFuture<'a> {
        Box::pin(async move {
            assert_eq!(node.id, "remote-fixture");
            assert!(!payload.bytes().is_empty());
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.release.notified().await;
            if self.reply == 3 {
                panic!("synthetic remote panic");
            }
            RemoteUpdateReply::observed(
                if self.reply == 2 { None } else { Some(if self.reply == 1 { 1 } else { 0 }) },
                payload.expected_response().as_bytes(),
                RemoteUpdateStage::Placed,
                true,
            )
        })
    }
}
fn remote_request(engine: &Engine, dir: &Path, id: u64, seq: u64) -> RemoteUpdateRequest {
    let source = dir.join("source");
    std::fs::create_dir_all(&source).unwrap();
    crate::native::synthetic_sampler_source(&source);
    let prepared = tune_core::helpers::native_install::prepare_native_sampler(&source).unwrap();
    let architecture = if cfg!(windows) { RemoteArchitecture::X64 } else { RemoteArchitecture::Arm64 };
    let target = RemoteTargetSnapshot::checked(engine, "remote-fixture", Some(architecture)).unwrap();
    RemoteUpdateRequest::new(id, seq, Instant::now() + Duration::from_secs(120), target, prepared).unwrap()
}
fn remote_host(
    label: &str,
    reply: u8,
    owner_busy: bool,
) -> (PathBuf, Arc<Engine>, Arc<MonitorHost>, Arc<FakeRemoteUpdate>, Arc<std::sync::atomic::AtomicUsize>) {
    let (dir, engine, _) = host_fixture(label);
    let os = if cfg!(windows) { "windows" } else { "macos" };
    std::fs::write(dir.join("nodes.json"), json!({"nodes":[{"id":"remote-fixture","os":os}]}).to_string()).unwrap();
    engine.reload_config();
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true}))).unwrap();
    let live = Live::new(Arc::new(RemoteRoute), Opts::default(), Box::new(|_| {}));
    let transport = Arc::new(FakeRemoteUpdate { entered: tokio::sync::Notify::new(), release: tokio::sync::Notify::new(), reply, calls: 0.into() });
    let mut host = MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false)));
    host.remote_updates = Some(Arc::new(RemoteUpdateControl::new(transport.clone())));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    host.collection_owner = Some(Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        if owner_busy { Err(tune_core::gui_collection::AdmissionError::Busy) } else { Ok(()) }
    }));
    (dir, engine, Arc::new(host), transport, calls)
}
async fn remote_hello(host: &MonitorHost) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while host.live.brief("remote-fixture").is_none_or(|b| b["impl"] != "rust") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn remote_update_owned_shutdown_keeps_join_placement_and_ack_separate() {
    for (reply, missing_ack) in [(0, false), (1, false), (2, false), (3, false), (0, true)] {
        let (dir, engine, host, transport, owner) = remote_host(&format!("remote-{reply}-{missing_ack}"), reply, false);
        let gate = crate::gate::Gate::new(|| {});
        host.start_monitor_from_ui(&gate).unwrap();
        remote_hello(&host).await;
        let seq = host.request.borrow().start_seq;
        host.admit_remote_update(remote_request(&engine, &dir, 1, seq), &gate).unwrap();
        transport.entered.notified().await;
        assert!(owner.load(Ordering::SeqCst) >= 2, "same real host ownership boundary uses the explicit fake provider");
        assert!(host.admit_install_work(&gate, || panic!("remote admission preserves ever-collection")).is_err());
        host.pause_monitor().unwrap();
        assert_eq!(host.snapshot().status.phase, RunPhase::Stopping);
        assert!(host.admit_install_work(&gate, || panic!("Pause cannot reset local install admission")).is_err());
        gate.close();
        if missing_ack {
            engine.with_db(|d| d.conn().execute_batch("DROP TABLE monitor_events")).unwrap();
        }
        let draining = host.clone();
        let mut task = tokio::spawn(async move { draining.shutdown().await });
        if reply == 0 && !missing_ack {
            assert!(tokio::time::timeout(Duration::from_millis(3150), &mut task).await.is_err());
            assert_eq!(host.snapshot().status.phase, RunPhase::Stopping);
            assert_eq!(engine.monitor_journal_health()["running"], true);
        }
        transport.release.notify_one();
        let report = task.await.unwrap();
        assert_eq!(report.monitor_complete, Some(true));
        assert_eq!(report.remote_update_complete, Some(reply != 3));
        assert_eq!(report.journal_ack, Some(!missing_ack));
        assert_eq!(report.confirmed(), reply == 0 && !missing_ack);
        assert_eq!(report.manual_complete, None);
        assert_eq!(report.install_complete, None);
        assert_eq!(report.remote_update.unwrap().placement_known(), reply == 0);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(host.shutdown().await, report);
        drop((host, engine));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[tokio::test]
async fn remote_update_admission_refuses_owner_local_install_gate_and_quit_conflicts() {
    // Owner refusal is checked before any worker/admission is created.
    let (dir, engine, host, transport, owner) = remote_host("remote-owner-busy", 0, true);
    host.live.start_owned(tune_core::live::Owner::Ui, &engine.config().nodes, &[]);
    remote_hello(&host).await;
    host.request.send_modify(|r| {
        r.mode = RunMode::Run;
        r.start_seq = 1;
    });
    let gate = crate::gate::Gate::new(|| {});
    assert!(host.admit_remote_update(remote_request(&engine, &dir, 1, 1), &gate).is_err());
    assert_eq!(owner.load(Ordering::SeqCst), 1);
    assert!(!host.remote_updates.as_ref().unwrap().busy());
    assert!(!host.native_install_snapshot().ever_collection_admitted);
    assert!(!host.snapshot().started);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(host.shutdown().await.confirmed());
    drop((host, engine));
    std::fs::remove_dir_all(dir).unwrap();
    // A real admitted local fake transaction blocks remote admission before IO.
    let (dir, engine, host, transport, _) = remote_host("remote-install-conflict", 0, false);
    let gate = crate::gate::Gate::new(|| {});
    let (release, wait) = mpsc::channel();
    host.admit_install_work(&gate, move || {
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
        Err(native_install::prepare_native_sampler(Path::new("relative")).unwrap_err())
    })
    .unwrap();
    assert!(host.admit_remote_update(remote_request(&engine, &dir, 1, 0), &gate).is_err());
    release.send(()).unwrap();
    assert!(host.shutdown().await.confirmed());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    drop((host, engine));
    std::fs::remove_dir_all(dir).unwrap();
    let (dir, engine, host, transport, _) = remote_host("remote-recovery-conflict", 0, false);
    MonitorHost::record_install(&mut host.state.lock().unwrap(), NativeInstallResult::WorkerFailed);
    assert!(host.admit_remote_update(remote_request(&engine, &dir, 1, 0), &crate::gate::Gate::new(|| {})).is_err());
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(!host.shutdown().await.confirmed());
    drop((host, engine));
    std::fs::remove_dir_all(dir).unwrap();
    // Closed Gate, new Start, Pause and Quit synchronously withdraw Pending.
    let (dir, engine, host, transport, _) = remote_host("remote-pending-close", 0, false);
    let gate = crate::gate::Gate::new(|| {});
    host.start_monitor_from_ui(&gate).unwrap();
    remote_hello(&host).await;
    gate.close();
    let seq = host.request.borrow().start_seq;
    assert!(host.admit_remote_update(remote_request(&engine, &dir, 1, seq), &gate).is_err());
    let gate = crate::gate::Gate::new(|| {});
    host.admit_remote_update(remote_request(&engine, &dir, 1, seq), &gate).unwrap();
    host.pause_monitor().unwrap();
    host.start_monitor_from_ui(&gate).unwrap();
    assert!(!host.remote_updates.as_ref().unwrap().busy());
    let seq = host.request.borrow().start_seq;
    host.admit_remote_update(remote_request(&engine, &dir, 2, seq), &gate).unwrap();
    host.request_exit();
    assert!(!host.remote_updates.as_ref().unwrap().busy());
    assert!(host.admit_remote_update(remote_request(&engine, &dir, 3, seq), &gate).is_err());
    assert!(matches!(host.shutdown().await.remote_update, Some(RemoteUpdateOutcome::Refused { attempt_id: 2, .. })));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    drop((host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn remote_update_monitor_join_failure_cannot_confirm_shutdown() {
    let (dir, engine, host, _, _) = remote_host("remote-join-failure", 0, false);
    host.state.lock().unwrap().worker = Some(tokio::spawn(async {
        panic!("synthetic controlled worker join loss");
    }));
    let report = host.shutdown().await;
    assert_eq!(report.monitor_complete, Some(false));
    assert_eq!(report.remote_update_complete, Some(false));
    assert!(!report.confirmed());
    assert_eq!(report.journal_ack, Some(true));
    drop((host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}
