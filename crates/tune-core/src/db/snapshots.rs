//! 分析結果（snapshot）と、その推移（履歴）

use rusqlite::params;
use serde_json::Value;

use super::{Result, SNAPSHOT_KEEP, Store, row_json, sql};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tune_snapshots (
  node_id TEXT NOT NULL, at INTEGER NOT NULL, wall_s REAL, score INTEGER,
  bench_ms REAL, cpu REAL, mem_avail REAL, swap_gb REAL, critical INTEGER, warn INTEGER,
  findings TEXT, data TEXT NOT NULL,
  PRIMARY KEY (node_id, at)
) WITHOUT ROWID;
";

/// 保存した分析結果（data は probe の出力そのまま）
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub node_id: String,
    pub at: i64,
    pub wall_s: Option<f64>,
    pub data: Value,
}

impl Store {
    /// 最新の失敗だけを保存する（成功 snapshot はそのまま残す）。JS 版と同じ meta キー
    pub fn save_probe_failure(&self, node_id: &str, at: i64, error: &str) -> Result<()> {
        let key = format!("probeFailure:{node_id}");
        if self.get_meta(&key)?.and_then(|v| v["at"].as_i64()).is_some_and(|old| old > at) {
            return Ok(());
        }
        self.set_meta(&key, &serde_json::json!({ "at": at, "error": error }))
    }

    /// 成功より新しい失敗があれば返す。再起動・互換版でも同じ判定にする
    pub fn probe_error(&self, node_id: &str, last_ok_at: Option<i64>) -> Result<Option<String>> {
        let failed = self.get_meta(&format!("probeFailure:{node_id}"))?;
        Ok(failed
            .filter(|v| v["at"].as_i64().is_some_and(|at| last_ok_at.is_none_or(|ok| at >= ok)))
            .and_then(|v| v["error"].as_str().filter(|s| !s.is_empty()).map(str::to_string)))
    }

    /// summary = main.js の summarize（bench_ms, cpu, mem_avail, swap_gb, critical, warn）
    #[allow(clippy::too_many_arguments)] // lib/db.js の addSnapshot と同じ並び
    pub fn add_snapshot(&self, node_id: &str, at: i64, wall_s: Option<f64>, score: Option<i64>, findings: &Value, data: &Value, summary: &Value) -> Result<()> {
        let s = |k: &str| sql(summary.get(k));
        self.tx(|c| {
            c.execute(
                "INSERT OR REPLACE INTO tune_snapshots (node_id, at, wall_s, score, bench_ms, cpu, mem_avail, swap_gb, critical, warn, findings, data)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![node_id, at, wall_s, score, s("bench_ms"), s("cpu"), s("mem_avail"), s("swap_gb"), s("critical"), s("warn"), findings.to_string(), data.to_string()],
            )?;
            c.execute(
                &format!("DELETE FROM tune_snapshots WHERE node_id = ? AND at NOT IN (SELECT at FROM tune_snapshots WHERE node_id = ? ORDER BY at DESC LIMIT {SNAPSHOT_KEEP})"),
                params![node_id, node_id],
            )?;
            Ok(())
        })
    }

    /// 新しい順に n 件
    pub fn last_snapshots(&self, node_id: &str, n: i64) -> Result<Vec<Snapshot>> {
        let mut st = self.conn.prepare_cached("SELECT node_id, at, wall_s, data FROM tune_snapshots WHERE node_id = ? ORDER BY at DESC LIMIT ?")?;
        let rows = st.query_map(params![node_id, n], |r| {
            let data: String = r.get(3)?;
            Ok(Snapshot {
                node_id: r.get(0)?,
                at: r.get(1)?,
                wall_s: r.get(2)?,
                data: serde_json::from_str(&data).map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e)))?,
            })
        })?;
        rows.collect()
    }

    /// 推移（古い順）。グラフは画面でこの点を描くだけ
    pub fn history(&self, node_id: &str, n: i64) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached(
            "SELECT at, score, bench_ms, cpu, mem_avail, swap_gb, critical, warn, json(COALESCE(json_extract(data, '$.gpus'), '[]')) AS gpus, json_extract(data, '$.processes.count') AS process_count, json_extract(data, '$.processes.threads') AS thread_count, json_extract(data, '$.processes.handles') AS handle_count FROM tune_snapshots WHERE node_id = ? ORDER BY at DESC LIMIT ?",
        )?;
        let mut rows = st.query_map(params![node_id, n], row_json)?.collect::<Result<Vec<_>>>()?;
        for row in &mut rows {
            row["gpus"] = row["gpus"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or_else(|| serde_json::json!([]));
        }
        rows.reverse();
        Ok(rows)
    }
}
