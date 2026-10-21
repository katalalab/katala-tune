//! lifecycle専用表。OSログ/FTS/署名・既存minute行の意味を変えない。
use super::{Result, Store, now_ms};
use crate::monitor_journal::{EVENT_BYTES, Event, Filter, KEEP_DAYS, KEEP_ROWS};
use rusqlite::params;
use serde_json::{Value, json};
use std::time::Duration;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS monitor_events (
 id INTEGER PRIMARY KEY AUTOINCREMENT, collector_id TEXT NOT NULL, event_seq INTEGER NOT NULL,
 node_id TEXT, ts INTEGER NOT NULL, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 1024 AND json_valid(payload)),
 UNIQUE(collector_id,event_seq)
);
CREATE INDEX IF NOT EXISTS monitor_events_node_time ON monitor_events(node_id,ts,id);
";
impl Store {
    pub(crate) fn insert_monitor_events(&self, events: &[Event], now: i64) -> Result<usize> {
        // journalのblocking仕事をSQLiteの既定busy待機でshutdown期限の外へ残さない。
        // 呼出元はStore mutexを保持する。他のStore書き込みの待機設定は成功/失敗とも復元する。
        let busy_ms: u64 = self.conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?;
        self.conn.busy_timeout(Duration::ZERO)?;
        let result = self.insert_monitor_events_nowait(events, now);
        self.conn.busy_timeout(Duration::from_millis(busy_ms))?;
        result
    }

