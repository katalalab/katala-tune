//! ローカルの保存先（lib/db.js と同じファイル・同じ表）。rusqlite（bundled、FTS5 あり）。
//! Electron 版と同じ `<userData>/data/katala-tune.db` を開くので、どちらのアプリからも同じ内容が読める。
//! 表の名前と列は katala-fleet の中央 DB 設計に合わせてあり、後でそのまま送れる形にしている。
//!
//! 表は機能ごとのファイルに分けている（snapshots / actions / logs / checks / meta）。
//! 機能を足すとき（道具の棚卸し inventory・AI エージェントのセッション ai_sessions など）は、
//! `db/<機能>.rs` に `pub(super) const SCHEMA` と `impl Store { ... }` を書き、下の `SCHEMA_PARTS` に足す。
//! Electron 版と同じ DB を共有するので、表の定義は lib/db.js と揃える（CREATE TABLE IF NOT EXISTS のまま）。

mod actions;
mod ai_limits;
mod ai_sessions;
mod ai_usage;
mod checks;
mod inventory;
mod logs;
pub mod metrics;
mod netsec;
pub mod provenance;
mod snapshots;

use std::path::Path;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

pub use ai_limits::{LimitWindow, LimitsReport};
pub use ai_sessions::{AiIngested, SESSION_PAGE_MAX, Session as AiSession, TOK_KEYS, hour_deltas};
pub use ai_usage::{UsageRow, UsageWindow, dup_delta, span_state, summarize_usage};
pub use checks::{Change, Check};
pub use inventory::InventorySaved;
pub use logs::{LogCount, LogRow, TopSignature};
pub use netsec::NET_EVENT_DAYS;
pub use provenance::{RunMeta, SpanIn, sha256_hex};
pub use snapshots::Snapshot;

pub const DB_FILE: &str = "katala-tune.db";
pub const LOG_RETENTION_DAYS: i64 = 30;
pub const SNAPSHOT_KEEP: i64 = 200;
/// logs-query で一度に返す上限（lib/db.js と同じ）
pub const QUERY_LIMIT_MAX: i64 = 2000;
/// logs-signatures で一度に返す上限（画面には集計の上位だけ返す）
pub const SIGNATURE_LIMIT_MAX: i64 = 500;

