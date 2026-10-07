//! OpenTelemetry（OTLP/HTTP・JSON）の受け口。127.0.0.1 だけで待ち、Claude Code・Codex が送るログとメトリクスを受け、
//! **本文・引数・出力の断片を落としてから** ローカルの JSONL（上限と回転つき）に書く。後で調査がこのファイルを読む。
//!
//! 落とし方は「残すものを決める」（許可リスト）。知らない属性が増えても、文字列なら残らない:
//!
//! - 文字列の属性: [`KEEP_STRING`] にある名前で、短く（128 文字まで）制御文字を含まないものだけ残す
//!   （`prompt`・`tool_parameters`・Codex の `arguments`・`output`・`error`・`user.email` などは残らない）
//! - 数・真偽の属性: 名前が識別子の形なら残す（本文を運べない）
//! - 配列・入れ子・バイト列の属性: 残さない
//! - ログの本文（body）: イベント名の形（英数と `._:-`、80 文字まで）のときだけ残す
//! - トレース ID・スパン ID・exemplar（メトリクスに付く標本）: 残さない
//!
//! 受けるのは `POST /v1/logs`・`POST /v1/metrics` の `application/json`（`http/json`）だけ。protobuf と圧縮は受けない
//! （依存を増やさないため。設定の手順は docs/observability.md）。ブラウザからの書き込み（DNS rebinding など）を
//! 防ぐため、Host が 127.0.0.1・localhost・[::1] 以外の要求は断る。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;

pub const OTLP_PORT: u16 = 4318;
pub const FILE_NAME: &str = "otel.jsonl";
/// 1 ファイルの上限と、残す世代の数（otel.jsonl.1 〜 .3）
pub const MAX_BYTES: u64 = 16 << 20;
pub const KEEP: usize = 3;
const MAX_HEADER: usize = 16 << 10;
const MAX_BODY: usize = 8 << 20;
/// 1 回の要求から書く行の上限
const MAX_LINES: usize = 10_000;
const MAX_STRING: usize = 128;

/// 文字列のまま残してよい属性の名前（Claude Code・Codex のイベントとメトリクスの、本文でない属性）
pub const KEEP_STRING: &[&str] = &[
    // リソース
    "service.name",
    "service.version",
    "os.type",
    "os.version",
    "host.arch",
    "telemetry.sdk.name",
    "telemetry.sdk.language",
    "telemetry.sdk.version",
    "deployment.environment",
    // イベントの共通
    "event.name",
    "event.timestamp",
    "event.kind",
    "session.id",
    "conversation.id",
    "prompt.id",
    "app.version",
    "app.entrypoint",
    "terminal.type",
    "auth_mode",
    "query_source",
    "effort",
    "speed",
    // モデル・設定
    "model",
    "slug",
    "provider_name",
    "reasoning_effort",
    "reasoning_summary",
    "approval_policy",
    "sandbox_policy",
    // ツール（名前と結果だけ。引数・出力は残さない）
    "tool_name",
    "tool",
    "tool_use_id",
    "tool_source",
    "call_id",
    "decision",
    "decision_type",
    "decision_source",
    "source",
    "success",
    "error_type",
    "mcp_server_scope",
    "type",
    "language",
    "status_code",
    "http.response.status_code",
];

