//! AI の使用量の集計（ファイルをまたぐ重複を除いた量と、費用の推定）と、数字の出どころの辿り方。
//! - Claude Code の同じ応答は複数のファイル（サブエージェントのファイルなど）に出る。応答ごとの量（ai_claude_messages）は
//!   機体ごとに応答 ID で重複を除いてから数える（同じ応答の写しは同じ値のはずで、違えば項目ごとに大きい方）。
//!   ファイルの中の重複（同じ応答が複数行・複数回の取り込みに分かれる）は取り込みの時点で除いてある（db/ai_sessions.rs）
//! - Codex と、応答ごとの量が無い Claude Code のセッション（台帳より前の取り込みで、元のファイルが消えて読み直せないもの）は、
//!   時間ごとの量（ai_usage_hourly）をそのまま使う。モデルはセッションの最後のモデル、1 時間のキャッシュ書きは 0（5 分として数える）
//! - 費用は prices.rs の単価表で換算する（API 単価換算の推定。サブスクの実請求ではない）。各集計に単価表の版を付け、
//!   単価表に無いモデルの量は「単価不明」として別に数える（0 円にしない）

use std::collections::{BTreeMap, HashSet};

use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

use super::{Result, Store, row_json};
use crate::js;
use crate::prices::{self, Tokens};

/// 集計の範囲。hour は epoch の時（ms / 3600000）。tz は UTC からのずれ（ms、東が正）。日はその tz の 0 時で区切る
#[derive(Clone, Debug, Default)]
pub struct UsageWindow {
    pub since_hour: i64,
    pub until_hour: i64,
    pub tz_ms: i64,
    pub node: Option<String>,
    pub tool: Option<String>,
}

/// 機体×日×ツール×モデルの量
#[derive(Clone, Debug, PartialEq)]
pub struct UsageRow {
    pub node_id: String,
    pub day: i64,
    pub tool: String,
    pub model: Option<String>,
    pub tokens: Tokens,
    pub reasoning: i64,
}

impl UsageWindow {
    /// SQL の ?1 = since_hour, ?2 = until_hour, ?3 = 機体（空は全部）, ?4 = ツール（空は全部）
    fn args(&self) -> Vec<rusqlite::types::Value> {
        vec![self.since_hour.into(), self.until_hour.into(), self.node.clone().unwrap_or_default().into(), self.tool.clone().unwrap_or_default().into()]
    }
}

/// 費用の集計（1 つの単位: 全体・機体×日・モデル）
#[derive(Clone, Debug, Default)]
struct CostAcc {
    tokens: Tokens,
    reasoning: i64,
    cost: f64,
    priced: bool,
    unpriced: Tokens,
    unpriced_models: Vec<String>,
}

impl CostAcc {
    fn add(&mut self, r: &UsageRow, c: Option<f64>) {
        self.tokens.add(&r.tokens);
        self.reasoning += r.reasoning;
        match c {
            Some(v) => {
                self.cost += v;
                self.priced = true;
            }
            None => {
                self.unpriced.add(&r.tokens);
                let t = &r.tokens;
                let m = format!("{}:{}", r.tool, r.model.clone().unwrap_or_else(|| "（不明）".into()));
                // 量の無いモデル（モデル名の無い空のセッションなど）は「単価不明」に並べない
                if t.input + t.output + t.cache_read + t.cache_write > 0 && !self.unpriced_models.contains(&m) {
                    self.unpriced_models.push(m);
                }
            }
        }
    }

    /// 量と費用。cost_usd は単価のわかる分の合計（単価不明の量は unpriced に別に出す）
    fn to_json(&self, version: &str) -> Value {
        let mut v = self.tokens.to_json();
        let unpriced_io = self.unpriced.input + self.unpriced.output + self.unpriced.cache_read + self.unpriced.cache_write;
        if let Value::Object(m) = &mut v {
            m.insert("tok_reasoning".into(), json!(self.reasoning));
            m.insert("cost_usd".into(), if self.priced { json!((self.cost * 10_000.0).round() / 10_000.0) } else { Value::Null });
            m.insert("unpriced".into(), if unpriced_io > 0 { self.unpriced.to_json() } else { Value::Null });
            m.insert("unpriced_models".into(), json!(self.unpriced_models));
            m.insert("prices_version".into(), json!(version));
        }
        v
    }
}

