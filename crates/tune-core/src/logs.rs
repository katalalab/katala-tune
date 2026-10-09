//! 各機体のログを前回の続きから取り込む。読み取り専用。仕様は lib/logs.js。
//! 取り込みは (node, source, uid) で一意なので、同じ範囲を読み直しても重複しない（失敗したら次回やり直せばよい）。

use std::borrow::Cow;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::collect::{self, MAC_LOGS, RunResult, WIN_LOGONS, WIN_LOGS, last_json_line, ssh_args};
use crate::db::{LogRow, Store, now_ms};
use crate::js::{self, Obj, get, present, string, truthy};
use crate::nodes::Node;

pub const MAX_MESSAGE: usize = 1000;
/// 1行が表す実際の件数の上限（probe が同種の繰り返しを count にまとめて返す。壊れた値で DB の合計が桁あふれしないように）
pub const MAX_OCCURRENCES: f64 = 1_000_000_000.0;

fn re(p: &str) -> Regex {
    Regex::new(&p.replace("{WS}", js::WS)).expect("regex")
}

// 秘密らしいものを伏せる。ログは機体の外へ出る前提で扱う。
// JS の \b・\d・\s・大文字小文字無視（i）は ASCII／JS の空白の意味に合わせて書き換えてある
static REDACT: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (re(r"(?-u:\b)(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})"), "<github-token>"),
        (re(r"(?-u:\b)sk-[A-Za-z0-9_-]{16,}"), "<api-key>"),
        (re(r"(?-u:\b)(AKIA|ASIA)[0-9A-Z]{16}(?-u:\b)"), "<aws-key>"),
        (re(r"(?-u:\b)ops_[A-Za-z0-9_-]{20,}"), "<op-token>"),
        (re(r"(?-u:\b)eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"), "<jwt>"),
        (re(r"((?i-u:Bearer|Basic))[{WS}]+[A-Za-z0-9._~+/=-]{12,}"), "${1} <redacted>"),
        (
            re(r#"(?-u:\b)((?i-u:password|passwd|pwd|token|secret|api[_-]?key|access[_-]?key))([{WS}]*[=:][{WS}]*)("[^"]*"|'[^']*'|[^{WS}]+)"#),
            "${1}${2}<redacted>",
        ),
    ]
});

/// 秘密らしい値を伏せ、1,000 文字（UTF-16 単位）で切る
pub fn redact(s: &str) -> String {
    let mut out = Cow::Borrowed(s);
    for (re, rep) in REDACT.iter() {
        // マッチしない規則では入力を借りたままにし、文字列全体のコピーを避ける。
        if let Cow::Owned(next) = re.replace_all(&out, *rep) {
            out = Cow::Owned(next);
        }
    }
    if js::len16(&out) > MAX_MESSAGE { js::slice16(&out, MAX_MESSAGE) + "…" } else { out.into_owned() }
}

static FP: LazyLock<[Regex; 6]> = LazyLock::new(|| {
    [
        re(r"\{?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\}?"),
        re(r"0x[0-9a-f]+"),
        re(r#"[a-z]:\\[^{WS}"']+|/(?:users|home|private|var|tmp|applications|library|system)/[^{WS}"']*"#),
        re(r#""[^"]*"|'[^']*'"#),
        re(r"[0-9]+(\.[0-9]+)?"),
        re(r"[{WS}]+"),
    ]
});

/// 同種のログをまとめる鍵。GUID・16進・数字・パス・引用符の中身を伏せてからハッシュする
pub fn fingerprint(source: &str, provider: Option<&Value>, event_id: Option<&Value>, message: &str) -> String {
    let lower = message.to_lowercase();
    let reps = ["<guid>", "<hex>", "<path>", "<q>", "<n>", " "];
    let mut norm = lower;
    for (re, rep) in FP.iter().zip(reps) {
        if let Cow::Owned(next) = re.replace_all(&norm, rep) {
            norm = next;
        }
    }
    let norm = js::slice16(js::trim(&norm), 400);
    let part = |v: Option<&Value>| if present(v) { string(v) } else { String::new() };
    let key = format!("{source}|{}|{}|{norm}", part(provider), part(event_id));
    let hex = sha1_smol::Sha1::from(key.as_bytes()).digest().to_string();
    hex[..16].to_string()
}

/// probe が返す count（同じ形のログを代表1行にまとめたときの実際の件数）。無い・不正なら 1
pub fn occurrences_of(count: Option<&Value>) -> i64 {
    let n = js::num(count);
    if n.is_finite() && n >= 1.0 { n.floor().min(MAX_OCCURRENCES) as i64 } else { 1 }
}

/// 取り込んだ行を正規化する（伏せ字・長さの上限・指紋・件数）
pub fn normalize(source: &str, rows: &[Value]) -> Vec<LogRow> {
    rows.iter()
        .filter(|r| truthy(Some(r)) && present(r.get("uid")) && truthy(r.get("ts")))
        .map(|r| {
            let m = r.get("message");
            let message = redact(&if truthy(m) { string(m) } else { String::new() });
            let level = r.get("level").and_then(Value::as_str).filter(|l| ["critical", "error", "warn", "info"].contains(l)).unwrap_or("info");
            let provider = r.get("provider");
            let event_id = r.get("event_id");
            LogRow {
                uid: js::slice16(&string(r.get("uid")), 200),
                ts: js::num(r.get("ts")),
                level: level.into(),
                provider: truthy(provider).then(|| js::slice16(&string(provider), 120)),
                event_id: present(event_id).then(|| js::slice16(&string(event_id), 60)),
                fingerprint: fingerprint(source, provider, event_id, &message),
                message,
                occurrences: occurrences_of(r.get("count")),
            }
        })
        .collect()
}

/// 取り込み元。mac_auth・win_security はログインの記録（provider に送り元のアドレスを入れる。netsec.rs・docs/observability.md の 6）
pub fn sources(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &["mac_diag", "mac_kernel", "mac_auth"],
        "windows" => &["win_system", "win_application", "neonmonitor", "win_security"],
        _ => &[],
    }
}

/// ログインの記録の取り込み元
pub const LOGIN_SOURCES: [&str; 2] = ["mac_auth", "win_security"];

/// 機体から取り込む元（lib/logs.js の sourcesOf）。台帳で `"network": false` の機体からは、ログインの記録（送り元のアドレス）も集めない
pub fn sources_of(node: &Node) -> Vec<&'static str> {
    let logins = crate::netsec::logins_enabled(&node.raw);
    sources(&node.os).iter().copied().filter(|s| logins || !LOGIN_SOURCES.contains(s)).collect()
}

