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
            tx.send(r#"{"type":"hello","impl":"rust","agent":"0.0.1"}"#.into()).await.unwrap();
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
    let (tx, rx) = watch::channel(RunRequest { mode, pull_seq: 0, start_seq: 0 });
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
    let (tx, _rx, stx, mut status) = channels(RunMode::Paused);
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx, None));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(live.active().is_empty(), "paused session cannot authorize saved enabled settings");
    phase(&mut status, RunPhase::Paused).await;
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn controlled_pause_resume_and_invalid_ledger_release_only_monitor() {
    let (engine, live, dir) = fixture();
    let (tx, _rx, stx, mut status) = channels(RunMode::Run);
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx, None));
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
    tx.send_modify(|r| {
        r.mode = RunMode::Run;
        r.start_seq += 1;
    });
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
    drop((live, engine));
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
    let worker_request = tx.clone();
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, worker_request, stx, None)), r.as_ref()).await });
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
    drop((live, engine));
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
    let worker_request = tx.clone();
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, worker_request, stx, None)), r.as_ref()).await });
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
    drop((live, engine));
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
    let worker_request = tx.clone();
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, worker_request, stx, None)), r.as_ref()).await });
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
    drop((live, engine));
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
    let worker_request = tx.clone();
    let worker = tokio::spawn(async move {
        run_loop(
            e,
            l,
            Arc::new(move |_| {
                flag.fetch_add(1, Ordering::SeqCst);
            }),
            None,
            None,
            Some((rx, worker_request, stx, None)),
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
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_paused_checks_full_ledger_removal_without_acquiring_monitor() {
    let (engine, live, dir) = fixture();
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    let (tx, _rx, stx, mut status) = channels(RunMode::Paused);
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx, None));
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
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_disabled_hub_pull_keeps_page_bound_and_cursor() {
    let (engine, live, dir) = fixture();
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":false,"hub":"a"}))).unwrap();
    let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: true.into(), gate: None, more: true });
    let (tx, rx, stx, mut status) = channels(RunMode::Run);
    let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
    let worker_request = tx.clone();
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, worker_request, stx, None)), r.as_ref()).await });
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
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_hub_change_wakes_pull_and_uses_new_hub_freshness() {
    let (engine, live, dir) = fixture();
    hub(&engine);
    let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: true.into(), gate: None, more: false });
    let (tx, rx, stx, mut status) = channels(RunMode::Run);
    let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
    let worker_request = tx.clone();
    let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, worker_request, stx, None)), r.as_ref()).await });
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
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_releases_monitor_before_new_noncollecting_pull_preserving_ui() {
    // Both transitions start with real Local ownership; the hub gate makes the
    // pre-pull boundary observable before any transport result can hide it.
    for paused in [false, true] {
        let (engine, live, dir) = fixture();
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[{"id":"a","os":"macos"},{"id":"b","os":"macos"}]}"#).unwrap();
        live.start_owned(Owner::Ui, &engine.reload_config().nodes[..1], &[]);
        let gate = Arc::new(tokio::sync::Notify::new());
        let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: true.into(), gate: Some(gate.clone()), more: false });
        let (tx, rx, stx, mut status) = channels(RunMode::Run);
        let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
        let worker_request = tx.clone();
        let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, worker_request, stx, None)), r.as_ref()).await });
        phase(&mut status, RunPhase::Running).await;
        assert!(live.heartbeat_owned(Owner::Monitor, "a"));
        assert!(live.heartbeat_owned(Owner::Monitor, "b"));
        tokio::time::timeout(Duration::from_secs(2), async {
            while live.samples("a").is_empty() || live.samples("b").is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":paused,"hub":"a"}))).unwrap();
        tx.send_modify(|request| {
            if paused {
                request.mode = RunMode::Paused;
                request.pull_seq += 1;
            }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while runner.calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let monitor_a = live.heartbeat_owned(Owner::Monitor, "a");
        let monitor_b = live.heartbeat_owned(Owner::Monitor, "b");
        let ui = live.heartbeat_owned(Owner::Ui, "a");
        let active = live.active();
        let before = live.samples("a").len();
        tokio::time::timeout(Duration::from_secs(2), async {
            while live.samples("a").len() <= before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("UI samples must continue while the hub transport is blocked");
        assert!(!worker.is_finished());
        tx.send_modify(|request| request.mode = RunMode::Exit);
        gate.notify_one();
        worker.await.unwrap();
        live.shutdown(Duration::from_secs(1)).await;
        assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
        drop((live, engine));
        std::fs::remove_dir_all(dir).unwrap();
        assert!(!monitor_a && !monitor_b, "paused={paused}: release Monitor ownership before entering the new blocked hub pull");
        assert!(ui, "paused={paused}: releasing Monitor must preserve the UI claim");
        assert_eq!(active, ["a"], "paused={paused}: stop the Monitor-only stream before pull, without stopping the shared UI stream");
    }
}

struct CountedRoute(Arc<AtomicUsize>);
impl Route for CountedRoute {
    fn open(&self, node: &Node, opts: &Opts) -> Result<Source, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        FakeRoute.open(node, opts)
    }
}

#[tokio::test(start_paused = true)]
async fn controlled_worker_input_failure_requires_explicit_restart_after_repair() {
    for failure in ["ledger", "json", "hub", "database"] {
        let (engine, _, dir) = fixture();
        let opens = Arc::new(AtomicUsize::new(0));
        let live = Live::new(Arc::new(CountedRoute(opens.clone())), Opts::default(), Box::new(|_| {}));
        let original = r#"{"nodes":[{"id":"a","os":"macos"},{"id":"b","os":"macos"}]}"#;
        std::fs::write(dir.join("nodes.json"), original).unwrap();
        engine.reload_config();
        hub(&engine);
        live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
        let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: false.into(), gate: None, more: false });
        let (tx, rx, stx, mut status) = channels(RunMode::Run);
        let (e, l, r) = (engine.clone(), live.clone(), runner.clone());
        let worker_request = tx.clone();
        let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, worker_request, stx, None)), r.as_ref()).await });
        phase(&mut status, RunPhase::Running).await;
        assert!(live.heartbeat_owned(Owner::Monitor, "a"));
        let collector = engine.monitor_journal_health()["collector_id"].clone();
        match failure {
            "ledger" => std::fs::write(dir.join("nodes.json"), "broken").unwrap(),
            "database" => engine.with_db(|d| d.conn().execute_batch("ALTER TABLE meta RENAME TO broken_meta")).unwrap(),
            raw => {
                let value = if raw == "json" { "{broken" } else { r#"{"enabled":true,"hub":123}"# };
                engine.with_db(|d| d.conn().execute("UPDATE meta SET v=? WHERE k='monitor'", [value])).unwrap();
            }
        }
        tokio::time::advance(Duration::from_secs(15)).await;
        phase(&mut status, RunPhase::Failed).await;
        assert_eq!(status.borrow().local_collecting, None);
        assert!(!live.heartbeat_owned(Owner::Monitor, "a"));
        assert!(live.heartbeat_owned(Owner::Ui, "a"));
        assert_eq!(tx.borrow().mode, RunMode::Paused);
        assert_eq!(live.active(), ["a"], "Monitor-only stream must be released");
        let samples = live.samples("a").len();
        let pulls = runner.calls.load(Ordering::SeqCst);
        let opened = opens.load(Ordering::SeqCst);
        std::fs::write(dir.join("nodes.json"), original).unwrap();
        if failure == "database" {
            engine.with_db(|d| d.conn().execute_batch("ALTER TABLE broken_meta RENAME TO meta")).unwrap();
        }
        engine.with_db(|d| Ok(set_settings(d, &engine.config(), &json!({"enabled":true,"hub":"b"})).unwrap())).unwrap();
        tx.send_modify(|_| {}); // the settings-save wake, without Start
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(15)).await;
            tokio::task::yield_now().await;
        }
        let resumed = live.heartbeat_owned(Owner::Monitor, "a");
        let extra_pulls = runner.calls.load(Ordering::SeqCst) - pulls;
        assert_eq!(opens.load(Ordering::SeqCst), opened, "repair/save/period cannot open a new stream");
        let ui_continues = live.heartbeat_owned(Owner::Ui, "a") && live.samples("a").len() > samples;
        // Explicit paused Pull may synchronize, but must not authorize Local.
        tx.send_modify(|r| r.pull_seq += 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while runner.calls.load(Ordering::SeqCst) == pulls {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        phase(&mut status, RunPhase::Paused).await;
        assert_eq!(runner.calls.load(Ordering::SeqCst), pulls + 1);
        assert_eq!(opens.load(Ordering::SeqCst), opened);
        let pull_collected = live.heartbeat_owned(Owner::Monitor, "a");
        tx.send_modify(|r| {
            r.mode = RunMode::Run;
            r.start_seq += 1;
        });
        phase(&mut status, RunPhase::Running).await;
        assert!(live.heartbeat_owned(Owner::Monitor, "a"));
        assert_eq!(engine.monitor_journal_health()["collector_id"], collector);
        tx.send_modify(|r| r.mode = RunMode::Exit);
        worker.await.unwrap();
        live.shutdown(Duration::from_secs(1)).await;
        assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
        drop((live, engine));
        std::fs::remove_dir_all(dir).unwrap();
        assert!(!resumed && extra_pulls == 0 && !pull_collected, "{failure}: repair/save/time/pull cannot grant Run permission");
        assert!(ui_continues, "{failure}: Monitor failure must preserve UI sampling");
    }
}

