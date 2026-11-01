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

/// Minimal fields needed to show a saved machine observation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineReading {
    pub last_at: Option<i64>,
    pub score: Option<i64>,
    pub critical: Option<u64>,
    pub warnings: Option<u64>,
    pub failure: ProbeFailure,
    pub schedule: StoredSchedule,
}

/// A missing record and a malformed record must remain distinct.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProbeFailure {
    #[default]
    None,
    At(i64),
    Invalid,
}

/// A missing schedule differs from malformed or unreadable stored metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StoredSchedule {
    #[default]
    Missing,
    Minutes(i64),
    Invalid,
}

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

impl Store {
    /// One bounded read of the requested ID's display fields and fixed metadata.
    /// Snapshot bodies, history details and failure text never leave SQLite.
    pub fn saved_machine_reading(&self, node_id: &str) -> Result<MachineReading> {
        type SavedMachineRow = (Option<i64>, Option<i64>, Option<i64>, Option<i64>, bool, Option<i64>, i64, Option<i64>);
        let row: SavedMachineRow = self.conn.query_row(
            "SELECT s.at, s.score, s.critical, s.warn, f.k IS NOT NULL,
                    CASE WHEN f.k IS NULL THEN NULL
                         WHEN typeof(f.v) != 'text' OR octet_length(f.v) > 8192 THEN NULL
                         WHEN NOT json_valid(f.v) THEN NULL
                         WHEN json_type(f.v) != 'object' THEN NULL
                         WHEN (SELECT count(*) FROM json_each(f.v) WHERE key = 'at') != 1 THEN NULL
                         WHEN (SELECT count(*) FROM json_each(f.v) WHERE key = 'error') != 1 THEN NULL
                         WHEN json_type(f.v, '$.at') IS NOT 'integer' THEN NULL
                         WHEN json_type(f.v, '$.error') IS NOT 'text' THEN NULL
                         ELSE json_extract(f.v, '$.at') END,
                    CASE WHEN q.k IS NULL THEN 0
                         WHEN typeof(q.v) != 'text' OR octet_length(q.v) > 8192 THEN 2
                         WHEN NOT json_valid(q.v) THEN 2
                         WHEN json_type(q.v) != 'object' THEN 2
                         WHEN (SELECT count(*) FROM json_each(q.v) WHERE key = 'probe_minutes') > 1 THEN 2
                         WHEN json_type(q.v, '$.probe_minutes') IS NULL THEN 0
                         WHEN json_type(q.v, '$.probe_minutes') = 'integer' THEN 1
                         ELSE 2 END,
                    CASE WHEN q.k IS NULL THEN NULL
                         WHEN typeof(q.v) != 'text' OR octet_length(q.v) > 8192 THEN NULL
                         WHEN NOT json_valid(q.v) THEN NULL
                         WHEN json_type(q.v) != 'object' THEN NULL
                         WHEN (SELECT count(*) FROM json_each(q.v) WHERE key = 'probe_minutes') > 1 THEN NULL
                         WHEN json_type(q.v, '$.probe_minutes') != 'integer' THEN NULL
                         ELSE json_extract(q.v, '$.probe_minutes') END
                 FROM (SELECT 1) AS seed
                 LEFT JOIN (
                     SELECT at, score, critical, warn FROM tune_snapshots
                     WHERE node_id = ?1 ORDER BY at DESC LIMIT 1
                 ) AS s ON 1
                 LEFT JOIN meta AS f ON f.k = ?2
                 LEFT JOIN meta AS q ON q.k = 'schedule'",
            params![node_id, format!("probeFailure:{node_id}")],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
        )?;
        let count = |value: Option<i64>, column| -> Result<Option<u64>> {
            value.map(|value| u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))).transpose()
        };
        Ok(MachineReading {
            last_at: row.0,
            score: row.1,
            critical: count(row.2, 2)?,
            warnings: count(row.3, 3)?,
            failure: match (row.4, row.5) {
                (false, _) => ProbeFailure::None,
                (true, Some(at)) => ProbeFailure::At(at),
                (true, None) => ProbeFailure::Invalid,
            },
            schedule: match (row.6, row.7) {
                (0, _) => StoredSchedule::Missing,
                (1, Some(minutes)) => StoredSchedule::Minutes(minutes),
                _ => StoredSchedule::Invalid,
            },
        })
    }
}

