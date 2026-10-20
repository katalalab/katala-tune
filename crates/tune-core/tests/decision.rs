use serde_json::json;
use tune_core::decision::{RemediationClass, StopRisk, can_auto_fix, process_stop_finding, process_stop_finding_at};
use tune_core::{db, rules};

const NOW: i64 = 1_760_000_002_000;

fn base() -> serde_json::Value {
    json!({
        "pid": 501, "ppid": 1, "name": "ExampleWorker", "start": "Thu Oct  1 08:12:03 2026",
        "decision": {
            "evidence_state": "observed", "reason_status": "confirmed", "process_class": "duplicate_candidate",
            "owner_observed": true, "collector_self": false, "service_match": true, "scheduler_match": true, "workload_match": true,
            "protect_ledger_readable": true,
            "evidence": [{ "run": "r1", "scope": "process", "source_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "observed_at": 1760000001000i64, "rowref": "p501" }],
            "cpu_samples": [
                { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 88.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": 1760000000000i64 },
                { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 91.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": 1760000001000i64 }
            ]
        }
    })
}

#[test]
fn name_and_cpu_alone_never_create_a_stop_action() {
    let f = process_stop_finding(&json!({ "pid": 99, "name": "Builder", "cpu": 120 }), &json!({}), false);
    assert!(f.proposed_action.is_none());
    assert!(f.needed_observation.iter().any(|v| v == "fresh_evidence"));
}

#[test]
fn incomplete_or_protected_evidence_never_creates_a_stop_action() {
    for (patch, needed) in [
        (json!({ "decision": { "owner_observed": false } }), "pid_name_start_owner_and_parent"),
        (json!({ "ppid": null }), "pid_name_start_owner_and_parent"),
        (json!({ "start": null }), "pid_name_start_owner_and_parent"),
        (json!({ "decision": { "evidence_state": "stale" } }), "fresh_evidence"),
        (json!({ "decision": { "evidence_state": "partial" } }), "fresh_evidence"),
        (json!({ "decision": { "process_class": "legitimate_parallel" } }), "service_scheduler_workload_attribution"),
        (json!({ "decision": { "service_match": false } }), "service_scheduler_workload_match"),
        (json!({ "decision": { "collector_self": true } }), "collector_self_excluded"),
    ] {
        let mut p = base();
        merge(&mut p, &patch);
        let f = process_stop_finding_at(&p, &json!({ "hold": false, "shared": false }), false, NOW);
        assert!(f.proposed_action.is_none());
        assert!(f.needed_observation.iter().any(|v| v == needed));
    }
    let hold = process_stop_finding_at(&base(), &json!({ "hold": true, "shared": false }), false, NOW);
    assert!(hold.proposed_action.is_none());
    assert!(hold.needed_observation.iter().any(|v| v == "explicit_node_hold_clear"));
    assert!(process_stop_finding_at(&base(), &json!({ "hold": false, "shared": true }), false, NOW).proposed_action.is_none());
    let protected = process_stop_finding_at(&base(), &json!({ "hold": false, "shared": false }), true, NOW);
    assert!(protected.proposed_action.is_none());
    assert!(protected.needed_observation.iter().any(|v| v == "protected_workload_no_stop"));
}

#[test]
fn duplicate_name_count_is_not_a_duplicate_classification() {
    let mut p = base();
    p["decision"].as_object_mut().unwrap().remove("process_class");
    p["same_name_count"] = json!(5);
    let f = process_stop_finding_at(&p, &json!({ "hold": false, "shared": false }), false, NOW);
    assert!(f.proposed_action.is_none());
    assert!(f.needed_observation.iter().any(|v| v == "service_scheduler_workload_attribution"));
}

#[test]
fn complete_confirmed_evidence_requires_approval_never_auto_fix() {
    let f = process_stop_finding_at(&base(), &json!({ "hold": false, "shared": false }), false, NOW);
    assert_eq!(f.proposed_action.as_ref().unwrap()["type"], "kill-process");
    assert_eq!(f.remediation["class"], "requires_approval");
    assert_eq!(f.remediation["can_auto_fix"], false);
    assert_eq!(f.symptom, "repeated_high_cpu_samples");
    assert_ne!(f.symptom, "sustained_high_cpu_process");
}

#[test]
fn stop_candidate_requires_explicit_shared_and_collector_exclusions() {
    for node in [
        json!({ "hold": false }),
        json!({ "hold": false, "shared": null }),
        json!({ "hold": false, "shared": true }),
        json!({ "hold": false, "shared": "false" }),
    ] {
        let f = process_stop_finding_at(&base(), &node, false, NOW);
        assert!(f.proposed_action.is_none());
        assert!(f.needed_observation.iter().any(|v| v == "explicit_node_shared_clear"));
    }
    for value in [json!(null), json!(true), json!("false")] {
        let mut p = base();
        p["decision"]["collector_self"] = value;
        let f = process_stop_finding_at(&p, &json!({ "hold": false, "shared": false }), false, NOW);
        assert!(f.proposed_action.is_none());
        assert!(f.needed_observation.iter().any(|v| v == "collector_self_excluded"));
    }
    let mut missing = base();
    missing["decision"].as_object_mut().unwrap().remove("collector_self");
    let missing_finding = process_stop_finding_at(&missing, &json!({ "hold": false, "shared": false }), false, NOW);
    assert!(missing_finding.proposed_action.is_none());
    assert!(missing_finding.needed_observation.iter().any(|v| v == "collector_self_excluded"));
    assert!(process_stop_finding_at(&base(), &json!({ "hold": false, "shared": false }), false, NOW).proposed_action.is_some());
}

#[test]
fn stop_candidate_requires_valid_identity_and_two_high_cpu_samples() {
    for patch in
        [json!({ "ppid": null }), json!({ "ppid": 0 }), json!({ "ppid": -1 }), json!({ "name": null }), json!({ "name": 7 }), json!({ "name": "unknown" })]
    {
        let mut p = base();
        merge(&mut p, &patch);
        let f = process_stop_finding_at(&p, &json!({ "hold": false, "shared": false }), false, NOW);
        assert!(f.proposed_action.is_none(), "{patch}");
        assert!(f.needed_observation.iter().any(|v| v == "pid_name_start_owner_and_parent"));
    }
    let mut missing_name = base();
    missing_name.as_object_mut().unwrap().remove("name");
    let missing_name_finding = process_stop_finding_at(&missing_name, &json!({ "hold": false, "shared": false }), false, NOW);
    assert!(missing_name_finding.proposed_action.is_none());
    assert!(missing_name_finding.needed_observation.iter().any(|v| v == "pid_name_start_owner_and_parent"));
    for samples in [
        json!([
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 0.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 2_000 },
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 0.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 1_000 }
        ]),
        json!([
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 49.9, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 2_000 },
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 91.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 1_000 }
        ]),
        json!([
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 10_001.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 2_000 },
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 91.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 1_000 }
        ]),
    ] {
        let mut p = base();
        p["decision"]["cpu_samples"] = samples;
        let f = process_stop_finding_at(&p, &json!({ "hold": false, "shared": false }), false, NOW);
        assert!(f.proposed_action.is_none());
        assert!(f.needed_observation.iter().any(|v| v == "two_cpu_time_samples"));
    }
}