#[tokio::test]
async fn controlled_old_pull_failure_cannot_revoke_new_start_or_exit() {
    for exit in [false, true] {
        let (engine, live, dir) = fixture();
        hub(&engine);
        live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
        let gate = Arc::new(tokio::sync::Notify::new());
        let runner = Arc::new(HubRunner { calls: AtomicUsize::new(0), fresh: false.into(), gate: Some(gate.clone()), more: false });
        let (tx, rx, stx, mut status) = channels(RunMode::Run);
        let (e, l, r, request) = (engine.clone(), live.clone(), runner.clone(), tx.clone());
        let worker = tokio::spawn(async move { run_loop(e, l, Arc::new(|_| {}), None, None, Some((rx, request, stx, None)), r.as_ref()).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while runner.calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if exit {
            engine.with_db(|d| d.conn().execute("UPDATE meta SET v=? WHERE k='monitor'", ["{broken"])).unwrap();
            tx.send_modify(|r| r.mode = RunMode::Exit);
        } else {
            // The older pull's settings no longer match. A validated new Start
            // owns the session before that queued failure is processed.
            engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true,"hub":null}))).unwrap();
            controlled_inputs(&engine).unwrap();
            tx.send_modify(|r| {
                r.start_seq += 1;
                r.mode = RunMode::Run;
            });
        }
        assert!(!worker.is_finished(), "accepted transport must be drained");
        gate.notify_one();
        phase(&mut status, if exit { RunPhase::Exited } else { RunPhase::Running }).await;
        assert_eq!(tx.borrow().mode, if exit { RunMode::Exit } else { RunMode::Run });
        assert_eq!(live.heartbeat_owned(Owner::Monitor, "a"), !exit);
        assert!(live.heartbeat_owned(Owner::Ui, "a"));
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        tx.send_modify(|r| r.mode = RunMode::Exit);
        worker.await.unwrap();
        live.shutdown(Duration::from_secs(1)).await;
        assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
        drop((live, engine));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn controlled_unset_settings_are_valid_and_legacy_malformed_defaults_stay_permissive() {
    let (engine, live, dir) = fixture();
    engine.with_db(|d| d.conn().execute("DELETE FROM meta WHERE k='monitor'", [])).unwrap();
    assert_eq!(controlled_inputs(&engine).unwrap().1, Settings::default());
    for raw in ["{broken", "null", r#"{"enabled":"invalid"}"#, r#"{"enabled":true,"hub":123}"#] {
        engine.with_db(|d| d.set_meta("monitor", &Value::Null)).unwrap();
        engine.with_db(|d| d.conn().execute("UPDATE meta SET v=? WHERE k='monitor'", [raw])).unwrap();
        assert!(controlled_inputs(&engine).is_err());
        let legacy = engine.with_db(read_settings).unwrap();
        assert_eq!(legacy.enabled, raw.contains("true"));
        assert_eq!(legacy.hub, None);
    }
    live.shutdown(Duration::from_secs(1)).await;
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn controlled_valid_ledger_removal_wins_over_malformed_settings() {
    let (engine, live, dir) = fixture();
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    let (tx, _rx, stx, mut status) = channels(RunMode::Run);
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx, None));
    phase(&mut status, RunPhase::Running).await;
    assert!(live.heartbeat_owned(Owner::Ui, "a"));
    assert!(live.heartbeat_owned(Owner::Monitor, "a"));
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true,"hub":123}))).unwrap();
    tx.send_modify(|_| {});
    phase(&mut status, RunPhase::Failed).await;
    let active = live.active();
    let ui = live.heartbeat_owned(Owner::Ui, "a");
    tx.send_modify(|r| r.mode = RunMode::Exit);
    worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
    assert!(active.is_empty() && !ui, "valid full-ledger removal must revoke even Ui ownership before rejecting malformed settings");
}

