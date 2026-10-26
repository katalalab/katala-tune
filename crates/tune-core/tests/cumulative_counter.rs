#[path = "../src/cumulative_counter.rs"]
mod cumulative_counter;

use serde_json::{Value, json};
use std::process::Command;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../test/fixtures/cumulative-counter.json")).unwrap()
}

fn run_cases() -> Value {
    let fixture = fixture();
    let mut all = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let mut baseline = Value::Null;
        let mut outputs = Vec::new();
        for (i, sample) in case["samples"].as_array().unwrap().iter().enumerate() {
            let got = cumulative_counter::step(&baseline, sample, &case["window"]);
            let want = &case["expected"][i];
            assert_eq!(got["reason"], want["reason"], "{} step {i}", case["name"]);
            assert_eq!(got["rate"], want["rate"], "{} rate {i}", case["name"]);
            let want_baseline = if want["baseline"] == true { sample } else { &Value::Null };
            assert_eq!(&got["baseline"], want_baseline, "{} baseline {i}", case["name"]);
            baseline = got["baseline"].clone();
            outputs.push(got);
        }
        all.push(Value::Array(outputs));
    }
    Value::Array(all)
}

#[test]
fn literal_oracles_cover_precision_bounds_and_recovery() {
    run_cases();
}

#[test]
fn shared_fixture_matches_node_including_baselines() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js/cumulative-counter.js");
    let node = std::env::var_os("NODE").unwrap_or_else(|| "node".into());
    let output = Command::new(node).arg(script).output().expect("Node must be available for parity");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let js: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(run_cases(), js);
}

#[test]
fn malformed_previous_baseline_cannot_create_a_rate() {
    let fixture = fixture();
    let sample = &fixture["cases"][0]["samples"][0];
    assert_eq!(cumulative_counter::step(&json!({"count":"0"}), sample, &Value::Null), json!({"baseline":sample,"reason":"invalid_baseline","rate":null}));
}

#[test]
fn returned_baseline_owns_input_values() {
    let fixture = fixture();
    let mut sample = fixture["cases"][0]["samples"][0].clone();
    let output = cumulative_counter::step(&Value::Null, &sample, &Value::Null);
    sample["count"] = json!("999");
    sample["descriptor"]["source"] = json!("changed");
    assert_eq!(output["baseline"]["count"], "100");
    assert_eq!(output["baseline"]["descriptor"]["source"], "s1");
}
