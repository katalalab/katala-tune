//! Deterministic actual Gate/Host/Store race. No transport or OS service runs.
use super::*;
use crate::gate::Gate;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{atomic::AtomicUsize, mpsc},
    time::Instant,
};
use tune_core::live::{Opts, Route, Source};
struct Never;
impl Route for Never {
    fn open(&self, _: &tune_core::nodes::Node, _: &Opts) -> Result<Source, String> {
        panic!("admission fixture cannot open a transport")
    }
}
fn fixture(label: &str) -> (PathBuf, Arc<Engine>, Arc<MonitorHost>) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("gpui-admit-{label}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir(&dir).unwrap();
    let config = if label == "pull" { json!({"nodes":[{"id":"hub-fixture","os":"macos"}]}) } else { json!({"nodes":[]}) };
    std::fs::write(dir.join("nodes.json"), config.to_string()).unwrap();
    let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
    engine.with_db(|db| db.set_meta("monitor", &json!({"enabled":true,"hub":if label == "pull" { Some("hub-fixture") } else { None }}))).unwrap();
    engine.set_schedule(&json!({"enabled":true})).unwrap();
    let live = Live::new(Arc::new(Never), Opts::default(), Box::new(|_| {}));
    let host = Arc::new(MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false))));
    (dir, engine, host)
}
fn rejected_after_blocked_db(label: &'static str) {
    // Keep tasks unpolled until the command result is known. In the RED version
    // a pull would otherwise contact its synthetic hub; Exit precedes any poll.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let enter = rt.enter();
    let (dir, engine, host) = fixture(label);
    let gate = Gate::new(|| {});
    let (held, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let db_engine = engine.clone();
    let db_thread = std::thread::spawn(move || {
        db_engine
            .with_db(|_| {
                held.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .unwrap()
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let pending_host = host.clone();
    let pending_gate = gate.clone();
    let command = std::thread::spawn(move || {
        assert!(!pending_gate.is_closed(), "the former UI early check has passed");
        match label {
            "monitor" => pending_host.start_monitor_from_ui(&pending_gate),
            "schedule" => pending_host.start_schedule_from_ui(&pending_gate),
            "pull" => pending_host.request_pull_from_ui(&pending_gate),
            _ => unreachable!(),
        }
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    while host.state.try_lock().is_ok() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    let reached_host = host.state.try_lock().is_err();
    let closing_gate = gate.clone();
    let (closed, receive_closed) = mpsc::channel();
    let close = std::thread::spawn(move || {
        closing_gate.close();
        closed.send(()).unwrap();
    });
    let closed_while_db_held = receive_closed.recv_timeout(Duration::from_secs(1)).is_ok();
    release.send(()).unwrap();
    db_thread.join().unwrap();
    close.join().unwrap();
    let outcome = command.join().unwrap();
    // A fresh window has its own Gate; it cannot make the old token valid.
    let reopened = Gate::new(|| {});
    assert!(!reopened.is_closed());
    let retry = match label {
        "monitor" => host.start_monitor_from_ui(&gate),
        "schedule" => host.start_schedule_from_ui(&gate),
        "pull" => host.request_pull_from_ui(&gate),
        _ => unreachable!(),
    };
    let workers = {
        let state = host.state.lock().unwrap();
        (state.worker.is_some(), state.ticker.is_some(), state.scheduler.is_some())
    };
    let request = *host.request.borrow();
    let schedule = host.schedule_snapshot().0;
    let journal = engine.monitor_journal_health()["running"].clone();
    assert!(!host.native_install_snapshot().ever_collection_admitted);
    host.request_exit();
    let report = rt.block_on(host.shutdown());
    drop((host, engine, enter));
    drop(rt);
    std::fs::remove_dir_all(dir).unwrap();
    assert!(reached_host, "fixture must enter host validation before closing UI");
    assert!(closed_while_db_held, "window close cannot wait for blocking DB validation");
    assert!(retry.is_err(), "reopening cannot revive the previous window token");
    assert!(outcome.is_err(), "{label}: closed UI must fail at the final permission commit");
    assert_eq!(workers, (false, false, false));
    assert_eq!(request.mode, RunMode::Paused);
    assert_eq!((request.start_seq, request.pull_seq), (0, 0));
    assert!(!schedule);
    assert_eq!(journal, false);
    assert!(report.confirmed());
}
#[test]
fn monitor_start_rechecks_revocation_after_real_db_wait() {
    rejected_after_blocked_db("monitor");
}
#[test]
fn schedule_start_rechecks_revocation_after_real_db_wait() {
    rejected_after_blocked_db("schedule");
}
#[test]
fn pull_start_rechecks_revocation_after_real_db_wait() {
    rejected_after_blocked_db("pull");
}

#[tokio::test]
async fn committed_sessions_survive_resident_close_and_quit_drains_same_workers() {
    let (dir, engine, host) = fixture("committed");
    let gate = Gate::new(|| {});
    host.start_monitor_from_ui(&gate).unwrap();
    host.start_schedule_from_ui(&gate).unwrap();
    let workers = {
        let s = host.state.lock().unwrap();
        (s.worker.as_ref().unwrap().id(), s.ticker.as_ref().unwrap().id(), s.scheduler.as_ref().unwrap().id())
    };
    gate.close();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while host.snapshot().status.phase != RunPhase::Running {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(host.snapshot().mode, RunMode::Run);
    assert!(host.schedule_snapshot().0);
    assert_eq!(engine.monitor_journal_health()["running"], true);
    assert!(host.start_monitor_from_ui(&gate).is_err());
    assert!(host.start_schedule_from_ui(&gate).is_err());
    let same = {
        let s = host.state.lock().unwrap();
        (s.worker.as_ref().unwrap().id(), s.ticker.as_ref().unwrap().id(), s.scheduler.as_ref().unwrap().id())
    };
    assert_eq!(workers, same);
    let report = host.shutdown().await;
    assert_eq!(report.monitor_complete, Some(true));
    assert_eq!(report.scheduler_complete, Some(true));
    assert!(report.confirmed());
    drop((host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

// Each contender is an independent copy of this test harness, never the product.
fn contender() {
    harness_child("collection_owner_rejects_independent_gui_admission");
}
fn harness_child(test: &str) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("monitor_host::admission_tests::{test}"), "--include-ignored", "--nocapture"])
        .env("TUNE_COLLECTION_CONTENDER", "1")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "independent GUI contender must be refused");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned GUI harness child timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn collection_owner_rejects_independent_gui_admission() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let enter = rt.enter();
    let (dir, engine, host) = fixture("collection-owner");
    let child = std::env::var_os("TUNE_COLLECTION_CONTENDER").is_some();
    let ids = if child { ["fixture-b", "fixture-c"] } else { ["fixture-a", "fixture-b"] };
    std::fs::write(dir.join("nodes.json"), json!({"nodes":ids.map(|id| json!({"id":id,"os":"macos"}))}).to_string()).unwrap();
    engine.reload_config();
    let count = Arc::new(AtomicUsize::new(0));
    let mut job = Job::default();
    if child {
        // Engine and saved settings work with a different data directory even
        // while collection is owned. All production wrappers must refuse.
        host.save_monitor_settings(json!({"enabled":true,"hub":"fixture-b"})).unwrap();
        host.save_schedule_settings(json!({"enabled":true})).unwrap();
        let gate = Gate::new(|| {});
        assert!(host.admit_install_work(&gate, || panic!("another GUI owns collection")).is_err());
        assert!(!host.native_install_snapshot().ever_collection_admitted);
        assert!(host.state.lock().unwrap().install_job.is_none());
        for kind in [Kind::Probe, Kind::Logs, Kind::Inventory, Kind::Ai, Kind::Specs, Kind::Network(false), Kind::Network(true)] {
            assert!(host.start_manual(&mut job, kind).is_err());
            assert!(host.start_manual_from_ui(&mut job, kind, &gate).is_err());
            let calls = count.clone();
            assert!(
                host.start_manual_with(&mut job, kind, move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Receipt::error(kind, "synthetic completed work")
                })
                .is_err()
            );
            assert!(job.running.is_none());
            assert!(job.result.is_none());
        }
        assert!(host.start_monitor().is_err());
        assert!(host.start_monitor_from_ui(&gate).is_err());
        assert!(host.request_pull().is_err());
        assert!(host.request_pull_from_ui(&gate).is_err());
        assert!(host.start_schedule().is_err());
        assert!(host.start_schedule_from_ui(&gate).is_err());
        let calls = count.clone();
        assert!(
            host.start_schedule_with(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            })
            .is_err()
        );
        assert!(host.ensure_ticker().is_err());
        assert!(host.ensure_ticker_from_ui(&gate).is_err());
        assert!(host.start_diagnostic_journal_from_ui(&gate).is_err());
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        crate::native::tests::assert_collection_refuses_live();
        rt.block_on(async {
            tokio::task::yield_now().await;
        });
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let state = host.state.lock().unwrap();
        assert!(state.manual.is_none() && state.worker.is_none() && state.ticker.is_none() && state.scheduler.is_none());
        drop(state);
        let request = *host.request.borrow();
        assert_eq!((request.start_seq, request.pull_seq), (0, 0));
        assert_eq!(request.mode, RunMode::Paused);
        assert!(!host.schedule_snapshot().0);
        assert!(!host.snapshot().started);
        assert_eq!(engine.monitor_journal_health()["running"], false);
        assert!(host.live.active().is_empty());
    } else {
        let calls = count.clone();
        host.start_fixture_manual(&mut job, move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Receipt::error(Kind::Probe, "synthetic completed work")
        })
        .expect("unexpected external collector ownership; never take over");
        contender(); // Admitted manual, before runtime polling.
        rt.block_on(async {
            let deadline = Instant::now() + Duration::from_secs(2);
            while job.running.is_some() {
                job.poll();
                assert!(Instant::now() < deadline);
                tokio::task::yield_now().await;
            }
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
        contender(); // Manual completion cannot release.
        // Empty ledger prevents any remote effects in this owner's worker.
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
        host.start_monitor().unwrap();
        host.start_schedule_with(|_| async { Ok(()) }).unwrap();
        host.pause_monitor().unwrap();
        host.stop_schedule().unwrap();
        let gate = Gate::new(|| {});
        gate.close();
        assert!(host.start_monitor_from_ui(&gate).is_err());
        assert!(host.start_manual_admitted(&mut job, Kind::Probe, Some(&gate), || panic!("closed gate")).is_err());
        assert!(host.ensure_ticker_from_ui(&gate).is_err());
        contender(); // Pause / Stop / resident window close cannot release.
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel();
        host.start_fixture_manual(&mut job, move || {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(10)).unwrap();
            Receipt::error(Kind::Probe, "synthetic drain completed")
        })
        .unwrap();
        rt.block_on(async {
            ready.await.unwrap();
            let draining = host.clone();
            let shutdown = tokio::spawn(async move { draining.shutdown().await });
            tokio::task::yield_now().await;
            assert!(host.state.lock().unwrap().exiting);
            assert!(!shutdown.is_finished());
            contender(); // Claim remains reserved throughout a blocked producer drain.
            release.send(()).unwrap();
            assert!(shutdown.await.unwrap().confirmed());
        });
    }
    host.request_exit();
    assert!(rt.block_on(host.shutdown()).confirmed());
    if !child {
        contender();
    } // Even shutdown return retains the process claim.
    drop((job, host, engine, enter));
    drop(rt);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "run alone: the passive parent must never share another unit test's process-static reservation"]
fn passive_host_does_not_reserve_collection() {
    if std::env::var_os("TUNE_COLLECTION_CONTENDER").is_some() {
        tune_core::gui_collection::ensure_gui_collection_owner()
            .expect("passive parent must leave collection unreserved; unexpected external ownership is not taken over");
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let enter = rt.enter();
    let (dir, engine, host) = fixture("passive");
    engine.reload_config();
    host.save_monitor_settings(json!({"enabled":true})).unwrap();
    host.save_schedule_settings(json!({"enabled":true})).unwrap();
    assert!(!host.snapshot().started);
    assert!(!host.schedule_snapshot().0);
    assert!(host.live.active().is_empty());
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    crate::native::tests::assert_passive_live_does_not_start();
    harness_child("passive_host_does_not_reserve_collection");
    assert!(rt.block_on(host.shutdown()).confirmed());
    harness_child("passive_host_does_not_reserve_collection");
    drop((host, engine, enter));
    drop(rt);
    std::fs::remove_dir_all(dir).unwrap();
}

fn rejected_install() -> Result<LocalInstallReceipt, InstallError> {
    Err(tune_core::helpers::native_install::prepare_native_sampler(std::path::Path::new("relative-source")).unwrap_err())
}

#[tokio::test]
async fn install_reservation_blocks_every_common_start_before_any_mutation() {
    let (dir, engine, host) = fixture("install-first");
    let gate = Gate::new(|| {});
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = mpsc::channel();
    host.admit_install_work(&gate, move || {
        entered.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
        rejected_install()
    })
    .unwrap();
    ready.await.unwrap();
    crate::native::tests::assert_install_blocks_live_pending(&host, host.live.clone());
    for kind in [Kind::Probe, Kind::Logs, Kind::Inventory, Kind::Ai, Kind::Specs, Kind::Network(false), Kind::Network(true)] {
        for gated in [false, true] {
            let mut job = Job::default();
            assert!(host.start_manual_admitted(&mut job, kind, gated.then_some(&gate), || panic!("not admitted")).is_err());
            assert!(job.running.is_none());
        }
    }
    assert!(host.start_monitor().is_err());
    assert!(host.start_monitor_from_ui(&gate).is_err());
    assert!(host.request_pull().is_err());
    assert!(host.request_pull_from_ui(&gate).is_err());
    assert!(host.start_schedule().is_err());
    assert!(host.start_schedule_from_ui(&gate).is_err());
    assert!(host.ensure_ticker().is_err());
    assert!(host.ensure_ticker_from_ui(&gate).is_err());
    assert!(host.start_diagnostic_journal_from_ui(&gate).is_err());
    assert!(host.admit_live_request(&gate, || panic!("pending must not commit")).is_err());
    assert!(!host.native_install_snapshot().ever_collection_admitted);
    assert!(host.admit_install_work(&gate, || panic!("duplicate install")).is_err());
    assert_eq!((host.request.borrow().start_seq, host.request.borrow().pull_seq), (0, 0));
    assert_eq!(host.snapshot().status.phase, RunPhase::Paused);
    assert!(!host.schedule_snapshot().0);
    assert_eq!(engine.monitor_journal_health()["running"], false);
    release.send(()).unwrap();
    assert_eq!(host.shutdown().await.install_complete, Some(true));
    assert!(matches!(host.native_install_snapshot().result.as_deref(), Some(NativeInstallResult::Completed(Err(_)))));
    drop((engine, host));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn live_pending_commits_ever_atomically_and_rejected_intent_does_not() {
    let (dir, engine, host) = fixture("live-pending");
    let gate = Gate::new(|| {});
    assert_eq!(host.admit_live_request(&gate, || None).unwrap(), None);
    assert!(!host.native_install_snapshot().ever_collection_admitted);
    assert!(host.state.lock().unwrap().ticker.is_none());
    assert_eq!(host.admit_live_request(&gate, || Some((7, "fixture".into()))).unwrap(), Some((7, "fixture".into())));
    assert!(host.native_install_snapshot().ever_collection_admitted);
    assert!(host.admit_install_work(&gate, || panic!("pending collection won")).is_err());
    host.pause_monitor().unwrap();
    host.stop_schedule().unwrap();
    gate.close();
    let reopened = Gate::new(|| {});
    assert!(host.admit_install_work(&reopened, || panic!("reopen cannot reset ever")).is_err());
    assert!(host.shutdown().await.confirmed());
    drop((engine, host));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn all_manual_kinds_commit_lifetime_even_after_work_finishes() {
    for kind in [Kind::Probe, Kind::Logs, Kind::Inventory, Kind::Ai, Kind::Specs, Kind::Network(false), Kind::Network(true)] {
        let (dir, engine, host) = fixture("manual-ever");
        let gate = Gate::new(|| {});
        let mut job = Job::default();
        host.start_manual_admitted(&mut job, kind, Some(&gate), move || Receipt::error(kind, "synthetic connection failure")).unwrap();
        assert!(host.native_install_snapshot().ever_collection_admitted);
        let report = host.shutdown().await;
        assert_eq!(report.manual_complete, Some(true));
        assert!(host.native_install_snapshot().ever_collection_admitted);
        drop((job, engine, host));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn closed_gate_and_quit_first_never_register_install_or_collection_intent() {
    let (dir, engine, host) = fixture("install-closed");
    let gate = Gate::new(|| {});
    gate.close();
    assert!(host.admit_install_work(&gate, || panic!("closed gate")).is_err());
    assert!(host.admit_live_request(&gate, || panic!("closed gate")).is_err());
    assert!(!host.native_install_snapshot().ever_collection_admitted);
    host.request_exit();
    let reopened = Gate::new(|| {});
    assert!(host.admit_install_work(&reopened, || panic!("Quit won")).is_err());
    let report = host.shutdown().await;
    assert_eq!(report.install_complete, None);
    assert!(report.confirmed());
    drop((engine, host));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn revocation_while_live_waits_for_gate_never_commits_host_intent() {
    let (dir, engine, host) = fixture("live-revoke");
    crate::native::tests::assert_revoked_live_generation(host.clone(), host.live.clone(), || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while host.state.try_lock().is_ok() {
            assert!(Instant::now() < deadline, "request must hold host before revocation");
            std::thread::yield_now();
        }
    });
    assert!(!host.native_install_snapshot().ever_collection_admitted);
    assert!(host.state.lock().unwrap().ticker.is_none());
    assert!(host.shutdown().await.confirmed());
    drop((engine, host));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn completed_rejection_releases_busy_but_retains_result_during_next_attempt() {
    let (dir, engine, host) = fixture("install-repeat");
    let gate = Gate::new(|| {});
    host.admit_install_work(&gate, rejected_install).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while host.native_install_snapshot().busy {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let previous = host.native_install_snapshot().result.unwrap();
    let (release, wait) = mpsc::channel();
    host.admit_install_work(&gate, move || {
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        rejected_install()
    })
    .unwrap();
    assert!(Arc::ptr_eq(&previous, &host.native_install_snapshot().result.unwrap()));
    assert!(host.native_install_snapshot().busy);
    assert!(!host.native_install_snapshot().ever_collection_admitted);
    release.send(()).unwrap();
    let report = host.shutdown().await;
    assert_eq!(report.install_complete, Some(true));
    assert!(report.confirmed());
    drop((previous, engine, host));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn each_successful_collection_entry_permanently_closes_install_admission() {
    for label in ["monitor", "pull", "schedule", "ticker", "journal"] {
        let (dir, engine, host) = fixture(label);
        let gate = Gate::new(|| {});
        match label {
            "monitor" => host.start_monitor_from_ui(&gate),
            "pull" => host.request_pull_from_ui(&gate),
            "schedule" => host.start_schedule_from_ui(&gate),
            "ticker" => host.ensure_ticker_from_ui(&gate),
            _ => host.start_diagnostic_journal_from_ui(&gate),
        }
        .unwrap();
        assert!(host.native_install_snapshot().ever_collection_admitted, "{label}");
        assert!(host.admit_install_work(&gate, || panic!("collection already admitted")).is_err());
        // Exit before polling the synthetic hub worker: no transport may open.
        host.request_exit();
        assert!(host.shutdown().await.confirmed());
        assert!(host.native_install_snapshot().ever_collection_admitted);
        drop((engine, host));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
