//! ローカルの保存先（lib/db.js と同じファイル・同じ表）。rusqlite（bundled、FTS5 あり）。
//! Electron 版と同じ `<userData>/data/katala-tune.db` を開くので、どちらのアプリからも同じ内容が読める。
//! 表の名前と列は katala-fleet の中央 DB 設計に合わせてあり、後でそのまま送れる形にしている。
//!
//! 表は機能ごとのファイルに分けている（snapshots / actions / logs / checks / meta）。
//! 機能を足すとき（道具の棚卸し inventory・AI エージェントのセッション ai_sessions など）は、
//! `db/<機能>.rs` に `pub(super) const SCHEMA` と `impl Store { ... }` を書き、下の `SCHEMA_PARTS` に足す。
//! Electron 版と同じ DB を共有するので、表の定義は lib/db.js と揃える（CREATE TABLE IF NOT EXISTS のまま）。

mod actions;
mod checks;
mod inventory;
mod logs;
mod snapshots;

use std::path::Path;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

pub use checks::{Change, Check};
pub use logs::{LogCount, LogRow, TopSignature};
pub use snapshots::Snapshot;

pub const DB_FILE: &str = "katala-tune.db";
pub const LOG_RETENTION_DAYS: i64 = 30;
pub const SNAPSHOT_KEEP: i64 = 200;
/// logs-query で一度に返す上限（lib/db.js と同じ）
pub const QUERY_LIMIT_MAX: i64 = 2000;
/// logs-signatures で一度に返す上限（画面には集計の上位だけ返す）
pub const SIGNATURE_LIMIT_MAX: i64 = 500;

const PRAGMAS: &str = "PRAGMA journal_mode = WAL;\nPRAGMA synchronous = NORMAL;\n";
const SCHEMA_PARTS: &[&str] = &[snapshots::SCHEMA, actions::SCHEMA, logs::SCHEMA, checks::SCHEMA, META_SCHEMA, inventory::SCHEMA];
const META_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT, updated_at INTEGER);\n";

pub type Result<T> = rusqlite::Result<T>;