#[test]
fn rules_entrypoint_does_not_treat_old_snapshot_at_as_the_current_time() {
    let old_at = db::now_ms() - 300_001;
    let mut process = base();
    process["decision"]["evidence"][0]["observed_at"] = json!(old_at);
    process["decision"]["cpu_samples"][0]["at"] = json!(old_at - 1_000);
    process["decision"]["cpu_samples"][1]["at"] = json!(old_at);
    let findings = rules::analyze(
        &json!({
            "probe": "mac", "at": old_at, "host": { "cores": 8 },
            "processes": { "top_cpu": [{
                "pid": process["pid"], "ppid": process["ppid"], "name": process["name"], "start": process["start"], "cpu": 105,
                "decision": process["decision"]
            }], "apps": [], "apps_cpu": [] }
        }),
        &json!({ "hold": false, "shared": false }),
    );
    let finding = findings.iter().find(|f| f["id"] == "runaway-ExampleWorker-501").unwrap();
    assert!(finding.get("action").is_none());
    assert!(finding["decision"]["needed_observation"].as_array().unwrap().iter().any(|v| v == "fresh_timestamp_and_clock_consistency"));
}

#[test]
fn public_decision_omits_argv_owner_and_paths() {
    let mut p = base();
    p["argv"] = json!(["--token=secret"]);
    p["owner"] = json!("private-owner");
    p["path"] = json!("/private/path");
    p["decision"]["evidence"][0]["rowref"] = json!("/private/path");
    let output = serde_json::to_string(&process_stop_finding_at(&p, &json!({ "hold": false, "shared": false }), false, NOW)).unwrap();
    for private in ["secret", "private-owner", "/private/path"] {
        assert!(!output.contains(private));
    }
}

