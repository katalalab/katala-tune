//! ログ: (node_id, source, uid) で一意。同じものを何度取り込んでも1行。全文検索（FTS5）と同種ログの件数

use rusqlite::params;
use rusqlite::types::Value as SqlValue;
use serde_json::Value;

use super::{QUERY_LIMIT_MAX, Result, SIGNATURE_LIMIT_MAX, Store, now_ms, row_json, sql};
use crate::js;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS logs (
  id INTEGER PRIMARY KEY, node_id TEXT NOT NULL, source TEXT NOT NULL, uid TEXT NOT NULL,
  ts INTEGER NOT NULL, level TEXT NOT NULL, provider TEXT, event_id TEXT, message TEXT NOT NULL,
  fingerprint TEXT NOT NULL, ingested_at INTEGER NOT NULL,
  UNIQUE (node_id, source, uid)
);
CREATE INDEX IF NOT EXISTS logs_ts ON logs (ts DESC);
CREATE INDEX IF NOT EXISTS logs_node_ts ON logs (node_id, ts DESC);
CREATE INDEX IF NOT EXISTS logs_fp ON logs (fingerprint, ts DESC);
CREATE VIRTUAL TABLE IF NOT EXISTS logs_fts USING fts5 (message, provider, content='logs', content_rowid='id');
CREATE TABLE IF NOT EXISTS log_signatures (
  fingerprint TEXT PRIMARY KEY, source TEXT, provider TEXT, level TEXT, sample TEXT,
  first_seen INTEGER, last_seen INTEGER, total INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS log_cursors (
  node_id TEXT NOT NULL, source TEXT NOT NULL, cursor TEXT, updated_at INTEGER,
  last_ok_at INTEGER, last_error TEXT, last_count INTEGER, dropped_total INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (node_id, source)
) WITHOUT ROWID;
";

/// 正規化済みのログ1件（lib/logs.js の normalize の出力）
#[derive(Clone, Debug, PartialEq)]
pub struct LogRow {
    pub uid: String,
    /// JS の Number(r.ts)。整数なら INTEGER として入る
    pub ts: f64,
    pub level: String,
    pub provider: Option<String>,
    pub event_id: Option<String>,
    pub message: String,
    pub fingerprint: String,
}

/// 機体ごとの集計（所見づくり用）
#[derive(Clone, Debug)]
pub struct LogCount {
    pub source: String,
    pub provider: Option<String>,
    pub event_id: Option<String>,
    pub level: String,
    pub n: i64,
}

/// 期間内で最も多い同種ログ
#[derive(Clone, Debug)]
pub struct TopSignature {
    pub fingerprint: String,
    pub source: String,
    pub provider: Option<String>,
    pub n: i64,
    pub sample: String,
}

fn ts_sql(ts: f64) -> SqlValue {
    sql(Some(&js::jnum(ts)))
}

/// 画面からの絞り込みの数値（JS の比較と同じく、文字列の数も数として扱う）
fn filter_num(f: &Value, k: &str) -> Option<f64> {
    let v = f.get(k);
    if !js::truthy(v) {
        return None;
    }
    let x = js::num(v);
    x.is_finite().then_some(x)
}

fn limit_of(f: &Value, default: i64, max: i64) -> i64 {
    match f.get("limit") {
        None => default,
        v => {
            let x = js::min(&[js::num(v), max as f64]);
            if x.is_nan() { default } else { x.max(0.0) as i64 }
        }
    }
}

fn offset_of(f: &Value) -> i64 {
    filter_num(f, "offset").map(|x| x.max(0.0) as i64).unwrap_or(0)
}

fn filter_str(f: &Value, k: &str) -> Option<String> {
    let v = f.get(k);
    js::truthy(v).then(|| js::string(v))
}

impl Store {
    /// 正規化済みのログを入れる。新しく入った件数を返す（再送分は数えない）
    pub fn insert_logs(&self, node_id: &str, source: &str, rows: &[LogRow], now: i64) -> Result<usize> {
        self.tx(|c| {
            let mut ins = c.prepare_cached(
                "INSERT OR IGNORE INTO logs (node_id, source, uid, ts, level, provider, event_id, message, fingerprint, ingested_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?;
            let mut fts = c.prepare_cached("INSERT INTO logs_fts (rowid, message, provider) VALUES (?, ?, ?)")?;
            let mut sig = c.prepare_cached(
                "INSERT INTO log_signatures (fingerprint, source, provider, level, sample, first_seen, last_seen, total) VALUES (?, ?, ?, ?, ?, ?, ?, 1)
                 ON CONFLICT (fingerprint) DO UPDATE SET total = total + 1, last_seen = max(last_seen, excluded.last_seen), first_seen = min(first_seen, excluded.first_seen)",
            )?;
            let mut inserted = 0;
            for r in rows {
                let n = ins.execute(params![node_id, source, r.uid, ts_sql(r.ts), r.level, r.provider, r.event_id, r.message, r.fingerprint, now])?;
                if n > 0 {
                    inserted += 1;
                    fts.execute(params![c.last_insert_rowid(), r.message, r.provider.clone().unwrap_or_default()])?;
                    sig.execute(params![r.fingerprint, source, r.provider, r.level, js::slice16(&r.message, 300), ts_sql(r.ts), ts_sql(r.ts)])?;
                }
            }
            Ok(inserted)
        })
    }

    pub fn cursor(&self, node_id: &str, source: &str) -> Result<Option<Value>> {
        let mut st = self.conn.prepare_cached("SELECT * FROM log_cursors WHERE node_id = ? AND source = ?")?;
        let mut rows = st.query_map(params![node_id, source], row_json)?;
        rows.next().transpose()
    }

    pub fn cursors(&self) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached("SELECT * FROM log_cursors ORDER BY node_id, source")?;
        st.query_map([], row_json)?.collect()
    }

    /// 取り込みの成功。位置・件数を記録し、捨てた数を積み上げる（dropped は `src.dropped || 0`）
    pub fn cursor_ok(&self, node_id: &str, source: &str, cursor: Option<&Value>, count: i64, dropped: &Value) -> Result<()> {
        let now = now_ms();
        let dropped = if js::truthy(Some(dropped)) { sql(Some(dropped)) } else { SqlValue::Integer(0) };
        self.conn.execute(
            "INSERT INTO log_cursors (node_id, source, cursor, updated_at, last_ok_at, last_error, last_count, dropped_total) VALUES (?, ?, ?, ?, ?, NULL, ?, ?)
             ON CONFLICT (node_id, source) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at, last_ok_at = excluded.last_ok_at,
             last_error = NULL, last_count = excluded.last_count, dropped_total = log_cursors.dropped_total + excluded.dropped_total",
            params![node_id, source, sql(cursor), now, now, count, dropped],
        )?;
        Ok(())
    }

    /// 取り込みの失敗。位置は保ったまま理由を残す
    pub fn cursor_error(&self, node_id: &str, source: &str, err: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO log_cursors (node_id, source, updated_at, last_error) VALUES (?, ?, ?, ?)
             ON CONFLICT (node_id, source) DO UPDATE SET updated_at = excluded.updated_at, last_error = excluded.last_error",
            params![node_id, source, now_ms(), js::slice16(err, 500)],
        )?;
        Ok(())
    }

    /// 全文検索と絞り込み。filter = { node_id, level, source, q, since, limit = 300, offset = 0 }。上限 2000 件
    pub fn query_logs(&self, f: &Value) -> Result<Vec<Value>> {
        let mut wh = Vec::new();
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(q) = filter_str(f, "q") {
            wh.push("l.id IN (SELECT rowid FROM logs_fts WHERE logs_fts MATCH ?)");
            args.push(SqlValue::Text(q));
        }
        for (k, cond) in [("node_id", "l.node_id = ?"), ("level", "l.level = ?"), ("source", "l.source = ?")] {
            if let Some(v) = filter_str(f, k) {
                wh.push(cond);
                args.push(SqlValue::Text(v));
            }
        }
        if let Some(since) = filter_num(f, "since") {
            wh.push("l.ts >= ?");
            args.push(sql(Some(&js::jnum(since))));
        }
        args.push(SqlValue::Integer(limit_of(f, 300, QUERY_LIMIT_MAX)));
        args.push(SqlValue::Integer(offset_of(f)));
        let sql_text = format!(
            "SELECT l.id, l.node_id, l.source, l.ts, l.level, l.provider, l.event_id, l.message, l.fingerprint FROM logs l {} ORDER BY l.ts DESC LIMIT ? OFFSET ?",
            if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) }
        );
        let mut st = self.conn.prepare(&sql_text)?;
        st.query_map(rusqlite::params_from_iter(args), row_json)?.collect()
    }

    /// 同種ログの上位。期間内の件数と、何台で出ているか。filter = { since, node_id, limit = 50, offset = 0 }
    pub fn signatures(&self, f: &Value) -> Result<Vec<Value>> {
        let since = js::nullish(f.get("since"), None).map(|v| sql(Some(&js::jnum(js::num(Some(v)))))).unwrap_or(SqlValue::Integer(0));
        let mut args = vec![since];
        let mut node_cond = "";
        if let Some(n) = filter_str(f, "node_id") {
            node_cond = "AND l.node_id = ?";
            args.push(SqlValue::Text(n));
        }
        args.push(SqlValue::Integer(limit_of(f, 50, SIGNATURE_LIMIT_MAX)));
        args.push(SqlValue::Integer(offset_of(f)));
        let mut st = self.conn.prepare(&format!(
            "SELECT l.fingerprint, s.level, s.provider, s.source, s.sample, count(*) AS n, count(DISTINCT l.node_id) AS nodes,
               group_concat(DISTINCT l.node_id) AS node_ids, max(l.ts) AS last_ts, s.total
             FROM logs l JOIN log_signatures s USING (fingerprint)
             WHERE l.ts >= ? {node_cond} GROUP BY l.fingerprint ORDER BY n DESC LIMIT ? OFFSET ?"
        ))?;
        st.query_map(rusqlite::params_from_iter(args), row_json)?.collect()
    }

    /// 機体ごとの集計（所見づくり用）。source × provider × event_id × level の件数
    pub fn log_counts(&self, node_id: &str, since: i64) -> Result<Vec<LogCount>> {
        let mut st = self.conn.prepare_cached(
            "SELECT source, provider, event_id, level, count(*) AS n, max(ts) AS last_ts FROM logs WHERE node_id = ? AND ts >= ? GROUP BY source, provider, event_id, level",
        )?;
        st.query_map(params![node_id, since], |r| Ok(LogCount { source: r.get(0)?, provider: r.get(1)?, event_id: r.get(2)?, level: r.get(3)?, n: r.get(4)? }))?
            .collect()
    }

    /// 期間内で最も多い同種ログ（洪水の検出用）
    pub fn top_signatures(&self, node_id: &str, since: i64, limit: i64) -> Result<Vec<TopSignature>> {
        let mut st = self.conn.prepare_cached(
            "SELECT l.fingerprint, l.source, l.provider, l.level, count(*) AS n, max(l.message) AS sample FROM logs l
             WHERE l.node_id = ? AND l.ts >= ? AND l.level != 'info' AND l.source NOT IN ('mac_auth', 'win_security')
             GROUP BY l.fingerprint ORDER BY n DESC LIMIT ?",
        )?;
        st.query_map(params![node_id, since, limit], |r| {
            Ok(TopSignature { fingerprint: r.get(0)?, source: r.get(1)?, provider: r.get(2)?, n: r.get(4)?, sample: r.get(5)? })
        })?
        .collect()
    }

    /// 取り込みで捨てた件数（洪水で上限を超えた量）
    pub fn dropped_total(&self, node_id: &str) -> Result<i64> {
        self.conn.query_row("SELECT coalesce(sum(dropped_total), 0) AS n FROM log_cursors WHERE node_id = ?", [node_id], |r| r.get(0))
    }
}
