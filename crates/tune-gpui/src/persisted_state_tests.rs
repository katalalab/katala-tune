//! Current-schema synthetic reopen coverage, not historical-schema or OS migration proof.
use super::*;
use crate::{native_host::NativeHost, os};
use serde_json::json;
use std::{path::PathBuf, sync::atomic::AtomicUsize};
use tune_core::{
    db::Store,
    live::{Opts, Route, Source},
};

const KEYS: [&str; 5] = ["monitor", "schedule", "hubCursor", "hubPull", "helpers"];
type RawMeta = Vec<(String, String, i64)>;

fn raw_meta(db: &Store) -> RawMeta {
    KEYS.into_iter()
        .map(|key| db.conn().query_row("SELECT k, v, updated_at FROM meta WHERE k = ?", [key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap())
        .collect()
}

struct Fixture {
    dir: PathBuf,
    ledger: Vec<u8>,
    helpers: Vec<(&'static str, Vec<u8>)>,
    meta: RawMeta,
}
impl Fixture {
    fn new(label: &str, fresh: bool) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("gpui-persisted-{label}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir(&dir).unwrap();
        // The v2 record is synthetic provenance, not approval of executable bytes.
        let ledger = serde_json::to_vec_pretty(&json!({
            "nodes":[{"id":"example-hub", "alias":"example-hub", "os":"windows", "action_helper":{
                "schema":"katala-tune/action-helper/v2", "source_sha":"a".repeat(40),
                "tune_sha256":"b".repeat(64), "manifest_sha256":"c".repeat(64), "run_id":1, "attempt":1
            }}],
            "schedule":{"enabled":false, "probe_minutes":30, "logs_minutes":45, "inventory_hours":12, "ai_minutes":60}
        }))
        .unwrap();
        std::fs::write(dir.join("nodes.json"), &ledger).unwrap();
        std::fs::create_dir_all(dir.join("data/bin")).unwrap();
        let helpers = vec![
            ("tune.exe", b"synthetic action helper sentinel, never execute\n".to_vec()),
            ("tune-agent", b"synthetic sampler sentinel, never execute\n".to_vec()),
        ];
        for (name, bytes) in &helpers {
            let path = dir.join("data/bin").join(name);
            std::fs::write(&path, bytes).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o111, 0);
            }
        }
        let now = tune_core::db::now_ms();
        let values = [
            json!({"enabled":true, "hub":"example-hub"}),
            json!({"enabled":true, "probe_minutes":15, "ai_minutes":20, "fixture_extension":"keep"}),
            json!(123456),
            json!({"hub":"example-hub", "ok":true, "fresh":true, "snapshot_fresh":true,
                "at":if fresh {now} else {now - 3_600_000}, "rows":7, "next":123456}),
            json!({"at":1234, "version":"synthetic", "tools":[{"name":"tune", "changed":false}]}),
        ];
        let meta = {
            let old = Store::open(&dir.join("data")).unwrap();
            for (i, (key, value)) in KEYS.into_iter().zip(values).enumerate() {
                // Whitespace and old timestamps detect even a same-value startup re-save.
                let raw = format!("  {}\n", serde_json::to_string_pretty(&value).unwrap());
                old.conn().execute("INSERT INTO meta (k, v, updated_at) VALUES (?, ?, ?)", (key, raw, 1000 + i as i64)).unwrap();
            }
            raw_meta(&old)
        }; // The seeding Store is closed before Engine opens this data.
        Self { dir, ledger, helpers, meta }
    }
    fn assert_saved(&self, expected: &RawMeta) {
        let reopened = Store::open_read_only(&self.dir.join("data")).unwrap();
        assert_eq!(raw_meta(&reopened), *expected, "target raw meta and timestamps must survive reopen unchanged");
        assert_eq!(std::fs::read(self.dir.join("nodes.json")).unwrap(), self.ledger);
        for (name, bytes) in &self.helpers {
            assert_eq!(std::fs::read(self.dir.join("data/bin").join(name)).unwrap(), *bytes);
        }
    }
    fn open(&self) -> Session {
        let fake = Arc::new(PermittedOs { calls: Default::default(), state: Mutex::new(os::State::Enabled) });
        let dirty = Arc::new(AtomicBool::new(false));
        let native = Arc::new(NativeHost::new(dirty.clone(), fake.clone(), Handle::current()));
        let engine = Engine::open(self.dir.join("nodes.json"), self.dir.join("data"), native.clone()).unwrap();
        let route = Arc::new(RefusedRoute(AtomicUsize::new(0)));
        let live = Live::new(route.clone(), Opts::default(), Box::new(|_| {}));
        let host = Arc::new(super::shutdown_tests::with_fake_owner(MonitorHost::new(engine.clone(), live, Handle::current(), dirty)));
        Session { engine, native, host, fake, route }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

// All five OS boundaries remain fake; real Engine and host reads are exercised.
struct PermittedOs {
    calls: [AtomicUsize; 5],
    state: Mutex<os::State>,
}
impl os::Service for PermittedOs {
    fn query(&self, done: os::Completion<os::Observation>) {
        self.calls[0].fetch_add(1, Ordering::SeqCst);
        let state = self.state.lock().unwrap().clone();
        done(os::Observation { source: "synthetic OS".into(), notification: state.clone(), login: state });
    }
    fn permission(&self, done: os::Completion<os::Effect>) {
        self.calls[1].fetch_add(1, Ordering::SeqCst);
        done(os::Effect::Accepted);
    }
    fn settings(&self, done: os::Completion<os::Effect>) {
        self.calls[2].fetch_add(1, Ordering::SeqCst);
        done(os::Effect::Accepted);
    }
    fn login(&self, enabled: bool, done: os::Completion<os::Effect>) {
        self.calls[3].fetch_add(1, Ordering::SeqCst);
        *self.state.lock().unwrap() = if enabled { os::State::Enabled } else { os::State::Disabled };
        done(os::Effect::Accepted);
    }
    fn notify(&self, _: String, _: String, done: os::Completion<os::Effect>) {
        self.calls[4].fetch_add(1, Ordering::SeqCst);
        done(os::Effect::Accepted);
    }
}
struct RefusedRoute(AtomicUsize);
impl Route for RefusedRoute {
    fn open(&self, _: &tune_core::nodes::Node, _: &Opts) -> Result<Source, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("synthetic fixture refuses all streams".into())
    }
}
struct Session {
    engine: Arc<Engine>,
    native: Arc<NativeHost>,
    host: Arc<MonitorHost>,
    fake: Arc<PermittedOs>,
    route: Arc<RefusedRoute>,
}
impl Session {
    fn assert_passive(&self) {
        let state = self.host.state.lock().unwrap();
        assert!(state.manual.is_none() && state.worker.is_none() && state.scheduler.is_none() && state.ticker.is_none());
        assert!(!self.host.snapshot().started);
        assert_eq!(self.host.snapshot().streams, 0);
        assert!(!self.host.schedule_snapshot().0);
        assert_eq!(self.host.schedule_snapshot().1.cycles, 0);
        assert_eq!(self.route.0.load(Ordering::SeqCst), 0);
        assert!(!self.engine.is_probing() && !self.engine.is_syncing());
        assert_eq!(self.engine.monitor_journal_health()["running"], false);
        for key in ["lastProbeAt", "lastLogsAt", "lastInventoryAt", "lastAiAt"] {
            assert_eq!(self.engine.with_db(|db| db.get_meta(key)).unwrap(), None, "unexpected collection receipt: {key}");
        }
        assert_eq!(self.fake.calls.each_ref().map(|c| c.load(Ordering::SeqCst)), [0; 5]);
        assert_eq!(*self.fake.state.lock().unwrap(), os::State::Enabled);
        let cache = self.native.snapshot();
        assert_eq!(cache.observed.login, os::State::Unknown);
        assert_eq!(cache.observed.notification, os::State::Unknown);
        assert!(cache.queried_at.is_none() && cache.action.is_none() && cache.notification.is_none() && !cache.busy);
        assert_eq!(self.engine.status_snapshot().unwrap()["openAtLogin"], Value::Null);
    }
    async fn close(self) {
        self.assert_passive();
        self.host.request_exit(); // Same admission close used for last-window close and Quit.
        let report = self.host.shutdown().await;
        assert_eq!(
            report,
            ShutdownReport {
                manual_complete: None,
                monitor_complete: None,
                scheduler_complete: None,
                journal_ack: Some(true),
                install_complete: None,
                install_recovery_required: false,
                remote_update_complete: None,
                remote_update: None
            }
        );
        assert_eq!(self.host.shutdown().await, report);
        self.assert_passive();
        let old_engine = Arc::downgrade(&self.engine);
        drop(self);
        assert!(old_engine.upgrade().is_none(), "all old Engine/host handles must close before reopen");
    }
}

async fn survives_passive_reopen(fresh: bool) {
    let fixture = Fixture::new(if fresh { "fresh" } else { "stale" }, fresh);
    for _ in 0..2 {
        let session = fixture.open();
        session.assert_passive();
        assert_eq!(session.host.snapshot().mode, RunMode::Paused);
        for _ in 0..3 {
            let cfg = session.engine.reload_config();
            assert_eq!(cfg.nodes[0].raw["action_helper"]["schema"], "katala-tune/action-helper/v2");
            assert_eq!(cfg.schedule.unwrap()["probe_minutes"], 30);
            session.engine.last().unwrap();
            let status = session.engine.status().unwrap();
            assert_eq!(status["schedule"]["enabled"], true);
            assert_eq!(status["schedule"]["probe_minutes"], 15);
            assert_eq!(status["schedule"]["logs_minutes"], 45);
            assert_eq!(status["schedule"]["inventory_hours"], 12);
            assert_eq!(status["schedule"]["ai_minutes"], 20);
            let view = crate::dashboard::load_saved(&session.engine, 60).unwrap();
            assert_eq!(view.saved["schedule"], status["schedule"]);
            assert_eq!(view.saved["monitor"], json!({"enabled":true,"hub":"example-hub"}));
            assert_eq!(view.saved["collecting"], !fresh, "saved policy is distinct from session run permission");
            assert_eq!(view.saved["hub_pull"]["next"], 123456);
            assert_eq!(view.saved["app"]["helpers"]["version"], "synthetic");
            session.native.snapshot();
            session.host.snapshot();
        }
        tokio::time::advance(Duration::from_secs(500)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        session.assert_passive();
        fixture.assert_saved(&fixture.meta);
        session.host.pause_monitor().unwrap();
        fixture.assert_saved(&fixture.meta);
        session.close().await;
        fixture.assert_saved(&fixture.meta);
    }
}

#[tokio::test(start_paused = true)]
async fn fresh_saved_state_survives_passive_engine_host_close_and_reopen() {
    survives_passive_reopen(true).await;
}
#[tokio::test(start_paused = true)]
async fn stale_saved_state_does_not_authorize_collection_on_reopen() {
    survives_passive_reopen(false).await;
}

#[tokio::test(start_paused = true)]
async fn explicit_patches_preserve_unrelated_saved_values_and_do_not_authorize_work() {
    let fixture = Fixture::new("patch", true);
    let session = fixture.open();
    session.host.save_monitor_settings(json!({"hub":null})).unwrap();
    let after_monitor = session.engine.with_db(|db| Ok(raw_meta(db))).unwrap();
    assert_eq!(&after_monitor[1..], &fixture.meta[1..]);
    assert_ne!(after_monitor[0].2, fixture.meta[0].2);
    assert_eq!(session.engine.with_db(|db| db.get_meta("monitor")).unwrap(), Some(json!({"enabled":true,"hub":null})));
    fixture.assert_saved(&after_monitor);
    session.host.save_schedule_settings(json!({"probe_minutes":25})).unwrap();
    let after_schedule = session.engine.with_db(|db| Ok(raw_meta(db))).unwrap();
    for i in [0, 2, 3, 4] {
        assert_eq!(after_schedule[i], after_monitor[i]);
    }
    assert_ne!(after_schedule[1].2, after_monitor[1].2);
    assert_eq!(
        session.engine.with_db(|db| db.get_meta("schedule")).unwrap(),
        Some(json!({
            "enabled":true, "probe_minutes":25, "ai_minutes":20, "fixture_extension":"keep"
        }))
    );
    session.host.pause_monitor().unwrap();
    session.host.stop_schedule().unwrap();
    session.assert_passive();
    fixture.assert_saved(&after_schedule);
    session.close().await;
    fixture.assert_saved(&after_schedule);
    let reopened = fixture.open();
    assert_eq!(reopened.engine.checked_schedule().unwrap()["probe_minutes"], 25);
    assert_eq!(reopened.engine.with_db(|db| db.get_meta("monitor")).unwrap(), Some(json!({"enabled":true,"hub":null})));
    reopened.close().await;
    fixture.assert_saved(&after_schedule);
}

#[tokio::test(start_paused = true)]
async fn corrupt_saved_json_survives_passive_reopen_and_checked_admission_refuses_it() {
    for key in ["monitor", "schedule"] {
        let fixture = Fixture::new(key, false);
        let expected = {
            let db = Store::open(&fixture.dir.join("data")).unwrap();
            db.conn().execute("UPDATE meta SET v = ?, updated_at = ? WHERE k = ?", ("{synthetic broken JSON", 42, key)).unwrap();
            raw_meta(&db)
        };
        for _ in 0..2 {
            let session = fixture.open();
            session.engine.reload_config();
            session.engine.last().unwrap();
            session.engine.status().unwrap();
            let dashboard = crate::dashboard::load_saved(&session.engine, 60);
            assert_eq!(dashboard.is_err(), key == "schedule");
            fixture.assert_saved(&expected);
            if key == "schedule" {
                assert!(session.host.start_schedule().is_err());
                assert_eq!(session.host.schedule_snapshot().1.phase, SchedulePhase::Failed);
            } else {
                assert!(session.host.start_monitor().is_err());
                assert!(session.host.request_pull().is_err());
                assert_eq!(session.host.snapshot().status.phase, RunPhase::Failed);
            }
            assert_eq!(session.host.snapshot().mode, RunMode::Paused);
            session.assert_passive();
            session.close().await;
            fixture.assert_saved(&expected);
        }
    }
}