#[test]
fn observed_label_does_not_override_stale_future_or_inconsistent_timestamps() {
    for (patch, needed) in [
        (
            json!({ "decision": { "evidence": [{ "run": "r1", "scope": "process", "source_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "observed_at": NOW - 300_001, "rowref": "p501" }] } }),
            "fresh_timestamp_and_clock_consistency",
        ),
        (
            json!({ "decision": { "evidence": [{ "run": "r1", "scope": "process", "source_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "observed_at": NOW + 1, "rowref": "p501" }] } }),
            "fresh_timestamp_and_clock_consistency",
        ),
        (
            json!({ "decision": { "cpu_samples": [
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 88.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 1_000 },
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 91.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW + 1 }
        ] } }),
            "fresh_timestamp_and_clock_consistency",
        ),
        (
            json!({ "decision": { "cpu_samples": [
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 88.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 2_000 },
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 91.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 2000, "at": NOW }
        ] } }),
            "two_cpu_time_samples",
        ),
        (
            json!({ "decision": { "cpu_samples": [
            { "pid": 999, "start": "Thu Oct  1 08:12:03 2026", "value": 88.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 2_000 },
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 91.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 1_000 }
        ] } }),
            "two_cpu_time_samples",
        ),
        (
            json!({ "decision": { "cpu_samples": [
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 88.0, "unit": "/private/path", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 2_000 },
            { "pid": 501, "start": "Thu Oct  1 08:12:03 2026", "value": 91.0, "unit": "percent_of_core", "definition": "cpu_time_delta", "sample_window_ms": 1000, "at": NOW - 1_000 }
        ] } }),
            "two_cpu_time_samples",
        ),
    ] {
        let mut p = base();
        merge(&mut p, &patch);
        let f = process_stop_finding_at(&p, &json!({ "hold": false, "shared": false }), false, NOW);
        assert!(f.proposed_action.is_none(), "{patch}");
        assert!(f.needed_observation.iter().any(|v| v == needed));
    }
}

#[test]
fn auto_fix_is_derived_from_safe_read_only_conditions() {
    assert!(can_auto_fix(RemediationClass::AutoSafe, StopRisk::None, false, true));
    assert!(!can_auto_fix(RemediationClass::RequiresApproval, StopRisk::None, false, true));
    assert!(!can_auto_fix(RemediationClass::AutoSafe, StopRisk::High, false, true));
    assert!(!can_auto_fix(RemediationClass::AutoSafe, StopRisk::None, true, true));
}

fn merge(target: &mut serde_json::Value, patch: &serde_json::Value) {
    if let (Some(a), Some(b)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in b {
            if value.is_object() && a.get(key).is_some_and(serde_json::Value::is_object) {
                merge(a.get_mut(key).unwrap(), value);
            } else {
                a.insert(key.clone(), value.clone());
            }
        }
    }
}