    fn insert_monitor_events_nowait(&self, events: &[Event], now: i64) -> Result<usize> {
        // TransactionのDropでCOMMIT失敗時もrollbackする。別featureのtransactionには触れない。
        let tx = self.conn.unchecked_transaction()?;
        let pruned = {
            let mut st = tx.prepare_cached("INSERT OR IGNORE INTO monitor_events(collector_id,event_seq,node_id,ts,payload) VALUES(?,?,?,?,?)")?;
            for e in events {
                let body = serde_json::to_string(e).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                if body.len() > EVENT_BYTES {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                st.execute(params![e.collector_id, i64::try_from(e.event_seq).map_err(|_| rusqlite::Error::InvalidQuery)?, e.node_id, e.ts, body])?;
            }
            let age = tx.execute("DELETE FROM monitor_events WHERE ts < ?", [now - KEEP_DAYS * 86_400_000])?;
            let count = tx.execute("DELETE FROM monitor_events WHERE id IN (SELECT id FROM monitor_events ORDER BY id DESC LIMIT -1 OFFSET ?)", [KEEP_ROWS])?;
            age + count
        };
        tx.commit()?;
        Ok(pruned)
    }

    pub fn monitor_events(&self, f: &Filter) -> Result<Value> {
        if f.node_id.as_ref().is_some_and(|s| s.len() > 128 || s.chars().any(char::is_control)) || f.collector_id.as_ref().is_some_and(|s| s.len() > 128) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let generation = f.generation.map(|v| i64::try_from(v).map_err(|_| rusqlite::Error::InvalidQuery)).transpose()?;
        let limit = f.limit.unwrap_or(100).clamp(1, 500);
        let visible_since = now_ms() - KEEP_DAYS * 86_400_000;
        let mut st = self.conn.prepare("SELECT id,payload FROM monitor_events WHERE (?1 IS NULL OR node_id=?1) AND (?2 IS NULL OR id<?2) AND ts>=?3 AND (?5 IS NULL OR collector_id=?5) AND (?6 IS NULL OR json_extract(payload,'$.kind')=?6) AND (?7 IS NULL OR json_extract(payload,'$.generation')=?7) AND (?8 IS NULL OR json_extract(payload,'$.attempt')=?8) ORDER BY id DESC LIMIT ?4")?;
        let rows = st.query_map(
            params![
                f.node_id,
                f.before_id,
                f.since.unwrap_or(0).max(visible_since),
                limit,
                f.collector_id,
                f.kind.map(|k| serde_json::to_value(k).unwrap().as_str().unwrap().to_string()),
                generation,
                f.attempt
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (id, s) = row?;
            let e: Event = serde_json::from_str(&s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e)))?;
            if e.v != 1 {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let mut v = e.json();
            v["id"] = id.into();
            out.push(v);
        }
        let (count, oldest, newest): (i64, Option<i64>, Option<i64>) =
            self.conn
                .query_row("SELECT count(*),min(ts),max(ts) FROM monitor_events WHERE ts>=?", [visible_since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(
            json!({ "events": out, "next_before_id": out.last().and_then(|e|e["id"].as_i64()), "retention": { "rows": count, "oldest_at":oldest,"newest_at":newest,"keep_days_max":KEEP_DAYS,"keep_rows_max":KEEP_ROWS } }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor_journal::{Data, Kind};
    fn event(seq: u64, ts: i64) -> Event {
        Event {
            v: 1,
            collector_id: "fixture".into(),
            app_version: "test".into(),
            event_seq: seq,
            node_id: Some("node".into()),
            generation: 1,
            attempt: 1,
            ts,
            kind: Kind::Open,
            data: Data::default(),
        }
    }

    #[test]
    fn retention_read_hides_expired_rows_without_modifying_storage() {
        let db = Store::open_in_memory().unwrap();
        let now = crate::db::now_ms();
        db.insert_monitor_events(&[event(1, now), event(2, now)], now).unwrap();
        let expired = now - KEEP_DAYS * 86_400_000 - 1;
        db.conn().execute("UPDATE monitor_events SET ts=?1,payload=json_set(payload,'$.ts',?1) WHERE event_seq=1", [expired]).unwrap();
        let changes = db.conn().total_changes();
        let out = db.monitor_events(&Filter { since: Some(0), ..Default::default() }).unwrap();
        assert_eq!(db.conn().total_changes(), changes, "reads do not prune or start a writer");
        let physical: i64 = db.conn().query_row("SELECT count(*) FROM monitor_events", [], |r| r.get(0)).unwrap();
        assert_eq!(physical, 2);
        assert_eq!(out["events"].as_array().unwrap().len(), 1);
        assert_eq!(out["events"][0]["event_seq"], 2);
        assert_eq!(out["retention"]["rows"], 1);
        assert_eq!(out["retention"]["oldest_at"], now);
    }

    #[test]
    fn retention_age_row_cap_idempotency_and_bounded_query() {
        let db = Store::open_in_memory().unwrap();
        let now = 2_000_000_000_000i64;
        let e = event(1, now);
        let body = serde_json::to_string(&e).unwrap();
        db.conn().execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<50002) INSERT INTO monitor_events(collector_id,event_seq,node_id,ts,payload) SELECT 'fixture',x,'node',?1,json_set(?2,'$.event_seq',x) FROM n",params![now,body]).unwrap();
        db.insert_monitor_events(&[event(60000, now - KEEP_DAYS * 86_400_000 - 1)], now).unwrap();
        let out = db.monitor_events(&Filter { limit: Some(9000), ..Filter::default() }).unwrap();
        assert_eq!(out["retention"]["rows"], KEEP_ROWS);
        assert_eq!(out["events"].as_array().unwrap().len(), 500);
        assert_eq!(out["retention"]["oldest_at"], now);
        db.insert_monitor_events(&[event(50002, now)], now).unwrap();
        assert_eq!(db.monitor_events(&Filter::default()).unwrap()["retention"]["rows"], KEEP_ROWS);
        assert!(db.monitor_events(&Filter { node_id: Some("absent".into()), ..Filter::default() }).unwrap()["events"].as_array().unwrap().is_empty());
        assert_eq!(db.conn().query_row("SELECT count(*) FROM logs", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn temporary_store_reopens_and_legacy_tables_survive() {
        let dir = std::env::temp_dir().join(format!("monitor-journal-reopen-{}-{}", std::process::id(), crate::db::now_ms()));
        {
            let db = Store::open(&dir).unwrap();
            db.set_meta("fixture", &json!({"held":true})).unwrap();
            db.insert_monitor_events(&[event(1, crate::db::now_ms())], crate::db::now_ms()).unwrap();
        }
        let db = Store::open(&dir).unwrap();
        assert_eq!(db.get_meta("fixture").unwrap().unwrap()["held"], true);
        assert_eq!(db.monitor_events(&Filter::default()).unwrap()["events"][0]["app_version"], "test");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn journal_busy_is_immediate_and_restores_the_store_timeout() {
        let dir = std::env::temp_dir().join(format!("monitor-journal-busy-{}-{}", std::process::id(), crate::db::now_ms()));
        let db = Store::open(&dir).unwrap();
        let blocker = Store::open(&dir).unwrap();
        let timeout = std::time::Duration::from_millis(2500);
        db.conn().busy_timeout(timeout).unwrap();
        let configured = || db.conn().query_row("PRAGMA busy_timeout", [], |r| r.get::<_, u64>(0)).unwrap();
        db.insert_monitor_events(&[event(1, crate::db::now_ms())], crate::db::now_ms()).unwrap();
        assert_eq!(configured(), 2500, "successful journal writes restore the existing timeout");
        blocker.conn().execute_batch("BEGIN IMMEDIATE").unwrap();
        let began = std::time::Instant::now();
        let err = db.insert_monitor_events(&[event(2, crate::db::now_ms())], crate::db::now_ms()).unwrap_err();
        let journal_elapsed = began.elapsed();
        let restored = configured();
        let began = std::time::Instant::now();
        let other = db.set_meta("blocked", &json!(true));
        let other_elapsed = began.elapsed();
        blocker.conn().execute_batch("ROLLBACK").unwrap();
        db.set_meta("recovered", &json!(true)).unwrap();
        db.insert_monitor_events(&[event(2, crate::db::now_ms())], crate::db::now_ms()).unwrap();
        let final_timeout = configured();
        drop(blocker);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
        assert!(journal_elapsed < std::time::Duration::from_secs(1), "journal waited {}ms", journal_elapsed.as_millis());
        assert_eq!(restored, 2500, "failed journal writes restore the existing timeout");
        assert!(other.is_err());
        assert!(other_elapsed >= std::time::Duration::from_secs(1), "other Store writes retain their busy wait");
        assert_eq!(final_timeout, 2500);
    }
    #[test]
    fn correlation_filters_and_cursor_return_only_the_requested_attempt() {
        let db = Store::open_in_memory().unwrap();
        let now = crate::db::now_ms();
        let a = event(1, now);
        let mut b = event(2, now);
        b.generation = 2;
        b.attempt = 3;
        b.kind = Kind::End;
        let mut c = b.clone();
        c.collector_id = "second".into();
        db.insert_monitor_events(&[a, b, c], now).unwrap();
        let out = db
            .monitor_events(&Filter {
                kind: Some(Kind::End),
                collector_id: Some("fixture".into()),
                generation: Some(2),
                attempt: Some(3),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(out["events"].as_array().unwrap().len(), 1);
        assert_eq!(out["events"][0]["event_seq"], 2);
        let page = db.monitor_events(&Filter { limit: Some(1), ..Default::default() }).unwrap();
        let next = db.monitor_events(&Filter { before_id: page["next_before_id"].as_i64(), ..Default::default() }).unwrap();
        assert_eq!(next["events"].as_array().unwrap().len(), 2);
        db.conn().execute("UPDATE monitor_events SET payload=json_set(payload,'$.v',2)", []).unwrap();
        assert!(db.monitor_events(&Filter::default()).is_err());
    }
}
