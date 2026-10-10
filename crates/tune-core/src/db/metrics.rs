//! 常時監視の分ごとの集計（monitor.rs）。1 機体 1 分 1 行。値は JSON（項目ごとに [平均, 最大]）にして、
//! 項目を足しても表を変えなくて済むようにする。src はどの操作卓が集めたか（ハブから写した行はハブの名前）。

use rusqlite::params;
use serde_json::{Value, json};

use super::{Result, Store};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS metrics_minute (
  node_id TEXT NOT NULL, minute INTEGER NOT NULL, n INTEGER NOT NULL, v TEXT NOT NULL, src TEXT,
  PRIMARY KEY (node_id, minute)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS metrics_minute_by_time ON metrics_minute (minute);
";

/// 1 分の集計
#[derive(Clone, Debug, PartialEq)]
pub struct MinuteRow {
    pub node_id: String,
    /// その分の始まり（ms）
    pub minute: i64,
    /// 点の数
    pub n: i64,
    pub v: Value,
    pub src: Option<String>,
}

impl MinuteRow {
    pub fn json(&self) -> Value {
        json!({ "node_id": self.node_id, "minute": self.minute, "n": self.n, "v": self.v, "src": self.src })
    }

    pub fn from_json(x: &Value) -> Option<MinuteRow> {
        Some(MinuteRow {
            node_id: x.get("node_id")?.as_str()?.to_string(),
            minute: x.get("minute")?.as_i64()?,
            n: x.get("n")?.as_i64()?,
            v: x.get("v").filter(|v| v.is_object())?.clone(),
            src: x.get("src").and_then(Value::as_str).map(str::to_string),
        })
    }
}

impl Store {
    /// 同じ機体・同じ分は置き換える（集め直し・ハブからの写しは新しい方が勝つ）
    pub fn add_metrics(&self, rows: &[MinuteRow]) -> Result<usize> {
        self.tx(|c| {
            let mut st = c.prepare_cached("INSERT OR REPLACE INTO metrics_minute (node_id, minute, n, v, src) VALUES (?, ?, ?, ?, ?)")?;
            for r in rows {
                st.execute(params![r.node_id, r.minute, r.n, r.v.to_string(), r.src])?;
            }
            Ok(rows.len())
        })
    }

    /// 期間の行（古い順）。node が None なら全機体
    pub fn metrics(&self, node: Option<&str>, since: i64, until: i64) -> Result<Vec<MinuteRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT node_id, minute, n, v, src FROM metrics_minute WHERE (?1 IS NULL OR node_id = ?1) AND minute >= ?2 AND minute < ?3 ORDER BY minute, node_id",
        )?;
        let rows = st.query_map(params![node, since, until], |r| {
            let v: String = r.get(3)?;
            Ok(MinuteRow { node_id: r.get(0)?, minute: r.get(1)?, n: r.get(2)?, v: serde_json::from_str(&v).unwrap_or(Value::Null), src: r.get(4)? })
        })?;
        rows.collect()
    }

    /// 機体ごとの一番新しい分
    pub fn latest_metric_minutes(&self) -> Result<Vec<(String, i64)>> {
        let mut st = self.conn.prepare_cached("SELECT node_id, max(minute) FROM metrics_minute GROUP BY node_id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// 古い行を消す
    pub fn prune_metrics(&self, before: i64) -> Result<usize> {
        self.conn.execute("DELETE FROM metrics_minute WHERE minute < ?", [before])
    }

    /// 写し（ハブとの同期）に使う: 新しい分析結果の行をそのまま（node_id, at, wall_s, score, findings, data, 要約）
    pub fn snapshots_since(&self, since: i64) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached(
            "SELECT node_id, at, wall_s, score, bench_ms, cpu, mem_avail, swap_gb, critical, warn, findings, data FROM tune_snapshots t
             WHERE at > ? AND at = (SELECT max(at) FROM tune_snapshots u WHERE u.node_id = t.node_id) ORDER BY node_id",
        )?;
        let rows = st.query_map([since], |r| {
            let parse =
                |i: usize| -> rusqlite::Result<Value> { Ok(r.get::<_, Option<String>>(i)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)) };
            Ok(json!({
                "node_id": r.get::<_, String>(0)?,
                "at": r.get::<_, i64>(1)?,
                "wall_s": r.get::<_, Option<f64>>(2)?,
                "score": r.get::<_, Option<i64>>(3)?,
                "summary": {
                    "bench_ms": r.get::<_, Option<f64>>(4)?, "cpu": r.get::<_, Option<f64>>(5)?, "mem_avail": r.get::<_, Option<f64>>(6)?,
                    "swap_gb": r.get::<_, Option<f64>>(7)?, "critical": r.get::<_, Option<i64>>(8)?, "warn": r.get::<_, Option<i64>>(9)?,
                },
                "findings": parse(10)?,
                "data": parse(11)?,
            }))
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(node: &str, minute: i64, cpu: f64) -> MinuteRow {
        MinuteRow { node_id: node.into(), minute, n: 60, v: json!({"cpu": [cpu, cpu]}), src: Some("me".into()) }
    }

    #[test]
    fn metrics_round_trip_replace_and_prune() {
        let s = Store::open_in_memory().unwrap();
        s.add_metrics(&[row("a", 60_000, 1.0), row("a", 120_000, 2.0), row("b", 60_000, 3.0)]).unwrap();
        s.add_metrics(&[row("a", 120_000, 9.0)]).unwrap();
        let all = s.metrics(None, 0, i64::MAX).unwrap();
        assert_eq!(all.len(), 3);
        let a = s.metrics(Some("a"), 0, i64::MAX).unwrap();
        assert_eq!(a.iter().map(|r| r.v["cpu"][0].as_f64().unwrap()).collect::<Vec<_>>(), [1.0, 9.0], "同じ分は置き換える");
        assert_eq!(s.metrics(Some("a"), 120_000, 180_000).unwrap().len(), 1, "until は含まない");
        let mut latest = s.latest_metric_minutes().unwrap();
        latest.sort();
        assert_eq!(latest, [("a".to_string(), 120_000), ("b".to_string(), 60_000)]);
        assert_eq!(s.prune_metrics(120_000).unwrap(), 2);
        assert_eq!(MinuteRow::from_json(&all[0].json()), Some(all[0].clone()));
        assert_eq!(MinuteRow::from_json(&json!({"node_id": "a", "minute": 1, "n": 1, "v": 3})), None, "v はオブジェクトだけ");
    }

    #[test]
    fn snapshots_since_returns_only_the_latest_per_node() {
        let s = Store::open_in_memory().unwrap();
        let sum = json!({"critical": 1, "warn": 0});
        s.add_snapshot("a", 1000, Some(1.0), Some(90), &json!([]), &json!({"x": 1}), &sum).unwrap();
        s.add_snapshot("a", 2000, Some(1.0), Some(80), &json!([{"id": "f"}]), &json!({"x": 2}), &sum).unwrap();
        s.add_snapshot("b", 500, None, None, &json!([]), &json!({}), &json!({})).unwrap();
        let v = s.snapshots_since(900).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!((v[0]["node_id"].as_str(), v[0]["at"].as_i64(), v[0]["score"].as_i64()), (Some("a"), Some(2000), Some(80)));
        assert_eq!(v[0]["findings"][0]["id"], "f");
        assert_eq!(v[0]["summary"]["critical"], 1);
        assert_eq!(s.snapshots_since(0).unwrap().len(), 2);
    }
}