fn ident(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

fn short_text(s: &str) -> bool {
    s.chars().count() <= MAX_STRING && !s.chars().any(char::is_control)
}

/// OTLP/JSON の AnyValue を、残してよいものだけ JSON の値に（落としたら None）
fn keep_value(key: &str, v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    if let Some(b) = o.get("boolValue").and_then(Value::as_bool) {
        return Some(json!(b));
    }
    if let Some(d) = o.get("doubleValue").and_then(Value::as_f64) {
        return Some(json!(d));
    }
    if let Some(i) = o.get("intValue") {
        // proto3 の JSON では int64 は文字列
        return i.as_i64().or_else(|| i.as_str().and_then(|s| s.parse::<i64>().ok())).map(|n| json!(n));
    }
    if let Some(s) = o.get("stringValue").and_then(Value::as_str) {
        return (KEEP_STRING.contains(&key) && short_text(s)).then(|| json!(s));
    }
    None
}

/// 属性の並び（`[{key, value}]`）から残すものだけ。落とした数も返す
fn keep_attrs(list: Option<&Value>) -> (Map<String, Value>, usize) {
    let mut out = Map::new();
    let mut dropped = 0;
    for kv in list.and_then(Value::as_array).into_iter().flatten() {
        let key = kv.get("key").and_then(Value::as_str).unwrap_or("");
        match kv.get("value").filter(|_| ident(key, 64)).and_then(|v| keep_value(key, v)) {
            Some(v) => {
                out.insert(key.to_string(), v);
            }
            None => dropped += 1,
        }
    }
    (out, dropped)
}

fn str_field(v: &Value, k: &str) -> Value {
    v.get(k).and_then(Value::as_str).filter(|s| short_text(s)).map_or(Value::Null, |s| json!(s))
}

fn ident_field(v: &Value, k: &str, max: usize) -> Value {
    v.get(k).and_then(Value::as_str).filter(|s| ident(s, max)).map_or(Value::Null, |s| json!(s))
}

/// 数（Unix 時刻のナノ秒など）。JSON では文字列のことがあるので文字列の数字にそろえる
fn nanos(v: &Value, k: &str) -> Value {
    match v.get(k) {
        Some(Value::String(s)) if !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()) => json!(s),
        Some(Value::Number(n)) => json!(n.to_string()),
        _ => Value::Null,
    }
}

struct Resource {
    service: Value,
    attrs: Map<String, Value>,
    dropped: usize,
}

fn resource(r: &Value) -> Resource {
    let (attrs, dropped) = keep_attrs(r.get("resource").and_then(|x| x.get("attributes")));
    Resource { service: attrs.get("service.name").cloned().unwrap_or(Value::Null), attrs, dropped }
}

/// `ExportLogsServiceRequest`（JSON）→ 1 レコード 1 行
pub fn logs_to_lines(req: &Value, received_ms: i64) -> Vec<Value> {
    let mut lines = Vec::new();
    for rl in req.get("resourceLogs").and_then(Value::as_array).into_iter().flatten() {
        let res = resource(rl);
        for sl in rl.get("scopeLogs").and_then(Value::as_array).into_iter().flatten() {
            let scope = sl.get("scope").map_or(Value::Null, |s| ident_field(s, "name", 128));
            for rec in sl.get("logRecords").and_then(Value::as_array).into_iter().flatten() {
                if lines.len() >= MAX_LINES {
                    return lines;
                }
                let (attrs, mut dropped) = keep_attrs(rec.get("attributes"));
                // 本文はイベント名の形のときだけ残す
                let body = rec.get("body").and_then(|b| b.get("stringValue")).and_then(Value::as_str).filter(|s| ident(s, 80));
                if rec.get("body").is_some_and(|b| !b.is_null()) && body.is_none() {
                    dropped += 1;
                }
                let event = attrs.get("event.name").cloned().or_else(|| body.map(|b| json!(b))).unwrap_or(Value::Null);
                lines.push(json!({
                    "kind": "log",
                    "recv": received_ms,
                    "time": nanos(rec, "timeUnixNano"),
                    "observed": nanos(rec, "observedTimeUnixNano"),
                    "severity": ident_field(rec, "severityText", 16),
                    "event": event,
                    "body": body,
                    "service": res.service,
                    "scope": scope,
                    "attrs": attrs,
                    "resource": res.attrs,
                    "dropped": dropped + res.dropped,
                }));
            }
        }
    }
    lines
}