// Prepared native source and same-OS eligibility exist only for these tuples.
// Earlier controlled tests and the CPU parser tests stay OS-independent.
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
use crate::helpers::remote_update::{
    self, RemoteTargetSnapshot, RemoteUpdateControl, RemoteUpdateFuture, RemoteUpdateOutcome, RemoteUpdatePayload, RemoteUpdateReply, RemoteUpdateRequest,
    RemoteUpdateStage, RemoteUpdateTransport,
};
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
fn update_fixture() -> (Arc<Engine>, Arc<Live>, std::path::PathBuf) {
    let (engine, live, dir) = fixture();
    if cfg!(windows) {
        std::fs::write(dir.join("nodes.json"), r#"{"nodes":[{"id":"a","os":"windows"},{"id":"failed","os":"windows"}]}"#).unwrap();
        engine.reload_config();
    }
    (engine, live, dir)
}
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
struct HeldUpdate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
    reply: u8,
}
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
impl HeldUpdate {
    fn new(reply: u8) -> Arc<Self> {
        Arc::new(Self { entered: tokio::sync::Notify::new(), release: tokio::sync::Notify::new(), calls: std::sync::Mutex::new(Vec::new()), reply })
    }
}
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
impl RemoteUpdateTransport for HeldUpdate {
    fn place<'a>(&'a self, node: &'a Node, payload: RemoteUpdatePayload<'a>) -> RemoteUpdateFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((node.id.clone(), payload.bytes().to_vec()));
            self.entered.notify_one();
            self.release.notified().await;
            match self.reply {
                0 => RemoteUpdateReply::observed(Some(0), payload.expected_response().as_bytes(), RemoteUpdateStage::Placed, true),
                1 => RemoteUpdateReply::observed(Some(1), payload.expected_response().as_bytes(), RemoteUpdateStage::Placed, true),
                2 => RemoteUpdateReply::observed(Some(0), format!("placed-{}", "0".repeat(64)).as_bytes(), RemoteUpdateStage::Placed, true),
                3 => RemoteUpdateReply::observed(Some(2), b"partial", RemoteUpdateStage::Staged, true),
                4 => RemoteUpdateReply::observed(None, b"", RemoteUpdateStage::Placed, true),
                5 => panic!("synthetic update panic"),
                6 => RemoteUpdateReply::observed(Some(0), payload.expected_response().as_bytes(), RemoteUpdateStage::Staged, true),
                7 => RemoteUpdateReply::observed(Some(0), format!("unexpected {}", payload.expected_response()).as_bytes(), RemoteUpdateStage::Placed, true),
                8 => RemoteUpdateReply::observed(Some(0), payload.expected_response().as_bytes(), RemoteUpdateStage::Placed, false),
                _ => unreachable!(),
            }
        })
    }
}
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
fn update_request(engine: &Engine, dir: &std::path::Path, id: u64, seq: u64, ttl: Duration) -> RemoteUpdateRequest {
    let target = RemoteTargetSnapshot::checked(engine, "a", remote_update::tests::architecture()).unwrap();
    let prepared = remote_update::tests::prepared(dir, crate::helpers::VERSION);
    RemoteUpdateRequest::new(id, seq, std::time::Instant::now() + ttl, target, prepared).unwrap()
}
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
async fn outdated(engine: &Engine, live: &Live) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !crate::helpers::agent_outdated(&engine.config().nodes[0], live.brief("a").as_ref()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_exit_must_drain_fake_transport() {
    for exit in [false, true] {
        let (engine, live, dir) = update_fixture();
        let (tx, _rx, stx, mut status) = channels(RunMode::Run);
        live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
        outdated(&engine, &live).await;
        let transport = HeldUpdate::new(0);
        let updates = Arc::new(RemoteUpdateControl::new(transport.clone()));
        updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 1, 0, Duration::from_secs(120))).unwrap();
        let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx.clone(), Some(updates.clone())));
        transport.entered.notified().await;
        stx.send_modify(|s| s.phase = RunPhase::Stopping);
        tx.send_modify(|r| r.mode = if exit { RunMode::Exit } else { RunMode::Paused });
        // Accepted work precedes the later three-second Live/journal stage.
        tokio::time::sleep(Duration::from_millis(3100)).await;
        assert!(!worker.is_finished());
        assert_eq!(status.borrow().phase, RunPhase::Stopping);
        assert!(updates.busy());
        assert!(!updates.withdraw(1));
        assert_eq!(updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 2, 0, Duration::from_secs(120))), Err(remote_update::AdmissionError::Busy));
        assert!(live.heartbeat_owned(Owner::Ui, "a"));
        transport.release.notify_one();
        phase(&mut status, if exit { RunPhase::Exited } else { RunPhase::Paused }).await;
        assert_eq!(updates.outcome(), Some(RemoteUpdateOutcome::ConfirmedPlacement { attempt_id: 1 }));
        assert!(live.heartbeat_owned(Owner::Ui, "a"));
        tx.send_modify(|r| r.mode = RunMode::Exit);
        let result = worker.await.unwrap();
        assert_eq!(result.update_complete, Some(true));
        assert_eq!(result.remote_update, updates.outcome());
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
        live.shutdown(Duration::from_secs(1)).await;
        assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
        drop((live, engine));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_target_only_and_immutable_prepared_source() {
    let (engine, live, dir) = update_fixture();
    let os = if cfg!(windows) { "windows" } else { "macos" };
    let ledger = json!({"nodes":[{"id":"a","os":os},{"id":"b","os":os},{"id":"c","os":os},{"id":"deferred","os":os,"status":"paused"}]});
    std::fs::write(dir.join("nodes.json"), ledger.to_string()).unwrap();
    let cfg = engine.reload_config();
    live.start_owned(Owner::Ui, &cfg.nodes[..3], &[]);
    outdated(&engine, &live).await;
    let (tx, _rx, stx, _status) = channels(RunMode::Run);
    let transport = HeldUpdate::new(0);
    let updates = Arc::new(RemoteUpdateControl::new(transport.clone()));
    let update = update_request(&engine, &dir, 1, 0, Duration::from_secs(120));
    let expected = std::fs::read(dir.join("synthetic-source").join(crate::helpers::exe("tune-agent"))).unwrap();
    updates.admit(&engine, &live, &tx, update).unwrap();
    std::fs::write(dir.join("synthetic-source").join(crate::helpers::exe("tune-agent")), b"replaced source sentinel").unwrap();
    assert!(crate::helpers::native_install::prepare_native_sampler(&dir.join("synthetic-source")).is_err());
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx, Some(updates.clone())));
    transport.entered.notified().await;
    // Post-claim replacement never redirects the already accepted endpoint.
    std::fs::write(dir.join("nodes.json"), json!({"nodes":[{"id":"a","os":os,"alias":"replacement"}]}).to_string()).unwrap();
    transport.release.notify_one();
    tx.send_modify(|r| r.mode = RunMode::Exit);
    assert_eq!(worker.await.unwrap().remote_update, Some(RemoteUpdateOutcome::ConfirmedPlacement { attempt_id: 1 }));
    assert_eq!(*transport.calls.lock().unwrap(), vec![("a".into(), expected)]);
    assert!(live.heartbeat_owned(Owner::Ui, "a"));
    live.shutdown(Duration::from_secs(1)).await;
    assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_rechecks_changed_or_invalid_inputs_before_claim() {
    for scenario in ["missing", "duplicate", "alias", "raw", "status", "os", "local", "ledger", "settings", "disabled", "hub-fresh", "start", "expiry"] {
        let (engine, live, dir) = update_fixture();
        let (tx, _rx, stx, mut status) = channels(RunMode::Run);
        live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
        outdated(&engine, &live).await;
        let transport = HeldUpdate::new(0);
        let updates = Arc::new(RemoteUpdateControl::new(transport.clone()));
        updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 1, 0, Duration::from_secs(120))).unwrap();
        assert!(updates.busy(), "{scenario}");
        assert_eq!(updates.outcome(), None, "{scenario}");
        let mut ledger: Value = serde_json::from_slice(&std::fs::read(dir.join("nodes.json")).unwrap()).unwrap();
        match scenario {
            "missing" => {
                ledger["nodes"] = json!([]);
            }
            "duplicate" => {
                let node = ledger["nodes"][0].clone();
                ledger["nodes"].as_array_mut().unwrap().push(node);
            }
            "alias" => ledger["nodes"][0]["alias"] = json!("changed"),
            "raw" => ledger["nodes"][0]["ssh_shell"] = json!("changed"),
            "status" => ledger["nodes"][0]["status"] = json!("paused"),
            "os" => ledger["nodes"][0]["os"] = json!("unsupported"),
            "local" => ledger["nodes"][0]["local_hostname"] = json!(crate::nodes::local_host()),
            "settings" => {
                engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":"broken"}))).unwrap();
            }
            "disabled" => {
                engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":false}))).unwrap();
            }
            "hub-fresh" => {
                hub(&engine);
                engine.with_db(|d| d.set_meta("hubPull", &json!({"ok":true,"at":now_ms(),"fresh":true}))).unwrap();
            }
            "start" => tx.send_modify(|r| r.start_seq += 1),
            "expiry" => remote_update::tests::expire_pending(&updates),
            "ledger" => {}
            _ => unreachable!(),
        }
        std::fs::write(dir.join("nodes.json"), if scenario == "ledger" { "broken".into() } else { ledger.to_string() }).unwrap();
        // A fresh-hub policy must refuse without starting a pull. All other
        // cases run the same production controlled worker with fake routes.
        if scenario == "hub-fresh" {
            assert!(updates.claim(&engine, &live, &tx).is_none());
            assert_eq!(updates.outcome(), Some(RemoteUpdateOutcome::Refused { attempt_id: 1, reason: remote_update::Refusal::Policy }));
        } else {
            let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx, Some(updates.clone())));
            tokio::time::timeout(Duration::from_secs(2), async {
                while updates.outcome().is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            if scenario == "expiry" {
                assert_eq!(updates.outcome(), Some(RemoteUpdateOutcome::Refused { attempt_id: 1, reason: remote_update::Refusal::Expired }));
            } else {
                assert!(matches!(updates.outcome(), Some(RemoteUpdateOutcome::Refused { .. })), "{scenario}");
            }
            tx.send_modify(|r| r.mode = RunMode::Exit);
            phase(&mut status, RunPhase::Exited).await;
            worker.await.unwrap();
        }
        assert!(transport.calls.lock().unwrap().is_empty(), "{scenario}");
        live.shutdown(Duration::from_secs(1)).await;
        assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
        drop((live, engine));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_pending_withdrawal_busy_and_attempt_binding() {
    let (engine, live, dir) = update_fixture();
    let (tx, _rx, _stx, _status) = channels(RunMode::Run);
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    outdated(&engine, &live).await;
    let transport = HeldUpdate::new(0);
    let updates = RemoteUpdateControl::new(transport.clone());
    updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 1, 0, Duration::from_secs(120))).unwrap();
    assert_eq!(updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 2, 0, Duration::from_secs(120))), Err(remote_update::AdmissionError::Busy));
    assert!(updates.withdraw(1));
    assert_eq!(
        updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 1, 0, Duration::from_secs(120))),
        Err(remote_update::AdmissionError::Refused(remote_update::Refusal::InvalidRequest))
    );
    updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 2, 0, Duration::from_secs(120))).unwrap();
    assert!(!updates.withdraw(1));
    assert!(updates.busy());
    // Same short watch -> slot order used by host Pause/Exit/new Start.
    tx.send_modify(|r| {
        updates.refuse_pending(remote_update::Refusal::Session);
        r.mode = RunMode::Paused;
    });
    tx.send_modify(|r| {
        r.start_seq += 1;
        r.mode = RunMode::Run;
    });
    assert!(updates.claim(&engine, &live, &tx).is_none());
    assert!(transport.calls.lock().unwrap().is_empty());
    let target = RemoteTargetSnapshot::checked(&engine, "a", None);
    assert!(target.is_err());
    let target = RemoteTargetSnapshot::checked(&engine, "a", remote_update::tests::architecture()).unwrap();
    assert!(matches!(
        RemoteUpdateRequest::new(3, 1, std::time::Instant::now() + Duration::from_secs(120), target, remote_update::tests::prepared(&dir, "0.0.1")),
        Err(remote_update::Refusal::Source)
    ));
    live.shutdown(Duration::from_secs(1)).await;
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_unknown_is_sticky_without_retry_or_unrelated_policy_change() {
    for reply in 1..=8 {
        let (engine, live, dir) = update_fixture();
        let (tx, _rx, stx, mut status) = channels(RunMode::Run);
        live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
        outdated(&engine, &live).await;
        let settings = engine.with_db(|d| d.get_meta("monitor")).unwrap();
        let transport = HeldUpdate::new(reply);
        let updates = Arc::new(RemoteUpdateControl::new(transport.clone()));
        updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 1, 0, Duration::from_secs(120))).unwrap();
        let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx, Some(updates.clone())));
        transport.entered.notified().await;
        transport.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while updates.outcome().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let outcome = updates.outcome().unwrap();
        assert!(!outcome.placement_known());
        if reply == 3 {
            assert!(matches!(outcome, RemoteUpdateOutcome::Unknown { stage: RemoteUpdateStage::Staged, .. }));
        }
        tx.send_modify(|r| r.mode = RunMode::Paused);
        phase(&mut status, RunPhase::Paused).await;
        tx.send_modify(|r| {
            r.mode = RunMode::Run;
            r.start_seq += 1;
        });
        phase(&mut status, RunPhase::Running).await;
        assert_eq!(
            updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 2, 1, Duration::from_secs(120))),
            Err(remote_update::AdmissionError::Closed)
        );
        assert_eq!(updates.outcome(), Some(outcome));
        assert!(live.heartbeat_owned(Owner::Ui, "a"));
        assert_eq!(engine.with_db(|d| d.get_meta("monitor")).unwrap(), settings);
        tx.send_modify(|r| r.mode = RunMode::Exit);
        let exit = worker.await.unwrap();
        assert_eq!(exit.update_complete, Some(true));
        assert_eq!(exit.remote_update, Some(outcome));
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
        live.shutdown(Duration::from_secs(1)).await;
        assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
        drop((live, engine));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_pause_wins_while_claim_rechecks_blocked_inputs() {
    let (engine, live, dir) = update_fixture();
    let (tx, _rx, _stx, _status) = channels(RunMode::Run);
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    outdated(&engine, &live).await;
    let transport = HeldUpdate::new(0);
    let updates = Arc::new(RemoteUpdateControl::new(transport.clone()));
    updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 1, 0, Duration::from_secs(120))).unwrap();
    let db = engine.db.lock().unwrap();
    let (e, l, u, t) = (engine.clone(), live.clone(), updates.clone(), tx.clone());
    let (entered, ready) = std::sync::mpsc::channel();
    let (done, received) = std::sync::mpsc::channel();
    let claiming = std::thread::spawn(move || {
        entered.send(()).unwrap();
        let result = u.claim(&e, &l, &t);
        done.send(result.is_some()).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(received.recv_timeout(Duration::from_millis(30)).is_err(), "claim must wait at checked DB boundary");
    // The command must acquire watch and slot while claim IO is blocked.
    let (paused, acknowledged) = std::sync::mpsc::channel();
    let (pause_request, pause_updates) = (tx.clone(), updates.clone());
    let pause = std::thread::spawn(move || {
        pause_request.send_modify(|r| {
            pause_updates.refuse_pending(remote_update::Refusal::Session);
            r.mode = RunMode::Paused;
        });
        paused.send(()).unwrap();
    });
    let paused_before_io = acknowledged.recv_timeout(Duration::from_secs(1)).is_ok();
    drop(db);
    pause.join().unwrap();
    assert!(!received.recv_timeout(Duration::from_secs(2)).unwrap());
    claiming.join().unwrap();
    assert!(paused_before_io, "no request or slot guard may span checked IO");
    tx.send_modify(|r| {
        r.mode = RunMode::Run;
        r.start_seq = 1;
    });
    updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 2, 1, Duration::from_secs(120))).unwrap();
    updates.refuse_start(0, remote_update::Refusal::Inputs);
    assert!(updates.busy(), "an old cycle's refusal cannot withdraw a newer Start");
    assert!(!updates.withdraw(1));
    assert!(updates.withdraw(2));
    assert!(transport.calls.lock().unwrap().is_empty());
    live.shutdown(Duration::from_secs(1)).await;
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_rejects_prepared_binary_cpu_declared_as_other_cpu() {
    let (engine, live, dir) = update_fixture();
    let prepared = remote_update::tests::prepared(&dir, crate::helpers::VERSION);
    let source = prepared.source().to_path_buf();
    let sampler = source.join(crate::helpers::exe("tune-agent"));
    let mut bytes = std::fs::read(&sampler).unwrap();
    if cfg!(windows) {
        bytes[132..134].copy_from_slice(&0xaa64u16.to_le_bytes());
    } else {
        bytes[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
    }
    std::fs::write(sampler, &bytes).unwrap();
    let manifest = source.join("manifest.json");
    let mut raw: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    raw["files"].as_array_mut().unwrap().iter_mut().find(|f| f["role"] == "native-sampler").unwrap()["sha256"] = json!(crate::db::sha256_hex(&bytes));
    std::fs::write(manifest, serde_json::to_vec(&raw).unwrap()).unwrap();
    // Existing structural native prepare accepts either CPU. The stricter
    // remote boundary rejects this declaration/content disagreement.
    let prepared = crate::helpers::native_install::prepare_native_sampler(&source).unwrap();
    let target = RemoteTargetSnapshot::checked(&engine, "a", remote_update::tests::architecture()).unwrap();
    assert!(matches!(
        RemoteUpdateRequest::new(1, 0, std::time::Instant::now() + Duration::from_secs(120), target, prepared),
        Err(remote_update::Refusal::Source)
    ));
    live.shutdown(Duration::from_secs(1)).await;
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_coalesced_start_then_exit_finishes_old_pending() {
    let (engine, live, dir) = update_fixture();
    let (tx, _rx, stx, _status) = channels(RunMode::Run);
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    outdated(&engine, &live).await;
    let transport = HeldUpdate::new(0);
    let updates = Arc::new(RemoteUpdateControl::new(transport.clone()));
    updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 1, 0, Duration::from_secs(120))).unwrap();
    tx.send_modify(|r| {
        r.start_seq = 1;
        r.mode = RunMode::Run;
    });
    tx.send_modify(|r| r.mode = RunMode::Exit);
    // The actual controlled worker first polls only the latest coalesced value.
    let result = run_controlled(engine.clone(), live.clone(), |_| {}, tx, stx, Some(updates.clone())).await;
    let outcome = updates.outcome();
    let busy = updates.busy();
    updates.withdraw(1); // RED cleanup only, after observing the terminal facts.
    live.shutdown(Duration::from_secs(1)).await;
    assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(transport.calls.lock().unwrap().len(), 0);
    assert_eq!(outcome, Some(RemoteUpdateOutcome::Refused { attempt_id: 1, reason: remote_update::Refusal::Session }));
    assert!(!busy);
    assert_eq!(result.update_complete, Some(true));
    assert_eq!(result.remote_update, outcome);
}

