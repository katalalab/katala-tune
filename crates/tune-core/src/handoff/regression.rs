//! Real temporary Config/SQLite; only the terminal launch is replaced. RED cannot send to an AI.
use super::*;
use crate::engine::NoHost;
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    dir: PathBuf,
    engine: Option<Arc<Engine>>,
    prepared: Prepared,
}

impl Fixture {
    fn new() -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("kt-handoff-audit-{}-{}-{}", std::process::id(), now_ms(), SEQ.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&dir).unwrap();
        let config = dir.join("nodes.json");
        std::fs::write(&config, r#"{"nodes":[{"id":"a","alias":"a","os":"macos"}],"protect":["protected-a"]}"#).unwrap();
        let e = Engine::open(config, dir.join("data"), Arc::new(NoHost)).unwrap();
        e.with_db(|d| d.add_snapshot("a", now_ms(), None, None, &json!([]), &json!({"memory":{"pressure":"warn"}}), &json!({}))).unwrap();
        let prepared = e.handoff_prepare("a", "mem-pressure").unwrap();
        Self { dir, engine: Some(e), prepared }
    }

    fn e(&self) -> &Engine {
        self.engine.as_deref().unwrap()
    }

    fn sql(&self, sql: &str) {
        self.e().with_db(|d| d.conn().execute_batch(sql)).unwrap();
    }

    fn actions(&self) -> Vec<Value> {
        self.e().with_db(|d| d.actions(10)).unwrap()
    }

