use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tune_core::collect::{RunFuture, RunResult, Runner, probe_node_with};
use tune_core::nodes::Node;

const BUILTIN_NULL_METRICS: &str = r#"{"probe":"mac","probe_version":2,"host":{"hostname":"synthetic","os":"macos","model":null,"cpu":null,"cores":null,"p_cores":null,"e_cores":null,"uptime_h":null},"load":[]}"#;
const BUILTIN_OMITTED_METRICS: &str = r#"{"probe":"mac","probe_version":2,"host":{"hostname":"synthetic","os":"macos"},"load":[]}"#;

struct FakeRunner {
    code: Option<i32>,
    out: &'static str,
    calls: AtomicUsize,
}

impl Runner for FakeRunner {
    fn run_mac_probe<'a>(&'a self, cmd: &'a str, args: &'a [String], input: Option<&'a [u8]>, timeout: Duration) -> tune_core::collect::MacProbeFuture<'a> {
        Box::pin(async move { Ok(self.run(cmd, args, input, timeout).await) })
    }

    fn probe_exchange<'a>(&'a self, _node: &'a Node, _helper_sha256: &'a str, _benchmark: bool, _network: bool, _observe_wsl: bool) -> RunFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { RunResult { code: self.code, out: self.out.to_owned(), err: String::new() } })
    }
    fn run<'a>(&'a self, _cmd: &'a str, _args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { RunResult { code: self.code, out: self.out.to_owned(), err: String::new() } })
    }
}

fn immediate<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("unexpected_pending"),
    }
}

fn synthetic_node(os: &str) -> Node {
    Node {
        id: "synthetic".into(),
        alias: "synthetic.invalid".into(),
        os: os.into(),
        shared: false,
        local: false,
        raw: serde_json::json!({ "action_helper": { "schema": "katala-tune/action-helper/v2", "run_id": 1, "source_sha": "a".repeat(40), "tune_sha256": "b".repeat(64), "manifest_sha256": "c".repeat(64), "attempt": 1 } }),
    }
}

fn observed_success(code: Option<i32>, out: &'static str) -> bool {
    observed_success_os("macos", code, out)
}

fn observed_success_os(os: &str, code: Option<i32>, out: &'static str) -> bool {
    observed_success_os_with_calls(os, code, out, 1)
}

fn observed_success_os_with_calls(os: &str, code: Option<i32>, out: &'static str, expected_calls: usize) -> bool {
    let runner = FakeRunner { code, out, calls: AtomicUsize::new(0) };
    let node = synthetic_node(os);
    let result = immediate(probe_node_with(&runner, &node));
    assert_eq!(runner.calls.load(Ordering::SeqCst), expected_calls, "fake_runner_call_count");
    result.get("ok").and_then(|value| value.as_bool()).expect("result_shape")
}

#[test]
fn mac_nonzero_object() {
    assert!(!observed_success(Some(9), BUILTIN_NULL_METRICS), "completion_integrity");
}

#[test]
fn mac_unfinished_object() {
    assert!(!observed_success(None, BUILTIN_NULL_METRICS), "completion_integrity");
}

#[test]
fn mac_zero_empty_object() {
    assert!(!observed_success(Some(0), "{}"), "completion_integrity");
}

