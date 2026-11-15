//! No GPUI application, desktop or system service is created by these tests.
//! Production window admission/revocation with real Engine/Store/MonitorHost/Live.
use super::*;
use crate::{
    monitor_host::MonitorHost,
    resident::{Backend, Windows},
};
use serde_json::json;
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{atomic::AtomicUsize, mpsc},
};

struct FakeOs(AtomicUsize);
impl crate::os::Service for FakeOs {
    fn query(&self, _: crate::os::Completion<crate::os::Observation>) {
        panic!("visibility must not query OS");
    }
    fn permission(&self, _: crate::os::Completion<crate::os::Effect>) {
        panic!("visibility must not request permission");
    }
    fn settings(&self, _: crate::os::Completion<crate::os::Effect>) {
        panic!("visibility must not open settings");
    }
    fn login(&self, _: bool, _: crate::os::Completion<crate::os::Effect>) {
        panic!("visibility must not alter login");
    }
    fn notify(&self, _: String, _: String, done: crate::os::Completion<crate::os::Effect>) {
        self.0.fetch_add(1, Ordering::SeqCst);
        done(crate::os::Effect::Accepted);
    }
}
struct View {
    id: usize,
    live: Arc<LiveControl>,
    gate: gate::Gate,
    job: Rc<RefCell<Job>>,
}
struct Ui {
    live: Arc<Live>,
    job: Rc<RefCell<Job>>,
    view: Option<View>,
    created: usize,
}
impl Backend<usize> for Ui {
    type Target = ();
    type Error = String;
    fn activate(&mut self, handle: usize, _: Option<&()>) -> bool {
        self.view.as_ref().is_some_and(|v| v.id == handle)
    }
    fn create(&mut self, _: Option<&()>) -> Result<usize, String> {
        assert!(self.view.is_none(), "never create a duplicate window");
        self.created += 1;
        self.view =
            Some(View { id: self.created, live: Arc::new(LiveControl::new(self.live.clone(), true)), gate: gate::Gate::new(|| {}), job: self.job.clone() });
        Ok(self.created)
    }
}
impl Ui {
    fn close(&mut self) {
        if let Some(view) = self.view.take() {
            revoke_ui(&view.live, &view.gate);
        }
    }
    fn quit(&mut self, windows: &mut Windows<usize>) {
        windows.quit();
        if let Some(view) = self.view.as_ref() {
            revoke_ui(&view.live, &view.gate);
        }
    }
}
fn fixture(label: &str) -> (PathBuf, Arc<Engine>, Arc<MonitorHost>, Ui, Arc<tests::SteadyRoute>, Arc<FakeOs>) {
    let dir = std::env::temp_dir().join(format!("gpui-resident-{label}-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    let dirty = Arc::new(AtomicBool::new(false));
    let fake = Arc::new(FakeOs(AtomicUsize::new(0)));
    let os = Arc::new(crate::native_host::NativeHost::new(dirty.clone(), fake.clone(), tokio::runtime::Handle::current()));
    let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), os).unwrap();
    engine.with_db(|d| d.set_meta("monitor", &json!({"enabled":true}))).unwrap();
    engine.set_schedule(&json!({"enabled":true})).unwrap();
    let route = Arc::new(tests::SteadyRoute::default());
    let live = Live::new(route.clone(), tune_core::live::Opts::default(), Box::new(|_| {}));
    let host = Arc::new(MonitorHost::new(engine.clone(), live.clone(), tokio::runtime::Handle::current(), dirty));
    (dir, engine, host, Ui { live, job: Default::default(), view: None, created: 0 }, route, fake)
}
async fn until(mut test: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn hidden_open_reopen_are_passive_and_quit_closes_the_current_gate() {
    let (dir, engine, host, mut ui, route, fake) = fixture("hidden");
    let mut windows = Windows::new(true);
    // Hidden launch creates no canvas/window. Saved enabled is not permission.
    assert!(ui.view.is_none());
    assert_eq!(ui.created, 0);
    assert!(!host.snapshot().started);
    assert!(!host.schedule_snapshot().0);
    assert_eq!(engine.monitor_journal_health()["running"], false);
    assert_eq!(windows.open(&mut ui, None).unwrap(), Some(1));
    assert_eq!(windows.open(&mut ui, None).unwrap(), Some(1));
    let dead_gate = ui.view.as_ref().unwrap().gate.clone();
    ui.close();
    assert!(!windows.closed());
    assert_eq!(windows.open(&mut ui, None).unwrap(), Some(2));
    assert_eq!(windows.open(&mut ui, None).unwrap(), Some(2));
    let gate = ui.view.as_ref().unwrap().gate.clone();
    let pending = tokio::spawn((gate.confirm())("fixture".into(), "fixture".into()));
    until(|| gate.pending().is_some()).await;
    assert!(dead_gate.is_closed());
    ui.quit(&mut windows);
    assert!(!pending.await.unwrap());
    assert!(!((gate.confirm())("late".into(), "late".into()).await));
    assert!(windows.open(&mut ui, None).unwrap().is_none());
    assert_eq!(ui.created, 2);
    assert_eq!(route.0.load(Ordering::SeqCst), 0);
    assert_eq!(fake.0.load(Ordering::SeqCst), 0);
    assert!(!host.snapshot().started);
    assert!(!host.schedule_snapshot().0);
    let report = host.shutdown().await;
    assert!(report.confirmed());
    assert_eq!(report.manual_complete, None);
    assert_eq!(report.monitor_complete, None);
    drop((ui, host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_releases_ui_only_keeps_monitor_manual_job_and_journal_then_quit_drains() {
    let (dir, engine, host, mut ui, route, _) = fixture("drain");
    host.start_monitor().unwrap();
    until(|| host.snapshot().status.phase == tune_core::monitor::RunPhase::Running).await;
    assert_eq!(engine.monitor_journal_health()["running"], true);
    // Synthetic owned stream, no real collector. The worker's next 15s tick is
    // outside this bounded fixture, so its empty ledger cannot replace this setup.
    let nodes = [nodes::Node::from_value(&json!({"id":"a","os":"macos"}), "synthetic-host")];
    ui.live.start_owned(Owner::Monitor, &nodes, &[]);
    let mut windows = Windows::new(true);
    windows.open(&mut ui, None).unwrap();
    let old = ui.view.as_ref().unwrap().live.clone();
    let (generation, id) = old.request("a").unwrap();
    old.complete(generation, &id, &nodes, None);
    tests::samples_after(&ui.live, 0).await;
    let gate = ui.view.as_ref().unwrap().gate.clone();
    let confirmation = tokio::spawn((gate.confirm())("fixture".into(), "fixture".into()));
    until(|| gate.pending().is_some()).await;
    // An owned synthetic producer commits through the real Store boundary.
    // The same start_manual_with gate/JoinHandle/receipt path owns production work.
    let (held, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let db_engine = engine.clone();
    let runtime = tokio::runtime::Handle::current();
    host.start_fixture_manual(&mut ui.job.borrow_mut(), move || {
        db_engine
            .with_db(|db| {
                held.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                db.set_meta("resident-fixture-committed", &json!(true))
            })
            .unwrap();
        runtime.block_on(collection::run(db_engine, Kind::Probe, None, Default::default()))
    })
    .unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    ui.close();
    assert!(!windows.closed());
    assert!(!confirmation.await.unwrap());
    old.complete(generation, &id, &nodes, None);
    assert!(old.request("a").is_none());
    assert!(!ui.live.heartbeat_owned(Owner::Ui, "a"));
    assert!(ui.live.heartbeat_owned(Owner::Monitor, "a"));
    let count = ui.live.samples("a").len();
    tests::samples_after(&ui.live, count).await;
    windows.open(&mut ui, None).unwrap();
    windows.open(&mut ui, None).unwrap();
    assert!(Rc::ptr_eq(&ui.job, &ui.view.as_ref().unwrap().job));
    assert!(ui.job.borrow().running.is_some());
    assert_eq!(route.0.load(Ordering::SeqCst), 1);
    assert_eq!(host.snapshot().mode, tune_core::monitor::RunMode::Run);
    ui.quit(&mut windows);
    let shutting = host.clone();
    let drain = tokio::spawn(async move { shutting.shutdown().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!drain.is_finished(), "quit must wait for the owned manual DB boundary");
    release.send(()).unwrap();
    let report = drain.await.unwrap();
    assert!(report.confirmed());
    assert_eq!(report.manual_complete, Some(true));
    assert_eq!(report.monitor_complete, Some(true));
    assert_eq!(report.journal_ack, Some(true));
    assert_eq!(engine.with_db(|d| d.get_meta("resident-fixture-committed")).unwrap(), Some(json!(true)));
    assert!(ui.job.borrow_mut().poll());
    assert!(ui.job.borrow().running.is_none());
    assert_eq!(engine.monitor_journal_health()["running"], false);
    assert!(ui.live.active().is_empty());
    drop((old, ui, host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn resident_tray_quit_does_not_report_missing_journal_ack_as_success() {
    let (dir, engine, host, mut ui, _, _) = fixture("ack");
    let mut windows = Windows::new(true);
    host.start_monitor().unwrap();
    until(|| host.snapshot().status.phase == tune_core::monitor::RunPhase::Running).await;
    engine.with_db(|d| d.conn().execute_batch("DROP TABLE monitor_events")).unwrap();
    ui.quit(&mut windows);
    assert!(windows.open(&mut ui, None).unwrap().is_none());
    let report = host.shutdown().await;
    assert_eq!(report.journal_ack, Some(false));
    assert!(!report.confirmed());
    drop((ui, host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn ordinary_preview_last_close_exits_and_cannot_reopen() {
    let (dir, engine, host, mut ui, route, _) = fixture("ordinary");
    let mut windows = Windows::new(false);
    windows.open(&mut ui, None).unwrap();
    ui.close();
    assert!(windows.closed());
    assert!(windows.open(&mut ui, None).unwrap().is_none());
    assert_eq!(ui.created, 1);
    assert_eq!(route.0.load(Ordering::SeqCst), 0);
    assert!(host.shutdown().await.confirmed());
    drop((ui, host, engine));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn removed_window_revokes_before_entity_release_and_quit_uses_reopened_controls() {
    let rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let (dir, engine, host, ui, route, fake) = rt.block_on(async { fixture("removed-handle") });
    let dirty = Arc::new(AtomicBool::new(false));
    let os = Arc::new(crate::native_host::NativeHost::new(dirty.clone(), fake, rt.handle().clone()));
    let first = Arc::new(LiveControl::new(ui.live.clone(), true));
    let first_gate = gate::Gate::new(|| {});
    let late = first.request("a").unwrap();
    let mut session = Session {
        windows: Windows::new(true),
        engine: engine.clone(),
        rt: rt.clone(),
        dirty,
        handles: LiveHandles {
            job: ui.job.clone(),
            os,
            monitor: host.clone(),
            live: ui.live.clone(),
            store: Default::default(),
            dirty: Arc::new(AtomicBool::new(false)),
        },
        initial: None,
        review: None,
        admission: Some((first.clone(), first_gate.clone())),
    };
    let waiting = rt.spawn((first_gate.confirm())("first".into(), "first".into()));
    rt.block_on(until(|| first_gate.pending().is_some()));
    // No WindowHandle can be updated now, but the root entity/controls are
    // deliberately still alive, exactly as GPUI's update_window_id orders it.
    assert!(!session.window_closed());
    assert!(!rt.block_on(waiting).unwrap());
    let node = nodes::Node::from_value(&json!({"id":"a","os":"macos"}), "synthetic-host");
    first.complete(late.0, &late.1, &[node], None);
    assert!(first.request("a").is_none());
    let reopened = Arc::new(LiveControl::new(ui.live.clone(), true));
    let reopened_gate = gate::Gate::new(|| {});
    session.admission = Some((reopened.clone(), reopened_gate.clone()));
    let waiting = rt.spawn((reopened_gate.confirm())("reopened".into(), "reopened".into()));
    rt.block_on(until(|| reopened_gate.pending().is_some()));
    session.close_ui();
    assert!(!rt.block_on(waiting).unwrap());
    assert!(reopened.request("a").is_none());
    assert_eq!(route.0.load(Ordering::SeqCst), 0);
    assert!(rt.block_on(host.shutdown()).confirmed());
    drop((session, ui, host, engine, first, reopened));
    drop(rt);
    std::fs::remove_dir_all(dir).unwrap();
}
