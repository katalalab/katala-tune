#[path = "../src/cumulative_counter.rs"]
mod cumulative_counter;
#[path = "support/owned_node.rs"]
mod owned_node;

use serde_json::{Value, json};
use std::time::Duration;

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
    let output = owned_node::capture_node(
        &[script.into_os_string()],
        owned_node::Limits { execution: Duration::from_secs(15), cleanup: Duration::from_secs(2), stdout: 4 * 1024 * 1024, stderr: 64 * 1024 },
    )
    .expect("Node parity requires bounded capture with successful exit and complete pipes");
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
