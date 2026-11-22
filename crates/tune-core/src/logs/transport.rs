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
        format!("KTLOG1\n{kind}\n{}\n{}\n{}\n{}\n{}\n{}\n", cursors[0], cursors[1], cursors[2], body.len(), crate::db::sha256_hex(&body), super::base64(&body));
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

// The non-test expansions are exactly the original expressions: no observer or
// extra future exists in production builds.
macro_rules! observe {
    ($trace:ident.$method:ident, $stage:ident, $expression:expr) => {{
        #[cfg(test)]
        {
            let result = $expression;
            $trace.$method(diagnostic::Stage::$stage, &result);
            result
        }
        #[cfg(not(test))]
        {
            $expression
        }
    }};
}

macro_rules! observe_future {
    ($trace:ident.$method:ident, $stage:ident, $expression:expr) => {{
        #[cfg(test)]
        {
            async { observe!($trace.$method, $stage, ($expression).await) }
        }
        #[cfg(not(test))]
        {
            $expression
        }
    }};
}

/// Release parser input only after stdin, both EOFs and exit 0 before deadline.
pub(super) async fn run(mut command: Command, input: &[u8], timeout: Duration) -> RunResult {
    #[cfg(test)]
    let trace = diagnostic::Trace::new();
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let Ok(mut child) = observe!(trace.io, Spawn, command.spawn()) else {
        #[cfg(test)]
        trace.report();
        return failure();
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let deadline = tokio::time::Instant::now() + timeout;
    #[cfg(test)]
    trace.begin_exchange();
    #[cfg(test)]
    let stdout = trace.reader(stdout, diagnostic::Stage::Stdout, MAX_STDOUT);
    #[cfg(test)]
    let stderr = trace.reader(stderr, diagnostic::Stage::Stderr, MAX_STDERR);
    let exchange = async {
        let writer = async {
            observe!(trace.io, StdinWrite, stdin.write_all(input).await)?;
            observe!(trace.io, StdinShutdown, stdin.shutdown().await)?;
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let (_, out, err, status) =
            tokio::try_join!(writer, read_complete(stdout, MAX_STDOUT), read_complete(stderr, MAX_STDERR), observe_future!(trace.wait, Wait, child.wait()))?;
        if !status.success() {
            #[cfg(test)]
            trace.record(diagnostic::Stage::Exit, diagnostic::Outcome::Nonzero);
            return Err(std::io::Error::other(FAILURE));
        }
        let out = observe!(trace.utf8, StdoutUtf8, String::from_utf8(out)).map_err(|_| std::io::Error::other(FAILURE))?;
        let err = observe!(trace.utf8, StderrUtf8, String::from_utf8(err)).map_err(|_| std::io::Error::other(FAILURE))?;
        Ok::<_, std::io::Error>(RunResult {
            code: Some(0),
            out: out.strip_prefix('\u{feff}').unwrap_or(&out).to_owned(),
            err: err.strip_prefix('\u{feff}').unwrap_or(&err).to_owned(),
        })
    };
    if let Ok(Ok(result)) = observe!(trace.exchange, TimeoutAt, tokio::time::timeout_at(deadline, exchange).await)
        && observe!(trace.deadline, AfterDeadline, tokio::time::Instant::now() <= deadline)
    {
        return result;
    }
    #[cfg(test)]
    trace.record(diagnostic::Stage::CleanupStart, diagnostic::Outcome::Ok);
    let _ = observe!(trace.io, Kill, child.start_kill());
    let _ = observe!(trace.reap, Reap, tokio::time::timeout(Duration::from_secs(1), child.wait()).await);
    #[cfg(test)]
    trace.report();
    failure()
}

fn failure() -> RunResult {
    RunResult { code: None, out: String::new(), err: FAILURE.into() }
}

#[cfg(test)]
mod diagnostic {
    use std::io;
    use std::pin::Pin;
    use std::process::ExitStatus;
    use std::sync::Mutex;
    use std::task::{Context, Poll};
    use std::time::Instant;
    use tokio::io::{AsyncRead, ReadBuf};

    #[derive(Clone, Copy, Debug)]
    pub(super) enum Stage {
        Spawn,
        StdinWrite,
        StdinShutdown,
        Stdout,
        Stderr,
        Wait,
        Exit,
        StdoutUtf8,
        StderrUtf8,
        TimeoutAt,
        AfterDeadline,
        CleanupStart,
        Kill,
        Reap,
    }

    const STAGES: [Stage; 14] = [
        Stage::Spawn,
        Stage::StdinWrite,
        Stage::StdinShutdown,
        Stage::Stdout,
        Stage::Stderr,
        Stage::Wait,
        Stage::Exit,
        Stage::StdoutUtf8,
        Stage::StderrUtf8,
        Stage::TimeoutAt,
        Stage::AfterDeadline,
        Stage::CleanupStart,
        Stage::Kill,
        Stage::Reap,
    ];

    // No error Display, command, environment, input or output content is retained.
    #[derive(Clone, Copy)]
    pub(super) enum Outcome {
        Ok,
        Io(io::ErrorKind, Option<i32>),
        Exit(Option<i32>, bool),
        Limit,
        Nonzero,
        Utf8,
        Timeout,
        Late,
    }

    impl std::fmt::Debug for Outcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Ok => f.write_str("Ok"),
                Self::Io(kind, raw) => f.debug_tuple("Io").field(kind).field(raw).finish(),
                Self::Exit(code, success) => f.debug_tuple("Exit").field(code).field(success).finish(),
                Self::Limit => f.write_str("Limit"),
                Self::Nonzero => f.write_str("Nonzero"),
                Self::Utf8 => f.write_str("Utf8"),
                Self::Timeout => f.write_str("Timeout"),
                Self::Late => f.write_str("Late"),
            }
        }
    }

    #[derive(Debug)]
    struct State {
        exchange_start_us: Option<u128>,
        phases: [Option<(u128, Outcome)>; 14],
        bytes: [usize; 2],
        eof_us: [Option<u128>; 2],
        first_failure: Option<(Stage, Outcome)>,
    }

    pub(super) struct Trace {
        start: Instant,
        state: Mutex<State>,
    }

    impl Trace {
        pub(super) fn new() -> Self {
            Self {
                start: Instant::now(),
                state: Mutex::new(State { exchange_start_us: None, phases: [None; 14], bytes: [0; 2], eof_us: [None; 2], first_failure: None }),
            }
        }

        pub(super) fn begin_exchange(&self) {
            self.state.lock().unwrap().exchange_start_us = Some(self.start.elapsed().as_micros());
        }

        pub(super) fn record(&self, stage: Stage, outcome: Outcome) {
            let mut state = self.state.lock().unwrap();
            state.phases[stage as usize] = Some((self.start.elapsed().as_micros(), outcome));
            if !matches!(outcome, Outcome::Ok | Outcome::Exit(_, true)) && state.first_failure.is_none() {
                state.first_failure = Some((stage, outcome));
            }
        }

        pub(super) fn io<T>(&self, stage: Stage, result: &io::Result<T>) {
            self.record(
                stage,
                match result {
                    Ok(_) => Outcome::Ok,
                    Err(error) => Outcome::Io(error.kind(), error.raw_os_error()),
                },
            );
        }

        pub(super) fn wait(&self, stage: Stage, result: &io::Result<ExitStatus>) {
            match result {
                Ok(status) => self.record(stage, Outcome::Exit(status.code(), status.success())),
                Err(_) => self.io(stage, result),
            }
        }

        pub(super) fn utf8(&self, stage: Stage, result: &Result<String, std::string::FromUtf8Error>) {
            self.record(stage, if result.is_ok() { Outcome::Ok } else { Outcome::Utf8 });
        }

        pub(super) fn exchange<T>(&self, stage: Stage, result: &Result<T, tokio::time::error::Elapsed>) {
            if result.is_err() {
                self.record(stage, Outcome::Timeout);
            }
        }

        pub(super) fn deadline(&self, stage: Stage, within: &bool) {
            if !within {
                self.record(stage, Outcome::Late);
            }
        }

        pub(super) fn reap(&self, stage: Stage, result: &Result<io::Result<ExitStatus>, tokio::time::error::Elapsed>) {
            match result {
                Ok(status) => self.wait(stage, status),
                Err(_) => self.record(stage, Outcome::Timeout),
            }
        }

        pub(super) fn reader<R>(&self, pipe: R, stage: Stage, limit: usize) -> Reader<'_, R> {
            Reader { pipe, trace: self, stage, limit }
        }

        pub(super) fn report(&self) {
            let state = self.state.lock().unwrap();
            // A fixed-size summary, once per failed run. None means unobserved,
            // including cancellation by try_join!, and is not a causal claim.
            let phases = STAGES.map(|stage| (stage, state.phases[stage as usize]));
            eprintln!(
                "log_transport_phase_v1 elapsed_us={} exchange_start_us={:?} phases={:?} stdout_stderr_bytes={:?} stdout_stderr_eof_us={:?} first_failure={:?}",
                self.start.elapsed().as_micros(),
                state.exchange_start_us,
                phases,
                state.bytes,
                state.eof_us,
                state.first_failure
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn diagnostic_categories_distinguish_limit_io_timeout_and_late_success() {
        let limit = Trace::new();
        assert!(super::read_complete(limit.reader(tokio::io::repeat(0), Stage::Stdout, 3), 3).await.is_err());
        {
            let state = limit.state.lock().unwrap();
            assert!(matches!(state.first_failure, Some((Stage::Stdout, Outcome::Limit))));
            assert_eq!(state.bytes, [4, 0]);
            assert_eq!(state.eof_us, [None, None]);
        }

        let io = Trace::new();
        io.io::<()>(Stage::StdinWrite, &Err(io::Error::from_raw_os_error(5)));
        io.record(Stage::TimeoutAt, Outcome::Timeout);
        assert!(matches!(io.state.lock().unwrap().first_failure, Some((Stage::StdinWrite, Outcome::Io(_, Some(5))))));

        let timeout = Trace::new();
        let result = tokio::time::timeout(std::time::Duration::from_millis(1), std::future::pending::<()>()).await;
        timeout.exchange(Stage::TimeoutAt, &result);
        assert!(matches!(timeout.state.lock().unwrap().first_failure, Some((Stage::TimeoutAt, Outcome::Timeout))));
        assert!(timeout.state.lock().unwrap().phases[Stage::AfterDeadline as usize].is_none());

        let late = Trace::new();
        late.exchange(Stage::TimeoutAt, &Ok::<_, tokio::time::error::Elapsed>(Ok::<_, io::Error>(())));
        late.deadline(Stage::AfterDeadline, &false);
        assert!(matches!(late.state.lock().unwrap().first_failure, Some((Stage::AfterDeadline, Outcome::Late))));
        assert!(late.state.lock().unwrap().phases[Stage::TimeoutAt as usize].is_none());
    }

    pub(super) struct Reader<'a, R> {
        pipe: R,
        trace: &'a Trace,
        stage: Stage,
        limit: usize,
    }

    impl<R: AsyncRead + Unpin> AsyncRead for Reader<'_, R> {
        fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            let before = buf.filled().len();
            let capacity = buf.remaining();
            let result = Pin::new(&mut self.pipe).poll_read(cx, buf);
            match &result {
                Poll::Ready(Ok(())) => {
                    let read = buf.filled().len() - before;
                    let index = usize::from(matches!(self.stage, Stage::Stderr));
                    let mut state = self.trace.state.lock().unwrap();
                    state.bytes[index] += read;
                    if read == 0 && capacity != 0 {
                        state.eof_us[index] = Some(self.trace.start.elapsed().as_micros());
                    }
                    let limited = state.bytes[index] > self.limit;
                    drop(state);
                    if limited {
                        self.trace.record(self.stage, Outcome::Limit);
                    } else if read == 0 && capacity != 0 {
                        self.trace.record(self.stage, Outcome::Ok);
                    }
                }
                Poll::Ready(Err(error)) => self.trace.record(self.stage, Outcome::Io(error.kind(), error.raw_os_error())),
                Poll::Pending => {}
            }
            result
        }
    }
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
        let result = run(cmd, &vec![b'x'; 20000], Duration::from_secs(5)).await;
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