impl Store {
    /// 機体×日×ツール×モデルの量。dedup = true なら Claude Code の応答の写し（ファイル・区間をまたぐ）を 1 つに数える。
    /// false なら写しも全部足す（除く前の量。時間ごとの量の合計と同じ数え方）
    pub fn usage_rows(&self, w: &UsageWindow, dedup: bool) -> Result<Vec<UsageRow>> {
        let agg = if dedup { "MAX" } else { "SUM" };
        let tz = w.tz_ms;
        let tok = |i: usize| format!("{agg}(coalesce(json_extract(a.usage, '$[{i}]'), 0))");
        let sql = format!(
            "WITH r AS (
               SELECT a.node_id, a.message_id, MIN(a.hour) AS hour, MAX(coalesce(a.model, s.model)) AS model,
                      {t0} AS tok_in, {t1} AS tok_out, {t2} AS tok_cache_read, {t3} AS tok_cache_write, {agg}(a.cw1h) AS cw1h, {t4} AS tok_reasoning
               FROM ai_claude_messages a LEFT JOIN ai_sessions s ON s.node_id = a.node_id AND s.file = a.file
               WHERE a.hour >= ?1 AND a.hour < ?2 AND (?3 = '' OR a.node_id = ?3) GROUP BY a.node_id, a.message_id
             ), u AS (
               SELECT node_id, 'claude' AS tool, hour, model, tok_in, tok_out, tok_cache_read, tok_cache_write, cw1h, tok_reasoning FROM r
               UNION ALL
               SELECT h.node_id, h.tool, h.hour, s.model, h.tok_in, h.tok_out, h.tok_cache_read, h.tok_cache_write, 0, h.tok_reasoning
               FROM ai_usage_hourly h JOIN ai_sessions s ON s.node_id = h.node_id AND s.file = h.file
               WHERE h.hour >= ?1 AND h.hour < ?2 AND (?3 = '' OR h.node_id = ?3)
                 AND (h.tool != 'claude' OR NOT EXISTS (SELECT 1 FROM ai_claude_messages m WHERE m.node_id = h.node_id AND m.file = h.file))
             )
             SELECT node_id, ((hour * 3600000 + {tz}) / 86400000) * 86400000 - {tz} AS day, tool, model,
                    SUM(tok_in), SUM(tok_out), SUM(tok_cache_read), SUM(tok_cache_write), SUM(cw1h), SUM(tok_reasoning)
             FROM u WHERE (?4 = '' OR tool = ?4) GROUP BY node_id, day, tool, model ORDER BY day, node_id, tool, model",
            t0 = tok(0),
            t1 = tok(1),
            t2 = tok(2),
            t3 = tok(3),
            t4 = tok(4),
        );
        let mut st = self.conn.prepare(&sql)?;
        st.query_map(rusqlite::params_from_iter(w.args()), |r| {
            Ok(UsageRow {
                node_id: r.get(0)?,
                day: r.get(1)?,
                tool: r.get(2)?,
                model: r.get(3)?,
                tokens: Tokens {
                    input: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    output: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    cache_read: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    cache_write: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    cache_write_1h: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
                },
                reasoning: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
            })
        })?
        .collect()
    }

    /// Claude Code の応答の重複（期間内）。responses: 応答の数（重複を除く）、copies: 写しも数えた行、
    /// cross_file: 2 つ以上のファイルに出た応答、tok_*_raw: 写しも全部足した量、tok_*: 重複を除いた量
    pub fn dup_stats(&self, w: &UsageWindow) -> Result<Value> {
        let sql = "SELECT COUNT(*) AS responses, coalesce(SUM(copies), 0) AS copies, coalesce(SUM(copies > 1), 0) AS cross_file,
                          coalesce(SUM(raw_in), 0) AS tok_in_raw, coalesce(SUM(tok_in), 0) AS tok_in,
                          coalesce(SUM(raw_out), 0) AS tok_out_raw, coalesce(SUM(tok_out), 0) AS tok_out,
                          coalesce(SUM(raw_cr), 0) AS tok_cache_read_raw, coalesce(SUM(tok_cache_read), 0) AS tok_cache_read
                   FROM (SELECT COUNT(*) AS copies,
                                SUM(coalesce(json_extract(usage, '$[0]'), 0)) AS raw_in, MAX(coalesce(json_extract(usage, '$[0]'), 0)) AS tok_in,
                                SUM(coalesce(json_extract(usage, '$[1]'), 0)) AS raw_out, MAX(coalesce(json_extract(usage, '$[1]'), 0)) AS tok_out,
                                SUM(coalesce(json_extract(usage, '$[2]'), 0)) AS raw_cr, MAX(coalesce(json_extract(usage, '$[2]'), 0)) AS tok_cache_read
                         FROM ai_claude_messages WHERE hour >= ?1 AND hour < ?2 AND (?3 = '' OR node_id = ?3) AND (?4 = '' OR ?4 = 'claude')
                         GROUP BY node_id, message_id)";
        self.conn.prepare(sql)?.query_row(rusqlite::params_from_iter(w.args()), row_json)
    }

    /// 1 機体の 1 日（day は集計の day。その日の 0 時の epoch ms）の内訳: どのファイル（セッション）から来たか、
    /// ほかのファイルと重なった応答、重複を除いた量と費用
    pub fn ai_trace_day(&self, node_id: &str, day: i64) -> Result<Value> {
        let (h0, h1) = (day.div_euclid(3_600_000), day.div_euclid(3_600_000) + 24);
        let mut files: Vec<Value> = self
            .conn
            .prepare(
                "SELECT h.file, h.tool, s.session_id, s.parent_id, s.model, s.cwd, s.last_ts,
                        EXISTS (SELECT 1 FROM source_span sp WHERE sp.node_id = h.node_id AND sp.file = h.file AND sp.superseded_by IS NULL) AS tracked,
                        SUM(h.tok_in) AS tok_in, SUM(h.tok_out) AS tok_out, SUM(h.tok_cache_read) AS tok_cache_read, SUM(h.tok_cache_write) AS tok_cache_write,
                        SUM(h.prompts) AS prompts, SUM(h.tool_calls) AS tool_calls,
                        (SELECT count(*) FROM source_span sp WHERE sp.node_id = h.node_id AND sp.file = h.file AND sp.superseded_by IS NULL) AS spans
                 FROM ai_usage_hourly h JOIN ai_sessions s ON s.node_id = h.node_id AND s.file = h.file
                 WHERE h.node_id = ?1 AND h.hour >= ?2 AND h.hour < ?3 GROUP BY h.file ORDER BY SUM(h.tok_in + h.tok_out) DESC, h.file LIMIT 100",
            )?
            .query_map(params![node_id, h0, h1], row_json)?
            .collect::<Result<_>>()?;
        let shared: Vec<Value> = self
            .conn
            .prepare(
                "SELECT a.file, COUNT(*) AS responses, SUM(coalesce(json_extract(a.usage, '$[1]'), 0)) AS tok_out,
                        SUM(EXISTS (SELECT 1 FROM ai_claude_messages b WHERE b.node_id = a.node_id AND b.message_id = a.message_id AND b.file != a.file)) AS shared,
                        SUM(CASE WHEN EXISTS (SELECT 1 FROM ai_claude_messages b WHERE b.node_id = a.node_id AND b.message_id = a.message_id AND b.file != a.file)
                            THEN coalesce(json_extract(a.usage, '$[1]'), 0) ELSE 0 END) AS shared_out
                 FROM ai_claude_messages a WHERE a.node_id = ?1 AND a.hour >= ?2 AND a.hour < ?3 GROUP BY a.file",
            )?
            .query_map(params![node_id, h0, h1], row_json)?
            .collect::<Result<_>>()?;
        let live: HashSet<String> = self.ai_files(node_id)?.keys().cloned().collect();
        let has_cursor = self.conn.prepare("SELECT 1 FROM ai_cursors WHERE node_id = ?")?.exists([node_id])?;
        for f in &mut files {
            let key = js::string(f.get("file"));
            let sh = shared.iter().find(|s| js::is_str(s.get("file"), &key));
            if let Value::Object(m) = f {
                m.insert("tracked".into(), json!(js::truthy(m.get("tracked"))));
                m.insert("gone".into(), json!(has_cursor && !live.contains(&key)));
                m.insert("responses".into(), sh.and_then(|s| s.get("responses").cloned()).unwrap_or(Value::Null));
                m.insert("shared".into(), sh.and_then(|s| s.get("shared").cloned()).unwrap_or(Value::Null));
                m.insert("shared_out".into(), sh.and_then(|s| s.get("shared_out").cloned()).unwrap_or(Value::Null));
            }
        }
        let w = UsageWindow { since_hour: h0, until_hour: h1, tz_ms: 0, node: Some(node_id.into()), tool: None };
        let t = prices::table();
        let mut dedup = CostAcc::default();
        for r in self.usage_rows(&w, true)? {
            let c = t.cost(&r.tool, r.model.as_deref(), &r.tokens);
            dedup.add(&r, c);
        }
        let mut raw = CostAcc::default();
        for r in self.usage_rows(&w, false)? {
            let c = t.cost(&r.tool, r.model.as_deref(), &r.tokens);
            raw.add(&r, c);
        }
        Ok(json!({
            "node_id": node_id, "day": day, "totals": dedup.to_json(&t.version), "raw": raw.to_json(&t.version),
            "dup": self.dup_stats(&w)?, "files": files,
        }))
    }

    /// 1 ファイル（セッション）の出どころ: セッションの要約、区間（新しい順。調査スクリプトの版・取り込み時刻・照合の結果つき）、検算
    pub fn ai_trace_file(&self, node_id: &str, file: &str) -> Result<Value> {
        let session: Option<Value> = self
            .conn
            .prepare(
                "SELECT node_id, file, tool, session_id, parent_id, cwd, version, model, first_ts, last_ts, prompts, assistant_msgs, tool_calls,
                        tok_in, tok_out, tok_cache_read, tok_cache_write, tok_reasoning, tokens_mode, updated_at
                 FROM ai_sessions WHERE node_id = ? AND file = ?",
            )?
            .query_row(params![node_id, file], row_json)
            .optional()?;
        let live: HashSet<String> = self.ai_files(node_id)?.keys().cloned().collect();
        let has_cursor = self.conn.prepare("SELECT 1 FROM ai_cursors WHERE node_id = ?")?.exists([node_id])?;
        let gone = has_cursor && !live.contains(file);
        let mut spans = self.spans_of(node_id, file, 200)?;
        for s in &mut spans {
            let st = span_state(s, gone);
            if let Value::Object(m) = s {
                m.insert("state".into(), json!(st));
            }
        }
        // 検算: 生きている区間が 0 からすき間なくつながっているか、行数・応答の数・出力トークンの合計がセッションと合うか
        let mut alive: Vec<&Value> = spans.iter().filter(|s| s.get("superseded_by").is_none_or(Value::is_null)).collect();
        alive.sort_by_key(|s| s.get("byte_start").and_then(Value::as_i64).unwrap_or(0));
        let mut pos = 0i64;
        let mut gaps: Vec<Value> = Vec::new();
        for s in &alive {
            let (a, b) = (s.get("byte_start").and_then(Value::as_i64).unwrap_or(0), s.get("byte_end").and_then(Value::as_i64).unwrap_or(0));
            if a != pos {
                gaps.push(json!({ "from": pos, "to": a }));
            }
            pos = b;
        }
        let sum = |k: &str| alive.iter().map(|s| s.get(k).and_then(Value::as_i64).unwrap_or(0)).sum::<i64>();
        let mut states: BTreeMap<String, i64> = BTreeMap::new();
        for s in &alive {
            *states.entry(js::string(s.get("state"))).or_insert(0) += 1;
        }
        // 出どころの台帳つきで読んだか（生きている区間がある）。台帳より前に取り込んだだけのセッションは区間が無い
        let tracked = !alive.is_empty();
        let sess_out = session.as_ref().and_then(|s| s.get("tok_out")).and_then(Value::as_i64);
        let checks = json!({
            "contiguous": !alive.is_empty() && gaps.is_empty(), "gaps": gaps, "covered_to": pos,
            "lines": sum("lines"), "used": sum("used"), "skipped": sum("skipped"),
            "responses_recorded": sum("responses"), "responses_now": sum("responses_now"),
            "responses_match": sum("responses") == sum("responses_now"),
            "tok_out_spans": sum("tok_out"), "tok_out_session": sess_out,
            "tok_out_match": tracked && sess_out == Some(sum("tok_out")),
            "states": states,
        });
        Ok(json!({ "node_id": node_id, "file": file, "gone": gone, "tracked": tracked, "session": session, "spans": spans, "checks": checks }))
    }
}