/// `String(Number.parseInt(cursor ?? '0', 10) || 0)` のうち、安全な整数だけを渡す
fn cursor_arg(c: Option<&Value>) -> String {
    let s = match c {
        None | Some(Value::Null) => "0".to_string(),
        v => string(v),
    };
    let t = js::trim(&s);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return "0".into();
    }
    let Ok(mut x) = digits.parse::<i128>() else {
        return "0".into();
    };
    if neg {
        x = -x;
    }
    if x.unsigned_abs() > 9_007_199_254_740_991 { "0".into() } else { x.to_string() }
}

/// Windows Security の cursor は `RecordId:TimeCreatedEpochMs`。2値を個別検証してから引数へ渡す。
fn security_cursor_args(c: Option<&Value>) -> (String, String) {
    let s = match c {
        None | Some(Value::Null) => "0".to_string(),
        v => string(v),
    };
    let parts: Vec<&str> = js::trim(&s).split(':').collect();
    if parts.is_empty() || parts.len() > 2 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return ("0".into(), "0".into());
    }
    let Ok(record_id) = parts[0].parse::<u64>() else { return ("0".into(), "0".into()) };
    let Ok(at) = parts.get(1).unwrap_or(&"0").parse::<u64>() else { return ("0".into(), "0".into()) };
    if record_id > i64::MAX as u64 || at > i64::MAX as u64 {
        return ("0".into(), "0".into());
    }
    (record_id.to_string(), at.to_string())
}

fn windows_logons_params(c: Option<&Value>) -> String {
    let (record_id, at) = security_cursor_args(c);
    format!("-SecCursor {record_id} -SecTime {at}")
}

pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHABET[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// スクリプトを Base64 にして標準入力から ScriptBlock として実行する（固定ファイルを使わない）。
/// コマンド行に載るので 8191 バイトを超えたら送らない（lib/logs.js の windowsScriptTransport）
fn windows_script_transport(script: &[u8], params: &str) -> Result<(String, String), String> {
    let script = script.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(script);
    let payload = base64(script);
    let bootstrap = format!(
        "[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);$s=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String([Console]::In.ReadToEnd()));& ([ScriptBlock]::Create($s)) {params}"
    );
    let utf16: Vec<u8> = bootstrap.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let command = format!("printf %s {payload} | powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {}", base64(&utf16));
    if command.len() >= 8191 {
        return Err("Windows remote log command exceeds 8191 bytes".into());
    }
    Ok((command, bootstrap))
}

fn windows_remote_transport(cursors: &Map<String, Value>, script: &[u8]) -> Result<(String, String), String> {
    let c = |s: &str| cursor_arg(cursors.get(s));
    windows_script_transport(script, &format!("-SysCursor {} -AppCursor {} -NeonCursor {}", c("win_system"), c("win_application"), c("neonmonitor")))
}

/// ログオンの記録（win_security）は別のスクリプト（probes/win_logons.ps1）。win_logs.ps1 と合わせると 8191 バイトに収まらないため
fn windows_logons_transport(cursors: &Map<String, Value>, script: &[u8]) -> Result<(String, String), String> {
    windows_script_transport(script, &windows_logons_params(cursors.get("win_security")))
}

/// 2つの取り込みの出力を1つにする（win_logs.ps1 の結果に win_security を足す）。ログオンの方が失敗したら、その元だけをエラーにする
fn merge_logons(main: RunResult, logons: RunResult) -> RunResult {
    let Some(mut data) = last_json_line(&main.out).filter(|d| d.get("sources").is_some_and(|v| truthy(Some(v)))) else { return main };
    let sec = last_json_line(&logons.out).and_then(|d| d.get("sources").and_then(|s| s.get("win_security")).cloned()).filter(|v| truthy(Some(v)));
    let sec = sec.unwrap_or_else(|| {
        let e = if !logons.err.is_empty() {
            logons.err.clone()
        } else if !logons.out.is_empty() {
            logons.out.clone()
        } else {
            format!("exit {}", logons.code_str())
        };
        json!({ "error": js::slice16_tail(js::trim(&e), 400) })
    });
    if let Some(Value::Object(m)) = data.get_mut("sources") {
        m.insert("win_security".into(), sec);
    }
    RunResult { out: data.to_string(), ..main }
}

async fn ssh_transport(node: &Node, transport: Result<(String, String), String>, t: Duration) -> RunResult {
    match transport {
        Ok((remote, _)) => collect::run("ssh", &ssh_args(&node.alias, &remote), None, t).await,
        Err(err) => RunResult { code: None, out: String::new(), err },
    }
}

async fn fetch_logs(node: &Node, cursors: &Map<String, Value>) -> RunResult {
    let c = |s: &str| cursor_arg(cursors.get(s));
    let logins = crate::netsec::logins_enabled(&node.raw);
    if node.is_mac() {
        let mut args = vec![c("mac_diag"), c("mac_kernel"), c("mac_auth")];
        if !logins {
            args.push("nonet".into());
        }
        let t = Duration::from_secs(60);
        return if node.local {
            let mut a = vec!["python3".to_string(), "-".to_string()];
            a.extend(args.iter().cloned());
            collect::run("/usr/bin/env", &a, Some(MAC_LOGS.as_bytes()), t).await
        } else {
            let a = args.join(" ");
            let remote = format!("command -v python3 >/dev/null && exec python3 - {a} || exec /usr/bin/python3 - {a}");
            collect::run("ssh", &ssh_args(&node.alias, &remote), Some(MAC_LOGS.as_bytes()), t).await
        };
    }
    let t = Duration::from_secs(90);
    if node.local {
        let params = format!("-SysCursor {} -AppCursor {} -NeonCursor {}", c("win_system"), c("win_application"), c("neonmonitor"));
        if !logins {
            return collect::local::powershell_file(WIN_LOGS, &params, t).await;
        }
        let sec_params = windows_logons_params(cursors.get("win_security"));
        let (main, sec) = tokio::join!(collect::local::powershell_file(WIN_LOGS, &params, t), collect::local::powershell_file(WIN_LOGONS, &sec_params, t));
        return merge_logons(main, sec);
    }
    if !logins {
        return ssh_transport(node, windows_remote_transport(cursors, WIN_LOGS), t).await;
    }
    let (main, sec) = tokio::join!(
        ssh_transport(node, windows_remote_transport(cursors, WIN_LOGS), t),
        ssh_transport(node, windows_logons_transport(cursors, WIN_LOGONS), t)
    );
    merge_logons(main, sec)
}

/// 取り込み元の note のうち、取れても 0 件になる理由（lib/logs.js の SOURCE_NOTES）。「静か」ではなく「見るものが無い」ことを区別する
pub const SOURCE_NOTES: [(&str, &str); 2] = [("not-installed", "対象外（未導入）"), ("no-guard-log", "ログ無し")];

/// 取り込み元の note が SOURCE_NOTES にあるときだけ保存する値
pub fn source_note(src: Option<&Value>) -> Option<&'static str> {
    let n = get(src, "note")?.as_str()?;
    SOURCE_NOTES.iter().find(|(k, _)| *k == n).map(|(k, _)| *k)
}

