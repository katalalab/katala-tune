//! 道具の棚卸し（lib/db.js の inventory / inventory_events / inventory_nodes と saveInventory・inventory・inventoryEvents）。
//! 表の定義と書き方は lib/db.js と同じ（Electron 版と同じ DB を読み書きする）。

use std::collections::{HashMap, HashSet};

use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

use super::{Result, Store, row_json};
use crate::inventory::Item;
use crate::js;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS inventory (
  node_id TEXT NOT NULL, source TEXT NOT NULL, name TEXT NOT NULL, version TEXT, explicit INTEGER NOT NULL DEFAULT 1, extra TEXT,
  first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, removed_at INTEGER,
  PRIMARY KEY (node_id, source, name)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS inventory_events (
  id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, node_id TEXT NOT NULL, source TEXT NOT NULL, name TEXT NOT NULL,
  kind TEXT NOT NULL, from_version TEXT, to_version TEXT
);
CREATE INDEX IF NOT EXISTS inventory_events_ts ON inventory_events (ts DESC);
CREATE TABLE IF NOT EXISTS inventory_nodes (node_id TEXT PRIMARY KEY, baseline_at INTEGER NOT NULL, last_ok_at INTEGER NOT NULL) WITHOUT ROWID;
";

/// saveInventory の戻り値
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InventorySaved {
    pub added: i64,
    pub removed: i64,
    pub updated: i64,
    pub total: usize,
    pub baseline: bool,
    pub skipped: Vec<String>,
}

struct Prev {
    source: String,
    name: String,
    version: Option<String>,
    first_seen: i64,
    removed_at: Option<i64>,
}

fn nz(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.is_empty())
}

impl Store {
    /// 棚卸しの結果で機体の一覧を置き換える。初回は増減を記録しない（全部が「追加」になるため）。
    /// skip_sources（取り方が失敗した種類）は前回の一覧を残し、削除と見なさない
    pub fn save_inventory(&self, node_id: &str, items: &[Item], now: i64, skip_sources: &[String]) -> Result<InventorySaved> {
        let mut prev_order: Vec<(String, String)> = Vec::new();
        let mut prev: HashMap<(String, String), Prev> = HashMap::new();
        {
            let mut st = self.conn.prepare_cached("SELECT source, name, version, first_seen, removed_at FROM inventory WHERE node_id = ?")?;
            let rows =
                st.query_map([node_id], |r| Ok(Prev { source: r.get(0)?, name: r.get(1)?, version: r.get(2)?, first_seen: r.get(3)?, removed_at: r.get(4)? }))?;
            for p in rows {
                let p = p?;
                let k = (p.source.clone(), p.name.clone());
                prev_order.push(k.clone());
                prev.insert(k, p);
            }
        }
        let marker: bool = self.conn.prepare_cached("SELECT 1 FROM inventory_nodes WHERE node_id = ?")?.exists([node_id])?;
        // 印の無い古い DB で行だけあるときは、初回は済んでいるとみなす
        let baseline = !marker && prev.is_empty();
        let mut skipped: Vec<String> = Vec::new();
        for s in skip_sources {
            if !skipped.contains(s) {
                skipped.push(s.clone());
            }
        }
        let skip: HashSet<&String> = skipped.iter().collect();
        let mut c = InventorySaved { added: 0, removed: 0, updated: 0, total: items.len(), baseline, skipped: skipped.clone() };
        // 0 や空文字は JS では偽
        let removed = |p: &Prev| p.removed_at.is_some_and(|x| x != 0);
        self.tx(|db| {
            let mut ev = db.prepare_cached("INSERT INTO inventory_events (ts, node_id, source, name, kind, from_version, to_version) VALUES (?, ?, ?, ?, ?, ?, ?)")?;
            let mut up = db.prepare_cached(
                "INSERT INTO inventory (node_id, source, name, version, explicit, extra, first_seen, last_seen, removed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)
                 ON CONFLICT (node_id, source, name) DO UPDATE SET version = excluded.version, explicit = excluded.explicit, extra = excluded.extra,
                 last_seen = excluded.last_seen,
                 first_seen = CASE WHEN inventory.removed_at IS NULL THEN inventory.first_seen ELSE excluded.first_seen END, removed_at = NULL",
            )?;
            for it in items {
                let old = prev.remove(&(it.source.clone(), it.name.clone()));
                let first = match &old {
                    Some(o) if !removed(o) => o.first_seen,
                    _ => now,
                };
                let extra = it.extra.as_ref().filter(|x| js::truthy(Some(x))).map(Value::to_string);
                up.execute(params![node_id, it.source, it.name, it.version, i64::from(it.explicit), extra, first, now])?;
                if baseline {
                    continue;
                }
                match &old {
                    None => {
                        ev.execute(params![now, node_id, it.source, it.name, "added", None::<String>, it.version])?;
                        c.added += 1;
                    }
                    Some(o) if removed(o) => {
                        ev.execute(params![now, node_id, it.source, it.name, "added", None::<String>, it.version])?;
                        c.added += 1;
                    }
                    Some(o) if nz(&o.version) != nz(&it.version) => {
                        ev.execute(params![now, node_id, it.source, it.name, "updated", o.version, it.version])?;
                        c.updated += 1;
                    }
                    _ => {}
                }
            }
            for k in &prev_order {
                let Some(old) = prev.get(k) else { continue };
                if removed(old) || skip.contains(&old.source) {
                    continue;
                }
                db.prepare_cached("UPDATE inventory SET removed_at = ? WHERE node_id = ? AND source = ? AND name = ?")?.execute(params![now, node_id, old.source, old.name])?;
                ev.execute(params![now, node_id, old.source, old.name, "removed", old.version, None::<String>])?;
                c.removed += 1;
            }
            db.prepare_cached(
                "INSERT INTO inventory_nodes (node_id, baseline_at, last_ok_at) VALUES (?, ?, ?)
                 ON CONFLICT (node_id) DO UPDATE SET last_ok_at = excluded.last_ok_at",
            )?
            .execute(params![node_id, now, now])?;
            Ok(())
        })?;
        Ok(c)
    }

