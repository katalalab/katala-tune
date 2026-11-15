use super::*;
use crate::live::{Ended, Owner, Route, Source, Stop};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{mpsc, oneshot, watch};

struct FakeRoute;
impl Route for FakeRoute {
    fn open(&self, node: &Node, _: &Opts) -> Result<Source, String> {
        if node.id == "failed" {
            return Err("synthetic unavailable".into());
        }
        let (tx, rx) = mpsc::channel(4);
        let (stop, mut stopped) = oneshot::channel();
        let (done, finished) = oneshot::channel();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(5));
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    _ = iv.tick() => if tx.send(r#"{"type":"s","cpu":5}"#.into()).await.is_err() { break; }
                }
            }
            let _ = done.send(Ended::default());
        });
        Ok(Source::new(rx, stop, finished))
    }
}
static SEQ: AtomicUsize = AtomicUsize::new(0);
fn fixture() -> (Arc<Engine>, Arc<Live>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("monitor-control-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[{"id":"a","os":"macos"},{"id":"failed","os":"macos"}]}"#).unwrap();
    let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(crate::engine::NoHost)).unwrap();
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true}))).unwrap();
    let live = Live::new(Arc::new(FakeRoute), Opts { max_retries: 0, ..Opts::default() }, Box::new(|_| {}));
    (engine, live, dir)
}
fn channels(mode: RunMode) -> (watch::Sender<RunRequest>, watch::Receiver<RunRequest>, watch::Sender<RunStatus>, watch::Receiver<RunStatus>) {
    let (tx, rx) = watch::channel(RunRequest { mode, pull_seq: 0 });
    let (stx, srx) = watch::channel(RunStatus { phase: RunPhase::Starting, policy: None, local_collecting: None, hub_configured: false });
    (tx, rx, stx, srx)
}
async fn phase(rx: &mut watch::Receiver<RunStatus>, wanted: RunPhase) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while rx.borrow_and_update().phase != wanted {
            rx.changed().await.unwrap();
        }
    })
    .await
    .expect("worker must acknowledge the requested phase");
}
#[tokio::test]
async fn controlled_paused_never_collects_with_enabled_settings() {
    let (engine, live, dir) = fixture();
    let (tx, rx, stx, mut status) = channels(RunMode::Paused);
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, rx, stx, None));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(live.active().is_empty(), "paused session cannot authorize saved enabled settings");
    phase(&mut status, RunPhase::Paused).await;
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn controlled_pause_resume_and_invalid_ledger_release_only_monitor() {
    let (engine, live, dir) = fixture();
    let (tx, rx, stx, mut status) = channels(RunMode::Run);
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, rx, stx, None));
    phase(&mut status, RunPhase::Running).await;
    assert!(live.heartbeat_owned(Owner::Monitor, "a"));
    tokio::time::timeout(Duration::from_secs(2), async {
        while live.samples("a").len() < 3 || live.brief("failed").is_none_or(|b| b["state"] != "stopped") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let collector = engine.monitor_journal_health()["collector_id"].clone();
    tx.send_modify(|r| r.mode = RunMode::Paused);
    phase(&mut status, RunPhase::Paused).await;
    assert!(!live.heartbeat_owned(Owner::Monitor, "a"));
    assert!(live.heartbeat_owned(Owner::Ui, "a"));
    assert_eq!(engine.monitor_journal_health()["running"], true);
    assert_eq!(engine.monitor_journal_health()["collector_id"], collector);
    tx.send_modify(|r| r.mode = RunMode::Run);
    phase(&mut status, RunPhase::Running).await;
    assert!(live.heartbeat_owned(Owner::Monitor, "a"));
    assert_eq!(engine.monitor_journal_health()["collector_id"], collector);
    std::fs::write(dir.join("nodes.json"), "broken").unwrap();
    tx.send_modify(|_| {});
    phase(&mut status, RunPhase::Failed).await;
    assert_eq!(status.borrow().local_collecting, None);
    assert!(!live.heartbeat_owned(Owner::Monitor, "a"));
    assert!(live.heartbeat_owned(Owner::Ui, "a"));
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.release_owned(Owner::Ui, &live.active(), Stop::User);
    live.shutdown(Duration::from_secs(1)).await;
    assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
    std::fs::remove_dir_all(dir).unwrap();
}

struct HubRunner {
    calls: AtomicUsize,
    fresh: std::sync::atomic::AtomicBool,
    gate: Option<Arc<tokio::sync::Notify>>,
    more: bool,
}
impl collect::Runner for HubRunner {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], _: Option<&'a [u8]>, timeout: Duration) -> collect::RunFuture<'a> {
        assert_eq!(cmd, "ssh");
        assert!(args.last().unwrap().contains("export --since"));
        assert_eq!(timeout, Duration::from_secs(60));
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0
                && let Some(gate) = &self.gate
            {
                gate.notified().await;
            }
            let now = now_ms();
            let minute = if self.fresh.load(Ordering::SeqCst) { now - MINUTE } else { now - 10 * MINUTE };
            collect::RunResult { code: Some(0), err: String::new(), out: json!({"v":1,"host":"synthetic","at":now,"next":now,"more":self.more,"latest":{"a":minute},"metrics":[{"node_id":"a","minute":minute,"n":1,"v":{"cpu":[2,2]}}],"snapshots":[]}).to_string() }
        })
    }
}
fn hub(engine: &Engine) {
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true,"hub":"a"}))).unwrap();
}
#[tokio::test]
async fn controlled_fresh_hub_does_not_start_local_then_stale_and_db_error_transition() {
    let (engine, live, dir) = fixture();
    hub(&engine);
    let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: true.into(), gate: None, more: false });
    let (tx, rx, stx, mut status) = channels(RunMode::Run);
    let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, stx)), r.as_ref()).await });
    phase(&mut status, RunPhase::Running).await;
    assert_eq!(status.borrow().policy, Some(Policy::Hub));
    assert!(live.active().is_empty(), "fresh pull must be observed before acquiring local ownership");
    runner.fresh.store(false, Ordering::SeqCst);
    tx.send_modify(|r| r.pull_seq += 1);
    tokio::time::timeout(Duration::from_secs(2), async {
        while status.borrow_and_update().local_collecting != Some(true) {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(live.heartbeat_owned(Owner::Monitor, "a"));
    engine.with_db(|d| d.conn().execute_batch("DROP TABLE meta")).unwrap();
    tx.send_modify(|_| {});
    phase(&mut status, RunPhase::Failed).await;
    assert_eq!(status.borrow().policy, None);
    assert_eq!(status.borrow().local_collecting, None);
    assert!(!live.heartbeat_owned(Owner::Monitor, "a"));
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    engine.finish_monitor_journal(Duration::from_secs(1)).await;
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn controlled_paused_pull_coalesces_pending_requests_without_collection() {
    let (engine, live, dir) = fixture();
    hub(&engine);
    let gate = Arc::new(tokio::sync::Notify::new());
    let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: false.into(), gate: Some(gate.clone()), more: false });
    let (tx, rx, stx, mut status) = channels(RunMode::Paused);
    tx.send_modify(|r| r.pull_seq = 1);
    let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, stx)), r.as_ref()).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while runner.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for _ in 0..10 {
        tx.send_modify(|r| r.pull_seq += 1);
    }
    assert!(live.active().is_empty());
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while runner.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    phase(&mut status, RunPhase::Paused).await;
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    assert!(live.active().is_empty(), "explicit pull is never Start permission");
    assert!(engine.with_db(|d| d.get_meta("hubCursor")).unwrap().is_some());
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    engine.finish_monitor_journal(Duration::from_secs(1)).await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_exit_waits_for_current_transport_without_starting_collection() {
    let (engine, live, dir) = fixture();
    hub(&engine);
    let gate = Arc::new(tokio::sync::Notify::new());
    let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: false.into(), gate: Some(gate.clone()), more: false });
    let (tx, rx, stx, mut status) = channels(RunMode::Run);
    let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, stx)), r.as_ref()).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while runner.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tx.send_modify(|r| r.mode = RunMode::Exit);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!worker.is_finished(), "exit acknowledges completion, not cancellation of in-flight transport");
    gate.notify_one();
    phase(&mut status, RunPhase::Exited).await;
    worker.await.unwrap();
    assert!(live.active().is_empty());
    live.shutdown(Duration::from_secs(1)).await;
    engine.finish_monitor_journal(Duration::from_secs(1)).await;
    std::fs::remove_dir_all(dir).unwrap();
}
struct FailedHub;
impl collect::Runner for FailedHub {
    fn run<'a>(&'a self, _: &'a str, _: &'a [String], _: Option<&'a [u8]>, _: Duration) -> collect::RunFuture<'a> {
        Box::pin(async { collect::RunResult { code: Some(1), out: String::new(), err: "synthetic failure".into() } })
    }
}
#[tokio::test]
async fn controlled_failed_pull_notifies_saved_result_without_collecting() {
    let (engine, live, dir) = fixture();
    hub(&engine);
    let (tx, rx, stx, mut status) = channels(RunMode::Paused);
    tx.send_modify(|r| r.pull_seq = 1);
    let (e, l) = (engine.clone(), live.clone());
    let notified = Arc::new(AtomicUsize::new(0));
    let flag = notified.clone();
    let worker = tokio::spawn(async move {
        run_loop(
            e,
            l,
            Arc::new(move |_| {
                flag.fetch_add(1, Ordering::SeqCst);
            }),
            None,
            None,
            Some((rx, stx)),
            &FailedHub,
        )
        .await
    });
    phase(&mut status, RunPhase::Paused).await;
    assert_eq!(engine.with_db(|d| d.get_meta("hubPull")).unwrap().unwrap()["ok"], false);
    assert_eq!(notified.load(Ordering::SeqCst), 1, "saved failure must wake the reader too");
    assert!(live.active().is_empty());
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    engine.finish_monitor_journal(Duration::from_secs(1)).await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_paused_checks_full_ledger_removal_without_acquiring_monitor() {
    let (engine, live, dir) = fixture();
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    let (tx, rx, stx, mut status) = channels(RunMode::Paused);
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, rx, stx, None));
    phase(&mut status, RunPhase::Paused).await;
    assert!(!live.heartbeat_owned(Owner::Monitor, "a"));
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    tx.send_modify(|_| {});
    tokio::time::timeout(Duration::from_millis(300), async {
        while !live.active().is_empty() {
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("validated full ledger removal must also apply while paused");
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    engine.finish_monitor_journal(Duration::from_secs(1)).await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_disabled_hub_pull_keeps_page_bound_and_cursor() {
    let (engine, live, dir) = fixture();
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":false,"hub":"a"}))).unwrap();
    let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: true.into(), gate: None, more: true });
    let (tx, rx, stx, mut status) = channels(RunMode::Run);
    let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, stx)), r.as_ref()).await });
    phase(&mut status, RunPhase::Running).await;
    assert_eq!(status.borrow().policy, Some(Policy::Disabled));
    assert_eq!(status.borrow().local_collecting, Some(false));
    assert!(live.active().is_empty());
    assert_eq!(runner.calls.load(Ordering::SeqCst), 16);
    let receipt = engine.with_db(|d| d.get_meta("hubPull")).unwrap().unwrap();
    assert_eq!(receipt["more"], true);
    assert_eq!(receipt["pages"], 16);
    assert_eq!(engine.with_db(|d| d.get_meta("hubCursor")).unwrap().unwrap(), receipt["next"]);
    assert_eq!(engine.with_db(|d| d.metrics(Some("a"), 0, i64::MAX)).unwrap()[0].v["cpu"], json!([2, 2]));
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    engine.finish_monitor_journal(Duration::from_secs(1)).await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_hub_change_wakes_pull_and_uses_new_hub_freshness() {
    let (engine, live, dir) = fixture();
    hub(&engine);
    let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: true.into(), gate: None, more: false });
    let (tx, rx, stx, mut status) = channels(RunMode::Run);
    let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, stx)), r.as_ref()).await });
    phase(&mut status, RunPhase::Running).await;
    assert_eq!(status.borrow().policy, Some(Policy::Hub));
    runner.fresh.store(false, Ordering::SeqCst);
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true,"hub":"failed"}))).unwrap();
    tx.send_modify(|_| {});
    let switched = tokio::time::timeout(Duration::from_millis(300), async {
        while engine.with_db(|d| d.get_meta("hubPull")).unwrap().unwrap()["hub"] != "failed" {
            status.changed().await.unwrap();
        }
    })
    .await;
    assert!(switched.is_ok(), "a settings wake must synchronize the new hub without waiting for the old hub's minute interval");
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    assert_eq!(status.borrow().policy, Some(Policy::Local));
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    engine.finish_monitor_journal(Duration::from_secs(1)).await;
    std::fs::remove_dir_all(dir).unwrap();
}
