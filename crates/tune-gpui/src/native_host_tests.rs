use super::*;
use crate::os;
use serde_json::json;
use std::sync::atomic::AtomicUsize;
use tune_core::{db::Check, engine::Engine};
struct Fake {
    queries: AtomicUsize,
    permission: AtomicUsize,
    settings: AtomicUsize,
    login: AtomicUsize,
    login_requests: Mutex<Vec<bool>>,
    state: Mutex<State>,
    result: Mutex<Effect>,
    sent: Mutex<Vec<(String, String)>>,
    held: Mutex<Option<os::Completion<Effect>>>,
    hold: AtomicBool,
}
impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queries: AtomicUsize::new(0),
            permission: AtomicUsize::new(0),
            settings: AtomicUsize::new(0),
            login: AtomicUsize::new(0),
            login_requests: Mutex::new(vec![]),
            state: Mutex::new(State::Disabled),
            result: Mutex::new(Effect::Accepted),
            sent: Mutex::new(vec![]),
            held: Mutex::new(None),
            hold: AtomicBool::new(false),
        })
    }
}
impl Service for Fake {
    fn query(&self, done: os::Completion<Observation>) {
        self.queries.fetch_add(1, Ordering::SeqCst);
        let state = self.state.lock().unwrap().clone();
        done(Observation { source: "synthetic OS".into(), notification: state.clone(), login: state });
    }
    fn permission(&self, done: os::Completion<Effect>) {
        self.permission.fetch_add(1, Ordering::SeqCst);
        done(self.result.lock().unwrap().clone());
    }
    fn settings(&self, done: os::Completion<Effect>) {
        self.settings.fetch_add(1, Ordering::SeqCst);
        done(self.result.lock().unwrap().clone());
    }
    fn login(&self, enabled: bool, done: os::Completion<Effect>) {
        self.login.fetch_add(1, Ordering::SeqCst);
        self.login_requests.lock().unwrap().push(enabled);
        done(self.result.lock().unwrap().clone());
    }
    fn notify(&self, title: String, body: String, done: os::Completion<Effect>) {
        self.sent.lock().unwrap().push((title, body));
        if self.hold.load(Ordering::SeqCst) {
            *self.held.lock().unwrap() = Some(done);
        } else {
            done(self.result.lock().unwrap().clone());
        }
    }
}
async fn completed(host: &NativeHost) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while host.snapshot().busy {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn fixture(host: Arc<NativeHost>, name: &str) -> (Arc<Engine>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("native-os-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("nodes.json"), r#"{"nodes":[]}"#).unwrap();
    let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), host).unwrap();
    (engine, dir)
}
#[tokio::test]
async fn startup_reads_reload_saved_data_and_dirty_callbacks_have_no_os_effects() {
    let fake = Fake::new();
    let dirty = Arc::new(AtomicBool::new(false));
    let host = Arc::new(NativeHost::new(dirty.clone(), fake.clone(), Handle::current()));
    let (engine, dir) = fixture(host.clone(), "reads");
    assert_eq!(engine.status_snapshot().unwrap()["openAtLogin"], serde_json::Value::Null);
    assert_eq!(host.open_at_login_state(), None);
    for _ in 0..3 {
        engine.reload_config();
        engine.last().unwrap();
        engine.status().unwrap();
        crate::dashboard::load_saved(&engine, 60).unwrap();
        host.snapshot();
        engine.set_schedule(&json!({"enabled":true})).unwrap();
    }
    let before = engine.status_snapshot().unwrap()["checks"].clone();
    dirty.store(false, Ordering::SeqCst);
    host.probe_result(&json!({}));
    host.logs_synced(&json!({}));
    host.inventory_result(&json!({}));
    host.ai_synced(&json!({}));
    host.topology_updated(&json!({}));
    host.checks_updated();
    host.state_changed();
    assert!(dirty.load(Ordering::SeqCst));
    assert_eq!(engine.status_snapshot().unwrap()["checks"], before);
    assert_eq!(fake.queries.load(Ordering::SeqCst), 0);
    assert_eq!(fake.permission.load(Ordering::SeqCst), 0);
    assert_eq!(fake.settings.load(Ordering::SeqCst), 0);
    assert_eq!(fake.login.load(Ordering::SeqCst), 0);
    assert!(fake.login_requests.lock().unwrap().is_empty());
    assert!(fake.sent.lock().unwrap().is_empty());
    assert_eq!(engine.monitor_journal_health()["running"], false);
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn real_engine_notifications_use_shared_text_and_never_wait_for_os_or_ui_callback() {
    let fake = Fake::new();
    fake.hold.store(true, Ordering::SeqCst);
    let host = Arc::new(NativeHost::new(Arc::new(AtomicBool::new(false)), fake.clone(), Handle::current()));
    let (engine, dir) = fixture(host.clone(), "notify");
    engine.compute_checks(false, false);
    engine.with_db(|db| db.save_checks("_app", &[Check { id: "db".into(), name: "synthetic db".into(), status: "fail".into(), detail: None }], 1)).unwrap();
    let work = engine.clone();
    let changes =
        tokio::time::timeout(std::time::Duration::from_secs(2), tokio::task::spawn_blocking(move || work.compute_checks(true, true))).await.unwrap().unwrap();
    let important: Vec<_> = changes.into_iter().filter(|c| c.from.is_some() && c.id == "db").collect();
    assert!(!important.is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while fake.held.lock().unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(*fake.sent.lock().unwrap(), vec![engine::notification_text(&important)]);
    // Completion still withheld: the producer/Store have already completed.
    assert!(host.snapshot().notification.is_none());
    engine.status_snapshot().unwrap();
    fake.held.lock().unwrap().take().unwrap()(Effect::Denied);
    assert_eq!(host.snapshot().notification.unwrap().1, Effect::Denied);
    assert_eq!(fake.permission.load(Ordering::SeqCst), 0);
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test]
async fn explicit_queries_propagate_all_states_without_assuming_mutation_success() {
    let fake = Fake::new();
    let host = Arc::new(NativeHost::new(Arc::new(AtomicBool::new(false)), fake.clone(), Handle::current()));
    let (engine, dir) = fixture(host.clone(), "states");
    for state in [State::Enabled, State::Disabled, State::Denied, State::Unknown, State::Unsupported("fixture".into()), State::Failed("fixture".into())] {
        *fake.state.lock().unwrap() = state.clone();
        host.act(Action::Refresh);
        completed(&host).await;
        assert_eq!(host.snapshot().observed.login, state);
        assert_eq!(host.snapshot().observed.notification, state);
        assert!(host.snapshot().queried_at.is_some());
        assert_eq!(host.open_at_login_state(), state.observed());
        assert_eq!(engine.status_snapshot().unwrap()["openAtLogin"], json!(state.observed()));
        let status = engine.status().unwrap();
        let login = status["checks"].as_array().unwrap().iter().find(|c| c["id"] == "login").unwrap();
        assert_eq!(login["status"], if state == State::Enabled { "ok" } else { "unknown" });
        if state.observed().is_none() {
            assert!(login["detail"].as_str().unwrap().starts_with("未確認"));
        }
    }
    let before = host.snapshot();
    *fake.result.lock().unwrap() = Effect::Failed("synthetic denied".into());
    host.act(Action::Login(true));
    completed(&host).await;
    assert_eq!(host.snapshot().observed.login, before.observed.login);
    assert_eq!(host.snapshot().queried_at, before.queried_at);
    assert_eq!(host.snapshot().action.unwrap().1, Effect::Failed("synthetic denied".into()));
    assert_eq!(fake.login.load(Ordering::SeqCst), 1);
    assert_eq!(fake.permission.load(Ordering::SeqCst), 0);
    host.act(Action::Permission);
    completed(&host).await;
    assert_eq!(fake.permission.load(Ordering::SeqCst), 1);
    host.act(Action::Test);
    completed(&host).await;
    let sent = fake.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].0.contains("合成テスト"));
    drop(sent);
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
#[tokio::test(start_paused = true)]
async fn pending_os_callback_times_out_without_ui_and_does_not_claim_success() {
    let fake = Fake::new();
    fake.hold.store(true, Ordering::SeqCst);
    let host = NativeHost::new(Arc::new(AtomicBool::new(false)), fake.clone(), Handle::current());
    host.act(Action::Test);
    // Wait for the OS boundary, not for real time or a UI event loop.
    while fake.held.lock().unwrap().is_none() {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    completed(&host).await;
    assert!(matches!(host.snapshot().action.unwrap().1, Effect::Failed(_)));
    assert_eq!(host.open_at_login_state(), None);
    fake.held.lock().unwrap().take().unwrap()(Effect::Accepted);
    assert!(matches!(host.snapshot().action.unwrap().1, Effect::Failed(_)));
}

#[tokio::test]
async fn pending_operation_rejects_settings_and_login_until_completion_without_implicit_query() {
    let fake = Fake::new();
    let host = NativeHost::new(Arc::new(AtomicBool::new(false)), fake.clone(), Handle::current());
    host.act(Action::Refresh);
    completed(&host).await;
    let before = host.snapshot();
    fake.hold.store(true, Ordering::SeqCst);
    host.act(Action::Test);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while fake.held.lock().unwrap().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(host.snapshot().busy);
    host.act(Action::Settings);
    host.act(Action::Login(false));
    assert!(host.snapshot().busy);
    assert_eq!(fake.settings.load(Ordering::SeqCst), 0);
    assert!(fake.login_requests.lock().unwrap().is_empty());
    fake.held.lock().unwrap().take().unwrap()(Effect::Accepted);
    completed(&host).await;

    host.act(Action::Settings);
    completed(&host).await;
    assert_eq!(fake.settings.load(Ordering::SeqCst), 1);
    host.act(Action::Login(false));
    completed(&host).await;
    assert_eq!(*fake.login_requests.lock().unwrap(), vec![false]);
    assert_eq!(fake.login.load(Ordering::SeqCst), 1);
    assert_eq!(fake.permission.load(Ordering::SeqCst), 0);
    assert_eq!(fake.sent.lock().unwrap().len(), 1);
    assert_eq!(fake.queries.load(Ordering::SeqCst), 1);
    let after = host.snapshot();
    assert_eq!(after.queried_at, before.queried_at);
    assert_eq!(after.observed.notification, before.observed.notification);
    assert_eq!(after.observed.login, before.observed.login);
    assert_eq!(after.action.unwrap().1, Effect::Accepted);
}