    /// 今の一覧（inventory）。explicit は真偽、extra は JSON に戻す
    pub fn inventory(&self, node_id: Option<&str>, include_removed: bool) -> Result<Vec<Value>> {
        let mut where_: Vec<&str> = Vec::new();
        if node_id.is_some() {
            where_.push("node_id = ?");
        }
        if !include_removed {
            where_.push("removed_at IS NULL");
        }
        let sql = format!(
            "SELECT * FROM inventory {} ORDER BY node_id, source, name",
            if where_.is_empty() { String::new() } else { format!("WHERE {}", where_.join(" AND ")) }
        );
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = match node_id {
            Some(n) => st.query_map([n], row_json)?.collect::<Result<Vec<_>>>()?,
            None => st.query_map([], row_json)?.collect::<Result<Vec<_>>>()?,
        };
        Ok(rows
            .into_iter()
            .map(|mut r| {
                if let Value::Object(m) = &mut r {
                    let e = js::truthy(m.get("explicit"));
                    m.insert("explicit".into(), Value::Bool(e));
                    let extra = match m.get("extra") {
                        Some(Value::String(s)) if !s.is_empty() => serde_json::from_str(s).unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                    m.insert("extra".into(), extra);
                }
                r
            })
            .collect())
    }

    /// 増減・版の変化の記録（新しい順）
    pub fn inventory_events(&self, limit: i64) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached("SELECT * FROM inventory_events ORDER BY ts DESC, id DESC LIMIT ?")?;
        st.query_map([limit], row_json)?.collect()
    }

    /// 機体ごとの棚卸しの記録（初回の日時・最後に成功した日時）
    pub fn inventory_nodes(&self) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached("SELECT * FROM inventory_nodes ORDER BY node_id")?;
        st.query_map([], row_json)?.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(source: &str, name: &str, version: &str) -> Item {
        Item { source: source.into(), name: name.into(), version: Some(version.into()), explicit: true, extra: None }
    }

    #[test]
    fn baseline_then_changes() {
        let db = Store::open_in_memory().unwrap();
        let c = db.save_inventory("n1", &[it("brew", "a", "1"), it("brew", "b", "1")], 1000, &[]).unwrap();
        assert_eq!(c, InventorySaved { added: 0, removed: 0, updated: 0, total: 2, baseline: true, skipped: vec![] });
        assert!(db.inventory_events(200).unwrap().is_empty());
        let c = db.save_inventory("n1", &[it("brew", "a", "2"), it("brew", "c", "1")], 2000, &[]).unwrap();
        assert_eq!((c.added, c.removed, c.updated, c.baseline), (1, 1, 1, false));
        let now: Vec<String> =
            db.inventory(Some("n1"), false).unwrap().iter().map(|r| format!("{}@{}", r["name"].as_str().unwrap(), r["version"].as_str().unwrap())).collect();
        assert_eq!(now, ["a@2", "c@1"]);
        db.save_inventory("n1", &[it("brew", "a", "2"), it("brew", "b", "1"), it("brew", "c", "1")], 3000, &[]).unwrap();
        let b = db.inventory(Some("n1"), false).unwrap().into_iter().find(|r| r["name"] == "b").unwrap();
        assert_eq!((b["removed_at"].clone(), b["first_seen"].clone()), (Value::Null, Value::from(3000)));
        let ev: Vec<String> =
            db.inventory_events(200).unwrap().iter().map(|e| format!("{}:{}", e["kind"].as_str().unwrap(), e["name"].as_str().unwrap())).collect();
        assert_eq!(ev, ["added:b", "removed:b", "added:c", "updated:a"]);
    }

    #[test]
    fn failed_sources_are_not_removals() {
        let db = Store::open_in_memory().unwrap();
        db.save_inventory("n1", &[it("brew", "a", "1"), it("app", "X", "1")], 1000, &[]).unwrap();
        let c = db.save_inventory("n1", &[it("app", "X", "1")], 2000, &["brew".into()]).unwrap();
        assert_eq!((c.removed, c.skipped.clone()), (0, vec!["brew".to_string()]));
        assert_eq!(db.save_inventory("n1", &[it("app", "X", "1")], 3000, &[]).unwrap().removed, 1);
        assert!(db.save_inventory("empty", &[], 1000, &[]).unwrap().baseline);
        let d = db.save_inventory("empty", &[it("brew", "new", "1")], 2000, &[]).unwrap();
        assert_eq!((d.baseline, d.added), (false, 1));
    }
}
