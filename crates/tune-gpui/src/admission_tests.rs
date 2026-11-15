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
