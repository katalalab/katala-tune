use std::{path::Path, sync::Mutex, time::Duration};

use serde_json::json;
use tune_core::{
    actions,
    collect::{RunFuture, RunResult, Runner},
    db,
    nodes::{Node, parse_config},
    rules,
};

type CaptureCall = (String, Vec<String>, Option<Vec<u8>>);

#[derive(Default)]
struct CaptureRunner(Mutex<Vec<CaptureCall>>);

impl Runner for CaptureRunner {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        let call = (cmd.to_string(), args.to_vec(), input.map(Vec::from));
        Box::pin(async move {
            self.0.lock().unwrap().push(call);
            RunResult { code: Some(9), out: "GPU activity is active; operation refused".into(), err: String::new() }
        })
    }
}

fn worker(now: i64) -> serde_json::Value {
    json!({
        "pid": 4242, "ppid": 1, "name": "TrainerHost", "app": "TrainerHost", "cpu": 8.0, "avg_core": 80.0, "start": "20261021010101000",
        "decision": {
            "evidence_state":"observed", "reason_status":"confirmed", "process_class":"duplicate_candidate", "collector_self":false,
            "owner_observed":true, "service_match":true, "scheduler_match":true, "workload_match":true, "protect_ledger_readable":true,
            "evidence":[{"run":"r1","scope":"process","source_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","observed_at":now - 1000,"rowref":"p4242"}],
            "cpu_samples":[
                {"pid":4242,"start":"20261021010101000","value":90.0,"unit":"percent_of_core","definition":"cpu_time_delta","sample_window_ms":1000,"at":now - 2000},
                {"pid":4242,"start":"20261021010101000","value":91.0,"unit":"percent_of_core","definition":"cpu_time_delta","sample_window_ms":1000,"at":now - 1000}
            ]
        }
    })
}

#[test]
fn compute_pid_or_unknown_gpu_blocks_stop_candidates() {
    let now = db::now_ms();
    for compute in [
        json!({"availability":"available","apps":[{"pid":4242,"used_memory_mb":null}]}),
        json!({"availability":"unavailable","apps":[],"reason":"query-failed"}),
    ] {
        let snap = json!({"probe":"windows","at":now,"host":{"cores":20},"gpus":[{"util":0.0}],"gpu_compute":compute,"processes":{"top_cpu":[worker(now)],"apps":[],"apps_cpu":[]}});
        let finding = rules::analyze(&snap, &json!({"hold":false,"shared":false})).into_iter().find(|f| f["id"] == "runaway-TrainerHost-4242").unwrap();
        assert!(finding.get("action").is_none());
        assert_eq!(finding["decision"]["reason"]["code"], "protected_workload");
    }
}

#[test]
fn plans_recheck_gpu_and_ledger_attribute_is_optional() {
    let normal = Node::from_value(&json!({"id":"w","alias":"w","os":"windows"}), "other");
    let kill =
        actions::plan(&normal, &json!({"type":"kill-process","params":{"pid":4242,"name":"TrainerHost","start":"20261021010101000"}}), Some(&[])).unwrap();
    let power = actions::plan(
        &normal,
        &json!({"type":"set-gpu-power-limit","params":{"uuid":"GPU-01234567-89ab-cdef-0123-456789abcdef","watts":180,"min":100,"max":250,"prev_w":200}}),
        Some(&[]),
    )
    .unwrap();
    for plan in [&kill, &power] {
        assert!(plan.script.contains("--query-compute-apps=pid,used_gpu_memory"));
        assert!(plan.script.contains("[double]::IsNaN($util)"));
        assert!(plan.script.contains("[int]::TryParse($part[0].Trim(), [ref]$gpuPid)"));
        assert!(plan.script.contains("Get-CimInstance Win32_VideoController -Property Name,PNPDeviceID"));
        assert!(plan.script.contains("exit 9"));
        assert!(plan.script.contains("BaseStream.ReadAsync($outBuffer, 0, $outBuffer.Length)"));
        assert!(plan.script.contains("BaseStream.ReadAsync($errBuffer, 0, $errBuffer.Length)"));
        assert!(plan.script.contains("$stdout.Length + $outCount -gt 65536"));
        assert!(plan.script.contains("$stderrCount + $errCount -gt 65536"));
        assert!(plan.script.contains("[Threading.Tasks.Task]::WaitAny"));
        assert!(plan.script.contains("if ($failed) { $script:KatalaGpuSmiTransportFailed = $true;"));
        assert!(
            plan.script.contains(
                "$p.StartInfo = $si; $smiDeadline = [Math]::Min(9000, [int]$script:KatalaGpuGuardClock.ElapsedMilliseconds + 4000); [void]$p.Start();"
            )
        );
        assert!(plan.script.contains("$killAck = $Process.WaitForExit([Math]::Min(1000, $left)); if (-not $killAck -or -not $Process.HasExited)"));
        assert!(!plan.script.contains("ReadToEndAsync"));
    }
    assert_eq!(actions::exit_text(9), Some("GPU の活動を確認できた、または取得できないため中止した"));
    let protected = Node::from_value(&json!({"id":"w","alias":"w","os":"windows","protect_gpu":true}), "other");
    assert!(
        actions::plan(&protected, &json!({"type":"kill-process","params":{"pid":4242,"name":"TrainerHost"}}), Some(&[])).unwrap_err().contains("GPU 学習保護")
    );
    assert!(parse_config(r#"{"nodes":[{"id":"w","alias":"w","os":"windows"}]}"#, Path::new("nodes.json"), "other").is_ok());
    assert!(parse_config(r#"{"nodes":[{"id":"w","protect_gpu":"true"}]}"#, Path::new("nodes.json"), "other").unwrap_err().contains("protect_gpu"));
}

#[tokio::test]
async fn remote_windows_gpu_action_uses_receiver_and_complete_stdin_packet() {
    let runner = CaptureRunner::default();
    let node = Node::from_value(&json!({"id":"w","alias":"w","os":"windows"}), "other");
    let action =
        json!({"type":"set-gpu-power-limit","params":{"uuid":"GPU-01234567-89ab-cdef-0123-456789abcdef","watts":180,"min":100,"max":250,"prev_w":200}});
    let result = actions::execute_with(&runner, &node, &action, Some(&[])).await.unwrap();
    assert_eq!(result.code, Some(9));
    let calls = runner.0.lock().unwrap();
    let (cmd, args, input) = &calls[0];
    assert_eq!(cmd, "ssh");
    assert!(args.last().is_some_and(|arg| arg.starts_with("powershell.exe -NoProfile -NonInteractive -EncodedCommand ")));
    let input = input.as_ref().unwrap();
    assert!(input.starts_with(b"KT1 "));
    assert!(std::str::from_utf8(input).unwrap().contains("Invoke-KatalaGpuSmi"));
}
