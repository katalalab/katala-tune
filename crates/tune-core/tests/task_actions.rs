use serde_json::{Value, json};
use std::time::Duration;
use tune_core::{
    actions,
    collect::{RunFuture, RunResult, Runner},
    nodes::Node,
};

struct Reply(&'static str, &'static str);
impl Runner for Reply {
    fn run<'a>(&'a self, _: &'a str, _: &'a [String], _: Option<&'a [u8]>, _: Duration) -> RunFuture<'a> {
        Box::pin(async move { RunResult { code: Some(0), out: self.0.into(), err: self.1.into() } })
    }
}
fn task(ty: &str) -> Value {
    json!({"type":ty,"params":{"path":"\\Fixture\\","name":"OwnedFixture"}})
}
fn node() -> Node {
    Node::from_value(&json!({"id":"task-fixture","alias":"mock-task","os":"windows"}), "other")
}

#[tokio::test]
async fn task_refuses_missing_contradictory_error_and_duplicate_receipts() {
    for reply in [Reply("Ready", ""), Reply("KT_TASK_STATE 1 1", ""), Reply("KT_TASK_STATE 1 0", "denied"), Reply("KT_TASK_STATE 1 0\nKT_TASK_STATE 1 0", "")] {
        let r = actions::execute_with(&reply, &node(), &task("task-disable"), Some(&[])).await.unwrap();
        assert!(!r.ok, "{}", reply.0);
        assert!(r.undo.is_none());
    }
}

#[tokio::test]
async fn task_undo_requires_verified_change_and_expected_state() {
    let r = actions::execute_with(&Reply("KT_TASK_STATE 1 0", ""), &node(), &task("task-disable"), Some(&[])).await.unwrap();
    let mut undo = task("task-enable");
    undo["params"]["expected_enabled"] = json!(false);
    assert!(r.ok);
    assert_eq!(r.undo, Some(undo.clone()));
    let noop = actions::execute_with(&Reply("KT_TASK_STATE 0 0", ""), &node(), &task("task-disable"), Some(&[])).await.unwrap();
    assert!(noop.ok);
    assert!(noop.undo.is_none());
    let stale = actions::execute_with(&Reply("KT_TASK_STATE 1 1", ""), &node(), &undo, Some(&[])).await.unwrap();
    assert!(!stale.ok);
    assert!(stale.undo.is_none());
}

#[test]
fn task_expected_state_is_boolean() {
    for value in [Value::Null, json!("false"), json!(0), json!({})] {
        let mut a = task("task-enable");
        a["params"]["expected_enabled"] = value;
        assert!(actions::plan(&node(), &a, Some(&[])).is_err());
    }
}

#[tokio::test]
async fn task_run_requires_completed_command_receipt_without_undo() {
    for out in ["Ready", "KT_TASK_RUN\nKT_TASK_RUN"] {
        let r = actions::execute_with(&Reply(out, ""), &node(), &task("task-run"), Some(&[])).await.unwrap();
        assert!(!r.ok);
        assert!(r.undo.is_none());
    }
    let r = actions::execute_with(&Reply("KT_TASK_RUN", ""), &node(), &task("task-run"), Some(&[])).await.unwrap();
    assert!(r.ok);
    assert!(r.undo.is_none());
}
