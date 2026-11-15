//! Actual Engine/Store with a refused Route and deterministic Tokio clock.
use super::*;
use serde_json::json;
use std::sync::atomic::AtomicUsize;
use tune_core::live::{Opts, Route, Source};
struct Refused;
impl Route for Refused {
    fn open(&self, _: &tune_core::nodes::Node, _: &Opts) -> Result<Source, String> {
        panic!("passive fixture must not open a stream")
    }
}
fn fixture(label: &str) -> (std::path::PathBuf, Arc<Engine>, Arc<MonitorHost>) {
    let dir = std::env::temp_dir().join(format!("gpui-schedule-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true}))).unwrap();
    engine.set_schedule(&json!({"enabled":true})).unwrap();
    let live = Live::new(Arc::new(Refused), Opts::default(), Box::new(|_| {}));
    let host = Arc::new(MonitorHost::new(engine.clone(), live, Handle::current(), Arc::new(AtomicBool::new(false))));
    (dir, engine, host)
}
async fn advance(seconds: u64) {
    tokio::time::advance(Duration::from_secs(seconds)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}
#[tokio::test(start_paused = true)]
async fn passive_saved_enabled_and_rejected_start_never_authorize_work() {
    let (dir, engine, host) = fixture("passive");
    for _ in 0..3 {
        crate::dashboard::load_saved(&engine, 60).unwrap();
        host.snapshot();
    }
    advance(500).await;
    assert!(host.state.lock().unwrap().scheduler.is_none());
    assert!(host.state.lock().unwrap().ticker.is_none());
    assert_eq!(engine.monitor_journal_health()["running"], false);
    host.save_schedule_settings(json!({"enabled":false})).unwrap();
    assert!(host.start_schedule().is_err());
    host.save_schedule_settings(json!({"enabled":true})).unwrap();
    advance(500).await;
    assert!(!host.schedule_snapshot().0);
    assert_eq!(host.schedule_snapshot().1.cycles, 0);
    assert!(host.state.lock().unwrap().scheduler.is_none());
    assert_eq!(host.shutdown().await.scheduler_complete, None);
    assert!(host.start_schedule().is_err());
    assert!(host.stop_schedule().is_err());
    assert!(host.save_schedule_settings(json!({"enabled":false})).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test(start_paused = true)]
async fn explicit_start_one_task_five_then_sixty_seconds_with_actual_engine_tick() {
    let (dir, engine, host) = fixture("cadence");
    host.start_schedule().unwrap();
    let id = host.state.lock().unwrap().scheduler.as_ref().unwrap().id();
    host.start_schedule().unwrap();
    assert_eq!(host.state.lock().unwrap().scheduler.as_ref().unwrap().id(), id);
    advance(0).await;
    advance(4).await;
    assert_eq!(host.schedule_snapshot().1.cycles, 0);
    advance(1).await;
    assert_eq!(host.schedule_snapshot().1.cycles, 1);
    assert!(engine.with_db(|d| d.get_meta("lastProbeAt")).unwrap().is_some());
    advance(59).await;
    assert_eq!(host.schedule_snapshot().1.cycles, 1);
    advance(1).await;
    assert_eq!(host.schedule_snapshot().1.cycles, 2);
    host.stop_schedule().unwrap();
    advance(120).await;
    assert_eq!(host.schedule_snapshot().1.cycles, 2);
    assert_eq!(host.shutdown().await.scheduler_complete, Some(true));
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test(start_paused = true)]
async fn ticks_wait_for_completion_and_stop_close_drain_inflight() {
    for close in [false, true] {
        let (dir, engine, host) = fixture(if close { "close" } else { "stop" });
        let gate = Arc::new(tokio::sync::Notify::new());
        let count = Arc::new(AtomicUsize::new(0));
        let (g, c) = (gate.clone(), count.clone());
        host.start_schedule_with(move |engine| {
            let (g, c) = (g.clone(), c.clone());
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                g.notified().await;
                engine.checked_tick().await
            }
        })
        .unwrap();
        advance(0).await;
        advance(5).await;
        advance(120).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(host.schedule_snapshot().1.cycles, 0);
        let h = host.clone();
        let drain = if close {
            host.request_exit();
            Some(tokio::spawn(async move { h.shutdown().await }))
        } else {
            host.stop_schedule().unwrap();
            None
        };
        advance(120).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(drain.as_ref().is_none_or(|t| !t.is_finished()));
        gate.notify_one();
        advance(0).await;
        assert!(engine.with_db(|d| d.get_meta("lastProbeAt")).unwrap().is_some());
        advance(120).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let report = match drain {
            Some(d) => d.await.unwrap(),
            None => host.shutdown().await,
        };
        assert_eq!(report.scheduler_complete, Some(true));
        assert!(report.confirmed());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
#[tokio::test(start_paused = true)]
async fn active_manual_skips_cycle_and_latest_input_failure_revokes_permission() {
    let (dir, engine, host) = fixture("manual");
    let mut job = Job::default();
    let (release, wait) = std::sync::mpsc::channel();
    host.start_manual_with(&mut job, Kind::Logs, move || {
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
        Receipt::error(Kind::Logs, "synthetic")
    })
    .unwrap();
    host.start_schedule().unwrap();
    advance(0).await;
    advance(5).await;
    assert_eq!(host.schedule_snapshot().1.phase, SchedulePhase::ManualBusy);
    assert_eq!(host.schedule_snapshot().1.cycles, 0);
    release.send(()).unwrap();
    while !host.state.lock().unwrap().manual.as_ref().unwrap().is_finished() {
        tokio::task::yield_now().await;
    }
    advance(60).await;
    assert_eq!(host.schedule_snapshot().1.cycles, 1);
    std::fs::write(dir.join("nodes.json"), "invalid").unwrap();
    advance(60).await;
    assert_eq!(host.schedule_snapshot().1.phase, SchedulePhase::Failed);
    assert!(!host.schedule_snapshot().0);
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    host.save_schedule_settings(json!({"enabled":true})).unwrap();
    advance(120).await;
    assert_eq!(host.schedule_snapshot().1.cycles, 1);
    host.start_schedule().unwrap();
    advance(0).await;
    engine.with_db(|d| d.conn().execute_batch("DROP TABLE meta")).unwrap();
    advance(5).await;
    assert_eq!(host.schedule_snapshot().1.phase, SchedulePhase::Failed);
    assert!(!host.schedule_snapshot().0);
    assert!(host.start_schedule().is_err());
    host.shutdown().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(start_paused = true)]
async fn close_before_first_poll_and_failed_producer_completion_are_truthful() {
    let (dir, _, host) = fixture("early-exit");
    host.start_schedule().unwrap();
    let report = host.shutdown().await;
    assert_eq!(report.scheduler_complete, Some(true));
    assert_eq!(host.schedule_snapshot().1.cycles, 0);
    std::fs::remove_dir_all(dir).unwrap();
    let (dir, _, host) = fixture("failed-producer");
    host.start_schedule_with(|_| async { panic!("synthetic failed scheduler") }).unwrap();
    advance(0).await;
    advance(5).await;
    let report = host.shutdown().await;
    assert_eq!(report.scheduler_complete, Some(false));
    assert!(!report.confirmed());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(start_paused = true)]
async fn minute_cadence_never_overlaps_an_unfinished_tick() {
    let (dir, _, host) = fixture("serial");
    let gate = Arc::new(tokio::sync::Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let (g, c) = (gate.clone(), count.clone());
    host.start_schedule_with(move |engine| {
        let (g, c) = (g.clone(), c.clone());
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            g.notified().await;
            engine.checked_tick().await
        }
    })
    .unwrap();
    advance(0).await;
    advance(5).await;
    advance(60).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    gate.notify_one();
    advance(0).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    gate.notify_one();
    advance(0).await;
    advance(59).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    advance(1).await;
    assert_eq!(count.load(Ordering::SeqCst), 3);
    host.stop_schedule().unwrap();
    gate.notify_one();
    assert_eq!(host.shutdown().await.scheduler_complete, Some(true));
    std::fs::remove_dir_all(dir).unwrap();
}