/// 1台分を取り込む。戻り値は `{ node_id, sources: { <source>: { inserted, fetched, dropped, note? } | { error } }, meta, error? }`
/// db はロックの道具（取り込み中もほかの読み出しを止めない）
pub async fn sync_node(db: &(impl DbAccess + ?Sized), node: &Node) -> Value {
    let srcs = sources_of(node);
    let mut cursors = Map::new();
    for s in &srcs {
        let c = db.with(|d| d.cursor(&node.id, s)).ok().flatten();
        cursors.insert((*s).into(), c.and_then(|c| c.get("cursor").cloned()).unwrap_or(Value::Null));
    }
    let res = fetch_logs(node, &cursors).await;
    let data = last_json_line(&res.out);
    let mut sources_out = Map::new();
    let mut meta = Map::new();
    let Some(data_sources) = data.as_ref().and_then(|d| d.get("sources")).filter(|v| truthy(Some(v))) else {
        let e = if !res.err.is_empty() {
            res.err.clone()
        } else if !res.out.is_empty() {
            res.out.clone()
        } else {
            format!("exit {}", res.code_str())
        };
        let err = js::slice16_tail(js::trim(&e), 400);
        for s in &srcs {
            let _ = db.with(|d| d.cursor_error(&node.id, s, &err));
        }
        return json!({ "node_id": node.id, "sources": {}, "meta": {}, "error": err });
    };
    for s in &srcs {
        let src = data_sources.get(*s);
        let src_err = get(src, "error");
        if !truthy(src) || truthy(src_err) {
            let e = if truthy(src_err) { src_err.cloned().unwrap_or(Value::Null) } else { Value::from("no data") };
            let _ = db.with(|d| d.cursor_error(&node.id, s, &string(Some(&e))));
            sources_out.insert((*s).into(), json!({ "error": e }));
            continue;
        }
        let raw_rows = match get(src, "rows") {
            Some(Value::Array(a)) => a.clone(),
            r if truthy(r) => vec![r.cloned().unwrap_or(Value::Null)],
            _ => Vec::new(),
        };
        let rows = normalize(s, &raw_rows);
        let dropped = js::or(get(src, "dropped"), Some(&Value::from(0))).cloned().unwrap_or(Value::from(0));
        let note_error = source_error(src);
        let inserted = db.with(|d| {
            let n = d.insert_logs(&node.id, s, &rows, now_ms())?;
            if let Some(e) = &note_error {
                d.cursor_error(&node.id, s, e)?;
            } else {
                d.cursor_ok(&node.id, s, js::nullish(get(src, "cursor"), None), rows.len() as i64, &dropped, source_note(src))?;
            }
            Ok(n)
        });
        match inserted {
            Ok(n) => {
                sources_out.insert(
                    (*s).into(),
                    Obj::new()
                        .set("fetched", rows.len())
                        .set("inserted", n)
                        .set("dropped", dropped)
                        .opt("note", get(src, "note").cloned())
                        .opt("events", get(src, "events").filter(|v| truthy(Some(v))).cloned())
                        .opt("error", note_error.map(Value::from))
                        .build(),
                );
            }
            Err(e) => {
                // 書き込みの失敗は成功扱いにしない
                let _ = db.with(|d| d.cursor_error(&node.id, s, &e.to_string()));
                sources_out.insert((*s).into(), json!({ "error": e.to_string() }));
            }
        }
        if let Some(m) = get(src, "meta").filter(|v| truthy(Some(v))) {
            meta.insert((*s).into(), m.clone());
        }
    }
    json!({ "node_id": node.id, "sources": sources_out, "meta": meta })
}

fn source_error(src: Option<&Value>) -> Option<String> {
    js::is_str(get(src, "note"), "no-permission").then(|| "no-permission".into())
}

/// DB を短く借りる道具（アプリでは Mutex 越し、テストでは直接）
pub trait DbAccess: Send + Sync {
    fn with<T>(&self, f: impl FnOnce(&Store) -> rusqlite::Result<T>) -> rusqlite::Result<T>;
}

impl DbAccess for std::sync::Mutex<Store> {
    fn with<T>(&self, f: impl FnOnce(&Store) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        let g = self.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&g)
    }
}

/// 全機体を並列に取り込む。on_result は機体ごとに終わった順で呼ぶ
pub async fn sync_all<D, F>(db: std::sync::Arc<D>, nodes: &[Node], on_result: F) -> Vec<Value>
where
    D: DbAccess + 'static,
    F: Fn(&Value) + Send + Sync + 'static,
{
    let on_result = std::sync::Arc::new(on_result);
    let mut set = tokio::task::JoinSet::new();
    for (i, n) in nodes.iter().cloned().enumerate() {
        let (db, cb) = (db.clone(), on_result.clone());
        set.spawn(async move {
            let r = sync_node(db.as_ref(), &n).await;
            cb(&r);
            (i, r)
        });
    }
    let mut out: Vec<Option<Value>> = vec![None; nodes.len()];
    while let Some(j) = set.join_next().await {
        if let Ok((i, r)) = j {
            out[i] = Some(r);
        }
    }
    out.into_iter().enumerate().map(|(i, r)| r.unwrap_or_else(|| json!({ "node_id": nodes[i].id, "error": "取り込みの途中で内部エラー" }))).collect()
}

