use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tune_core::collect::{BoundedResult, run_bounded_with_env};
use tune_core::db::Store;

struct WriteLock(Store);
impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = self.0.conn().execute_batch("ROLLBACK");
    }
}

struct Observation {
    result: BoundedResult,
    elapsed: Duration,
    started: bool,
    locked: bool,
    stopped: bool,
    persisted_events: Option<i64>,
    config_held: bool,
    removed: bool,
}

async fn finite_monitor(hold_write_lock: bool) -> Observation {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let root: PathBuf =
        std::env::temp_dir().join(format!("kt-monitor-cli-{}-{}-{}", std::process::id(), tune_core::db::now_ms(), SEQ.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&root).unwrap();
    let cfg = root.join("nodes.json");
    let data = root.join("data");
    let text = r#"{"nodes":[]}"#;
    std::fs::write(&cfg, text).unwrap();
    // Empty roster: no sampler, helper upload, SSH or physical-machine probe.
    drop(Store::open(&data).unwrap());
    let child_cfg = cfg.to_string_lossy().into_owned();
    let child_data = data.to_string_lossy().into_owned();
    let began = Instant::now();
    let child = tokio::spawn(async move {
        run_bounded_with_env(
            env!("CARGO_BIN_EXE_tune"),
            &["monitor-run".into(), "--seconds".into(), "15".into()],
            &[],
            Duration::from_secs(30),
            64 * 1024,
            8 * 1024,
            &[("KATALA_TUNE_CONFIG", &child_cfg), ("KATALA_TUNE_DATA_DIR", &child_data)],
        )
        .await
    });
    // Condition barrier, not a fixed sleep: the child's journal must have committed.
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut started = false;
    while tokio::time::Instant::now() < until {
        started = Store::open_read_only(&data)
            .ok()
            .and_then(|d| {
                d.conn()
                    .query_row("SELECT EXISTS(SELECT 1 FROM monitor_events WHERE json_extract(payload,'$.kind')='collector_started')", [], |r| {
                        r.get::<_, bool>(0)
                    })
                    .ok()
            })
            .unwrap_or(false);
        if started {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let lock = if hold_write_lock && started {
        Store::open(&data).ok().and_then(|d| d.conn().execute_batch("BEGIN IMMEDIATE").ok().map(|_| WriteLock(d)))
    } else {
        None
    };
    let locked = lock.is_some();
    // No assertions while the retained, 30s-bounded child is active. The lock is RAII.
    let result = child.await.unwrap();
    let elapsed = began.elapsed();
    drop(lock);
    let persisted = Store::open_read_only(&data).ok().and_then(|d| {
        d.conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM monitor_events WHERE json_extract(payload,'$.kind')='collector_stopped'), (SELECT COUNT(*) FROM monitor_events)",
                [],
                |r| Ok((r.get::<_, bool>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok()
    });
    let stopped = persisted.is_some_and(|p| p.0);
    let persisted_events = persisted.map(|p| p.1);
    let config_held = std::fs::read_to_string(&cfg).is_ok_and(|s| s == text);
    let removed = std::fs::remove_dir_all(&root).is_ok();
    Observation { result, elapsed, started, locked, stopped, persisted_events, config_held, removed }
}

fn bounded_and_clean(o: &Observation) {
    assert!(
        !o.result.timed_out && !o.result.output_limited && !o.result.spawn_failed,
        "bounded monitor: timed_out={} output_limited={} spawn_failed={} code={:?} elapsed_ms={}",
        o.result.timed_out,
        o.result.output_limited,
        o.result.spawn_failed,
        o.result.code,
        o.elapsed.as_millis()
    );
    assert!(o.started, "the private journal must reach the persistent start barrier");
    assert!(o.config_held && o.removed, "private config is unchanged and all DB handles close before cleanup");
}

#[tokio::test]
async fn finite_monitor_confirms_journal_tail_before_success() {
    let o = finite_monitor(false).await;
    bounded_and_clean(&o);
    assert_eq!(o.result.code, Some(0));
    let dashboard: Value = serde_json::from_slice(&o.result.stdout).unwrap();
    // The ACK confirms the commit; writer termination is a separate observation.
    assert!(dashboard["journal"]["running"].is_boolean());
    assert_eq!(dashboard["journal"]["committed_total"].as_i64(), o.persisted_events);
    assert_eq!(dashboard["journal"]["unconfirmed_acks"], 0);
    assert!(o.stopped, "collector_stopped is persistent, without implying remote child termination");
}

#[tokio::test]
async fn finite_monitor_refuses_success_when_own_database_blocks_tail_ack() {
    let o = finite_monitor(true).await;
    bounded_and_clean(&o);
    assert!(o.locked, "own DB write lock must be held through the child's exit");
    assert_eq!(o.result.code, Some(1));
    assert!(o.result.stdout.is_empty(), "unconfirmed tail must not emit a successful dashboard JSON");
    assert!(String::from_utf8_lossy(&o.result.stderr).contains("収集記録の終了時保存を確認できません"));
    assert!(!o.stopped, "a blocked tail must not be reported as committed");
}