const PRAGMAS: &str = "PRAGMA journal_mode = WAL;\nPRAGMA synchronous = NORMAL;\n";
const SCHEMA_PARTS: &[&str] = &[
    snapshots::SCHEMA,
    actions::SCHEMA,
    logs::SCHEMA,
    checks::SCHEMA,
    META_SCHEMA,
    inventory::SCHEMA,
    ai_sessions::SCHEMA,
    netsec::SCHEMA,
    provenance::SCHEMA,
    ai_limits::SCHEMA,
    metrics::SCHEMA,
];
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
        // 出どころの台帳・費用の列を足す前の DB の ai_claude_messages / ai_sessions に列が無い（CREATE TABLE IF NOT EXISTS では足されない）ので、後で足す
        let old_ai = conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ai_sessions'")?.exists([])?;
        conn.execute_batch(&SCHEMA_PARTS.concat())?;
        let s = Store { conn };
        s.add_columns()?;
        s.migrate()?;
        s.tx(|c| ai_sessions::migrate(c, old_ai))?;
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

    // 移行: 後から足した列（CREATE TABLE IF NOT EXISTS は既存の表に列を足さない）。lib/db.js と同じ列を足す
    fn add_columns(&self) -> Result<()> {
        for (table, name, ddl) in [
            ("logs", "occurrences", "INTEGER NOT NULL DEFAULT 1"),
            ("log_cursors", "fail_streak", "INTEGER NOT NULL DEFAULT 0"),
            ("log_cursors", "note", "TEXT"),
        ] {
            let has: i64 = self.conn.query_row(&format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?"), [name], |r| r.get(0))?;
            if has == 0 {
                self.conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {name} {ddl}"))?;
            }
        }
        Ok(())
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

    /// 保持期限を過ぎたものを消す（ログ30日・状態の記録180日・宛先30日）
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
        })?;
        // ネットワークとセキュリティの記録（宛先 30 日・常駐の増減 180 日。db/netsec.rs）
        self.net_prune(now)
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
        assert_eq!(s.get_meta("migr_ai_provenance_v1").unwrap(), Some(Value::Bool(true)));
    }

    #[test]
    fn old_ai_tables_get_new_columns_and_reread_from_the_start() {
        // 出どころの台帳・費用の列を足す前の DB（ai_claude_messages に span_id・model・cw1h が無い。続きの位置がある）
        let dir = std::env::temp_dir().join(format!("kt-db-migr-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let h = 1_800_000_000_000 / 3_600_000;
        {
            let c = Connection::open(dir.join(DB_FILE)).unwrap();
            c.execute_batch(
                "CREATE TABLE ai_sessions (node_id TEXT NOT NULL, file TEXT NOT NULL, tool TEXT NOT NULL, session_id TEXT, parent_id TEXT, cwd TEXT, version TEXT,
                   origin TEXT, model TEXT, first_ts INTEGER, last_ts INTEGER, prompts INTEGER NOT NULL DEFAULT 0, assistant_msgs INTEGER NOT NULL DEFAULT 0,
                   tool_calls INTEGER NOT NULL DEFAULT 0, tool_errors INTEGER NOT NULL DEFAULT 0, turn_errors INTEGER NOT NULL DEFAULT 0, hook_errors INTEGER NOT NULL DEFAULT 0,
                   api_errors INTEGER NOT NULL DEFAULT 0, tok_in INTEGER NOT NULL DEFAULT 0, tok_out INTEGER NOT NULL DEFAULT 0, tok_cache_read INTEGER NOT NULL DEFAULT 0,
                   tok_cache_write INTEGER NOT NULL DEFAULT 0, tok_reasoning INTEGER NOT NULL DEFAULT 0, tokens_mode TEXT NOT NULL DEFAULT 'add',
                   tool_counts TEXT, tool_error_counts TEXT, prs TEXT, updated_at INTEGER NOT NULL, PRIMARY KEY (node_id, file)) WITHOUT ROWID;
                 INSERT INTO ai_sessions (node_id, file, tool, model, tok_out, updated_at) VALUES ('n1', 'claude:a.jsonl', 'claude', 'claude-opus-5-5', 5, 1);
                 CREATE TABLE ai_claude_messages (node_id TEXT NOT NULL, file TEXT NOT NULL, message_id TEXT NOT NULL, usage TEXT NOT NULL, hour INTEGER,
                   PRIMARY KEY (node_id, file, message_id)) WITHOUT ROWID;
                 CREATE TABLE ai_cursors (node_id TEXT PRIMARY KEY, files TEXT NOT NULL DEFAULT '{}', updated_at INTEGER, last_ok_at INTEGER, last_error TEXT, file_errors TEXT,
                   truncated INTEGER NOT NULL DEFAULT 0, no_python INTEGER NOT NULL DEFAULT 0, files_total INTEGER, files_changed INTEGER, bytes_pending INTEGER,
                   bytes_read INTEGER, elapsed_s REAL) WITHOUT ROWID;
                 INSERT INTO ai_cursors (node_id, files) VALUES ('n1', '{\"claude:a.jsonl\":5}');",
            )
            .unwrap();
            c.execute("INSERT INTO ai_claude_messages (node_id, file, message_id, usage, hour) VALUES ('n1', 'claude:a.jsonl', 'm', '[1,5,0,0,0]', ?)", [h])
                .unwrap();
        }
        let s = Store::open(&dir).unwrap();
        let old = s.ai_session("n1", "claude:a.jsonl").unwrap().unwrap();
        assert_eq!(old.tokens[1], 5, "前のセッションは残る");
        // 区間の無い続きの位置は渡さない（調査は最初から読み直す）。急いで読み直す
        assert_eq!(s.ai_state("n1").unwrap()["files"]["claude:a.jsonl"], serde_json::json!(0));
        assert!(s.ai_pending(&["n1".into()]).unwrap());
        assert!(!s.ai_pending(&["other".into()]).unwrap(), "取り込まない機体の古い印では急がない");
        // 応答ごとの量は残り、モデルが無い分はセッションのモデルで数える（1 時間のキャッシュ書きは 0）
        let w = UsageWindow { since_hour: h - 1, until_hour: h + 1, tz_ms: 0, node: None, tool: None };
        let rows = s.usage_rows(&w, true).unwrap();
        assert_eq!((rows.len(), rows[0].model.clone(), rows[0].tokens.output, rows[0].tokens.cache_write_1h), (1, Some("claude-opus-5-5".into()), 5, 0));
        drop(s);
        // 2 回目は何もしない
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.get_meta("migr_ai_provenance_v1").unwrap(), Some(Value::Bool(true)));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
