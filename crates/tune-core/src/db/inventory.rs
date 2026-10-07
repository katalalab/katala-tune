//! 道具の棚卸し（lib/db.js の inventory / inventory_events / inventory_nodes）。
//! いまは表の定義だけを揃えている（Electron 版と同じ DB を開くため）。保存・照合・Do-gu の移植は別の作業で、
//! ここに `impl Store { save_inventory, inventory, inventory_events }` を足す（lib/db.js の saveInventory などが仕様）。

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