/// ログから所見を作る（直近7日）。rules::analyze の結果に足して使う
pub fn log_findings(db: &Store, node_id: &str, now: i64) -> rusqlite::Result<Vec<Value>> {
    let rows = db.log_counts(node_id, now - 7 * 86_400_000)?;
    let sum = |pred: &dyn Fn(&crate::db::LogCount) -> bool| rows.iter().filter(|r| pred(r)).map(|r| r.n).sum::<i64>();
    let prov = |r: &crate::db::LogCount, p: &str| r.provider.as_deref() == Some(p);
    let ev = |r: &crate::db::LogCount, e: &str| r.event_id.as_deref() == Some(e);
    let mut out = Vec::new();
    let mut push = |id: &str, severity: &str, category: &str, title: String, detail: &str, advice: &str, log_query: Value| {
        out.push(json!({ "id": id, "severity": severity, "category": category, "title": title, "detail": detail, "advice": advice, "log_query": log_query }));
    };
    let whea = sum(&|r| prov(r, "Microsoft-Windows-WHEA-Logger"));
    if whea > 0 {
        push(
            "log-whea",
            if whea >= 5 { "critical" } else { "warn" },
            "stability",
            format!("ハードウェアエラー（WHEA）が7日で {whea} 件"),
            "System ログの Microsoft-Windows-WHEA-Logger",
            "CPU・メモリ・PCIe の訂正／訂正不能エラー。メモリの XMP を切る、枚数を減らす、MemTest86 で切り分ける。",
            json!({ "node_id": node_id, "q": "WHEA" }),
        );
    }
    let tdr = sum(&|r| prov(r, "Display") && ev(r, "4101")) + sum(&|r| prov(r, "nvlddmkm"));
    if tdr > 0 {
        push(
            "log-gpu-reset",
            "warn",
            "stability",
            format!("GPU ドライバのリセット・エラーが7日で {tdr} 件"),
            "Display 4101 / nvlddmkm",
            "GPU ドライバのタイムアウト。ドライバの入れ直し（DDU）、オーバークロックの解除、電源容量を確認する。",
            json!({ "node_id": node_id, "q": "nvlddmkm OR Display" }),
        );
    }
    let lowmem = sum(&|r| prov(r, "Microsoft-Windows-Resource-Exhaustion-Detector"));
    if lowmem > 0 {
        push(
            "log-lowmem",
            "warn",
            "memory",
            format!("メモリ枯渇の警告が7日で {lowmem} 件"),
            "Resource-Exhaustion-Detector 2004",
            "コミットが上限に達しかけた。どのプロセスが使っていたかはメッセージに出ている。ページファイルの拡大か大口の整理。",
            json!({ "node_id": node_id, "q": "Resource" }),
        );
    }
    // volmgr の 161・162 は BSOD の後のクラッシュダンプ作成の記録（ディスクの故障ではない。停止そのものは安定性で数える）
    let dump_record = |r: &crate::db::LogCount| prov(r, "volmgr") && (ev(r, "161") || ev(r, "162"));
    let disk = sum(&|r| ["disk", "Ntfs", "stornvme", "storahci", "volmgr"].iter().any(|p| prov(r, p)) && r.level != "info" && !dump_record(r));
    if disk > 0 {
        push(
            "log-disk",
            "warn",
            "disk",
            format!("ディスク・ファイルシステムのエラーが7日で {disk} 件"),
            "disk / Ntfs / stornvme",
            "SMART を確認し、バックアップを先に取る。ケーブルや M.2 の接触も疑う。",
            json!({ "node_id": node_id, "q": "disk OR Ntfs OR stornvme" }),
        );
    }
    // 1000: Application Error、1002: Application Hang、1026: .NET Runtime の未処理例外
    let crashes = sum(&|r| {
        (r.source == "win_application" && ["1000", "1002", "1026"].iter().any(|e| ev(r, e)))
            || (r.source == "mac_diag" && ["crash", "hang", "spin"].iter().any(|e| ev(r, e)))
    });
    if crashes >= 3 {
        push(
            "log-crashes",
            if crashes >= 15 { "warn" } else { "info" },
            "stability",
            format!("アプリのクラッシュ・ハングが7日で {crashes} 件"),
            "Application Error 1000 / Hang 1002、DiagnosticReports",
            "ログ画面の「同種ログ」で、どのアプリが繰り返しているかを見る。",
            json!({ "node_id": node_id, "level": "error" }),
        );
    }
    let panics = sum(&|r| ev(r, "kernel panic"));
    if panics > 0 {
        push(
            "log-panic",
            "critical",
            "stability",
            format!("カーネルパニックが7日で {panics} 件"),
            "DiagnosticReports",
            "直前に入れた拡張・周辺機器・OS 更新を疑う。パニックログの panicString を確認する。",
            json!({ "node_id": node_id, "q": "panic" }),
        );
    }
    let jetsam = sum(&|r| ev(r, "jetsam (memory)"));
    if jetsam > 0 {
        push(
            "log-jetsam",
            "warn",
            "memory",
            format!("メモリ不足でアプリが落とされた記録が7日で {jetsam} 件"),
            "jetsam",
            "メモリの大口を減らす。",
            json!({ "node_id": node_id, "q": "jetsam" }),
        );
    }
    let neon = sum(&|r| r.source == "neonmonitor" && r.level != "info");
    if neon > 0 {
        push(
            "log-neon",
            "warn",
            "memory",
            format!("NeonMonitor の自動保護が7日で {neon} 回動いた"),
            "guard.log の警告・強制終了・失敗",
            "逼迫が繰り返している。強制終了で落ちたアプリが無いか確認し、根本の大口を減らす。",
            json!({ "node_id": node_id, "source": "neonmonitor" }),
        );
    }
    // サービスの起動失敗・異常終了（7000/7009/7023/7031/7034）
    let svc = sum(&|r| prov(r, "Service Control Manager") && ["7000", "7009", "7023", "7031", "7034"].iter().any(|e| ev(r, e)));
    if svc >= 3 {
        push(
            "log-service",
            "warn",
            "stability",
            format!("サービスの起動失敗・異常終了が7日で {svc} 件"),
            "Service Control Manager 7000/7009/7023/7031/7034",
            "どのサービスかはログに出ている。セキュリティ製品（Defender など）なら、保護が止まっている可能性があるので先に直す。",
            json!({ "node_id": node_id, "q": "\"Service Control Manager\"" }),
        );
    }
    // 同じエラーの洪水（24時間で200件以上）。クラッシュと再起動の繰り返しや、ログの出し過ぎを見つける
    static WS_RUN: LazyLock<Regex> = LazyLock::new(|| re(r"[{WS}]+"));
    for t in db.top_signatures(node_id, now - 86_400_000, 2)?.into_iter().filter(|x| x.n >= 200) {
        let who = t.provider.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| t.source.clone());
        let q = match t.provider.as_deref().filter(|p| !p.is_empty()) {
            Some(p) => format!("\"{}\"", p.replace('"', "")),
            None => String::new(),
        };
        push(
            &format!("log-flood-{}", t.fingerprint),
            "warn",
            "background",
            format!("同じエラーが24時間で {} 件繰り返している（{who}）", t.n),
            &js::slice16(&WS_RUN.replace_all(&t.sample, " "), 160),
            "起動と失敗を繰り返しているか、ログを出し過ぎている。どちらも CPU とディスクを使い続ける。出している側の設定を直すか止める。",
            json!({ "node_id": node_id, "q": q }),
        );
    }
    for f in login_findings(db, node_id, now)? {
        let s = |k: &str| js::string(f.get(k));
        push(&s("id"), &s("severity"), &s("category"), s("title"), &s("detail"), &s("advice"), f["log_query"].clone());
    }
    let dropped = db.dropped_total(node_id)?;
    if dropped >= 1000 {
        push(
            "log-dropped",
            "info",
            "background",
            format!("ログが多すぎて {dropped} 件を取り込めなかった"),
            "1回の取り込みは1か所あたり、同じ形のものをまとめた300行まで（まとめた繰り返しは件数に数えている）",
            "上の「同じエラーの繰り返し」を直すと収まる。",
            json!({ "node_id": node_id }),
        );
    }
    Ok(out)
}