#[test]
fn mac_zero_unrelated_object() {
    assert!(!observed_success(Some(0), r#"{"unrelated":true}"#), "completion_integrity");
}

#[test]
fn mac_zero_null() {
    assert!(!observed_success(Some(0), "null"), "completion_integrity");
}

#[test]
fn mac_zero_builtin_null_metrics() {
    assert!(observed_success(Some(0), BUILTIN_NULL_METRICS), "completion_integrity");
}

#[test]
fn mac_zero_builtin_omitted_metrics() {
    assert!(observed_success(Some(0), BUILTIN_OMITTED_METRICS), "completion_integrity");
}

const SYNTHETIC_MAC_VALID: &str = r#"{"probe":"mac","probe_version":2,"host":{}}"#;
const SYNTHETIC_WINDOWS_VALID: &str = r#"{"probe":"windows","probe_version":2,"host":{}}"#;

#[test]
fn rust_windows_fake_envelopes() {
    for out in [
        "{}",
        r#"{"unrelated":true}"#,
        r#"{"probe":"mac","probe_version":2,"host":{}}"#,
        r#"{"probe":"windows","probe_version":999,"host":{}}"#,
        r#"{"probe":"windows","probe_version":2}"#,
        r#"{"probe":"windows","probe_version":2,"host":null}"#,
        r#"{"probe":"windows","probe_version":2,"host":[]}"#,
        "null",
    ] {
        assert!(!observed_success_os("windows", Some(0), out));
    }
    assert!(!observed_success_os("windows", Some(1), SYNTHETIC_WINDOWS_VALID));
    assert!(!observed_success_os("windows", None, SYNTHETIC_WINDOWS_VALID));
    assert!(observed_success_os("windows", Some(0), SYNTHETIC_WINDOWS_VALID));
    assert!(observed_success_os("windows", Some(0), r#"{"probe":"windows","probe_version":2,"host":{},"metric":null}"#));
    assert!(observed_success_os("windows", Some(0), r#"{"probe":"windows","probe_version":2,"host":{},"optional":"unknown"}"#));
}

#[test]
fn rust_malformed_final_lines_do_not_restore_older_envelope() {
    for out in [
        concat!(r#"{"probe":"mac","probe_version":2,"host":{}}"#, "\nnull"),
        concat!(r#"{"probe":"mac","probe_version":2,"host":{}}"#, "\n[]"),
        concat!(r#"{"probe":"mac","probe_version":2,"host":{}}"#, "\nsynthetic trailing text"),
        concat!(r#"{"probe":"mac","probe_version":2,"host":{}}"#, "\n{\"incomplete\":"),
    ] {
        assert!(!observed_success_os("macos", Some(0), out));
    }
    for out in [
        concat!(r#"{"probe":"windows","probe_version":2,"host":{}}"#, "\nnull"),
        concat!(r#"{"probe":"windows","probe_version":2,"host":{}}"#, "\n[]"),
        concat!(r#"{"probe":"windows","probe_version":2,"host":{}}"#, "\nsynthetic trailing text"),
        concat!(r#"{"probe":"windows","probe_version":2,"host":{}}"#, "\n{\"incomplete\":"),
    ] {
        assert!(!observed_success_os("windows", Some(0), out));
    }
}

#[test]
fn rust_prefix_banner_and_separate_benchmark_are_allowed() {
    assert!(observed_success_os("macos", Some(0), concat!("synthetic banner\n", r#"{"probe":"mac","probe_version":2,"host":{}}"#)));
    assert!(observed_success_os("windows", Some(0), concat!("synthetic banner\n", r#"{"probe":"windows","probe_version":2,"host":{}}"#)));
    assert!(observed_success_os(
        "macos",
        Some(0),
        concat!(r#"{"probe":"mac","probe_version":2,"host":{}}"#, "\n@@KATALA_TUNE_BENCH@@\n", r#"{"synthetic_score":1}"#)
    ));
    assert!(observed_success_os(
        "windows",
        Some(0),
        concat!(r#"{"probe":"windows","probe_version":2,"host":{}}"#, "\n@@KATALA_TUNE_BENCH@@\n", r#"{"synthetic_score":1}"#)
    ));
}

#[test]
fn rust_unsupported_os_never_dispatches() {
    assert!(!observed_success_os_with_calls("unsupported", Some(0), SYNTHETIC_WINDOWS_VALID, 0));
}

struct LegacyOnlyRunner;
impl Runner for LegacyOnlyRunner {
    fn run<'a>(&'a self, _cmd: &'a str, _args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        let _ = (_cmd, _args, _input, _timeout);
        panic!("synthetic legacy run must not be called")
    }
}

struct FailureRunner(tune_core::collect::MacProbeFailure);
impl Runner for FailureRunner {
    fn run<'a>(&'a self, _cmd: &'a str, _args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        let _ = (_cmd, _args, _input, _timeout);
        panic!("synthetic legacy run must not be called")
    }

    fn run_mac_probe<'a>(&'a self, cmd: &'a str, args: &'a [String], input: Option<&'a [u8]>, timeout: Duration) -> tune_core::collect::MacProbeFuture<'a> {
        let _ = (cmd, args, input, timeout);
        let failure = self.0;
        Box::pin(async move { Err(failure) })
    }
}

#[test]
fn rust_legacy_runner_cannot_claim_completion() {
    let runner = LegacyOnlyRunner;
    let node = synthetic_node("macos");
    let result = immediate(probe_node_with(&runner, &node));
    assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(result.get("reason").and_then(|v| v.as_str()), Some("error"));
}

#[test]
fn rust_timeout_and_incomplete_reject_before_json() {
    for (failure, expected) in [(tune_core::collect::MacProbeFailure::Timeout, "timeout"), (tune_core::collect::MacProbeFailure::Incomplete, "error")] {
        let runner = FailureRunner(failure);
        let node = synthetic_node("macos");
        let result = immediate(probe_node_with(&runner, &node));
        assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(result.get("reason").and_then(|v| v.as_str()), Some(expected));
    }
}

#[test]
fn rust_disabled_benchmark_is_not_reintroduced() {
    const OUT: &str = concat!(r#"{"probe":"mac","probe_version":2,"host":{}}"#, "\n@@KATALA_TUNE_BENCH@@\n", r#"{"runs_ms":[1,100,1000],"median_ms":100}"#,);
    for enabled in [false, true] {
        let runner = FakeRunner { code: Some(0), out: OUT, calls: AtomicUsize::new(0) };
        let mut node = synthetic_node("macos");
        node.raw["benchmark"] = serde_json::json!(enabled);
        let result = immediate(probe_node_with(&runner, &node));
        assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(true));
        let bench = result.get("data").and_then(|v| v.get("bench")).expect("bench field");
        if enabled {
            assert!(bench.is_object());
        } else {
            assert!(bench.is_null());
        }
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
    }
}
