//! AI エージェント（Claude Code・Codex）のセッションの要約（tune-core だけの表。Electron 版は触らない）。
//! - ai_sessions: ファイル（= セッション）ごとの要約。会話の本文・ツールの入出力は持たない（調査が取り出さない）。cwd はホームを ~ にしたもの
//! - ai_usage_hourly: セッション×時間（epoch の時）ごとの使用量。日ごとの集計に使う（長いセッションも日をまたいで正しく割り振る）
//! - ai_cursors: 機体ごとの続きの位置（ファイルごとのバイト位置）と、前回の取り込みの様子（続きあり・python が無い・エラー）
//!
//! 調査（probes/ai_sessions.py）の出力をそのまま受け取り、mode が replace なら置き換え、add なら足す。
//! tokens_mode が max のトークン（Codex の累計）は最大値を使い、時間ごとの量は前回までの累計との差にする。

use std::collections::{BTreeMap, HashMap};

use rusqlite::{OptionalExtension, Row, params};
use serde_json::{Map, Value, json};

use super::{Result, Store, row_json};
use crate::js;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ai_sessions (
  node_id TEXT NOT NULL, file TEXT NOT NULL, tool TEXT NOT NULL,
  session_id TEXT, parent_id TEXT, cwd TEXT, version TEXT, origin TEXT, model TEXT, first_ts INTEGER, last_ts INTEGER,
  prompts INTEGER NOT NULL DEFAULT 0, assistant_msgs INTEGER NOT NULL DEFAULT 0, tool_calls INTEGER NOT NULL DEFAULT 0, tool_errors INTEGER NOT NULL DEFAULT 0,
  turn_errors INTEGER NOT NULL DEFAULT 0, hook_errors INTEGER NOT NULL DEFAULT 0, api_errors INTEGER NOT NULL DEFAULT 0,
  tok_in INTEGER NOT NULL DEFAULT 0, tok_out INTEGER NOT NULL DEFAULT 0, tok_cache_read INTEGER NOT NULL DEFAULT 0, tok_cache_write INTEGER NOT NULL DEFAULT 0,
  tok_reasoning INTEGER NOT NULL DEFAULT 0, tokens_mode TEXT NOT NULL DEFAULT 'add',
  tool_counts TEXT, tool_error_counts TEXT, prs TEXT, updated_at INTEGER NOT NULL,
  PRIMARY KEY (node_id, file)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ai_sessions_last ON ai_sessions (last_ts DESC);
CREATE TABLE IF NOT EXISTS ai_usage_hourly (
  node_id TEXT NOT NULL, file TEXT NOT NULL, tool TEXT NOT NULL, hour INTEGER NOT NULL,
  tok_in INTEGER NOT NULL DEFAULT 0, tok_out INTEGER NOT NULL DEFAULT 0, tok_cache_read INTEGER NOT NULL DEFAULT 0, tok_cache_write INTEGER NOT NULL DEFAULT 0,
  tok_reasoning INTEGER NOT NULL DEFAULT 0, tool_calls INTEGER NOT NULL DEFAULT 0, prompts INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (node_id, file, hour)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ai_usage_hourly_hour ON ai_usage_hourly (hour);
CREATE TABLE IF NOT EXISTS ai_cursors (
  node_id TEXT PRIMARY KEY, files TEXT NOT NULL DEFAULT '{}', updated_at INTEGER, last_ok_at INTEGER, last_error TEXT, file_errors TEXT,
  truncated INTEGER NOT NULL DEFAULT 0, no_python INTEGER NOT NULL DEFAULT 0,
  files_total INTEGER, files_changed INTEGER, bytes_pending INTEGER, bytes_read INTEGER, elapsed_s REAL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ai_claude_replay (
  node_id TEXT NOT NULL, file TEXT NOT NULL,
  PRIMARY KEY (node_id, file)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ai_claude_messages (
  node_id TEXT NOT NULL, file TEXT NOT NULL, message_id TEXT NOT NULL,
  usage TEXT NOT NULL, hour INTEGER,
  PRIMARY KEY (node_id, file, message_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ai_claude_tools (
  node_id TEXT NOT NULL, file TEXT NOT NULL, tool_use_id TEXT NOT NULL,
  name TEXT NOT NULL, hour INTEGER,
  PRIMARY KEY (node_id, file, tool_use_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ai_claude_tool_errors (
  node_id TEXT NOT NULL, file TEXT NOT NULL, tool_use_id TEXT NOT NULL,
  PRIMARY KEY (node_id, file, tool_use_id)
) WITHOUT ROWID;
";

/// トークンの5つ（入力・出力・キャッシュ読み・キャッシュ書き・推論）。調査の tokens のキーと表の列
pub const TOK_KEYS: [&str; 5] = ["in", "out", "cache_read", "cache_write", "reasoning"];
/// 一覧の1回の上限（画面にはページングして返す）
pub const SESSION_PAGE_MAX: i64 = 200;

/// 1セッションの要約（表の1行）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub tool: String,
    pub session_id: Option<String>,
    pub parent_id: Option<String>,
    pub cwd: Option<String>,
    pub version: Option<String>,
    pub origin: Option<String>,
    pub model: Option<String>,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    /// prompts, assistant_msgs, tool_calls, tool_errors, turn_errors, hook_errors, api_errors
    pub counts: [i64; 7],
    pub tokens: [i64; 5],
    pub tokens_max: bool,
    pub tool_counts: BTreeMap<String, i64>,
    pub tool_error_counts: BTreeMap<String, i64>,
    pub prs: Vec<String>,
}

const COUNT_KEYS: [&str; 7] = ["prompts", "assistant_msgs", "tool_calls", "tool_errors", "turn_errors", "hook_errors", "api_errors"];

fn int(v: Option<&Value>) -> i64 {
    let x = js::num(v);
    if x.is_finite() { x as i64 } else { 0 }
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

fn opt_int(v: Option<&Value>) -> Option<i64> {
    v.filter(|x| x.is_number()).map(|x| int(Some(x)))
}

fn counts_map(v: Option<&Value>) -> BTreeMap<String, i64> {
    v.and_then(Value::as_object).map(|m| m.iter().map(|(k, v)| (k.clone(), int(Some(v)))).collect()).unwrap_or_default()
}

impl Session {
    /// 調査の1件（sessions の要素）から
    pub fn from_probe(r: &Value) -> Session {
        let tok = r.get("tokens");
        Session {
            tool: js::string(r.get("tool")),
            session_id: opt_str(r.get("session_id")),
            parent_id: opt_str(r.get("parent_id")),
            cwd: opt_str(r.get("cwd")),
            version: opt_str(r.get("version")),
            origin: opt_str(r.get("origin")),
            model: opt_str(r.get("model")),
            first_ts: opt_int(r.get("first_ts")),
            last_ts: opt_int(r.get("last_ts")),
            counts: COUNT_KEYS.map(|k| int(r.get(k))),
            tokens: TOK_KEYS.map(|k| int(js::get(tok, k))),
            tokens_max: js::is_str(r.get("tokens_mode"), "max"),
            tool_counts: counts_map(r.get("tool_counts")),
            tool_error_counts: counts_map(r.get("tool_error_counts")),
            prs: js::arr(r.get("prs")).iter().filter_map(Value::as_str).map(str::to_string).collect(),
        }
    }

    /// 続き（mode = add）の分を足す。tokens_max の分は累計なので最大値
    pub fn merge_add(&self, add: &Session) -> Session {
        let keep = |a: &Option<String>, b: &Option<String>| a.clone().or_else(|| b.clone());
        let newer = |a: &Option<String>, b: &Option<String>| b.clone().or_else(|| a.clone());
        let mm = |a: Option<i64>, b: Option<i64>, f: fn(i64, i64) -> i64| match (a, b) {
            (Some(x), Some(y)) => Some(f(x, y)),
            (x, y) => x.or(y),
        };
        let mut tc = self.tool_counts.clone();
        for (k, v) in &add.tool_counts {
            *tc.entry(k.clone()).or_insert(0) += v;
        }
        let mut te = self.tool_error_counts.clone();
        for (k, v) in &add.tool_error_counts {
            *te.entry(k.clone()).or_insert(0) += v;
        }
        let mut prs = self.prs.clone();
        for p in &add.prs {
            if !prs.contains(p) {
                prs.push(p.clone());
            }
        }
        let mut tokens = self.tokens;
        for (i, t) in tokens.iter_mut().enumerate() {
            *t = if add.tokens_max { (*t).max(add.tokens[i]) } else { *t + add.tokens[i] };
        }
        let mut counts = self.counts;
        for (i, c) in counts.iter_mut().enumerate() {
            *c += add.counts[i];
        }
        Session {
            tool: if self.tool.is_empty() { add.tool.clone() } else { self.tool.clone() },
            session_id: keep(&self.session_id, &add.session_id),
            parent_id: keep(&self.parent_id, &add.parent_id),
            cwd: keep(&self.cwd, &add.cwd),
            origin: keep(&self.origin, &add.origin),
            version: newer(&self.version, &add.version),
            model: newer(&self.model, &add.model),
            first_ts: mm(self.first_ts, add.first_ts, i64::min),
            last_ts: mm(self.last_ts, add.last_ts, i64::max),
            counts,
            tokens,
            tokens_max: self.tokens_max || add.tokens_max,
            tool_counts: tc,
            tool_error_counts: te,
            prs,
        }
    }

    fn from_row(r: &Row<'_>) -> rusqlite::Result<Session> {
        let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok());
        Ok(Session {
            tool: r.get("tool")?,
            session_id: r.get("session_id")?,
            parent_id: r.get("parent_id")?,
            cwd: r.get("cwd")?,
            version: r.get("version")?,
            origin: r.get("origin")?,
            model: r.get("model")?,
            first_ts: r.get("first_ts")?,
            last_ts: r.get("last_ts")?,
            counts: [
                r.get("prompts")?,
                r.get("assistant_msgs")?,
                r.get("tool_calls")?,
                r.get("tool_errors")?,
                r.get("turn_errors")?,
                r.get("hook_errors")?,
                r.get("api_errors")?,
            ],
            tokens: [r.get("tok_in")?, r.get("tok_out")?, r.get("tok_cache_read")?, r.get("tok_cache_write")?, r.get("tok_reasoning")?],
            tokens_max: r.get::<_, String>("tokens_mode")? == "max",
            tool_counts: counts_map(parse(r.get("tool_counts")?).as_ref()),
            tool_error_counts: counts_map(parse(r.get("tool_error_counts")?).as_ref()),
            prs: parse(r.get("prs")?).map(|v| js::arr(Some(&v)).iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default(),
        })
    }
}

/// 時間ごとの量を、表に足す差分にする。tokens_max のときトークンは累計なので、prev（前回までの累計）からの増えた分にする
pub fn hour_deltas(hours: Option<&Value>, tokens_max: bool, prev: [i64; 5]) -> Vec<(i64, [i64; 7])> {
    let mut hs: Vec<(i64, [i64; 7])> = hours
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    let h = k.parse::<i64>().ok()?;
                    let a = js::arr(Some(v));
                    let mut x = [0i64; 7];
                    for (i, slot) in x.iter_mut().enumerate() {
                        *slot = int(a.get(i)).max(0);
                    }
                    Some((h, x))
                })
                .collect()
        })
        .unwrap_or_default();
    hs.sort_by_key(|(h, _)| *h);
    if tokens_max {
        let mut cum = prev;
        for (_, x) in &mut hs {
            for i in 0..5 {
                let c = x[i];
                x[i] = (c - cum[i]).max(0);
                cum[i] = cum[i].max(c);
            }
        }
    }
    hs.into_iter().filter(|(_, x)| x.iter().any(|v| *v > 0)).collect()
}

/// 取り込みの結果（機体ごと）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AiIngested {
    pub sessions: usize,
    pub hours: usize,
    pub files: usize,
}

fn counts_json(m: &BTreeMap<String, i64>) -> String {
    serde_json::to_string(m).unwrap_or_else(|_| "{}".into())
}
fn add_probe_hour(rec: &mut Value, h: Option<i64>, i: usize, n: i64) {
    if let Some(h) = h {
        let b = rec["hours"].as_object_mut().unwrap().entry(h.to_string()).or_insert_with(|| json!([0, 0, 0, 0, 0, 0, 0]));
        if let Some(a) = b.as_array_mut()
            && let Some(x) = a.get_mut(i)
        {
            *x = json!(int(Some(x)) + n);
        }
    }
}

fn claude_events(c: &rusqlite::Connection, node: &str, rec: &mut Value, out: &Value, replace: bool) -> Result<()> {
    if js::string(rec.get("tool")) != "claude" {
        return Ok(());
    }
    let file = js::string(rec.get("file"));
    if replace {
        for table in ["ai_claude_messages", "ai_claude_tools", "ai_claude_tool_errors"] {
            c.execute(&format!("DELETE FROM {table} WHERE node_id = ? AND file = ?"), params![node, file])?;
        }
    }
    let hour = |h: Option<&Value>| h.and_then(Value::as_i64);
    for e in js::arr(out.get("claude_usage")) {
        if js::string(e.get("file")) != file {
            continue;
        }
        let id = js::string(e.get("id"));
        let vals = js::arr(e.get("usage"));
        if id.is_empty() || id.len() > 256 || vals.len() != 5 {
            continue;
        }
        let current: [i64; 5] = std::array::from_fn(|i| int(vals.get(i)).max(0));
        let old: Option<(String, Option<i64>)> = c
            .query_row("SELECT usage, hour FROM ai_claude_messages WHERE node_id=? AND file=? AND message_id=?", params![node, file, id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let (prior, at, fresh) = match old {
            Some((s, h)) => (
                serde_json::from_str::<Value>(&s).ok().map(|v| std::array::from_fn(|i| int(v.as_array().and_then(|a| a.get(i))).max(0))).unwrap_or([0; 5]),
                h,
                false,
            ),
            None => ([0; 5], hour(e.get("hour")), true),
        };
        let next: [i64; 5] = std::array::from_fn(|i| prior[i].max(current[i]));
        c.execute("INSERT INTO ai_claude_messages (node_id,file,message_id,usage,hour) VALUES (?,?,?,?,?) ON CONFLICT(node_id,file,message_id) DO UPDATE SET usage=excluded.usage", params![node,file,id,serde_json::to_string(&next).unwrap(),at])?;
        for i in 0..5 {
            let d = next[i] - prior[i];
            if d > 0 {
                rec["tokens"][TOK_KEYS[i]] = json!(int(rec["tokens"].get(TOK_KEYS[i])) + d);
                add_probe_hour(rec, at, i, d);
            }
        }
        if fresh {
            rec["assistant_msgs"] = json!(int(rec.get("assistant_msgs")) + 1);
        }
    }
    for e in js::arr(out.get("claude_tools")) {
        if js::string(e.get("file")) != file {
            continue;
        }
        let id = js::string(e.get("id"));
        let name = js::string(e.get("name"));
        if id.is_empty() || id.len() > 256 || name.is_empty() || name.len() > 128 {
            continue;
        }
        let exists: Option<i64> =
            c.query_row("SELECT 1 FROM ai_claude_tools WHERE node_id=? AND file=? AND tool_use_id=?", params![node, file, id], |r| r.get(0)).optional()?;
        if exists.is_none() {
            let h = hour(e.get("hour"));
            c.execute("INSERT INTO ai_claude_tools (node_id,file,tool_use_id,name,hour) VALUES (?,?,?,?,?)", params![node, file, id, name, h])?;
            rec["tool_calls"] = json!(int(rec.get("tool_calls")) + 1);
            rec["tool_counts"][&name] = json!(int(rec["tool_counts"].get(&name)) + 1);
            add_probe_hour(rec, h, 5, 1);
        }
    }
    for e in js::arr(out.get("claude_errors")) {
        if js::string(e.get("file")) != file {
            continue;
        }
        let id = js::string(e.get("id"));
        if id.is_empty() || id.len() > 256 {
            continue;
        }
        let exists: Option<i64> = c
            .query_row("SELECT 1 FROM ai_claude_tool_errors WHERE node_id=? AND file=? AND tool_use_id=?", params![node, file, id], |r| r.get(0))
            .optional()?;
        if exists.is_none() {
            let name: Option<String> = c
                .query_row("SELECT name FROM ai_claude_tools WHERE node_id=? AND file=? AND tool_use_id=?", params![node, file, id], |r| r.get(0))
                .optional()?;
            let name = name.unwrap_or_else(|| "?".into());
            c.execute("INSERT INTO ai_claude_tool_errors (node_id,file,tool_use_id) VALUES (?,?,?)", params![node, file, id])?;
            rec["tool_errors"] = json!(int(rec.get("tool_errors")) + 1);
            rec["tool_error_counts"][&name] = json!(int(rec["tool_error_counts"].get(&name)) + 1);
        }
    }
    Ok(())
}

impl Store {
    /// 機体ごとの続きの位置（{ ファイルの鍵: バイト位置 }）
    pub fn ai_files(&self, node_id: &str) -> Result<Map<String, Value>> {
        let s: Option<String> = self.conn.query_row("SELECT files FROM ai_cursors WHERE node_id = ?", [node_id], |r| r.get(0)).optional()?;
        Ok(s.and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default())
    }

    /// 旧来の数値cursorだけの Claude ファイルは、ID台帳を作るため一度だけ replace で再読する。
    pub fn ai_claude_replay(&self, node_id: &str) -> Result<Value> {
        let files = self.ai_files(node_id)?;
        let mut replay = Vec::new();
        for (file, off) in files {
            if !file.starts_with("claude:") || int(Some(&off)) <= 0 {
                continue;
            }
            let seen: Option<i64> =
                self.conn.query_row("SELECT 1 FROM ai_claude_replay WHERE node_id = ? AND file = ?", params![node_id, file], |r| r.get(0)).optional()?;
            if seen.is_none() {
                replay.push(Value::String(file));
            }
        }
        Ok(Value::Array(replay))
    }

    /// 1セッション（無ければ None）
    pub fn ai_session(&self, node_id: &str, file: &str) -> Result<Option<Session>> {
        self.conn.prepare_cached("SELECT * FROM ai_sessions WHERE node_id = ? AND file = ?")?.query_row([node_id, file], Session::from_row).optional()
    }

    /// 調査の出力（out）を取り込む。セッション・時間ごとの量・続きの位置を1つのトランザクションで書く
    /// （途中で失敗したら何も書かない。次の回に同じ位置から読み直す）
    pub fn ai_ingest(&self, node_id: &str, out: &Value, now: i64) -> Result<AiIngested> {
        let mut files = self.ai_files(node_id)?;
        let mut res = AiIngested::default();
        self.tx(|c| {
            for source in js::arr(out.get("sessions")) {
                let mut rec = source.clone();
                let file = js::string(rec.get("file"));
                if file.is_empty() || file == "undefined" {
                    continue;
                }
                if !rec.get("hours").is_some_and(Value::is_object) { rec["hours"] = json!({}); }
                let replace = !js::is_str(rec.get("mode"), "add");
                let old = if replace { None } else { c.prepare_cached("SELECT * FROM ai_sessions WHERE node_id = ? AND file = ?")?.query_row(params![node_id, file], Session::from_row).optional()? };
                claude_events(c, node_id, &mut rec, out, replace)?;
                let add = Session::from_probe(&rec);
                let prev_tokens = old.as_ref().map_or([0; 5], |o| o.tokens);
                let s = match &old {
                    Some(o) => o.merge_add(&add),
                    None => add.clone(),
                };
                if replace {
                    c.prepare_cached("DELETE FROM ai_usage_hourly WHERE node_id = ? AND file = ?")?.execute(params![node_id, file])?;
                    if add.tool == "claude" { c.execute("INSERT OR IGNORE INTO ai_claude_replay (node_id,file) VALUES (?,?)", params![node_id,file])?; }
                }
                c.prepare_cached(
                    "INSERT OR REPLACE INTO ai_sessions (node_id, file, tool, session_id, parent_id, cwd, version, origin, model, first_ts, last_ts,
                       prompts, assistant_msgs, tool_calls, tool_errors, turn_errors, hook_errors, api_errors,
                       tok_in, tok_out, tok_cache_read, tok_cache_write, tok_reasoning, tokens_mode, tool_counts, tool_error_counts, prs, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )?
                .execute(params![
                    node_id,
                    file,
                    s.tool,
                    s.session_id,
                    s.parent_id,
                    s.cwd,
                    s.version,
                    s.origin,
                    s.model,
                    s.first_ts,
                    s.last_ts,
                    s.counts[0],
                    s.counts[1],
                    s.counts[2],
                    s.counts[3],
                    s.counts[4],
                    s.counts[5],
                    s.counts[6],
                    s.tokens[0],
                    s.tokens[1],
                    s.tokens[2],
                    s.tokens[3],
                    s.tokens[4],
                    if s.tokens_max { "max" } else { "add" },
                    counts_json(&s.tool_counts),
                    counts_json(&s.tool_error_counts),
                    serde_json::to_string(&s.prs).unwrap_or_else(|_| "[]".into()),
                    now
                ])?;
                // 時間ごとの量（累計のトークンは前回までの累計との差）
                for (h, x) in hour_deltas(rec.get("hours"), add.tokens_max, if replace { [0; 5] } else { prev_tokens }) {
                    c.prepare_cached(
                        "INSERT INTO ai_usage_hourly (node_id, file, tool, hour, tok_in, tok_out, tok_cache_read, tok_cache_write, tok_reasoning, tool_calls, prompts)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                         ON CONFLICT (node_id, file, hour) DO UPDATE SET tok_in = tok_in + excluded.tok_in, tok_out = tok_out + excluded.tok_out,
                           tok_cache_read = tok_cache_read + excluded.tok_cache_read, tok_cache_write = tok_cache_write + excluded.tok_cache_write,
                           tok_reasoning = tok_reasoning + excluded.tok_reasoning, tool_calls = tool_calls + excluded.tool_calls, prompts = prompts + excluded.prompts",
                    )?
                    .execute(params![node_id, file, s.tool, h, x[0], x[1], x[2], x[3], x[4], x[5], x[6]])?;
                    res.hours += 1;
                }
                res.sessions += 1;
            }
            if let Some(m) = out.get("cursors").and_then(Value::as_object) {
                for (k, v) in m {
                    files.insert(k.clone(), v.clone());
                }
            }
            for g in js::arr(out.get("gone")) {
                if let Some(k) = g.as_str() {
                    files.remove(k);
                    for table in ["ai_claude_messages", "ai_claude_tools", "ai_claude_tool_errors", "ai_claude_replay"] {
                        c.execute(&format!("DELETE FROM {table} WHERE node_id = ? AND file = ?"), params![node_id, k])?;
                    }
                }
            }
            res.files = files.len();
            let errs: Vec<&Value> = js::arr(out.get("errors")).iter().take(5).collect();
            c.prepare_cached(
                "INSERT INTO ai_cursors (node_id, files, updated_at, last_ok_at, last_error, file_errors, truncated, no_python, files_total, files_changed, bytes_pending, bytes_read, elapsed_s)
                 VALUES (?, ?, ?, ?, NULL, ?, ?, 0, ?, ?, ?, ?, ?)
                 ON CONFLICT (node_id) DO UPDATE SET files = excluded.files, updated_at = excluded.updated_at, last_ok_at = excluded.last_ok_at, last_error = NULL,
                   file_errors = excluded.file_errors, truncated = excluded.truncated, no_python = 0, files_total = excluded.files_total,
                   files_changed = excluded.files_changed, bytes_pending = excluded.bytes_pending, bytes_read = excluded.bytes_read, elapsed_s = excluded.elapsed_s",
            )?
            .execute(params![
                node_id,
                Value::Object(files.clone()).to_string(),
                now,
                now,
                if errs.is_empty() { None } else { Some(serde_json::to_string(&errs).unwrap_or_default()) },
                i64::from(js::truthy(out.get("truncated"))),
                opt_int(out.get("files_total")),
                opt_int(out.get("files_changed")),
                opt_int(out.get("bytes_pending")),
                opt_int(out.get("bytes_read")),
                out.get("elapsed_s").and_then(Value::as_f64)
            ])?;
            Ok(())
        })?;
        Ok(res)
    }

    /// 取り込みの失敗を残す（続きの位置は変えない）。no_python: 機体に python が無い
    pub fn ai_error(&self, node_id: &str, error: &str, no_python: bool, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ai_cursors (node_id, updated_at, last_error, no_python) VALUES (?, ?, ?, ?)
             ON CONFLICT (node_id) DO UPDATE SET updated_at = excluded.updated_at, last_error = excluded.last_error, no_python = excluded.no_python",
            params![node_id, now, js::slice16(error, 500), i64::from(no_python)],
        )?;
        Ok(())
    }

    /// 機体ごとの取り込みの様子（続きの位置そのものは返さない）
    pub fn ai_cursors(&self) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached(
            "SELECT c.node_id, c.updated_at, c.last_ok_at, c.last_error, c.file_errors, c.truncated, c.no_python, c.files_total, c.files_changed,
                    c.bytes_pending, c.bytes_read, c.elapsed_s, (SELECT count(*) FROM ai_sessions s WHERE s.node_id = c.node_id) AS sessions
             FROM ai_cursors c ORDER BY c.node_id",
        )?;
        let rows = st.query_map([], row_json)?.collect::<Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|mut r| {
                if let Value::Object(m) = &mut r {
                    for k in ["truncated", "no_python"] {
                        let b = js::truthy(m.get(k));
                        m.insert(k.into(), Value::Bool(b));
                    }
                    let fe = m.get("file_errors").and_then(Value::as_str).and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(json!([]));
                    m.insert("file_errors".into(), fe);
                }
                r
            })
            .collect())
    }

    /// 続きの残っている機体があるか（取り込み中・続きあり）
    pub fn ai_pending(&self, enabled: &[String]) -> Result<bool> {
        if enabled.is_empty() {
            return Ok(false);
        }
        let placeholders = std::iter::repeat_n("?", enabled.len()).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT count(*) FROM ai_cursors WHERE truncated = 1 AND node_id IN ({placeholders})");
        Ok(self.conn.query_row(&sql, rusqlite::params_from_iter(enabled.iter()), |r| r.get::<_, i64>(0))? > 0)
    }

    /// 集計（画面の AI）。f = { days（既定 30、1〜400）, tz（UTC からの分、東が正）, node_id, tool }
    pub fn ai_summary(&self, f: &Value, now: i64) -> Result<Value> {
        let days = (js::num(f.get("days")) as i64).clamp(1, 400);
        let days = if f.get("days").is_some_and(Value::is_number) { days } else { 30 };
        let tz_ms = (js::num(f.get("tz")).clamp(-24.0 * 60.0, 24.0 * 60.0) as i64) * 60_000;
        let tz_ms = if f.get("tz").is_some_and(Value::is_number) { tz_ms } else { 0 };
        // 期間の始まり: 今日（tz の日付）の 0 時から days - 1 日前
        let today = (now + tz_ms).div_euclid(86_400_000);
        let since = (today - (days - 1)) * 86_400_000 - tz_ms;
        let since_hour = since.div_euclid(3_600_000);
        let node = f.get("node_id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        let tool = f.get("tool").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        let mut cond_h = String::from("h.hour >= ?1");
        let mut cond_s = String::from("s.last_ts >= ?1");
        if node.is_some() {
            cond_h.push_str(" AND h.node_id = ?2");
            cond_s.push_str(" AND s.node_id = ?2");
        }
        if tool.is_some() {
            cond_h.push_str(" AND h.tool = ?3");
            cond_s.push_str(" AND s.tool = ?3");
        }
        let bind = |since_v: i64| -> Vec<rusqlite::types::Value> {
            vec![since_v.into(), node.clone().unwrap_or_default().into(), tool.clone().unwrap_or_default().into()]
        };
        let toks = "SUM(h.tok_in) AS tok_in, SUM(h.tok_out) AS tok_out, SUM(h.tok_cache_read) AS tok_cache_read, SUM(h.tok_cache_write) AS tok_cache_write, SUM(h.tok_reasoning) AS tok_reasoning";
        let q = |sql: &str, since_v: i64| -> Result<Vec<Value>> {
            // 使わない番号のパラメータがあっても SQLite は受け付ける（?2・?3 を条件に入れないとき）
            let mut st = self.conn.prepare(sql)?;
            let n = st.parameter_count();
            let args: Vec<rusqlite::types::Value> = bind(since_v).into_iter().take(n).collect();
            st.query_map(rusqlite::params_from_iter(args), row_json)?.collect()
        };
        // 機体×日
        let daily = q(
            &format!(
                "SELECT h.node_id, ((h.hour * 3600000 + {tz_ms}) / 86400000) * 86400000 - {tz_ms} AS day, COUNT(DISTINCT h.file) AS sessions,
                        SUM(h.prompts) AS prompts, SUM(h.tool_calls) AS tool_calls, {toks}
                 FROM ai_usage_hourly h WHERE {cond_h} GROUP BY h.node_id, day ORDER BY day, h.node_id"
            ),
            since_hour,
        )?;
        let totals = q(
            &format!(
                "SELECT COUNT(DISTINCT h.node_id || char(0) || h.file) AS sessions, coalesce(SUM(h.prompts), 0) AS prompts, coalesce(SUM(h.tool_calls), 0) AS tool_calls,
                        coalesce(SUM(h.tok_in), 0) AS tok_in, coalesce(SUM(h.tok_out), 0) AS tok_out, coalesce(SUM(h.tok_cache_read), 0) AS tok_cache_read,
                        coalesce(SUM(h.tok_cache_write), 0) AS tok_cache_write, coalesce(SUM(h.tok_reasoning), 0) AS tok_reasoning
                 FROM ai_usage_hourly h WHERE {cond_h}"
            ),
            since_hour,
        )?
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
        // モデル別（その期間に使ったトークン）
        let models = q(
            &format!(
                "SELECT s.tool, s.model, COUNT(DISTINCT h.node_id || char(0) || h.file) AS sessions, {toks},
                        SUM(h.tok_in + h.tok_out) AS io
                 FROM ai_usage_hourly h JOIN ai_sessions s ON s.node_id = h.node_id AND s.file = h.file
                 WHERE {cond_h} GROUP BY s.tool, s.model ORDER BY io DESC LIMIT 30"
            ),
            since_hour,
        )?;
        // 失敗の多いツール（期間内に動いたセッションの、ツール名ごとの呼び出しと失敗）
        let calls = q(
            &format!(
                "SELECT j.key AS name, SUM(j.value) AS calls, COUNT(*) AS sessions FROM ai_sessions s, json_each(s.tool_counts) j WHERE {cond_s} GROUP BY j.key"
            ),
            since,
        )?;
        let fails = q(
            &format!(
                "SELECT j.key AS name, SUM(j.value) AS errors, COUNT(*) AS sessions FROM ai_sessions s, json_each(s.tool_error_counts) j WHERE {cond_s} GROUP BY j.key"
            ),
            since,
        )?;
        let mut tools: HashMap<String, Value> = HashMap::new();
        for c in &calls {
            let name = js::string(c.get("name"));
            tools.insert(name.clone(), json!({ "name": name, "calls": c["calls"], "errors": 0, "sessions": 0 }));
        }
        for e in &fails {
            let name = js::string(e.get("name"));
            let t = tools.entry(name.clone()).or_insert_with(|| json!({ "name": name, "calls": 0 }));
            t["errors"] = e["errors"].clone();
            t["sessions"] = e["sessions"].clone();
        }
        let mut tools: Vec<Value> = tools.into_values().filter(|t| int(t.get("errors")) > 0).collect();
        tools.sort_by(|a, b| int(b.get("errors")).cmp(&int(a.get("errors"))).then_with(|| js::string(a.get("name")).cmp(&js::string(b.get("name")))));
        tools.truncate(20);
        // 長いセッション（期間内に動いたもの。開始から最後まで）
        let long = q(
            &format!(
                "SELECT s.node_id, s.file, s.tool, s.session_id, s.parent_id, s.cwd, s.model, s.version, s.first_ts, s.last_ts, (s.last_ts - s.first_ts) AS duration_ms,
                        s.prompts, s.tool_calls, s.tool_errors, s.tok_in, s.tok_out, s.tok_cache_read, s.tok_cache_write, s.tok_reasoning
                 FROM ai_sessions s WHERE {cond_s} AND s.first_ts IS NOT NULL ORDER BY duration_ms DESC LIMIT 10"
            ),
            since,
        )?;
        // PR へのリンク（新しい順・重複は1つ）
        let pr_rows = q(
            &format!(
                "SELECT s.node_id, s.tool, s.cwd, s.session_id, s.last_ts, s.prs FROM ai_sessions s WHERE {cond_s} AND s.prs IS NOT NULL AND s.prs != '[]' ORDER BY s.last_ts DESC"
            ),
            since,
        )?;
        let mut prs: Vec<Value> = Vec::new();
        for r in &pr_rows {
            let list: Value = r.get("prs").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(json!([]));
            for u in js::arr(Some(&list)).iter().filter_map(Value::as_str) {
                if prs.len() < 50 && !prs.iter().any(|p| p["url"] == u) {
                    prs.push(json!({ "url": u, "node_id": r["node_id"], "tool": r["tool"], "cwd": r["cwd"], "session_id": r["session_id"], "last_ts": r["last_ts"] }));
                }
            }
        }
        // 機体ごとの版（ツールごとに、いちばん新しいセッションの版）
        let versions = self
            .conn
            .prepare_cached("SELECT s.node_id, s.tool, s.version, MAX(s.last_ts) AS last_ts FROM ai_sessions s WHERE s.version IS NOT NULL GROUP BY s.node_id, s.tool ORDER BY s.tool, s.node_id")?
            .query_map([], row_json)?
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({
            "days": days, "since": since, "tz": tz_ms / 60_000,
            "totals": totals, "daily": daily, "models": models, "tools": tools, "long": long, "prs": prs, "versions": versions,
        }))
    }

    /// セッションの一覧（ページング）。f = { node_id, tool, kind: main | sub, q（cwd・モデル・ID の部分一致）, days, sort: last | duration | tokens | errors, offset, limit }
    pub fn ai_session_list(&self, f: &Value, now: i64) -> Result<Value> {
        let mut where_: Vec<String> = Vec::new();
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(n) = f.get("node_id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            where_.push("node_id = ?".into());
            args.push(n.to_string().into());
        }
        if let Some(t) = f.get("tool").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            where_.push("tool = ?".into());
            args.push(t.to_string().into());
        }
        match f.get("kind").and_then(Value::as_str) {
            Some("main") => where_.push("parent_id IS NULL".into()),
            Some("sub") => where_.push("parent_id IS NOT NULL".into()),
            _ => {}
        }
        if let Some(d) = f.get("days").and_then(Value::as_f64).filter(|d| *d > 0.0) {
            where_.push("last_ts >= ?".into());
            args.push((now - (d * 86_400_000.0) as i64).into());
        }
        if let Some(q) = f.get("q").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
            let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            where_.push("(cwd LIKE ? ESCAPE '\\' OR model LIKE ? ESCAPE '\\' OR session_id LIKE ? ESCAPE '\\' OR version LIKE ? ESCAPE '\\')".into());
            for _ in 0..4 {
                args.push(like.clone().into());
            }
        }
        if f.get("errors").and_then(Value::as_bool) == Some(true) {
            where_.push("(tool_errors + turn_errors + api_errors + hook_errors) > 0".into());
        }
        let w = if where_.is_empty() { String::new() } else { format!("WHERE {}", where_.join(" AND ")) };
        let order = match f.get("sort").and_then(Value::as_str) {
            Some("duration") => "(last_ts - first_ts) DESC",
            Some("tokens") => "(tok_in + tok_out) DESC",
            Some("errors") => "(tool_errors + turn_errors + api_errors + hook_errors) DESC",
            _ => "last_ts DESC",
        };
        let limit = (js::num(f.get("limit")) as i64).clamp(1, SESSION_PAGE_MAX);
        let limit = if f.get("limit").is_some_and(Value::is_number) { limit } else { 50 };
        let offset = (js::num(f.get("offset")) as i64).max(0);
        let total: i64 =
            self.conn.prepare(&format!("SELECT count(*) FROM ai_sessions {w}"))?.query_row(rusqlite::params_from_iter(args.clone()), |r| r.get(0))?;
        let mut a = args;
        a.push(limit.into());
        a.push(offset.into());
        let sql = format!(
            "SELECT node_id, file, tool, session_id, parent_id, cwd, version, origin, model, first_ts, last_ts, (last_ts - first_ts) AS duration_ms,
                    prompts, assistant_msgs, tool_calls, tool_errors, turn_errors, hook_errors, api_errors,
                    tok_in, tok_out, tok_cache_read, tok_cache_write, tok_reasoning, tool_error_counts, prs
             FROM ai_sessions {w} ORDER BY {order}, node_id, file LIMIT ? OFFSET ?"
        );
        let rows = self.conn.prepare(&sql)?.query_map(rusqlite::params_from_iter(a), row_json)?.collect::<Result<Vec<_>>>()?;
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|mut r| {
                if let Value::Object(m) = &mut r {
                    for k in ["tool_error_counts", "prs"] {
                        let v = m.get(k).and_then(Value::as_str).and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null);
                        m.insert(k.into(), v);
                    }
                }
                r
            })
            .collect();
        Ok(json!({ "rows": rows, "total": total, "offset": offset, "limit": limit }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 1_800_000_000_000 / 3_600_000;

    fn out(sessions: Vec<Value>, cursors: Value) -> Value {
        json!({ "sessions": sessions, "cursors": cursors, "gone": [], "files_total": 2, "files_changed": 1, "bytes_pending": 100, "bytes_read": 100, "truncated": false, "errors": [] })
    }

    fn claude(mode: &str, prompts: i64, tok_in: i64, hour: i64) -> Value {
        json!({ "tool": "claude", "file": "claude:p/s1.jsonl", "mode": mode, "session_id": "s1", "cwd": "~/work", "model": "model-a", "version": "1.0",
                "first_ts": hour * 3_600_000, "last_ts": hour * 3_600_000 + 60_000, "prompts": prompts, "assistant_msgs": 1, "tool_calls": 2, "tool_errors": 1,
                "tokens": { "in": tok_in, "out": 5, "cache_read": 0, "cache_write": 0, "reasoning": 0 }, "tokens_mode": "add",
                "tool_counts": { "Bash": 2 }, "tool_error_counts": { "Bash": 1 }, "prs": ["https://example.com/pr/1"],
                "hours": { (hour.to_string()): [tok_in, 5, 0, 0, 0, 2, prompts] } })
    }

    fn codex(mode: &str, cum_in: i64, hour: i64) -> Value {
        json!({ "tool": "codex", "file": "codex:2026/r.jsonl", "mode": mode, "session_id": "c1", "model": "model-b", "version": "2.0",
                "first_ts": hour * 3_600_000, "last_ts": hour * 3_600_000 + 1000, "prompts": 1, "tool_calls": 1,
                "tokens": { "in": cum_in, "out": 10, "cache_read": 0, "cache_write": 0, "reasoning": 0 }, "tokens_mode": "max",
                "tool_counts": { "shell": 1 }, "tool_error_counts": {}, "prs": [],
                "hours": { (hour.to_string()): [cum_in, 10, 0, 0, 0, 1, 1] } })
    }

    #[test]
    fn replace_then_add_and_max() {
        let db = Store::open_in_memory().unwrap();
        db.ai_ingest("n1", &out(vec![claude("replace", 1, 10, H), codex("replace", 100, H)], json!({ "claude:p/s1.jsonl": 50, "codex:2026/r.jsonl": 70 })), 1)
            .unwrap();
        // 続き: Claude は足し、Codex は累計なので最大値。時間ごとの量は差分
        db.ai_ingest("n1", &out(vec![claude("add", 2, 7, H + 30), codex("add", 250, H + 30)], json!({ "claude:p/s1.jsonl": 90 })), 2).unwrap();
        let c = db.ai_session("n1", "claude:p/s1.jsonl").unwrap().unwrap();
        assert_eq!((c.counts[0], c.tokens[0], c.tool_counts["Bash"], c.prs.len()), (3, 17, 4, 1));
        assert_eq!((c.first_ts, c.last_ts), (Some(H * 3_600_000), Some((H + 30) * 3_600_000 + 60_000)));
        let x = db.ai_session("n1", "codex:2026/r.jsonl").unwrap().unwrap();
        assert_eq!((x.tokens[0], x.tokens_max), (250, true));
        let hourly: Vec<(i64, i64)> = db
            .conn()
            .prepare("SELECT hour, tok_in FROM ai_usage_hourly WHERE file = 'codex:2026/r.jsonl' ORDER BY hour")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(hourly, [(H, 100), (H + 30, 150)], "累計は前回までとの差にする");
        let files = db.ai_files("n1").unwrap();
        assert_eq!((files["claude:p/s1.jsonl"].clone(), files["codex:2026/r.jsonl"].clone()), (json!(90), json!(70)));
        // 置き換え（ファイルが短くなった）: 時間ごとの量も作り直す
        db.ai_ingest("n1", &out(vec![claude("replace", 1, 4, H + 40)], json!({ "claude:p/s1.jsonl": 10 })), 3).unwrap();
        let c = db.ai_session("n1", "claude:p/s1.jsonl").unwrap().unwrap();
        assert_eq!((c.counts[0], c.tokens[0]), (1, 4));
        let n: i64 = db.conn().query_row("SELECT count(*) FROM ai_usage_hourly WHERE file = 'claude:p/s1.jsonl'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // 消えたファイルの位置は忘れる（セッションの記録は残す）
        db.ai_ingest("n1", &json!({ "sessions": [], "cursors": {}, "gone": ["codex:2026/r.jsonl"], "truncated": true }), 4).unwrap();
        assert!(!db.ai_files("n1").unwrap().contains_key("codex:2026/r.jsonl"));
        assert!(db.ai_session("n1", "codex:2026/r.jsonl").unwrap().is_some());
        assert!(db.ai_pending(&["n1".into()]).unwrap());
        assert!(!db.ai_pending(&["disabled".into()]).unwrap(), "台帳から外れた機体のcursorは追いかけない");
    }

    #[test]
    fn errors_keep_the_cursor() {
        let db = Store::open_in_memory().unwrap();
        db.ai_ingest("n1", &out(vec![], json!({ "a": 5 })), 1).unwrap();
        db.ai_error("n1", "python が無い", true, 2).unwrap();
        assert_eq!(db.ai_files("n1").unwrap()["a"], json!(5));
        let c = &db.ai_cursors().unwrap()[0];
        assert_eq!((c["no_python"].clone(), c["last_error"].clone()), (json!(true), json!("python が無い")));
    }

    #[test]
    fn claude_metadata_is_exactly_once_across_polls_and_replace() {
        let db = Store::open_in_memory().unwrap();
        db.ai_ingest("n", &out(vec![], json!({ "claude:legacy": 9 })), 0).unwrap();
        assert_eq!(db.ai_claude_replay("n").unwrap(), json!(["claude:legacy"]));
        let record =
            |mode: &str| json!({ "mode": mode, "file": "claude:x", "tool": "claude", "tokens": {}, "hours": {}, "tool_counts": {}, "tool_error_counts": {} });
        let first = json!({ "sessions": [record("replace")], "cursors": {"claude:x": 10}, "gone": [], "claude_usage": [{"file":"claude:x","id":"m","usage":[10,2,0,0,0],"hour":H}], "claude_tools":[{"file":"claude:x","id":"t","name":"Bash","hour":H}], "claude_errors":[], "errors":[] });
        db.ai_ingest("n", &first, 1).unwrap();
        let before_files = db.ai_files("n").unwrap();
        db.conn.execute_batch("CREATE TRIGGER fail_ai BEFORE INSERT ON ai_usage_hourly BEGIN SELECT RAISE(ABORT, 'test rollback'); END;").unwrap();
        assert!(db.ai_ingest("n", &json!({ "sessions": [record("add")], "cursors": {"claude:x": 99}, "gone": [], "claude_usage": [{"file":"claude:x","id":"new","usage":[1,0,0,0,0],"hour":H}], "claude_tools": [], "claude_errors": [], "errors": [] }), 2).is_err());
        assert_eq!(db.ai_files("n").unwrap(), before_files, "cursor rolls back with metadata");
        let ids: i64 = db.conn.query_row("SELECT count(*) FROM ai_claude_messages WHERE node_id='n' AND file='claude:x'", [], |r| r.get(0)).unwrap();
        assert_eq!(ids, 1, "dedupe rows roll back");
        db.conn.execute_batch("DROP TRIGGER fail_ai;").unwrap();
        let second = json!({ "sessions": [record("add")], "cursors": {"claude:x": 20}, "gone": [], "claude_usage": [{"file":"claude:x","id":"m","usage":[15,1,0,0,0],"hour":H + 1}], "claude_tools":[], "claude_errors":[{"file":"claude:x","id":"t","hour":H + 1}], "errors":[] });
        db.ai_ingest("n", &second, 2).unwrap();
        db.ai_ingest("n", &second, 3).unwrap();
        let s = db.ai_session("n", "claude:x").unwrap().unwrap();
        assert_eq!(s.counts[..4], [0, 1, 1, 1]);
        assert_eq!(s.tokens, [15, 2, 0, 0, 0]);
        let hour: (i64, i64) = db
            .conn
            .query_row("SELECT tok_in, tool_calls FROM ai_usage_hourly WHERE node_id='n' AND file='claude:x' AND hour=?", [H], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(hour, (15, 1), "usage update remains attributed to its original hour");
        let reset = json!({ "sessions": [record("replace")], "cursors": {"claude:x": 30}, "gone": [], "claude_usage": [{"file":"claude:x","id":"m","usage":[4,0,0,0,0],"hour":H}], "claude_tools":[], "claude_errors":[], "errors":[] });
        db.ai_ingest("n", &reset, 4).unwrap();
        assert_eq!(db.ai_session("n", "claude:x").unwrap().unwrap().tokens[0], 4);
        db.ai_ingest("n", &json!({ "sessions": [], "cursors": {}, "gone": ["claude:x"], "errors": [] }), 5).unwrap();
        assert!(db.ai_session("n", "claude:x").unwrap().is_some(), "gone preserves historical session");
        let ids: i64 = db.conn.query_row("SELECT count(*) FROM ai_claude_messages WHERE node_id='n' AND file='claude:x'", [], |r| r.get(0)).unwrap();
        assert_eq!(ids, 0);
    }

    #[test]
    fn claude_replay_failure_rolls_back_all_tables_and_retries() {
        let db = Store::open_in_memory().unwrap();
        let record = |file, mode| json!({"tool":"claude","file":file,"mode":mode,"tokens":{},"hours":{}});
        let seed = json!({"sessions":[record("claude:old","replace")],"cursors":{"claude:old":10},"claude_usage":[{"file":"claude:old","id":"old","usage":[3,0,0,0,0],"hour":H}]});
        db.ai_ingest("n", &seed, 1).unwrap();
        let old_session = db.ai_session("n", "claude:old").unwrap();
        let old_cursors = db.ai_cursors().unwrap();
        db.conn.execute_batch("CREATE TRIGGER fail_new BEFORE INSERT ON ai_usage_hourly WHEN NEW.file='claude:new' BEGIN SELECT RAISE(ABORT, 'test migration rollback'); END;").unwrap();
        let batch = json!({"sessions":[record("claude:old","add"),record("claude:new","replace")],"cursors":{"claude:old":20,"claude:new":30},"claude_usage":[{"file":"claude:old","id":"old","usage":[5,0,0,0,0],"hour":H},{"file":"claude:new","id":"new","usage":[7,0,0,0,0],"hour":H}],"claude_tools":[{"file":"claude:new","id":"tool","name":"Bash","hour":H}],"claude_errors":[{"file":"claude:new","id":"tool","hour":H}]});
        assert!(db.ai_ingest("n", &batch, 2).is_err());
        assert_eq!(db.ai_session("n", "claude:old").unwrap(), old_session);
        assert_eq!(db.ai_cursors().unwrap(), old_cursors);
        for table in ["ai_sessions", "ai_usage_hourly", "ai_claude_messages", "ai_claude_tools", "ai_claude_tool_errors", "ai_claude_replay"] {
            let n: i64 = db.conn.query_row(&format!("SELECT count(*) FROM {table} WHERE file='claude:new'"), [], |r| r.get(0)).unwrap();
            assert_eq!(n, 0, "{table} must roll back with the migration cursor");
        }
        let usage: String = db.conn.query_row("SELECT usage FROM ai_claude_messages WHERE file='claude:old'", [], |r| r.get(0)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&usage).unwrap(), json!([3, 0, 0, 0, 0]));
        let tokens: i64 = db.conn.query_row("SELECT tok_in FROM ai_usage_hourly WHERE file='claude:old'", [], |r| r.get(0)).unwrap();
        assert_eq!(tokens, 3);
        db.conn.execute_batch("DROP TRIGGER fail_new;").unwrap();
        db.ai_ingest("n", &batch, 3).unwrap();
        assert_eq!(db.ai_session("n", "claude:old").unwrap().unwrap().tokens[0], 5);
        let fresh = db.ai_session("n", "claude:new").unwrap().unwrap();
        assert_eq!(fresh.tokens[0], 7);
        assert_eq!(fresh.counts[1..4], [1, 1, 1]);
        assert_eq!(db.ai_claude_replay("n").unwrap(), json!([]));
    }

    #[test]
    fn legacy_cursor_db_script_python_replay_then_incremental() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let python = ["python3", "python"]
            .into_iter()
            .find(|p| Command::new(p).arg("--version").status().is_ok_and(|s| s.success()))
            .expect("AI probe tests require Python");
        let home = std::env::temp_dir().join(format!("kt-ai-chain-{}-{}", std::process::id(), super::super::now_ms()));
        let project = home.join(".claude/projects/project");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("session.jsonl");
        let row = |input, stamp| {
            json!({"type":"assistant","sessionId":"session","timestamp":stamp,"message":{"id":"m","usage":{"input_tokens":input},"content":[{"type":"text","text":"private-message-body"},{"type":"tool_use","id":"t","name":"Bash","input":{"command":"private-command-body"}}]}}).to_string() + "\n"
        };
        std::fs::write(&path, row(10, "2026-10-07T00:00:00Z")).unwrap();
        let file = "claude:project/session.jsonl";
        let db = Store::open_in_memory().unwrap();
        let mut legacy = claude("replace", 0, 99, H);
        legacy["file"] = json!(file);
        db.ai_ingest("node", &out(vec![legacy], json!({file:std::fs::metadata(&path).unwrap().len()})), 1).unwrap();
        db.conn.execute("DELETE FROM ai_claude_replay", []).unwrap(); // 旧schema由来の数値cursorだけの状態
        let probe = || {
            let replay = db.ai_claude_replay("node").unwrap();
            let src = crate::ai_sessions::script(&db.ai_files("node").unwrap(), &replay);
            let mut child = Command::new(python)
                .args(["-X", "utf8", "-"])
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(src.as_bytes()).unwrap();
            let result = child.wait_with_output().unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            let raw = String::from_utf8(result.stdout).unwrap();
            assert!(!raw.contains("private-message-body") && !raw.contains("private-command-body"));
            serde_json::from_str::<Value>(&raw).unwrap()
        };
        assert_eq!(db.ai_claude_replay("node").unwrap(), json!([file]));
        let first = probe();
        assert_eq!(first["sessions"][0]["mode"], "replace", "不変サイズも旧cursorを再読する");
        db.ai_ingest("node", &first, 2).unwrap();
        assert_eq!(db.ai_session("node", file).unwrap().unwrap().tokens[0], 10, "旧二重集計を訂正する");
        assert_eq!(db.ai_claude_replay("node").unwrap(), json!([]));
        assert_eq!(probe()["sessions"], json!([]), "移行成功後は全量を再送しない");
        let mut writer = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writer.write_all(row(15, "2026-10-07T01:00:00Z").as_bytes()).unwrap();
        writer.write_all((json!({"type":"user","timestamp":"2026-10-07T01:00:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"t","is_error":true,"content":"private-message-body"}]}}).to_string()+"\n").as_bytes()).unwrap();
        drop(writer);
        let second = probe();
        assert_eq!(second["sessions"][0]["mode"], "add");
        db.ai_ingest("node", &second, 3).unwrap();
        let session = db.ai_session("node", file).unwrap().unwrap();
        assert_eq!(session.tokens[0], 15);
        assert_eq!(session.counts[1..4], [1, 1, 1]);
        assert_eq!(session.tool_error_counts["Bash"], 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn summary_and_list() {
        let db = Store::open_in_memory().unwrap();
        db.ai_ingest("n1", &out(vec![claude("replace", 1, 10, H), codex("replace", 100, H + 1)], json!({})), 1).unwrap();
        db.ai_ingest("n2", &out(vec![claude("replace", 2, 20, H + 2)], json!({})), 1).unwrap();
        let now = (H + 3) * 3_600_000;
        let s = db.ai_summary(&json!({ "days": 7, "tz": 0 }), now).unwrap();
        assert_eq!(s["totals"]["sessions"], json!(3));
        assert_eq!(s["totals"]["tok_in"], json!(130));
        assert_eq!(s["tools"][0]["name"], "Bash");
        assert_eq!(s["tools"][0]["errors"], json!(2));
        assert_eq!(s["prs"].as_array().unwrap().len(), 1, "同じ PR は1つ");
        assert_eq!(s["versions"].as_array().unwrap().len(), 3);
        let only = db.ai_summary(&json!({ "days": 7, "tz": 0, "tool": "codex" }), now).unwrap();
        assert_eq!(only["totals"]["tok_in"], json!(100));
        let l = db.ai_session_list(&json!({ "limit": 2, "sort": "tokens" }), now).unwrap();
        assert_eq!((l["total"].clone(), l["rows"].as_array().unwrap().len()), (json!(3), 2));
        assert_eq!(l["rows"][0]["tool"], "codex");
        let q = db.ai_session_list(&json!({ "q": "model-b" }), now).unwrap();
        assert_eq!(q["total"], json!(1));
        let lim = db.ai_session_list(&json!({ "limit": 100_000 }), now).unwrap();
        assert_eq!(lim["limit"], json!(SESSION_PAGE_MAX));
    }

    #[test]
    fn hour_deltas_from_cumulative() {
        let h = json!({ "10": [5, 1, 0, 0, 0, 0, 0], "11": [0, 0, 0, 0, 0, 2, 0], "12": [9, 3, 0, 0, 0, 0, 1] });
        assert_eq!(hour_deltas(Some(&h), true, [2, 0, 0, 0, 0]), vec![(10, [3, 1, 0, 0, 0, 0, 0]), (11, [0, 0, 0, 0, 0, 2, 0]), (12, [4, 2, 0, 0, 0, 0, 1])]);
        assert_eq!(hour_deltas(Some(&h), false, [0; 5])[0], (10, [5, 1, 0, 0, 0, 0, 0]));
    }
}