/// ログインの記録（mac_auth・win_security）から（lib/logs.js の loginFindings）。失敗は24時間、外部のアドレスからの成功は7日で見る
fn login_findings(db: &Store, node_id: &str, now: i64) -> rusqlite::Result<Vec<Value>> {
    use crate::db::LogCount;
    use crate::netsec::{LOGIN_CRIT, LOGIN_WARN, is_public};
    let by_source = |rows: &[&LogCount]| -> Vec<(String, i64)> {
        let mut order: Vec<String> = Vec::new();
        let mut m: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for r in rows {
            let p = r.provider.clone().unwrap_or_default();
            if !m.contains_key(&p) {
                order.push(p.clone());
            }
            *m.entry(p).or_insert(0) += r.n;
        }
        let mut v: Vec<(String, i64)> = order.into_iter().map(|p| (p.clone(), m[&p])).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.encode_utf16().cmp(b.0.encode_utf16())));
        v
    };
    let list = |src: &[(String, i64)]| {
        let s = src.iter().take(5).map(|(a, k)| format!("{} ×{k}", if a.is_empty() { "-" } else { a })).collect::<Vec<_>>().join("、");
        format!("{s}{}", if src.len() > 5 { " ほか" } else { "" })
    };
    let mut out = Vec::new();
    let day = db.log_counts(node_id, now - 86_400_000)?;
    let fails: Vec<&LogCount> = day
        .iter()
        .filter(|r| (r.source == "mac_auth" || r.source == "win_security") && matches!(r.event_id.as_deref(), Some("4625") | Some("ssh-fail")))
        .collect();
    let n: i64 = fails.iter().map(|r| r.n).sum();
    if n > 0 {
        let src = by_source(&fails);
        let public: i64 = src.iter().filter(|(a, _)| is_public(Some(&Value::from(a.as_str())))).map(|(_, k)| k).sum();
        let sev = if n >= LOGIN_CRIT {
            Some("critical")
        } else if n >= LOGIN_WARN {
            Some("warn")
        } else if public > 0 {
            Some("info")
        } else {
            None
        };
        if let Some(sev) = sev {
            let source = if fails.iter().any(|r| r.source == "win_security") { "win_security" } else { "mac_auth" };
            out.push(json!({
                "id": "log-login-fail", "severity": sev, "category": "security",
                "title": format!("ログインの失敗が24時間で {n} 件（送り元 {} か所{}）", src.len(), if public > 0 { format!("、うち外部のアドレスから {public} 件") } else { String::new() }),
                "detail": list(&src),
                "advice": "総当たりの疑い。外から届く口（ssh・リモートデスクトップ）を閉じるか、Tailscale など内側の経路だけで受ける。パスワードでのログインを止め、鍵だけにする。この画面からは変更しない。",
                "log_query": { "node_id": node_id, "source": source },
            }));
        }
    }
    let week = db.log_counts(node_id, now - 7 * 86_400_000)?;
    let oks: Vec<&LogCount> = week
        .iter()
        .filter(|r| r.source == "win_security" && r.event_id.as_deref() == Some("4624") && is_public(r.provider.as_deref().map(Value::from).as_ref()))
        .collect();
    let k: i64 = oks.iter().map(|r| r.n).sum();
    if k > 0 {
        let src = by_source(&oks);
        out.push(json!({
            "id": "log-login-public", "severity": "warn", "category": "security",
            "title": format!("外部のアドレスからのログイン成功が7日で {k} 件"),
            "detail": list(&src),
            "advice": "自分の操作か確かめる。覚えが無ければパスワードを変え、外から届く口を閉じる。",
            "log_query": { "node_id": node_id, "source": "win_security" },
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets() {
        let fake_aws = ["AKIA", "ABCDEFGHIJKLMNOP"].concat();
        let s = redact(&format!(
            "token=abc123 Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig12345678 ghp_{} {fake_aws} sk-{} password: \"p w\"",
            "a".repeat(36),
            "x".repeat(30)
        ));
        for bad in ["abc123", "ghp_a", "AKIAABCD", "sk-xxx", "p w"] {
            assert!(!s.contains(bad), "{s}");
        }
        assert!(s.contains("token=<redacted>"));
        assert!(s.contains("<github-token>"));
        assert!(s.contains("<aws-key>"));
        assert!(js::len16(&redact(&"a".repeat(5000))) <= 1001);
    }

    #[test]
    fn shared_nodes_do_not_collect_login_metadata_without_opt_in() {
        let shared = Node::from_value(&json!({ "id": "shared", "os": "windows", "shared": true }), "host");
        assert_eq!(sources_of(&shared), vec!["win_system", "win_application", "neonmonitor"]);
        let opted_in = Node::from_value(&json!({ "id": "shared", "os": "windows", "shared": true, "network_logins": true }), "host");
        assert_eq!(sources_of(&opted_in), vec!["win_system", "win_application", "neonmonitor", "win_security"]);
    }

    #[test]
    fn same_shape_same_fingerprint() {
        let p = Some(Value::from("disk"));
        let e = Some(Value::from("7"));
        let a = fingerprint("win_system", p.as_ref(), e.as_ref(), "The device \\Device\\Harddisk1\\DR1 has a bad block at 0x1F3 in C:\\Users\\a\\x.txt");
        let b = fingerprint("win_system", p.as_ref(), e.as_ref(), "The device \\Device\\Harddisk2\\DR2 has a bad block at 0xAB in C:\\Users\\b\\y.txt");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn cursor_arg_like_parse_int() {
        assert_eq!(cursor_arg(None), "0");
        assert_eq!(cursor_arg(Some(&Value::from("1791352417838"))), "1791352417838");
        assert_eq!(cursor_arg(Some(&Value::from(" 12abc"))), "12");
        assert_eq!(cursor_arg(Some(&Value::from("abc"))), "0");
        assert_eq!(cursor_arg(Some(&Value::from("-5"))), "-5");
        assert_eq!(security_cursor_args(Some(&Value::from("301:1800000000000"))), ("301".into(), "1800000000000".into()));
        assert_eq!(security_cursor_args(Some(&Value::from("301"))), ("301".into(), "0".into()));
        assert_eq!(security_cursor_args(Some(&Value::from("9007199254740993:9223372036854775807"))), ("9007199254740993".into(), "9223372036854775807".into()));
        assert_eq!(security_cursor_args(Some(&Value::from("9223372036854775808:1"))), ("0".into(), "0".into()));
        assert_eq!(security_cursor_args(Some(&Value::from("301:2; bad"))), ("0".into(), "0".into()));
        assert_eq!(windows_logons_params(Some(&Value::from("9007199254740993:1800000000000"))), "-SecCursor 9007199254740993 -SecTime 1800000000000");
    }

    #[test]
    fn windows_remote_logs_use_base64_stdin_without_fixed_file() {
        let mut cursors = Map::new();
        cursors.insert("win_system".into(), Value::from("42; Write-Error bad"));
        cursors.insert("win_application".into(), Value::from(" 7tail"));
        cursors.insert("neonmonitor".into(), Value::from("not-a-number"));
        let (command, bootstrap) = windows_remote_transport(&cursors, b"\xef\xbb\xbfparam([long]$SysCursor)\n").unwrap();
        assert!(!command.contains("katala-tune") && !command.contains("logs.ps1") && !command.contains("-File") && !command.contains("cygpath"));
        assert!(command.starts_with("printf %s "));
        assert!(command.contains(" | powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand "));
        assert!(command.len() < 8191);
        assert!(bootstrap.contains("FromBase64String([Console]::In.ReadToEnd())"));
        assert!(bootstrap.contains("[ScriptBlock]::Create($s)"));
        assert!(bootstrap.ends_with("-SysCursor 42 -AppCursor 7 -NeonCursor 0"));
        let mut max_cursors = Map::new();
        for source in ["win_system", "win_application", "neonmonitor"] {
            max_cursors.insert(source.into(), Value::from("9007199254740991"));
        }
        assert!(windows_remote_transport(&max_cursors, WIN_LOGS).unwrap().0.len() < 8191);
        assert!(windows_remote_transport(&Map::new(), &[0; 6000]).is_err());
        // ログオンの記録は別のスクリプトで、同じ運び方・同じ上限
        max_cursors.insert("win_security".into(), Value::from("9007199254740991:1800000000000"));
        let (command, bootstrap) = windows_logons_transport(&max_cursors, WIN_LOGONS).unwrap();
        assert!(command.len() < 8191);
        assert!(bootstrap.ends_with("-SecCursor 9007199254740991 -SecTime 1800000000000"));
    }

    #[test]
    fn logons_are_merged_into_the_log_output() {
        let r = |out: &str, err: &str| RunResult { code: Some(0), out: out.into(), err: err.into() };
        let main = r("{\"probe\":\"win_logs\",\"sources\":{\"win_system\":{\"cursor\":\"1\",\"rows\":[]}}}", "");
        let m = merge_logons(main.clone(), r("noise\n{\"sources\":{\"win_security\":{\"cursor\":\"9\",\"rows\":[],\"note\":\"no-permission\"}}}", ""));
        let d = last_json_line(&m.out).unwrap();
        assert_eq!((d["sources"]["win_system"]["cursor"].clone(), d["sources"]["win_security"]["note"].clone()), (json!("1"), json!("no-permission")));
        // ログオンの方が失敗しても、ほかの元はそのまま取り込む
        let f = last_json_line(&merge_logons(main.clone(), r("", "ssh: timeout")).out).unwrap();
        assert_eq!((f["sources"]["win_system"]["cursor"].clone(), f["sources"]["win_security"]["error"].clone()), (json!("1"), json!("ssh: timeout")));
        // 本体が失敗したら本体のまま（全部の元のエラーになる）
        let bad = r("", "exit 255");
        assert_eq!(merge_logons(bad.clone(), r("{}", "")).err, "exit 255");
    }

    #[test]
    fn no_permission_note_is_a_cursor_error() {
        assert_eq!(source_error(Some(&json!({ "rows": [], "note": "no-permission" }))), Some("no-permission".into()));
        assert_eq!(source_error(Some(&json!({ "rows": [], "note": "ok" }))), None);
    }

    #[test]
    fn occurrences_default_to_one_and_are_capped() {
        let n = |v: Value| occurrences_of(Some(&v));
        assert_eq!((n(json!(5)), n(json!("7")), n(json!(2.9)), n(json!(1e15))), (5, 7, 2, 1_000_000_000));
        assert_eq!((n(json!(0)), n(json!(-5)), n(json!("x")), n(Value::Null), occurrences_of(None)), (1, 1, 1, 1, 1));
    }

    #[test]
    fn only_known_notes_are_kept() {
        let src = |n: &str| json!({ "note": n });
        assert_eq!(source_note(Some(&src("not-installed"))), Some("not-installed"));
        assert_eq!(source_note(Some(&src("no-guard-log"))), Some("no-guard-log"));
        assert_eq!(source_note(Some(&src("no-permission"))), None);
        assert_eq!(source_note(Some(&json!({}))), None);
        assert_eq!(source_note(None), None);
    }
}
