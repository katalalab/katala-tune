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

#[test]
fn close_releases_ui_only_keeps_monitor_manual_job_and_journal_then_quit_drains() {
    let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap());
    rt.block_on(async {
        let (dir, engine, host, mut ui, route, fake) = fixture("drain");
        host.start_monitor().unwrap();
        host.start_schedule().unwrap();
        until(|| host.snapshot().status.phase == tune_core::monitor::RunPhase::Running).await;
        assert_eq!(engine.monitor_journal_health()["running"], true);
        // Synthetic owned stream, no real collector. The worker's next 15s tick is
        // outside this bounded fixture, so its empty ledger cannot replace this setup.
        let nodes = [nodes::Node::from_value(&json!({"id":"a","os":"macos"}), "synthetic-host")];
        ui.live.start_owned(Owner::Monitor, &nodes, &[]);
        let mut windows = Windows::new(true);
        windows.open(&mut ui, None).unwrap();
        let old = ui.view.as_ref().unwrap().live.clone();
        let gate = ui.view.as_ref().unwrap().gate.clone();
        let (generation, id) = old.request(&host, &gate, "a").unwrap();
        old.complete(generation, &id, &nodes, None);
        tests::samples_after(&ui.live, 0).await;
        let mut session = native_session(&rt, &engine, &host, &ui, fake, true);
        let keeper = session.keeper.unwrap();
        session.windows.current = Some(gpui::WindowHandle::new(gpui::WindowId::from(1)));
        session.admission = Some((old.clone(), gate.clone()));
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
        assert!(!session.windows_changed(&[keeper]));
        assert!(gate.is_closed(), "native observer revokes before entity release");
        assert!(host.schedule_snapshot().0, "accepted scheduler survives Close");
        ui.close();
        assert!(!windows.closed());
        assert!(!confirmation.await.unwrap());
        old.complete(generation, &id, &nodes, None);
        assert!(old.request(&host, &gate, "a").is_none());
        assert!(!ui.live.heartbeat_owned(Owner::Ui, "a"));
        assert!(ui.live.heartbeat_owned(Owner::Monitor, "a"));
        let count = ui.live.samples("a").len();
        tests::samples_after(&ui.live, count).await;
        windows.open(&mut ui, None).unwrap();
        windows.open(&mut ui, None).unwrap();
        assert!(Rc::ptr_eq(&ui.job, &ui.view.as_ref().unwrap().job));
        assert!(host.native_install_snapshot().ever_collection_admitted, "Reopen preserves accepted collection history");
        assert!(ui.job.borrow().running.is_some());
        assert_eq!(route.0.load(Ordering::SeqCst), 1);
        assert_eq!(host.snapshot().mode, tune_core::monitor::RunMode::Run);
        let current = ui.view.as_ref().unwrap();
        session.windows.current = Some(gpui::WindowHandle::new(gpui::WindowId::from(2)));
        session.admission = Some((current.live.clone(), current.gate.clone()));
        session.close_ui();
        assert!(current.gate.is_closed());
        assert!(current.live.request(&host, &current.gate, "a").is_none());
        ui.quit(&mut windows);
        let shutting = host.clone();
        let drain = tokio::spawn(async move { shutting.shutdown().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!drain.is_finished(), "quit must wait for the owned manual DB boundary");
        assert_eq!(session.keeper, Some(keeper), "keeper survives synchronous quit drain");
        release.send(()).unwrap();
        let report = drain.await.unwrap();
        assert!(report.confirmed());
        assert_eq!(report.manual_complete, Some(true));
        assert_eq!(report.monitor_complete, Some(true));
        assert_eq!(report.scheduler_complete, Some(true));
        assert_eq!(report.journal_ack, Some(true));
        assert_eq!(engine.with_db(|d| d.get_meta("resident-fixture-committed")).unwrap(), Some(json!(true)));
        assert!(ui.job.borrow_mut().poll());
        assert!(ui.job.borrow().running.is_none());
        assert_eq!(engine.monitor_journal_health()["running"], false);
        assert!(ui.live.active().is_empty());
        assert_eq!(session.keeper, Some(keeper));
        drop((session, old, ui, host, engine));
        std::fs::remove_dir_all(dir).unwrap();
    });
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
    let mut session = native_session(&rt, &engine, &host, &ui, fake, true);
    let first = Arc::new(LiveControl::new(ui.live.clone(), true));
    let first_gate = gate::Gate::new(|| {});
    let late = first.request(&host, &first_gate, "a").unwrap();
    session.admission = Some((first.clone(), first_gate.clone()));
    let waiting = rt.spawn((first_gate.confirm())("first".into(), "first".into()));
    rt.block_on(until(|| first_gate.pending().is_some()));
    // No WindowHandle can be updated now, but the root entity/controls are
    // deliberately still alive, exactly as GPUI's update_window_id orders it.
    session.windows.current = Some(gpui::WindowHandle::new(gpui::WindowId::from(1)));
    assert!(!session.windows_changed(&[session.keeper.unwrap()]));
    assert!(!rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), waiting).await.expect("close revokes pending gate") }).unwrap());
    let node = nodes::Node::from_value(&json!({"id":"a","os":"macos"}), "synthetic-host");
    first.complete(late.0, &late.1, &[node], None);
    assert!(first.request(&host, &first_gate, "a").is_none());
    let reopened = Arc::new(LiveControl::new(ui.live.clone(), true));
    let reopened_gate = gate::Gate::new(|| {});
    session.admission = Some((reopened.clone(), reopened_gate.clone()));
    let waiting = rt.spawn((reopened_gate.confirm())("reopened".into(), "reopened".into()));
    rt.block_on(until(|| reopened_gate.pending().is_some()));
    session.close_ui();
    assert!(!rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), waiting).await.expect("close revokes pending gate") }).unwrap());
    assert!(reopened.request(&host, &reopened_gate, "a").is_none());
    assert_eq!(route.0.load(Ordering::SeqCst), 0);
    assert!(rt.block_on(host.shutdown()).confirmed());
    drop((session, ui, host, engine, first, reopened));
    drop(rt);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn windows_resident_keeps_one_hidden_raw_window_after_last_user_close() {
    // GPUI 0.2.2's Win32 close_one_window quits when its raw-window list is empty.
    // Use actual creation options, not a fake "resident means alive" assumption.
    for resident in [false, true] {
        for hidden in [false, true] {
            if hidden && !resident {
                continue; // rejected by startup::parse before any window creation
            }
            let options = keeper_options(resident, true);
            let mut raw = Vec::new();
            if options.is_some() {
                raw.push("keeper");
            }
            if hidden {
                assert_eq!(raw.len(), 1, "hidden resident needs one non-UI platform window");
            }
            for _ in 0..3 {
                raw.push("user");
                raw.retain(|id| *id != "user");
                assert_eq!(!raw.is_empty(), resident, "Win32 last raw window must not quit resident");
            }
            if let Some(options) = options {
                assert!(!options.show);
                assert!(!options.focus);
                assert_eq!(options.kind, gpui::WindowKind::PopUp); // WS_EX_TOOLWINDOW, not taskbar
                assert!(options.titlebar.is_none());
                assert!(!options.is_movable && !options.is_resizable && !options.is_minimizable);
                let Some(WindowBounds::Windowed(bounds)) = options.window_bounds else { panic!("bounded keeper") };
                assert_eq!(bounds.size, size(px(1.), px(1.)));
            }
        }
        assert!(keeper_options(resident, false).is_none(), "macOS needs no internal window");
    }
}

