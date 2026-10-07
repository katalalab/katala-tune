//! 機能チェックの現在値（scope = '_app' はアプリ自身）と、状態が変わったときだけの記録

use std::collections::BTreeMap;

use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

use super::{Result, Store, row_json};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS checks (
  scope TEXT NOT NULL, id TEXT NOT NULL, name TEXT, status TEXT NOT NULL, detail TEXT, since INTEGER NOT NULL, checked_at INTEGER NOT NULL,
  PRIMARY KEY (scope, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS check_events (
  id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, scope TEXT NOT NULL, check_id TEXT NOT NULL, name TEXT, from_status TEXT, to_status TEXT NOT NULL, detail TEXT
);
CREATE INDEX IF NOT EXISTS check_events_ts ON check_events (ts DESC);
";

/// 1つの機能チェック（lib/health.js の出力）
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    pub id: String,
    pub name: String,
    pub status: String,
    pub detail: Option<String>,
}

/// 状態が変わったもの（通知と履歴に使う）
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub scope: String,
    pub id: String,
    pub name: String,
    pub from: Option<String>,
    pub to: String,
    pub detail: Option<String>,
}

impl Store {
    /// 機能チェックを保存し、状態が変わったものを返す。消えたチェックは削除する
    pub fn save_checks(&self, scope: &str, checks: &[Check], now: i64) -> Result<Vec<Change>> {
        let mut prev: BTreeMap<String, (String, i64)> = BTreeMap::new();
        {
            let mut st = self.conn.prepare_cached("SELECT id, status, since FROM checks WHERE scope = ?")?;
            for r in st.query_map([scope], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))? {
                let (id, status, since) = r?;
                prev.insert(id, (status, since));
            }
        }
        self.tx(|c| {
            let mut changed = Vec::new();
            for ch in checks {
                let p = prev.remove(&ch.id);
                let since = match &p {
                    Some((st, since)) if *st == ch.status => *since,
                    _ => now,
                };
                c.prepare_cached("INSERT OR REPLACE INTO checks (scope, id, name, status, detail, since, checked_at) VALUES (?, ?, ?, ?, ?, ?, ?)")?
                    .execute(params![scope, ch.id, ch.name, ch.status, ch.detail, since, now])?;
                if p.as_ref().is_none_or(|(st, _)| *st != ch.status) {
                    let from = p.map(|(st, _)| st);
                    c.prepare_cached("INSERT INTO check_events (ts, scope, check_id, name, from_status, to_status, detail) VALUES (?, ?, ?, ?, ?, ?, ?)")?
                        .execute(params![now, scope, ch.id, ch.name, from, ch.status, ch.detail])?;
                    changed.push(Change {
                        scope: scope.into(),
                        id: ch.id.clone(),
                        name: ch.name.clone(),
                        from,
                        to: ch.status.clone(),
                        detail: ch.detail.clone(),
                    });
                }
            }
            for id in prev.keys() {
                c.execute("DELETE FROM checks WHERE scope = ? AND id = ?", params![scope, id])?;
            }
            Ok(changed)
        })
    }

    pub fn checks(&self) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached("SELECT * FROM checks ORDER BY scope, id")?;
        st.query_map([], row_json)?.collect()
    }

    pub fn check_events(&self, limit: i64) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached("SELECT * FROM check_events ORDER BY ts DESC, id DESC LIMIT ?")?;
        st.query_map([limit], row_json)?.collect()
    }

    /// 状態ごとの件数（ok / warn / fail / unknown）
    pub fn status_counts(&self) -> Result<BTreeMap<String, i64>> {
        let mut c: BTreeMap<String, i64> = ["ok", "warn", "fail", "unknown"].iter().map(|k| (k.to_string(), 0)).collect();
        let mut st = self.conn.prepare_cached("SELECT status FROM checks")?;
        for s in st.query_map([], |r| r.get::<_, String>(0))? {
            *c.entry(s?).or_insert(0) += 1;
        }
        Ok(c)
    }
}