/// `ExportMetricsServiceRequest`（JSON）→ 1 データ点 1 行
pub fn metrics_to_lines(req: &Value, received_ms: i64) -> Vec<Value> {
    let mut lines = Vec::new();
    for rm in req.get("resourceMetrics").and_then(Value::as_array).into_iter().flatten() {
        let res = resource(rm);
        for sm in rm.get("scopeMetrics").and_then(Value::as_array).into_iter().flatten() {
            for m in sm.get("metrics").and_then(Value::as_array).into_iter().flatten() {
                let name = ident_field(m, "name", 128);
                if name.is_null() {
                    continue;
                }
                let unit = str_field(m, "unit");
                for kind in ["sum", "gauge", "histogram", "exponentialHistogram", "summary"] {
                    let Some(data) = m.get(kind) else { continue };
                    for dp in data.get("dataPoints").and_then(Value::as_array).into_iter().flatten() {
                        if lines.len() >= MAX_LINES {
                            return lines;
                        }
                        let (attrs, dropped) = keep_attrs(dp.get("attributes"));
                        let mut line = json!({
                            "kind": "metric",
                            "recv": received_ms,
                            "time": nanos(dp, "timeUnixNano"),
                            "start": nanos(dp, "startTimeUnixNano"),
                            "service": res.service,
                            "name": name,
                            "unit": unit,
                            "type": kind,
                            "attrs": attrs,
                            "dropped": dropped + res.dropped,
                        });
                        let num = |k: &str| match dp.get(k) {
                            Some(Value::Number(n)) => n.as_f64().map(|f| json!(f)),
                            Some(Value::String(s)) => s.parse::<f64>().ok().map(|f| json!(f)),
                            _ => None,
                        };
                        if kind == "sum" || kind == "gauge" {
                            line["value"] = num("asDouble").or_else(|| num("asInt")).unwrap_or(Value::Null);
                        } else {
                            for k in ["count", "sum", "min", "max"] {
                                if let Some(v) = num(k) {
                                    line[k] = v;
                                }
                            }
                        }
                        lines.push(line);
                    }
                }
            }
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// ファイル（JSONL・上限と回転）
// ---------------------------------------------------------------------------

pub struct Sink {
    dir: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    size: u64,
}

impl Sink {
    pub fn open(dir: &Path, max_bytes: u64, keep: usize) -> std::io::Result<Sink> {
        tune_link::keys::ensure_private_dir(dir).map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut s = Sink { dir: dir.to_path_buf(), max_bytes: max_bytes.max(1024), keep: keep.max(1), file: None, size: 0 };
        s.reopen()?;
        Ok(s)
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    fn reopen(&mut self) -> std::io::Result<()> {
        let mut o = OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let f = o.open(self.path())?;
        self.size = f.metadata()?.len();
        self.file = Some(f);
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        let generation = |i: usize| self.dir.join(format!("{FILE_NAME}.{i}"));
        let _ = fs::remove_file(generation(self.keep));
        for i in (1..self.keep).rev() {
            let _ = fs::rename(generation(i), generation(i + 1));
        }
        fs::rename(self.path(), generation(1))?;
        self.reopen()
    }

    pub fn write_lines(&mut self, lines: &[Value]) -> std::io::Result<()> {
        for l in lines {
            let mut b = serde_json::to_vec(l).map_err(std::io::Error::other)?;
            b.push(b'\n');
            if self.size > 0 && self.size + b.len() as u64 > self.max_bytes {
                self.rotate()?;
            }
            let f = self.file.as_mut().ok_or_else(|| std::io::Error::other("ファイルが開いていない"))?;
            f.write_all(&b)?;
            self.size += b.len() as u64;
        }
        if let Some(f) = self.file.as_mut() {
            f.flush()?;
        }
        Ok(())
    }

    /// 今のファイルの大きさと、回転した世代の数（status 用）
    pub fn stat(dir: &Path) -> Value {
        let size = fs::metadata(dir.join(FILE_NAME)).map(|m| m.len()).ok();
        let rotated = (1..=KEEP).filter(|i| dir.join(format!("{FILE_NAME}.{i}")).exists()).count();
        json!({ "file": dir.join(FILE_NAME), "bytes": size, "rotated": rotated })
    }
}

// ---------------------------------------------------------------------------
// HTTP（OTLP/HTTP の最小限。1 要求ごとに閉じる）
// ---------------------------------------------------------------------------

struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

type HttpErr = (u16, &'static str);

struct Conn {
    s: TcpStream,
    buf: Vec<u8>,
}

impl Conn {
    async fn fill(&mut self, limit: usize) -> Result<(), HttpErr> {
        if self.buf.len() >= limit {
            return Err((413, "大きすぎる"));
        }
        let mut chunk = [0u8; 8192];
        let n = self.s.read(&mut chunk).await.map_err(|_| (400, "読めない"))?;
        if n == 0 {
            return Err((400, "途中で切れた"));
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    async fn take(&mut self, n: usize, limit: usize) -> Result<Vec<u8>, HttpErr> {
        while self.buf.len() < n {
            self.fill(limit).await?;
        }
        Ok(self.buf.drain(..n).collect())
    }

    async fn line(&mut self, limit: usize) -> Result<String, HttpErr> {
        loop {
            if let Some(i) = self.buf.windows(2).position(|w| w == b"\r\n") {
                let l: Vec<u8> = self.buf.drain(..i + 2).collect();
                return String::from_utf8(l[..i].to_vec()).map_err(|_| (400, "行が読めない"));
            }
            self.fill(limit).await?;
        }
    }
}

async fn read_request(c: &mut Conn) -> Result<Req, HttpErr> {
    let (method, path, headers, used) = loop {
        let mut hs = [httparse::EMPTY_HEADER; 64];
        let mut r = httparse::Request::new(&mut hs);
        match r.parse(&c.buf).map_err(|_| (400, "要求の形が違う"))? {
            httparse::Status::Complete(n) => {
                let headers = r.headers.iter().map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).into_owned())).collect::<Vec<_>>();
                break (r.method.unwrap_or("").to_string(), r.path.unwrap_or("").to_string(), headers, n);
            }
            httparse::Status::Partial => c.fill(MAX_HEADER).await?,
        }
    };
    c.buf.drain(..used);
    let mut req = Req { method, path, headers, body: Vec::new() };
    if req.header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        loop {
            let size_line = c.line(MAX_BODY + MAX_HEADER).await?;
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| (400, "chunk の長さが読めない"))?;
            if req.body.len() + size > MAX_BODY {
                return Err((413, "大きすぎる"));
            }
            if size == 0 {
                // 後ろのヘッダー（trailer）を空行まで読み捨てる
                while !c.line(MAX_BODY + MAX_HEADER).await?.is_empty() {}
                break;
            }
            let data = c.take(size + 2, MAX_BODY + MAX_HEADER).await?;
            req.body.extend_from_slice(&data[..size]);
        }
    } else if let Some(len) = req.header("content-length") {
        let len: usize = len.trim().parse().map_err(|_| (400, "Content-Length が読めない"))?;
        if len > MAX_BODY {
            return Err((413, "大きすぎる"));
        }
        req.body = c.take(len, MAX_BODY + MAX_HEADER).await?;
    }
    Ok(req)
}

fn host_is_loopback(host: Option<&str>) -> bool {
    let Some(h) = host.map(str::trim) else { return false };
    let name = if let Some(rest) = h.strip_prefix('[') { rest.split(']').next().unwrap_or("") } else { h.rsplit_once(':').map_or(h, |(a, _)| a) };
    matches!(name.to_ascii_lowercase().as_str(), "127.0.0.1" | "localhost" | "::1")
}

/// 1 つの要求を処理して、(状態コード, 書いた行の数)
fn route(req: &Req, sink: &Mutex<Sink>) -> Result<usize, HttpErr> {
    if !host_is_loopback(req.header("host")) {
        return Err((403, "Host が 127.0.0.1・localhost ではない"));
    }
    if req.method != "POST" {
        return Err((405, "POST だけ"));
    }
    let path = req.path.split('?').next().unwrap_or("");
    let to_lines: fn(&Value, i64) -> Vec<Value> = match path {
        "/v1/logs" => logs_to_lines,
        "/v1/metrics" => metrics_to_lines,
        _ => return Err((404, "/v1/logs と /v1/metrics だけ")),
    };
    if req.header("content-encoding").is_some_and(|e| !e.trim().eq_ignore_ascii_case("identity")) {
        return Err((415, "圧縮は受けない（OTEL_EXPORTER_OTLP_COMPRESSION を外してください）"));
    }
    if !req.header("content-type").is_some_and(|t| t.trim().to_ascii_lowercase().starts_with("application/json")) {
        return Err((415, "application/json（http/json）だけ受ける"));
    }
    let v: Value = serde_json::from_slice(&req.body).map_err(|_| (400, "JSON が読めない"))?;
    let lines = to_lines(&v, tune_link::now_ms());
    let mut s = sink.lock().map_err(|_| (500, "書けない"))?;
    s.write_lines(&lines).map_err(|_| (500, "書けない"))?;
    Ok(lines.len())
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        _ => "Internal Server Error",
    }
}

async fn handle(s: TcpStream, sink: Arc<Mutex<Sink>>) {
    let mut c = Conn { s, buf: Vec::new() };
    let result = match timeout(Duration::from_secs(10), read_request(&mut c)).await {
        Ok(Ok(req)) => route(&req, &sink),
        Ok(Err(e)) => Err(e),
        Err(_) => Err((400, "時間切れ")),
    };
    let (code, body) = match result {
        Ok(_) => (200, "{}".to_string()),
        Err((code, msg)) => (code, json!({ "message": msg }).to_string()),
    };
    let head = format!("HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reason(code), body.len());
    let _ = c.s.write_all(head.as_bytes()).await;
    let _ = c.s.write_all(body.as_bytes()).await;
    let _ = c.s.shutdown().await;
}

/// 受け口を動かし続ける。`listener` は 127.0.0.1 で開いたもの（呼び出し側で確かめる）
pub async fn serve(listener: TcpListener, sink: Arc<Mutex<Sink>>) {
    let limit = Arc::new(Semaphore::new(8));
    while let Ok((s, _)) = listener.accept().await {
        let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
        let sink = sink.clone();
        tokio::spawn(async move {
            handle(s, sink).await;
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        json!({ "stringValue": v })
    }

    fn kv(k: &str, v: Value) -> Value {
        json!({ "key": k, "value": v })
    }

    const SECRET_ARGS: &str = "rm -rf ~/very-secret-project && cat ~/.ssh/id_ed25519";
    const SECRET_OUT: &str = "-----BEGIN OPENSSH PRIVATE KEY----- output fragment";
    const SECRET_PROMPT: &str = "please refactor the payroll module for ACME";

    fn logs_fixture() -> Value {
        json!({ "resourceLogs": [{
            "resource": { "attributes": [kv("service.name", s("codex_cli_rs")), kv("user.email", s("someone@example.com")), kv("host.name", s("box"))] },
            "scopeLogs": [{
                "scope": { "name": "codex_otel" },
                "logRecords": [
                    { "timeUnixNano": "1700000000000000000", "severityText": "INFO", "body": s("codex.tool_result"),
                      "traceId": "0102", "spanId": "03",
                      "attributes": [kv("event.name", s("codex.tool_result")), kv("tool_name", s("shell")), kv("call_id", s("call_1")),
                                     kv("arguments", s(SECRET_ARGS)), kv("output", s(SECRET_OUT)), kv("duration_ms", json!({ "intValue": "42" })),
                                     kv("success", s("true"))] },
                    { "timeUnixNano": "1700000000000000001", "body": s(SECRET_PROMPT),
                      "attributes": [kv("event.name", s("claude_code.user_prompt")), kv("prompt", s(SECRET_PROMPT)), kv("prompt_length", json!({ "intValue": 43 })),
                                     kv("session.id", s("abc-123"))] },
                    { "attributes": [kv("event.name", s("claude_code.tool_result")), kv("tool_name", s("Bash")), kv("success", s("false")),
                                     kv("tool_parameters", s(&format!("{{\"bash_command\":\"{SECRET_ARGS}\"}}"))), kv("error", s(SECRET_OUT)),
                                     kv("nested", json!({ "kvlistValue": { "values": [kv("x", s(SECRET_ARGS))] } })),
                                     kv("list", json!({ "arrayValue": { "values": [s(SECRET_ARGS)] } })),
                                     kv("model", s(&"m".repeat(500))), kv("cost_usd", json!({ "doubleValue": 0.25 })), kv("bad key!", json!({ "intValue": 1 })),
                                     kv("tool_input", s(SECRET_ARGS)), kv("prompt_text", s(SECRET_PROMPT)),
                                     kv("vcs.repository.url.full", s("https://example.com/secret-repo")),
                                     kv("workspace.host_paths", json!({ "arrayValue": { "values": [s("/work/secret-repo")] } })),
                                     kv("error_type", s("permission_denied"))] }
                ]
            }]
        }]})
    }

    #[test]
    fn logs_drop_bodies_arguments_and_outputs() {
        let lines = logs_to_lines(&logs_fixture(), 5);
        assert_eq!(lines.len(), 3);
        let text = serde_json::to_string(&lines).unwrap();
        for secret in [SECRET_ARGS, SECRET_OUT, SECRET_PROMPT, "someone@example.com", "\"box\"", "0102", "bash_command", "secret-repo"] {
            assert!(!text.contains(secret), "{secret} が残っている: {text}");
        }
        // 残すもの: イベント名・ツール名・成否・所要時間・長さ・セッション・モデル以外の数
        assert_eq!(lines[0]["event"], "codex.tool_result");
        assert_eq!(lines[0]["attrs"]["tool_name"], "shell");
        assert_eq!(lines[0]["attrs"]["duration_ms"], 42);
        assert_eq!(lines[0]["attrs"]["success"], "true");
        assert_eq!(lines[0]["service"], "codex_cli_rs");
        assert_eq!(lines[0]["scope"], "codex_otel");
        assert_eq!(lines[0]["time"], "1700000000000000000");
        assert!(lines[0]["attrs"].get("arguments").is_none() && lines[0]["attrs"].get("output").is_none());
        assert_eq!(lines[0]["dropped"], 2 + 2, "引数・出力と、リソースのメールとホスト名");
        assert_eq!(lines[1]["body"], Value::Null, "本文の文は残さない");
        assert_eq!(lines[1]["attrs"]["prompt_length"], 43);
        assert_eq!(lines[1]["attrs"]["session.id"], "abc-123");
        assert!(lines[1]["attrs"].get("prompt").is_none());
        assert_eq!(lines[2]["attrs"]["cost_usd"], 0.25);
        assert!(lines[2]["attrs"].get("model").is_none(), "長すぎる文字列は残さない");
        assert!(lines[2]["attrs"].get("bad key!").is_none());
        assert_eq!(lines[2]["attrs"]["error_type"], "permission_denied");
        assert_eq!(lines[2]["dropped"], 10 + 2);
    }

    #[test]
    fn metrics_keep_numbers_and_safe_labels_only() {
        let req = json!({ "resourceMetrics": [{
            "resource": { "attributes": [kv("service.name", s("claude-code"))] },
            "scopeMetrics": [{ "metrics": [
                { "name": "claude_code.token.usage", "unit": "tokens", "sum": { "dataPoints": [
                    { "asInt": "1200", "timeUnixNano": "1", "attributes": [kv("type", s("input")), kv("model", s("some-model")), kv("user.email", s("someone@example.com"))],
                      "exemplars": [{ "filteredAttributes": [kv("prompt", s(SECRET_PROMPT))] }] }] } },
                { "name": "claude_code.cost.usage", "gauge": { "dataPoints": [{ "asDouble": 0.5 }] } },
                { "name": "x.duration", "histogram": { "dataPoints": [{ "count": "3", "sum": 9.0, "bucketCounts": ["1", "2"] }] } },
                { "name": "bad name with spaces", "sum": { "dataPoints": [{ "asInt": 1 }] } }
            ] }]
        }]});
        let lines = metrics_to_lines(&req, 7);
        let text = serde_json::to_string(&lines).unwrap();
        assert!(!text.contains(SECRET_PROMPT) && !text.contains("someone@example.com"));
        assert_eq!(lines.len(), 3);
        assert_eq!((lines[0]["value"].as_f64(), lines[0]["attrs"]["type"].as_str()), (Some(1200.0), Some("input")));
        assert_eq!(lines[0]["attrs"]["model"], "some-model");
        assert_eq!(lines[1]["value"], 0.5);
        assert_eq!((lines[2]["count"].as_f64(), lines[2]["sum"].as_f64()), (Some(3.0), Some(9.0)));
        assert!(lines[2].get("bucketCounts").is_none());
    }

    fn tmpdir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tune-agent-otlp-{tag}-{}-{}", std::process::id(), tune_link::now_ms()))
    }

    #[test]
    fn sink_rotates_and_keeps_limited_generations() {
        let dir = tmpdir("rotate");
        let mut sink = Sink::open(&dir, 1024, 2).unwrap();
        let line = json!({ "pad": "x".repeat(300) });
        for _ in 0..20 {
            sink.write_lines(std::slice::from_ref(&line)).unwrap();
        }
        assert!(fs::metadata(sink.path()).unwrap().len() <= 1024);
        assert!(dir.join(format!("{FILE_NAME}.1")).exists() && dir.join(format!("{FILE_NAME}.2")).exists());
        assert!(!dir.join(format!("{FILE_NAME}.3")).exists(), "残すのは 2 世代まで");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(sink.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    async fn http(addr: &str, raw: &str) -> (u16, String) {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(raw.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        let code = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        (code, out)
    }

    #[tokio::test]
    async fn receiver_writes_only_sanitized_lines() {
        let dir = tmpdir("http");
        let sink = Arc::new(Mutex::new(Sink::open(&dir, MAX_BYTES, KEEP).unwrap()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(serve(listener, sink.clone()));
        let body = logs_fixture().to_string();
        let post = |ct: &str, host: &str, extra: &str, body: &str| {
            format!("POST /v1/logs HTTP/1.1\r\nHost: {host}\r\nContent-Type: {ct}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len())
        };
        assert_eq!(http(&addr, &post("application/json", &addr, "", &body)).await.0, 200);
        // chunked でも受ける
        let half = body.len() / 2;
        let chunked = format!(
            "POST /v1/logs HTTP/1.1\r\nHost: localhost:4318\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            half,
            &body[..half],
            body.len() - half,
            &body[half..]
        );
        assert_eq!(http(&addr, &chunked).await.0, 200);
        // 断るもの
        assert_eq!(http(&addr, &post("application/x-protobuf", &addr, "", "x")).await.0, 415);
        assert_eq!(http(&addr, &post("application/json", &addr, "Content-Encoding: gzip\r\n", "{}")).await.0, 415);
        assert_eq!(http(&addr, &post("application/json", "evil.example:4318", "", "{}")).await.0, 403);
        assert_eq!(http(&addr, &format!("GET /v1/logs HTTP/1.1\r\nHost: {addr}\r\n\r\n")).await.0, 405);
        assert_eq!(
            http(&addr, &format!("POST /v1/traces HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}")).await.0,
            404
        );
        assert_eq!(
            http(&addr, &format!("POST /v1/logs HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1))
                .await
                .0,
            413
        );
        assert_eq!(http(&addr, &post("application/json", &addr, "", "{not json")).await.0, 400);
        let text = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert_eq!(text.lines().count(), 6, "受けた 2 回 × 3 レコードだけ");
        for secret in [SECRET_ARGS, SECRET_OUT, SECRET_PROMPT, "someone@example.com"] {
            assert!(!text.contains(secret));
        }
        assert!(text.contains("\"tool_name\":\"shell\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loopback_hosts() {
        for h in ["127.0.0.1", "127.0.0.1:4318", "localhost:4318", "LOCALHOST", "[::1]:4318", "[::1]"] {
            assert!(host_is_loopback(Some(h)), "{h}");
        }
        for h in ["evil.example", "127.0.0.2:4318", "localhost.evil.example", "10.0.0.1:4318", ""] {
            assert!(!host_is_loopback(Some(h)), "{h}");
        }
        assert!(!host_is_loopback(None));
    }
}
