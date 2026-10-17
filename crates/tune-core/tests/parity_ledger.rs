//! 台帳の運用状態（lib/ledger.js・lib/health.js）を仕様として、tune-core が同じ入力に同じ出力を返すかを確かめる。
//! 入力は架空の機体だけ。node（24 以上）が要る（KATALA_TUNE_PARITY=skip で飛ばせる）。

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tune_core::health;
use tune_core::nodes::{Node, Observation, partition, target_ledger};

fn cases() -> (Vec<Value>, Value) {
    let nodes = vec![
        json!({ "id": "a", "os": "macos" }),
        json!({ "id": "b", "os": "windows", "status": "online" }),
        json!({ "id": "c", "os": "windows", "status": "retired", "status_reason": " 解体 ", "status_date": "2026-09-30" }),
        json!({ "id": "d", "os": "macos", "status": "NO-SSH" }),
        json!({ "id": "e", "os": "windows", "status": "paused", "status_reason": 7 }),
        json!({ "id": "f", "os": "windows", "status": "out_of_scope", "status_date": "2026-10-01" }),
        json!({ "id": "g", "os": "windows", "status": "retried" }),
        json!({ "id": "h", "os": "windows", "status": 3 }),
        json!({ "id": "i", "os": "windows", "status": true }),
        json!({ "id": "j", "os": "macos", "status": null }),
        json!({ "id": "k", "os": "macos", "status": "" }),
        json!({ "id": "l", "os": "macos", "status": ["retired"] }),
        json!({ "id": "m", "os": "macos", "status": "a-very-long-unknown-status-value-that-exceeds-forty-characters" }),
        json!({ "id": "n", "os": "macos", "status": "  Retired  " }),
    ];
    let obs = json!({
        "a": { "ok": 1_800_000_000_000_i64 },
        "b": { "err": "ssh: connect to host b port 22: Operation timed out\n" },
        "j": { "ok": 5, "err": "x\ny\nz" },
    });
    (nodes, obs)
}

#[test]
fn ledger_matches_js() {
    if std::env::var("KATALA_TUNE_PARITY").as_deref() == Ok("skip") {
        eprintln!("KATALA_TUNE_PARITY=skip のため JS との比較を飛ばした");
        return;
    }
    let (raw, obs) = cases();
    let nodes: Vec<Node> = raw.iter().map(|v| Node::from_value(v, "no-such-host")).collect();
    let dir = std::env::temp_dir().join(format!("kt-parity-ledger-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("cases.json");
    std::fs::write(&f, json!({ "nodes": raw, "obs": obs }).to_string()).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("js").join("eval_ledger.js");
    let out = Command::new("node").arg("--no-warnings").arg(&script).arg(&f).output().expect("node が要る（KATALA_TUNE_PARITY=skip で飛ばせる）");
    assert!(out.status.success(), "eval_ledger.js が失敗: {}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
    let js: Value = serde_json::from_slice(&out.stdout).unwrap();

    let op: Vec<Value> = nodes.iter().map(|n| serde_json::to_value(OpJson::from(n)).unwrap()).collect();
    assert_eq!(js["opState"], Value::Array(op));
    let refusal: Vec<Value> = nodes.iter().map(|n| n.refusal().map_or(Value::Null, Value::String)).collect();
    assert_eq!(js["refusal"], Value::Array(refusal));
    let (targets, excluded) = partition(&nodes);
    assert_eq!(js["partition"], Value::Array(excluded));
    assert_eq!(js["targets"], json!(targets.iter().map(|n| n.id.clone()).collect::<Vec<_>>()));
    let led = target_ledger(&nodes, |id| Observation { last_ok_at: obs[id]["ok"].as_i64(), last_error: obs[id]["err"].as_str().map(str::to_string) });
    assert_eq!(js["targetLedger"], led);
    let checks: Vec<Value> = nodes
        .iter()
        .map(|n| {
            Value::Array(
                health::node_checks(&n.raw, None, &[], &json!({ "now": 1 }))
                    .into_iter()
                    .filter(|c| c.id == "ledger")
                    .map(|c| serde_json::to_value(c).unwrap())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(js["checks"], Value::Array(checks));
}

#[derive(serde::Serialize)]
struct OpJson {
    state: String,
    active: bool,
    valid: bool,
    reason: String,
    date: String,
}

impl From<&Node> for OpJson {
    fn from(n: &Node) -> Self {
        let s = n.op_state();
        OpJson { state: s.state, active: s.active, valid: s.valid, reason: s.reason, date: s.date }
    }
}
