//! Finite Windows log exchange, independent of action/probe controllers.
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::collect::RunResult;

const MAX_SCRIPT: usize = 1024 * 1024;
const MAX_STDOUT: usize = 4 * 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const FAILURE: &str = "Windows log transport failed";
const INVALID: &str = "Invalid Windows log transport request";

// In non-test builds this preserves the original result expression.
macro_rules! diagnostic_result {
    ($result:expr, $stage:ident) => {{
        let result = $result;
        #[cfg(test)]
        if result.is_err() {
            super::transport_diagnostics::fail(super::transport_diagnostics::Stage::$stage);
        }
        result
    }};
}

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub(super) struct Transport {
    pub command: String,
    pub bootstrap: String,
    pub input: String,
}

pub(super) fn windows_script_transport(script: &[u8], kind: &str, cursors: [&str; 3]) -> Result<Transport, String> {
    let script = script.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(script);
    let body: Vec<u8> = script.iter().enumerate().filter(|&(i, &b)| !(b == b'\r' && script.get(i + 1) == Some(&b'\n'))).map(|(_, &b)| b).collect();
    if body.is_empty() || body.len() > MAX_SCRIPT || !matches!(kind, "events" | "logons") {
        return Err(INVALID.into());
    }
    for cursor in cursors {
        let Ok(value) = cursor.parse::<i64>() else { return Err(INVALID.into()) };
        if value.to_string() != cursor || if kind == "events" { value.unsigned_abs() > 9_007_199_254_740_991 } else { value < 0 } {
            return Err(INVALID.into());
        }
    }
    if kind == "logons" && cursors[2] != "0" {
        return Err(INVALID.into());
    }
    let bootstrap = include_str!("../../../../probes/win_logs_bootstrap.ps1").trim_start_matches('\u{feff}').replace("\r\n", "\n").trim().to_owned();
    let utf16: Vec<u8> = bootstrap.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let command = format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}", super::base64(&utf16));
    if command.len() >= 8191 {
        return Err("Windows log bootstrap exceeds 8191 bytes".into());
    }
    let input =
        format!("KTLOG1\n{kind}\n{}\n{}\n{}\n{}\n{}\n{}", cursors[0], cursors[1], cursors[2], body.len(), crate::db::sha256_hex(&body), super::base64(&body));
    Ok(Transport { command, bootstrap, input })
}

async fn read_complete(pipe: impl AsyncRead + Unpin, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.take(limit as u64 + 1).read_to_end(&mut bytes).await?;
    if bytes.len() > limit {
        return Err(std::io::Error::other(FAILURE));
    }
    Ok(bytes)
}