fn native_session(rt: &Arc<tokio::runtime::Runtime>, engine: &Arc<Engine>, host: &Arc<MonitorHost>, ui: &Ui, fake: Arc<FakeOs>, resident: bool) -> Session {
    let dirty = Arc::new(AtomicBool::new(false));
    let os = Arc::new(crate::native_host::NativeHost::new(dirty.clone(), fake, rt.handle().clone()));
    Session {
        windows: Windows::new(resident),
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
        admission: None,
        keeper: keeper_options(resident, true).map(|_| gpui::WindowId::from(100)),
    }
}

#[test]
fn close_observer_counts_unknown_windows_releases_current_ui_and_quits_on_keeper_loss() {
    let rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let (dir, engine, host, ui, _, fake) = rt.block_on(async { fixture("window-inventory") });
    let unknown = gpui::WindowId::from(200);
    for resident in [false, true] {
        let mut session = native_session(&rt, &engine, &host, &ui, fake.clone(), resident);
        let mut remaining: Vec<_> = session.keeper.into_iter().collect();
        // Repeat actual Session observer transitions without a GPUI application.
        for id in 1..=3 {
            let id = gpui::WindowId::from(id);
            let live = Arc::new(LiveControl::new(ui.live.clone(), true));
            let gate = gate::Gate::new(|| {});
            session.windows.current = Some(gpui::WindowHandle::new(id));
            session.admission = Some((live.clone(), gate.clone()));
            remaining.extend([id, unknown]);
            assert!(!session.windows_changed(&remaining));
            assert!(!gate.is_closed());
            remaining.retain(|window| *window != id);
            assert!(!session.windows_changed(&remaining), "unknown window prevents last-window quit");
            assert!(gate.is_closed(), "closed current UI is revoked even while other windows exist");
            assert!(live.request(&host, &gate, "a").is_none());
            assert!(session.windows.current.is_none());
            assert!(!session.windows.exiting);
            remaining.retain(|window| *window != unknown);
            assert_eq!(session.windows_changed(&remaining), !resident);
            if !resident {
                break;
            }
        }
        if resident {
            let live = Arc::new(LiveControl::new(ui.live.clone(), true));
            let gate = gate::Gate::new(|| {});
            session.admission = Some((live.clone(), gate.clone()));
            // Even another window cannot substitute for the exact owned keeper.
            assert!(session.windows_changed(&[unknown]));
            assert!(session.windows.exiting);
            assert!(gate.is_closed());
            assert!(live.request(&host, &gate, "a").is_none());
            assert!(!session.windows_changed(&[]), "quit callbacks must not recurse");
        }
    }
    assert!(rt.block_on(host.shutdown()).confirmed());
    drop((ui, host, engine));
    drop(rt);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn resident_reopen_retains_install_and_native_shutdown_joins_the_same_host() {
    let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap());
    let (dir, engine, host, mut ui, _, fake) = rt.block_on(async { fixture("install-reopen") });
    let mut session = native_session(&rt, &engine, &host, &ui, fake, true);
    let mut windows = Windows::new(true);
    windows.open(&mut ui, None).unwrap();
    let gate = ui.view.as_ref().unwrap().gate.clone();
    let old_live = ui.view.as_ref().unwrap().live.clone();
    session.admission = Some((old_live, gate.clone()));
    session.windows.current = Some(gpui::WindowHandle::new(gpui::WindowId::from(1)));
    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    host.start_fixture_install(&gate, move || {
        entered.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        Err(tune_core::helpers::native_install::prepare_native_sampler(std::path::Path::new("relative-source")).unwrap_err())
    })
    .unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(!session.windows_changed(&[session.keeper.unwrap()]));
    ui.close();
    assert!(gate.is_closed());
    assert!(host.native_install_snapshot().busy);
    windows.open(&mut ui, None).unwrap();
    assert!(Arc::ptr_eq(&session.handles.monitor, &host));
    assert!(session.handles.monitor.native_install_snapshot().busy);
    let reopened = ui.view.as_ref().unwrap();
    session.windows.current = Some(gpui::WindowHandle::new(gpui::WindowId::from(2)));
    session.admission = Some((reopened.live.clone(), reopened.gate.clone()));
    session.close_ui();
    assert!(reopened.gate.is_closed());
    host.request_exit();
    let draining = host.clone();
    let runtime = rt.clone();
    let (done, result) = mpsc::channel();
    let join = std::thread::spawn(move || done.send(finish_shutdown(&runtime, &draining, &AtomicBool::new(false))).unwrap());
    assert!(result.recv_timeout(Duration::from_millis(150)).is_err());
    release.send(()).unwrap();
    let report = result.recv_timeout(Duration::from_secs(2)).unwrap();
    join.join().unwrap();
    assert_eq!(report.install_complete, Some(true));
    assert!(report.confirmed());
    assert!(matches!(session.handles.monitor.native_install_snapshot().result.as_deref(), Some(crate::monitor_host::NativeInstallResult::Completed(Err(_)))));
    assert_eq!(finish_shutdown(&rt, &host, &AtomicBool::new(false)), report);
    drop((session, ui, host, engine));
    drop(rt);
    std::fs::remove_dir_all(dir).unwrap();
}