#[cfg(test)]
mod scoped_observations_tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture_store() -> Store {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch("CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT, updated_at INTEGER)").unwrap();
        Store { conn }
    }

    #[test]
    fn returns_only_the_selected_summary_without_parsing_snapshot_body() {
        let store = fixture_store();
        store
            .conn
            .execute(
                "INSERT INTO tune_snapshots (node_id, at, score, critical, warn, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params!["fixture-one", 9000, 70, 1, 2, "{invalid synthetic json"],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO tune_snapshots (node_id, at, score, critical, warn, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params!["fixture-two", 9500, "not-an-integer", 1, 2, "{invalid synthetic json"],
            )
            .unwrap();
        let reading = store.saved_machine_reading("fixture-one").unwrap();
        assert_eq!((reading.last_at, reading.score, reading.critical, reading.warnings), (Some(9000), Some(70), Some(1), Some(2)));
        assert_eq!(reading.failure, ProbeFailure::None);
    }

    #[test]
    fn distinguishes_missing_and_invalid_failure_metadata() {
        let store = fixture_store();
        for (key, value) in [
            ("probeFailure:fixture-a", "not-json"),
            ("probeFailure:fixture-b", "{\"at\":\"invalid\",\"error\":\"fixture\"}"),
            ("probeFailure:fixture-c", "{\"at\":9000,\"error\":null}"),
            ("probeFailure:fixture-d", "{\"at\":9000,\"error\":\"fixture\"}"),
            ("probeFailure:fixture-e", "{\"at\":9000}"),
        ] {
            store.conn.execute("INSERT INTO meta (k, v, updated_at) VALUES (?1, ?2, 1)", params![key, value]).unwrap();
        }
        assert_eq!(store.saved_machine_reading("fixture-missing").unwrap().failure, ProbeFailure::None);
        for id in ["fixture-a", "fixture-b", "fixture-c", "fixture-e"] {
            assert_eq!(store.saved_machine_reading(id).unwrap().failure, ProbeFailure::Invalid);
        }
        assert_eq!(store.saved_machine_reading("fixture-d").unwrap().failure, ProbeFailure::At(9000));
    }

    #[test]
    fn binds_machine_id_without_falling_through_to_other_rows() {
        let store = fixture_store();
        store.conn.execute("INSERT INTO tune_snapshots (node_id, at, data) VALUES (?1, ?2, ?3)", params!["fixture-one", 9000, "{}"]).unwrap();
        let absent = store.saved_machine_reading("fixture-one' OR 1=1--").unwrap();
        assert_eq!(absent.last_at, None);
        assert_eq!(absent.failure, ProbeFailure::None);
    }

    #[test]
    fn duplicate_failure_and_schedule_keys_are_unknown() {
        for value in [r#"{"at":100,"at":9000,"error":"fixture"}"#, r#"{"at":9000,"error":"fixture","error":"another"}"#] {
            let store = fixture_store();
            store.conn.execute("INSERT INTO meta (k, v, updated_at) VALUES (?1, ?2, 1)", params!["probeFailure:fixture-one", value]).unwrap();
            assert_eq!(store.saved_machine_reading("fixture-one").unwrap().failure, ProbeFailure::Invalid);
        }
        let store = fixture_store();
        store.conn.execute("INSERT INTO meta (k, v, updated_at) VALUES (?1, ?2, 1)", params!["schedule", r#"{"probe_minutes":5,"probe_minutes":60}"#]).unwrap();
        assert_eq!(store.saved_machine_reading("fixture-one").unwrap().schedule, StoredSchedule::Invalid);
    }

    #[test]
    fn schedule_and_failure_metadata_respect_types_and_byte_limits() {
        let long = "あ".repeat(2800);
        let oversized_schedule = format!(r#"{{"probe_minutes":5,"padding":"{long}"}}"#);
        for (value, expected) in [
            ("{}".to_owned(), StoredSchedule::Missing),
            (r#"{"probe_minutes":5}"#.to_owned(), StoredSchedule::Minutes(5)),
            (r#"{"probe_minutes":"5"}"#.to_owned(), StoredSchedule::Invalid),
            ("null".to_owned(), StoredSchedule::Invalid),
            (oversized_schedule, StoredSchedule::Invalid),
        ] {
            let store = fixture_store();
            store.conn.execute("INSERT INTO meta (k, v, updated_at) VALUES (?1, ?2, 1)", params!["schedule", value]).unwrap();
            assert_eq!(store.saved_machine_reading("fixture-one").unwrap().schedule, expected);
        }
        let store = fixture_store();
        let oversized_failure = format!(r#"{{"at":9000,"error":"{long}"}}"#);
        store.conn.execute("INSERT INTO meta (k, v, updated_at) VALUES (?1, ?2, 1)", params!["probeFailure:fixture-one", oversized_failure]).unwrap();
        assert_eq!(store.saved_machine_reading("fixture-one").unwrap().failure, ProbeFailure::Invalid);
    }
}