/// Release parser input only after stdin, both EOFs and exit 0 before deadline.
pub(super) async fn run(mut command: Command, input: &[u8], timeout: Duration) -> RunResult {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let Ok(mut child) = diagnostic_result!(command.spawn(), Spawn) else { return failure() };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let deadline = tokio::time::Instant::now() + timeout;
    let exchange = async {
        let writer = async {
            diagnostic_result!(stdin.write_all(input).await, Write)?;
            diagnostic_result!(stdin.shutdown().await, Shutdown)?;
            drop(stdin);
            #[cfg(test)]
            super::transport_diagnostics::completed(1);
            Ok::<_, std::io::Error>(())
        };
        #[cfg(test)]
        let read_out = async {
            let result = diagnostic_result!(read_complete(stdout, MAX_STDOUT).await, StdoutRead)?;
            #[cfg(test)]
            super::transport_diagnostics::completed(2);
            Ok::<_, std::io::Error>(result)
        };
        #[cfg(test)]
        let read_err = async {
            let result = diagnostic_result!(read_complete(stderr, MAX_STDERR).await, StderrRead)?;
            #[cfg(test)]
            super::transport_diagnostics::completed(4);
            Ok::<_, std::io::Error>(result)
        };
        #[cfg(test)]
        let wait = async {
            let result = diagnostic_result!(child.wait().await, Wait)?;
            #[cfg(test)]
            super::transport_diagnostics::completed(8);
            Ok::<_, std::io::Error>(result)
        };
        #[cfg(test)]
        let (_, out, err, status) = tokio::try_join!(writer, read_out, read_err, wait)?;
        #[cfg(not(test))]
        let (_, out, err, status) = tokio::try_join!(writer, read_complete(stdout, MAX_STDOUT), read_complete(stderr, MAX_STDERR), child.wait())?;
        if !status.success() {
            #[cfg(test)]
            super::transport_diagnostics::fail(super::transport_diagnostics::Stage::Exit);
            return Err(std::io::Error::other(FAILURE));
        }
        let out = diagnostic_result!(String::from_utf8(out).map_err(|_| std::io::Error::other(FAILURE)), StdoutEncoding)?;
        let err = diagnostic_result!(String::from_utf8(err).map_err(|_| std::io::Error::other(FAILURE)), StderrEncoding)?;
        Ok::<_, std::io::Error>(RunResult {
            code: Some(0),
            out: out.strip_prefix('\u{feff}').unwrap_or(&out).to_owned(),
            err: err.strip_prefix('\u{feff}').unwrap_or(&err).to_owned(),
        })
    };
    #[cfg(test)]
    match tokio::time::timeout_at(deadline, exchange).await {
        Ok(Ok(result)) if tokio::time::Instant::now() <= deadline => return result,
        Ok(Ok(_)) => super::transport_diagnostics::fail(super::transport_diagnostics::Stage::Late),
        Err(_) => super::transport_diagnostics::fail(super::transport_diagnostics::Stage::Deadline),
        _ => {}
    }
    #[cfg(not(test))]
    if let Ok(Ok(result)) = tokio::time::timeout_at(deadline, exchange).await
        && tokio::time::Instant::now() <= deadline
    {
        return result;
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    failure()
}

fn failure() -> RunResult {
    RunResult { code: None, out: String::new(), err: FAILURE.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::{self, DbAccess};
    use serde_json::{Value, json};
    use std::io::Write;
    use std::sync::Mutex;

    fn fixture_result(c: &Value, valid: &Value) -> RunResult {
        RunResult {
            code: c["code"].as_i64().map(|n| n as i32),
            out: match c["output"].as_str().unwrap() {
                "valid" => valid.to_string(),
                "partial" => "{\"sources\":".into(),
                _ => String::new(),
            },
            err: c["err"].as_str().unwrap().into(),
        }
    }

    #[test]
    fn windows_log_transport_parity_matches_js() {
        if std::env::var("KATALA_TUNE_PARITY").as_deref() == Ok("skip") {
            return;
        }
        let cases: Value = serde_json::from_str(include_str!("../../../../test/fixtures/log-transport.json")).unwrap();
        let transports: Vec<Value> = cases["transport"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let bytes = c["script"].as_str().unwrap().repeat(c["repeat"].as_u64().unwrap_or(1) as usize);
                let cursors = c["cursors"].as_object().unwrap();
                let result = if c["kind"] == "events" {
                    logs::windows_remote_transport(cursors, bytes.as_bytes())
                } else {
                    logs::windows_logons_transport(cursors, bytes.as_bytes())
                };
                result.map(|r| serde_json::to_value(r).unwrap()).unwrap_or_else(|error| json!({"error":error}))
            })
            .collect();
        let node = crate::nodes::Node::from_value(&json!({"id":"example-node","alias":"example-node","os":"windows","network":false}), "no-such-host");
        let srcs = ["win_system", "win_application", "neonmonitor"];
        let valid = json!({"sources": srcs.iter().map(|s| ((*s).to_owned(), json!({"cursor":"10","rows":[]}))).collect::<serde_json::Map<_,_>>()});
        let mut results = Vec::new();
        let mut security = Vec::new();
        for c in cases["results"].as_array().unwrap() {
            let db = Mutex::new(crate::db::Store::open_in_memory().unwrap());
            for s in srcs {
                db.with(|d| d.cursor_ok(&node.id, s, Some(&json!("5")), 0, &json!(0), None)).unwrap();
            }
            let summary = logs::ingest_result(&db, &node, &srcs, fixture_result(c, &valid));
            let state: serde_json::Map<String, Value> = srcs
                .iter()
                .map(|s| {
                    let cursor = db.with(|d| d.cursor(&node.id, s)).unwrap().unwrap();
                    ((*s).into(), json!({"cursor":cursor["cursor"],"error":cursor["last_error"]}))
                })
                .collect();
            results.push(json!({"error":summary.get("error").unwrap_or(&Value::Null),"state":state}));
            let r = fixture_result(c, &json!({"sources":{"win_security":{"cursor":"9","rows":[]}}}));
            let main = RunResult { code: Some(0), out: valid.to_string(), err: String::new() };
            let merged = logs::merge_logons(main, r);
            let data = crate::collect::last_json_line(&merged.out).unwrap();
            let security_node = crate::nodes::Node::from_value(&json!({"id":"example-node","os":"windows"}), "no-such-host");
            let all = ["win_system", "win_application", "neonmonitor", "win_security"];
            let db = Mutex::new(crate::db::Store::open_in_memory().unwrap());
            for s in all {
                db.with(|d| d.cursor_ok(&security_node.id, s, Some(&json!("5")), 0, &json!(0), None)).unwrap();
            }
            logs::ingest_result(&db, &security_node, &all, merged);
            let state: serde_json::Map<String, Value> = ["win_system", "win_security"]
                .iter()
                .map(|s| {
                    let cursor = db.with(|d| d.cursor(&security_node.id, s)).unwrap().unwrap();
                    ((*s).into(), json!({"cursor":cursor["cursor"],"error":cursor["last_error"]}))
                })
                .collect();
            security.push(json!({"data":data,"state":state}));
        }
        let expected = json!({"transport":transports,"results":results,"security":security});
        let evaluator = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js/eval_log_transport.js");
        let mut child = std::process::Command::new("node")
            .arg(evaluator)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("node >=24 is required for transport parity");
        child.stdin.take().unwrap().write_all(cases.to_string().as_bytes()).unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success(), "log transport parity evaluator failed");
        let actual: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(expected, actual, "dedicated log transport parity");
    }

    #[tokio::test]
    async fn windows_log_runner_requires_write_both_eofs_and_zero_exit() {
        let script = "process.stdin.resume();let n=0;process.stdin.on('data',b=>n+=b.length);process.stdin.on('end',()=>process.stdout.write(JSON.stringify({bytes:n,text:'日本語'})));";
        let mut cmd = crate::collect::command("node");
        cmd.args(["-e", script]);
        let started = std::time::Instant::now();
        let (result, snapshot) = super::super::transport_diagnostics::capture(run(cmd, &vec![b'x'; 20000], Duration::from_secs(5))).await;
        if result.code.is_none() {
            eprintln!("log_runner_stage={:?} completed={} elapsed_ms={}", snapshot.stage, snapshot.completed, started.elapsed().as_millis());
        }
        assert_eq!(result.code, Some(0));
        assert_eq!(serde_json::from_str::<Value>(&result.out).unwrap(), json!({"bytes":20000,"text":"日本語"}));
        for script in [
            "process.stdin.resume();process.stdin.on('end',()=>{process.stdout.write('{}');process.exitCode=7;});",
            "process.stdin.destroy();process.stdout.write('{}');setInterval(()=>{},1000);",
            "process.stdin.resume();process.stdout.write('{}');setInterval(()=>{},1000);",
            "process.stdin.resume();process.stdin.on('end',()=>process.stdout.write(Buffer.alloc(4194305)));",
            "process.stdin.resume();process.stdin.on('end',()=>process.stderr.write(Buffer.alloc(65537)));",
            "process.stdin.resume();process.stdin.on('end',()=>process.stdout.write(Buffer.from([255])));",
        ] {
            let mut cmd = crate::collect::command("node");
            cmd.args(["-e", script]);
            let result = run(cmd, &vec![b'x'; 20000], Duration::from_millis(500)).await;
            assert!(result.code.is_none() && result.out.is_empty());
            assert_eq!(result.err, FAILURE);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn windows_log_runner_rejects_eof_held_after_root_exit() {
        let mut cmd = crate::collect::command("/bin/sh");
        cmd.args(["-c", "(sleep 0.3) >&2 & printf '{}'"]);
        let result = run(cmd, b"", Duration::from_millis(50)).await;
        assert!(result.code.is_none() && result.out.is_empty());
        // The fixture owns the short-lived background child; allow it to exit.
        tokio::time::sleep(Duration::from_millis(350)).await;
    }

    #[tokio::test]
    async fn windows_log_reader_does_not_treat_read_error_as_eof() {
        struct Broken;
        impl tokio::io::AsyncRead for Broken {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::other("synthetic")))
            }
        }
        assert!(read_complete(Broken, 100).await.is_err());
        assert!(tokio::time::timeout(Duration::from_millis(20), read_complete(tokio::io::repeat(1), 100)).await.unwrap().is_err());
        let (_writer, reader) = tokio::io::duplex(100);
        assert!(tokio::time::timeout(Duration::from_millis(20), read_complete(reader, 100)).await.is_err());
    }
}