/// 区間の状態: superseded（置き換え済み）・mismatch（不一致）・gone（元ファイルなし。指紋のみ）・verified（検証済み）・unverified（未検証）
pub fn span_state(s: &Value, file_gone: bool) -> &'static str {
    if s.get("superseded_by").is_some_and(|v| !v.is_null()) {
        return "superseded";
    }
    match s.get("verify_state").and_then(Value::as_str) {
        Some("mismatch") => "mismatch",
        Some("gone") => "gone",
        _ if file_gone => "gone",
        Some("ok") => "verified",
        _ => "unverified",
    }
}

/// 使用量の行から、全体・機体×日・モデルごとの量と費用を作る（単価表の版つき）
pub fn summarize_usage(rows: &[UsageRow]) -> (Value, BTreeMap<(String, i64), Value>, Vec<Value>) {
    let t = prices::table();
    let mut total = CostAcc::default();
    let mut daily: BTreeMap<(String, i64), CostAcc> = BTreeMap::new();
    let mut models: BTreeMap<(String, Option<String>), CostAcc> = BTreeMap::new();
    for r in rows {
        let c = t.cost(&r.tool, r.model.as_deref(), &r.tokens);
        total.add(r, c);
        daily.entry((r.node_id.clone(), r.day)).or_default().add(r, c);
        models.entry((r.tool.clone(), r.model.clone())).or_default().add(r, c);
    }
    let daily = daily.into_iter().map(|(k, v)| (k, v.to_json(&t.version))).collect();
    let mut ms: Vec<Value> = models
        .into_iter()
        .map(|((tool, model), v)| {
            let mut o = v.to_json(&t.version);
            if let Value::Object(m) = &mut o {
                m.insert("tool".into(), json!(tool));
                m.insert("price".into(), t.price_json(model.as_deref()));
                m.insert("model".into(), json!(model));
            }
            o
        })
        .collect();
    let io = |v: &Value| v.get("tok_in").and_then(Value::as_i64).unwrap_or(0) + v.get("tok_out").and_then(Value::as_i64).unwrap_or(0);
    ms.sort_by_key(|v| std::cmp::Reverse(io(v)));
    (total.to_json(&t.version), daily, ms)
}