#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64")))]
#[tokio::test]
async fn controlled_update_coalesced_start_then_pause_releases_old_pending_for_new_attempt() {
    let (engine, live, dir) = update_fixture();
    let (tx, _rx, stx, mut status) = channels(RunMode::Run);
    live.start_owned(Owner::Ui, &engine.config().nodes[..1], &[]);
    outdated(&engine, &live).await;
    let transport = HeldUpdate::new(0);
    let updates = Arc::new(RemoteUpdateControl::new(transport.clone()));
    updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 1, 0, Duration::from_secs(120))).unwrap();
    tx.send_modify(|r| {
        r.start_seq = 1;
        r.mode = RunMode::Run;
    });
    tx.send_modify(|r| r.mode = RunMode::Paused);
    let worker = tokio::spawn(run_controlled(engine.clone(), live.clone(), |_| {}, tx.clone(), stx, Some(updates.clone())));
    phase(&mut status, RunPhase::Paused).await;
    let outcome = updates.outcome();
    let busy = updates.busy();
    let old_calls = transport.calls.lock().unwrap().len();
    tx.send_modify(|r| {
        r.start_seq = 2;
        r.mode = RunMode::Run;
    });
    let new_admission = updates.admit(&engine, &live, &tx, update_request(&engine, &dir, 2, 2, Duration::from_secs(120)));
    if new_admission.is_ok() {
        tokio::time::timeout(Duration::from_secs(2), transport.entered.notified()).await.unwrap();
        transport.release.notify_one();
    } else {
        updates.withdraw(1); // RED cleanup after observing the failed admission.
    }
    tx.send_modify(|r| r.mode = RunMode::Exit);
    let result = worker.await.unwrap();
    live.shutdown(Duration::from_secs(1)).await;
    assert!(engine.finish_monitor_journal(Duration::from_secs(1)).await);
    drop((live, engine));
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(old_calls, 0);
    assert_eq!(outcome, Some(RemoteUpdateOutcome::Refused { attempt_id: 1, reason: remote_update::Refusal::Session }));
    assert!(!busy);
    assert_eq!(new_admission, Ok(()));
    assert_eq!(result.update_complete, Some(true));
    assert_eq!(result.remote_update, Some(RemoteUpdateOutcome::ConfirmedPlacement { attempt_id: 2 }));
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
}