pub struct Store {
    conn: Connection,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Store {
    /// `<dir>/katala-tune.db` を開く（無ければ作る）
    pub fn open(dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(dir).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let conn = Connection::open(dir.join(DB_FILE))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch(PRAGMAS)?;
        conn.execute_batch(&SCHEMA_PARTS.concat())?;
        let s = Store { conn };
        s.migrate()?;
        Ok(s)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// トランザクション。失敗したら ROLLBACK して Err を返す（書き込み失敗を握りつぶさない）
    pub(crate) fn tx<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN")?;
        match f(&self.conn) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    // 移行: v0.2 までの Windows の取り込みは時刻が UTC との時差ぶんずれていた。lib/db.js と同じ移行（済んでいれば何もしない）
    fn migrate(&self) -> Result<()> {
        let done: Option<i64> = self.conn.query_row("SELECT 1 FROM meta WHERE k = 'migr_win_ts_v1'", [], |r| r.get(0)).optional()?;
        if done.is_some() {
            return Ok(());
        }
        const WIN: &str = "('win_system', 'win_application', 'neonmonitor')";
        self.tx(|c| {
            c.execute_batch(&format!(
                "INSERT INTO logs_fts (logs_fts, rowid, message, provider) SELECT 'delete', id, message, coalesce(provider, '') FROM logs WHERE source IN {WIN};
                 DELETE FROM logs WHERE source IN {WIN};
                 DELETE FROM log_signatures WHERE source IN {WIN};
                 DELETE FROM log_cursors WHERE source IN {WIN};"
            ))?;
            c.execute("INSERT OR REPLACE INTO meta (k, v, updated_at) VALUES ('migr_win_ts_v1', 'true', ?)", [now_ms()])?;
            Ok(())
        })
    }

    pub fn get_meta(&self, k: &str) -> Result<Option<Value>> {
        let v: Option<Option<String>> = self.conn.query_row("SELECT v FROM meta WHERE k = ?", [k], |r| r.get(0)).optional()?;
        Ok(v.flatten().and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn set_meta(&self, k: &str, v: &Value) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO meta (k, v, updated_at) VALUES (?, ?, ?)", params![k, v.to_string(), now_ms()])?;
        Ok(())
    }

    /// (quick_check の結果, DB の大きさ)
    pub fn integrity(&self) -> Result<(String, i64)> {
        let check: String = self.conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        let bytes: i64 = self.conn.query_row("SELECT page_count * page_size AS b FROM pragma_page_count(), pragma_page_size()", [], |r| r.get(0))?;
        Ok((check, bytes))
    }

    /// 以前の JSON Lines（history/*.jsonl, actions.jsonl）を一度だけ取り込む（lib/db.js の importLegacy）。
    /// summarize は snapshot（data と findings）から summary を作る関数
    pub fn import_legacy(&self, dir: &Path, summarize: impl Fn(&Value) -> Value) -> std::io::Result<usize> {
        let mut n = 0;
        let hist = dir.join("history");
        if hist.is_dir() {
            for ent in std::fs::read_dir(&hist)? {
                let p = ent?.path();
                let Some(name) = p.file_name().and_then(|x| x.to_str()) else { continue };
                let Some(id) = name.strip_suffix(".jsonl") else { continue };
                for line in std::fs::read_to_string(&p)?.lines().filter(|l| !l.is_empty()) {
                    // 壊れた行は飛ばす
                    let Ok(e) = serde_json::from_str::<Value>(line) else { continue };
                    let at = e.get("at").and_then(Value::as_f64).map(|x| x as i64);
                    let Some(at) = at else { continue };
                    let ok = self.add_snapshot(
                        id,
                        at,
                        e.get("wall_s").and_then(Value::as_f64),
                        e.get("score").and_then(Value::as_i64),
                        e.get("findings").unwrap_or(&Value::Array(vec![])),
                        e.get("data").unwrap_or(&Value::Null),
                        &summarize(&e),
                    );
                    if ok.is_ok() {
                        n += 1;
                    }
                }
            }
            std::fs::rename(&hist, dir.join("history.imported"))?;
        }
        let af = dir.join("actions.jsonl");
        if af.is_file() {
            for line in std::fs::read_to_string(&af)?.lines().filter(|l| !l.is_empty()) {
                // 重複・壊れた行は飛ばす
                if let Ok(e) = serde_json::from_str::<Value>(line)
                    && self.add_action(&e).is_ok()
                {
                    n += 1;
                }
            }
            std::fs::rename(&af, dir.join("actions.jsonl.imported"))?;
        }
        Ok(n)
    }

    /// 保持期限を過ぎたものを消す（ログ30日・状態の記録180日）
    pub fn prune(&self, now: i64) -> Result<()> {
        self.conn.execute("DELETE FROM check_events WHERE ts < ?", [now - 180 * 86_400_000])?;
        let cut = now - LOG_RETENTION_DAYS * 86_400_000;
        self.tx(|c| {
            c.execute(
                "INSERT INTO logs_fts (logs_fts, rowid, message, provider) SELECT 'delete', id, message, coalesce(provider, '') FROM logs WHERE ts < ?",
                [cut],
            )?;
            c.execute("DELETE FROM logs WHERE ts < ?", [cut])?;
            Ok(())
        })
    }
}

/// JSON の値を SQLite に渡す形に。node:sqlite と同じく、数はすべて REAL（double）で渡す
/// （INTEGER の列では整数に、TEXT の列では "12345.0" になる。Electron 版が書く値と揃える）
pub(crate) fn sql(v: Option<&Value>) -> SqlValue {
    match v {
        None | Some(Value::Null) => SqlValue::Null,
        Some(Value::Bool(b)) => SqlValue::Integer(i64::from(*b)),
        Some(Value::Number(n)) => SqlValue::Real(n.as_f64().unwrap_or(f64::NAN)),
        Some(Value::String(s)) => SqlValue::Text(s.clone()),
        Some(other) => SqlValue::Text(other.to_string()),
    }
}

/// 1行を JSON のオブジェクトに（列名がキー）
pub(crate) fn row_json(row: &rusqlite::Row<'_>) -> Result<Value> {
    let stmt: &rusqlite::Statement<'_> = row.as_ref();
    let mut m = Map::new();
    for i in 0..stmt.column_count() {
        let name = stmt.column_name(i)?.to_string();
        m.insert(name, ref_json(row.get_ref(i)?));
    }
    Ok(Value::Object(m))
}

pub(crate) fn ref_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => Value::from(n),
        ValueRef::Real(f) => crate::js::jnum(f),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Array(b.iter().map(|x| Value::from(*x)).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_fts5_and_meta_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        s.set_meta("schedule", &serde_json::json!({ "enabled": false, "probe_minutes": 30 })).unwrap();
        assert_eq!(s.get_meta("schedule").unwrap().unwrap()["probe_minutes"], 30);
        assert_eq!(s.get_meta("missing").unwrap(), None);
        let (check, bytes) = s.integrity().unwrap();
        assert_eq!(check, "ok");
        assert!(bytes > 0);
        // 移行は一度だけ
        assert_eq!(s.get_meta("migr_win_ts_v1").unwrap(), Some(Value::Bool(true)));
    }
}