    fn break_config(&self, kind: &str) {
        let path = self.dir.join("nodes.json");
        match kind {
            "malformed" => std::fs::write(path, "{").unwrap(),
            "missing" => std::fs::remove_file(path).unwrap(),
            "not-file" => {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(path).unwrap();
            }
            _ => unreachable!(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.engine.take());
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn prepare_refuses_unreadable_config_even_with_a_cached_online_node() {
    for kind in ["malformed", "missing", "not-file"] {
        let f = Fixture::new();
        f.break_config(kind);
        let result = f.e().handoff_prepare("a", "mem-pressure");
        assert!(result.is_err(), "{kind}: cached permissions must not authorize a prompt/copy");
        assert!(!f.dir.join("data/handoff").exists());
    }
}

#[test]
fn launch_refuses_unreadable_config_after_approval_without_calling_the_launcher() {
    for kind in ["malformed", "missing", "not-file"] {
        let f = Fixture::new();
        f.break_config(kind);
        let calls = Cell::new(0);
        let result = f.e().handoff_launch_with(&f.prepared, Agent::Codex, |_, _, _| {
            calls.set(calls.get() + 1);
            Ok(())
        });
        assert_eq!(calls.get(), 0, "{kind}: unreadable Config must prevent launch");
        assert_eq!(result["ok"], false);
        assert!(result["refused"].as_str().unwrap().contains("台帳"));
        assert!(!f.dir.join("data/handoff").exists());
    }
}

#[test]
fn prepare_uses_the_same_fresh_config_for_the_finding_and_protection_rules() {
    let f = Fixture::new();
    std::fs::write(f.dir.join("nodes.json"), r#"{"nodes":[{"id":"a","alias":"new-route","os":"macos"}],"protect":["protected-new"]}"#).unwrap();
    let p = f.e().handoff_prepare("a", "mem-pressure").unwrap();
    assert!(p.prompt.contains("ssh new-route"));
    assert!(p.prompt.contains("protected-new"));
    assert!(!p.prompt.contains("protected-a"));
}

#[test]
fn changed_protection_rules_require_a_new_preview_without_launching_the_old_prompt() {
    let f = Fixture::new();
    std::fs::write(f.dir.join("nodes.json"), r#"{"nodes":[{"id":"a","alias":"a","os":"macos"}],"protect":["protected-a","protected-new"]}"#).unwrap();
    let calls = Cell::new(0);
    let r = f.e().handoff_launch_with(&f.prepared, Agent::Codex, |_, _, _| {
        calls.set(calls.get() + 1);
        Ok(())
    });
    assert_eq!(calls.get(), 0, "new protections must not be omitted from the approved prompt");
    assert_eq!(r["ok"], false);
    assert!(r["refused"].as_str().unwrap().contains("保護対象"));
    assert!(!f.dir.join("data/handoff").exists());
}

#[test]
fn pending_insert_failure_prevents_launch() {
    let f = Fixture::new();
    f.sql("CREATE TRIGGER reject_handoff BEFORE INSERT ON tune_actions WHEN NEW.type='ai-handoff' BEGIN SELECT RAISE(FAIL,'insert rejected'); END;");
    let calls = Cell::new(0);
    let r = f.e().handoff_launch_with(&f.prepared, Agent::Claude, |_, _, _| {
        calls.set(calls.get() + 1);
        Ok(())
    });
    assert_eq!(calls.get(), 0, "no external action is allowed without a pending audit row");
    assert_eq!(r["ok"], false);
    assert!(r["refused"].as_str().unwrap().contains("実行記録"));
    assert!(f.actions().is_empty());
    assert!(!f.dir.join("data/handoff").exists());
}

#[test]
fn pending_row_exists_before_launch_and_the_same_row_is_finalized() {
    let f = Fixture::new();
    let id = std::cell::RefCell::new(String::new());
    let r = f.e().handoff_launch_with(&f.prepared, Agent::Codex, |dir, prompt, agent| {
        assert_eq!(agent, Agent::Codex);
        assert_eq!(prompt, f.prepared.prompt);
        let rows = f.actions();
        assert_eq!(rows.len(), 1, "pending must be durable before invoking a terminal");
        assert_eq!(rows[0]["state"], "incomplete");
        assert_eq!(rows[0]["params"]["prompt_sha256"], sha256_hex(prompt.as_bytes()));
        assert_eq!(rows[0]["params"]["dir"], dir.to_string_lossy().as_ref());
        *id.borrow_mut() = rows[0]["id"].as_str().unwrap().to_string();
        Ok(())
    });
    assert_eq!(r["ok"], true);
    let rows = f.actions();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], id.borrow().as_str());
    assert_eq!(rows[0]["state"], "ok");
    assert!(rows[0]["undo"].is_null());
}

#[test]
fn finalize_error_or_zero_rows_reports_an_incomplete_launch_without_retrying() {
    for statement in ["SELECT RAISE(FAIL,'finalize rejected');", "SELECT RAISE(IGNORE);"] {
        let f = Fixture::new();
        f.sql(&format!("CREATE TRIGGER reject_finish BEFORE UPDATE ON tune_actions WHEN OLD.type='ai-handoff' BEGIN {statement} END;"));
        let calls = Cell::new(0);
        let r = f.e().handoff_launch_with(&f.prepared, Agent::Codex, |_, _, _| {
            calls.set(calls.get() + 1);
            Ok(())
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(r["ok"], false, "a missing final audit row is not a recorded success");
        assert_eq!(r["launch_requested"], true);
        assert_eq!(r["audit_incomplete"], true);
        assert!(r["error"].as_str().unwrap().contains("記録"));
        let rows = f.actions();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"], "incomplete");
    }
}

#[test]
fn launcher_error_finalizes_one_failed_row() {
    let f = Fixture::new();
    let r = f.e().handoff_launch_with(&f.prepared, Agent::Claude, |_, _, _| Err("fixture terminal unavailable".into()));
    assert_eq!(r["ok"], false);
    assert!(r["refused"].as_str().unwrap().contains("fixture terminal unavailable"));
    let rows = f.actions();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "failed");
}

#[test]
fn interrupted_launch_stays_incomplete_after_reopening_the_database() {
    let mut f = Fixture::new();
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.e().handoff_launch_with(&f.prepared, Agent::Codex, |_, _, _| panic!("synthetic interruption before finalize"))
    }));
    assert!(interrupted.is_err());
    drop(f.engine.take());
    f.engine = Some(Engine::open(f.dir.join("nodes.json"), f.dir.join("data"), Arc::new(NoHost)).unwrap());
    let rows = f.actions();
    assert_eq!(rows.len(), 1, "interruption must leave a durable pending audit row");
    assert_eq!(rows[0]["state"], "incomplete");
    assert_eq!(rows[0]["ok"], false);
    assert!(rows[0]["undo"].is_null());
}

#[test]
fn cancellation_reports_audit_insert_failure_without_creating_files() {
    let f = Fixture::new();
    f.sql("CREATE TRIGGER reject_cancel BEFORE INSERT ON tune_actions WHEN NEW.type='ai-handoff' BEGIN SELECT RAISE(FAIL,'cancel insert rejected'); END;");
    let r = f.e().handoff_cancel(&f.prepared, Agent::Claude);
    assert_eq!(r["cancelled"], true);
    assert_eq!(r["ok"], false);
    assert!(r["audit_error"].is_string(), "cancellation must expose a missing audit record");
    assert!(!f.dir.join("data/handoff").exists());
}