/// 重複を除く前（raw）と後の差（画面の「除いた分」）
pub fn dup_delta(raw: &Value, dedup: &Value) -> Value {
    let mut m = Map::new();
    for k in ["tok_in", "tok_out", "tok_cache_read", "tok_cache_write"] {
        let a = raw.get(k).and_then(Value::as_i64).unwrap_or(0);
        let b = dedup.get(k).and_then(Value::as_i64).unwrap_or(0);
        m.insert(k.into(), json!(a - b));
        m.insert(format!("{k}_pct"), if a > 0 { json!(((a - b) as f64 / a as f64 * 10_000.0).round() / 100.0) } else { Value::Null });
    }
    let (a, b) = (raw.get("cost_usd").and_then(Value::as_f64), dedup.get("cost_usd").and_then(Value::as_f64));
    m.insert(
        "cost_usd".into(),
        match (a, b) {
            (Some(a), Some(b)) => json!(((a - b) * 10_000.0).round() / 10_000.0),
            _ => Value::Null,
        },
    );
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_sessions::run_meta;
    use crate::db::sha256_hex;

    const H: i64 = 1_800_000_000_000 / 3_600_000;
    const T: i64 = H * 3_600_000;

    /// Claude Code の 1 ファイル分の調査の出力（セッションと区間）。トークンは応答（claude_usage）から入る
    fn rec(file: &str, mode: &str, start: i64, end: i64, model: &str) -> Value {
        json!({
            "tool": "claude", "file": file, "mode": mode, "session_id": file, "model": model, "first_ts": T, "last_ts": T + 1000,
            "tokens": {}, "tokens_mode": "add", "hours": {}, "tool_counts": {}, "tool_error_counts": {},
            "span": { "start": start, "end": end, "sha256": sha256_hex(format!("{file}:{start}:{end}").as_bytes()), "lines": 4, "used": 3, "skipped": 1,
                      "anchor": [end.min(64), sha256_hex(format!("{file}@{end}").as_bytes())] },
        })
    }

    /// 応答（file, model, [(応答 ID, 出力トークン)]）。入力 1・キャッシュ読み 100・キャッシュ書き 10（うち 1 時間 4）
    fn usage(file: &str, model: &str, resp: &[(&str, i64)]) -> Vec<Value> {
        resp.iter().map(|(id, o)| json!({ "file": file, "id": id, "usage": [1, o, 100, 10, 0], "hour": H, "cw1h": 4, "model": model })).collect()
    }

    fn ingest(db: &Store, sessions: Vec<Value>, usage: Vec<Value>, cursors: Value) -> crate::db::AiIngested {
        let out = json!({ "sessions": sessions, "claude_usage": usage, "cursors": cursors, "gone": [], "truncated": false, "errors": [] });
        db.ai_ingest_run("n1", &out, &run_meta("n1", 1, 2), 10).unwrap()
    }

    fn summary(db: &Store) -> Value {
        db.ai_summary(&json!({ "days": 7, "tz": 0 }), T + 3_600_000).unwrap()
    }

    #[test]
    fn duplicates_across_files_are_counted_once_and_costed() {
        let db = Store::open_in_memory().unwrap();
        let mut u = usage("claude:p/a.jsonl", "claude-opus-5-5", &[("1", 10), ("2", 20)]);
        // サブエージェントのファイルに同じ応答（2）が出る
        u.extend(usage("claude:p/a/subagents/x.jsonl", "claude-opus-5-5", &[("2", 20), ("3", 5)]));
        u.extend(usage("claude:p/b.jsonl", "mystery-model", &[("4", 7)]));
        let r = ingest(
            &db,
            vec![
                rec("claude:p/a.jsonl", "replace", 0, 100, "claude-opus-5-5"),
                rec("claude:p/a/subagents/x.jsonl", "replace", 0, 50, "claude-opus-5-5"),
                rec("claude:p/b.jsonl", "replace", 0, 10, "mystery-model"),
            ],
            u,
            json!({ "claude:p/a.jsonl": 100, "claude:p/a/subagents/x.jsonl": 50, "claude:p/b.jsonl": 10 }),
        );
        assert_eq!((r.spans, r.responses, r.rejected), (3, 5, 0));
        let s = summary(&db);
        assert_eq!(s["totals"]["tok_out"], json!(42), "応答 2 は 1 回だけ数える");
        assert_eq!(s["raw"]["tok_out"], json!(62));
        assert_eq!((s["dup"]["responses"].clone(), s["dup"]["cross_file"].clone(), s["dup"]["removed"]["tok_out"].clone()), (json!(4), json!(1), json!(20)));
        // 費用: 単価のわかるモデルだけ足し、わからないモデルは「単価不明」（0 円にしない）
        let p = crate::prices::table().lookup("claude-opus-5-5").unwrap().clone();
        // 応答 1・2・3: 入力 1・出力・キャッシュ読み 100・キャッシュ書き 10（うち 1 時間 4）
        let want = (3.0 * p.input + 35.0 * p.output + 300.0 * p.cache_read + 18.0 * p.cache_write_5m.unwrap() + 12.0 * p.cache_write_1h.unwrap()) / 1e6;
        assert!((s["totals"]["cost_usd"].as_f64().unwrap() - want).abs() < 1e-4, "{} vs {want}", s["totals"]["cost_usd"]);
        assert_eq!(s["totals"]["unpriced"]["tok_out"], json!(7));
        assert_eq!(s["totals"]["unpriced_models"], json!(["claude:mystery-model"]));
        assert_eq!(s["totals"]["prices_version"], json!(crate::prices::table().version));
        let m = s["models"].as_array().unwrap();
        let unknown = m.iter().find(|x| x["model"] == "mystery-model").unwrap();
        assert_eq!((unknown["cost_usd"].clone(), unknown["price"].clone()), (Value::Null, Value::Null));
        assert_eq!(s["daily"][0]["tok_out"], json!(42));
        assert!(s["daily"][0]["cost_usd"].is_number());
        // 出どころ: その日の内訳と、1 ファイルの検算
        let d = db.ai_trace_day("n1", (T / 86_400_000) * 86_400_000).unwrap();
        let a = d["files"].as_array().unwrap().iter().find(|f| f["file"] == "claude:p/a.jsonl").unwrap().clone();
        assert_eq!((a["responses"].clone(), a["shared"].clone(), a["shared_out"].clone(), a["spans"].clone()), (json!(2), json!(1), json!(20), json!(1)));
        assert_eq!(d["totals"]["tok_out"], json!(42));
        let f = db.ai_trace_file("n1", "claude:p/a.jsonl").unwrap();
        let c = &f["checks"];
        assert_eq!(
            (c["contiguous"].clone(), c["responses_match"].clone(), c["tok_out_match"].clone(), c["skipped"].clone()),
            (json!(true), json!(true), json!(true), json!(1))
        );
        assert_eq!(f["spans"][0]["state"], json!("unverified"));
        assert_eq!(f["spans"][0]["probe"], json!(crate::ai_sessions::AI_PROBE_NAME));
        assert_eq!(db.verify_chain().unwrap()["ok"], json!(true));
    }

    #[test]
    fn continuation_must_start_at_the_previous_end() {
        let db = Store::open_in_memory().unwrap();
        let a = "claude:p/a.jsonl";
        ingest(&db, vec![rec(a, "replace", 0, 100, "claude-opus-5-5")], usage(a, "claude-opus-5-5", &[("1", 10)]), json!({ a: 100 }));
        // 続きの位置と手前の指紋を渡す
        let st = db.ai_state("n1").unwrap();
        assert_eq!(st["files"][a], json!(100));
        assert_eq!(st["anchors"][a][1], json!(sha256_hex(b"claude:p/a.jsonl@100")));
        // 続き（100〜150）は受け付ける
        let r = ingest(&db, vec![rec(a, "add", 100, 150, "claude-opus-5-5")], usage(a, "claude-opus-5-5", &[("2", 5)]), json!({ a: 150 }));
        assert_eq!((r.spans, r.rejected), (1, 0));
        // 前回の終了（150）から始まらない続きは拒否し、次の回に最初から読み直す
        let r = ingest(&db, vec![rec(a, "add", 120, 200, "claude-opus-5-5")], usage(a, "claude-opus-5-5", &[("3", 99)]), json!({ a: 200 }));
        assert_eq!((r.spans, r.rejected), (0, 1));
        assert_eq!(db.ai_session("n1", a).unwrap().unwrap().tokens[1], 15, "拒否した分は足さない");
        assert_eq!(db.ai_state("n1").unwrap()["files"][a], json!(0));
        assert!(db.ai_pending(&["n1".into()]).unwrap(), "読み直しを急ぐ");
        let notes: Value =
            serde_json::from_str(&db.conn().query_row("SELECT notes FROM ingest_run ORDER BY id DESC LIMIT 1", [], |r| r.get::<_, String>(0)).unwrap())
                .unwrap();
        assert!(js::string(notes["rejected"][0].get("reason")).contains("前回の終了 150"));
        // 最初から読み直す: 前の区間は置き換え済み（履歴に残る）、応答は新しい区間のものだけ
        let r =
            ingest(&db, vec![rec(a, "replace", 0, 200, "claude-opus-5-5")], usage(a, "claude-opus-5-5", &[("1", 10), ("2", 5), ("3", 99)]), json!({ a: 200 }));
        assert_eq!((r.spans, r.rejected), (1, 0));
        let f = db.ai_trace_file("n1", a).unwrap();
        let states: Vec<String> = f["spans"].as_array().unwrap().iter().map(|s| js::string(s.get("state"))).collect();
        assert_eq!(states, ["unverified", "superseded", "superseded"]);
        assert_eq!(summary(&db)["totals"]["tok_out"], json!(114));
        assert_eq!(db.conn().query_row("SELECT count(*) FROM ai_claude_messages", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        assert!(f["tracked"].as_bool().unwrap());
        assert_eq!(db.verify_chain().unwrap()["ok"], json!(true));
    }

    #[test]
    fn legacy_rows_without_spans_still_count() {
        let db = Store::open_in_memory().unwrap();
        let mut x = rec("claude:p/old.jsonl", "replace", 0, 10, "claude-opus-5-5");
        x.as_object_mut().unwrap().remove("span");
        let r = ingest(&db, vec![x], usage("claude:p/old.jsonl", "claude-opus-5-5", &[("9", 8)]), json!({ "claude:p/old.jsonl": 10 }));
        assert_eq!((r.spans, r.responses), (0, 0), "区間の無い応答は台帳の数に入れない");
        assert_eq!(db.conn().query_row("SELECT count(*) FROM ai_claude_messages WHERE span_id IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert!(!db.ai_trace_file("n1", "claude:p/old.jsonl").unwrap()["tracked"].as_bool().unwrap());
        // 台帳の無いセッションも、応答ごとの量をそのまま数える
        assert_eq!(summary(&db)["totals"]["tok_out"], json!(8));
        // 区間の無い続きの位置は渡さない（最初から読み直す）
        assert_eq!(db.ai_state("n1").unwrap()["files"]["claude:p/old.jsonl"], json!(0));
    }

    #[test]
    fn claude_sessions_without_message_rows_use_hourly_and_gone_files_keep_dedup() {
        let db = Store::open_in_memory().unwrap();
        let (a, b) = ("claude:p/a.jsonl", "claude:p/gone.jsonl");
        // 応答ごとの量が無いセッション（時間ごとの量だけ）はそのまま数える
        let mut hourly_only = rec("claude:p/h.jsonl", "replace", 0, 10, "claude-opus-5-5");
        hourly_only["tokens"] = json!({ "in": 1, "out": 3, "cache_read": 0, "cache_write": 0, "reasoning": 0 });
        hourly_only["hours"] = json!({ (H.to_string()): [1, 3, 0, 0, 0, 0, 1] });
        let mut u = usage(a, "claude-opus-5-5", &[("1", 10)]);
        u.extend(usage(b, "claude-opus-5-5", &[("1", 10), ("2", 4)]));
        ingest(
            &db,
            vec![rec(a, "replace", 0, 10, "claude-opus-5-5"), rec(b, "replace", 0, 10, "claude-opus-5-5"), hourly_only],
            u,
            json!({ a: 10, b: 10, "claude:p/h.jsonl": 10 }),
        );
        assert_eq!(summary(&db)["totals"]["tok_out"], json!(10 + 4 + 3), "応答 1 は 1 回、時間ごとのだけのセッションは 3");
        // 元のファイルが消えても、応答ごとの量は残り、重複除去も変わらない
        let out = json!({ "sessions": [], "cursors": {}, "gone": [b], "truncated": false, "errors": [] });
        db.ai_ingest_run("n1", &out, &run_meta("n1", 3, 4), 11).unwrap();
        assert_eq!(summary(&db)["totals"]["tok_out"], json!(17));
        assert_eq!(db.ai_trace_file("n1", b).unwrap()["gone"], json!(true));
    }

    #[test]
    fn fast_and_geo_variants_are_unpriced_not_zero() {
        let db = Store::open_in_memory().unwrap();
        let a = "claude:p/a.jsonl";
        ingest(&db, vec![rec(a, "replace", 0, 10, "claude-opus-5-5@fast")], usage(a, "claude-opus-5-5@fast", &[("1", 10)]), json!({ a: 10 }));
        let s = summary(&db);
        assert_eq!((s["totals"]["cost_usd"].clone(), s["totals"]["unpriced_models"].clone()), (Value::Null, json!(["claude:claude-opus-5-5@fast"])));
    }

    #[test]
    fn span_states() {
        let s = |v: Value| span_state(&v, false);
        assert_eq!(s(json!({ "superseded_by": "r", "verify_state": "ok" })), "superseded");
        assert_eq!(s(json!({ "verify_state": "mismatch" })), "mismatch");
        assert_eq!(s(json!({ "verify_state": "ok" })), "verified");
        assert_eq!(span_state(&json!({ "verify_state": "ok" }), true), "gone", "元ファイルが消えたら指紋だけ");
        assert_eq!(s(json!({})), "unverified");
    }
}
