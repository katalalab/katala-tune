mod candidates;
mod facts;
mod ui;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
pub use ui::TopologyUiRequest;

use super::{Result, Store, row_json};
use crate::topology::adapters::github::GithubCache;
use crate::topology::{Edge, Entity, ScopeStatus, SourceCacheEntry, SourceCacheKey, TopologySnapshot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyLease {
    pub holder: String,
    pub generation: i64,
}

pub const TOPOLOGY_HISTORY_KEEP: i64 = 200;
pub const TOPOLOGY_SOURCE_CACHE_KEEP: i64 = 4_096;
pub const TOPOLOGY_GITHUB_CACHE_KEEP: i64 = 256;
const TOPOLOGY_PROGRESS_MAX_BYTES: usize = 8_192;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS docker_candidate_sequence (singleton INTEGER PRIMARY KEY CHECK(singleton=1), ordinal INTEGER NOT NULL);
INSERT OR IGNORE INTO docker_candidate_sequence (singleton,ordinal) VALUES (1,0);
CREATE TABLE IF NOT EXISTS docker_candidate_attempts (attempt_id TEXT PRIMARY KEY, ordinal INTEGER NOT NULL UNIQUE, holder TEXT NOT NULL, writer_generation INTEGER NOT NULL, reserved_at INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS docker_candidate_scopes (scope_id TEXT PRIMARY KEY, latest_ordinal INTEGER NOT NULL, latest_attempt TEXT NOT NULL, latest_status TEXT NOT NULL, complete_ordinal INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS docker_candidates (scope_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL, ordinal INTEGER NOT NULL, writer_generation INTEGER NOT NULL, payload TEXT NOT NULL, digest TEXT NOT NULL, bytes INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS topology_accepted_facts (scope_id TEXT PRIMARY KEY, accepted_run_id TEXT NOT NULL, facts_observed_at_ms INTEGER, version INTEGER NOT NULL DEFAULT 1) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS topology_runs (run_id TEXT PRIMARY KEY, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS topology_scopes (run_id TEXT NOT NULL, scope_id TEXT NOT NULL, status TEXT NOT NULL, PRIMARY KEY (run_id, scope_id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS topology_entities (scope_id TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL, label TEXT NOT NULL, attributes TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (scope_id, id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS topology_edges (scope_id TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL, from_id TEXT NOT NULL, to_id TEXT NOT NULL, attributes TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (scope_id, id)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS topology_entities_ui_id ON topology_entities(id,scope_id);
CREATE INDEX IF NOT EXISTS topology_edges_ui_id ON topology_edges(id,scope_id);
CREATE TABLE IF NOT EXISTS topology_changes (run_id TEXT NOT NULL, scope_id TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL, change_kind TEXT NOT NULL, PRIMARY KEY (run_id, scope_id, id, change_kind)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS topology_observations (run_id TEXT NOT NULL, scope_id TEXT NOT NULL, status TEXT NOT NULL, observed_at INTEGER NOT NULL, counters TEXT NOT NULL, failures TEXT NOT NULL, PRIMARY KEY (run_id, scope_id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS topology_refresh_lease (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), holder TEXT NOT NULL, generation INTEGER NOT NULL DEFAULT 1, heartbeat_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS topology_source_cache (source_id TEXT NOT NULL, content_sha256 TEXT NOT NULL, parser_version TEXT NOT NULL, kind TEXT NOT NULL, revision TEXT, fetched_at INTEGER NOT NULL, facts TEXT NOT NULL, PRIMARY KEY (source_id, content_sha256, parser_version, kind)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS topology_github_cache (cache_key TEXT PRIMARY KEY, fetched_at INTEGER NOT NULL, stored_at INTEGER NOT NULL, cache TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS topology_progress (scope_id TEXT PRIMARY KEY, cursor TEXT NOT NULL, updated_at INTEGER NOT NULL) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS docker_ui_envelopes (scope_id TEXT PRIMARY KEY, envelope TEXT NOT NULL, digest TEXT NOT NULL, ordinal INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS docker_ui_rows (scope_id TEXT NOT NULL, row_type TEXT NOT NULL, id TEXT NOT NULL, kind TEXT NOT NULL, search TEXT NOT NULL, from_id TEXT NOT NULL, to_id TEXT NOT NULL, display TEXT NOT NULL, digest TEXT NOT NULL, PRIMARY KEY(scope_id,row_type,id)) WITHOUT ROWID;
";

// Optional storage may belong to an unknown version. Never index it blindly.
pub(super) fn initialize_optional_indexes(db: &Connection) -> Result<()> {
    ui::initialize_index(db)
}

fn project_rows(rows: Vec<Value>) -> Vec<Value> {
    let mut projected = std::collections::BTreeMap::<String, Value>::new();
    for mut row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str).map(str::to_owned) else { continue };
        let scope = row.get("scope_id").and_then(Value::as_str).map(str::to_owned);
        if let Some(attributes) = row.get("attributes").and_then(Value::as_str).and_then(|value| serde_json::from_str(value).ok()) {
            row["attributes"] = attributes;
        }
        match projected.get_mut(&id) {
            Some(existing) => {
                if let Some(scope) = scope {
                    let scopes = existing["scope_ids"].as_array_mut().expect("projection scopes");
                    if !scopes.iter().any(|value| value.as_str() == Some(&scope)) {
                        scopes.push(Value::String(scope));
                    }
                }
            }
            None => {
                let scopes = scope.map(|scope| vec![Value::String(scope)]).unwrap_or_default();
                if let Some(object) = row.as_object_mut() {
                    object.insert("scope_ids".into(), Value::Array(scopes));
                }
                projected.insert(id, row);
            }
        }
    }
    projected.into_values().collect()
}

fn receipt_rows(mut rows: Vec<Value>) -> Vec<Value> {
    for row in &mut rows {
        for key in ["counters", "failures"] {
            if let Some(value) = row.get(key).and_then(Value::as_str).and_then(|value| serde_json::from_str(value).ok()) {
                row[key] = value;
            }
        }
    }
    rows
}

fn status(status: ScopeStatus) -> &'static str {
    match status {
        ScopeStatus::Complete => "complete",
        ScopeStatus::Partial => "partial",
        ScopeStatus::Failed => "failed",
        ScopeStatus::Excluded => "excluded",
    }
}
fn entity_json(entity: &Entity) -> String {
    entity.attributes.to_string()
}
fn edge_json(edge: &Edge) -> String {
    edge.attributes.to_string()
}

impl Store {
    /// A complete observation replaces one scope atomically. Partial and failed
    /// observations are receipts only: they never infer removals from missing data.
    pub fn record_topology(&self, run_id: &str, snapshot: &TopologySnapshot, at: i64) -> Result<Vec<Value>> {
        self.tx(|db| {
            let changes = record_inner(db, run_id, snapshot, at)?;
            prune_history_inner(db, TOPOLOGY_HISTORY_KEEP)?;
            Ok(changes)
        })
    }

    pub fn topology_entities(&self, scope_id: &str) -> Result<Vec<Value>> {
        let mut s = self.conn().prepare("SELECT scope_id,id,kind,label,attributes,updated_at FROM topology_entities WHERE scope_id = ? ORDER BY id")?;
        s.query_map([scope_id], row_json)?.collect()
    }

    /// Scope discovery is used only after the same complete local root-set
    /// generation. Callers record a partial receipt for unobserved scopes;
    /// their last successful graph remains intact.
    pub fn topology_scope_ids_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let escaped = prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let mut statement = self.conn().prepare("SELECT DISTINCT scope_id FROM topology_observations WHERE scope_id LIKE ? ESCAPE '\\' ORDER BY scope_id")?;
        statement.query_map([format!("{escaped}%")], |row| row.get(0))?.collect()
    }

    /// Accepted graph and receipts only; optional candidate storage is never read.
    pub fn topology_accepted_view(&self) -> Result<Value> {
        self.topology_read_view(false)
    }

    /// Explicit full internal view, including admitted Partial candidates.
    pub fn topology_view(&self) -> Result<Value> {
        self.topology_read_view(true)
    }

    fn topology_read_view(&self, include_candidates: bool) -> Result<Value> {
        // Bind attributes, adopted facts and receipts to one read-only SQLite snapshot.
        let read = self.conn().unchecked_transaction()?;
        let mut entities = self.conn().prepare("SELECT scope_id,id,kind,label,attributes,updated_at FROM topology_entities ORDER BY id,scope_id")?;
        let entities = project_rows(facts::annotate(self.conn(), entities.query_map([], row_json)?.collect::<Result<Vec<_>>>()?)?);
        let mut edges = self.conn().prepare("SELECT scope_id,id,kind,from_id,to_id,attributes,updated_at FROM topology_edges ORDER BY id,scope_id")?;
        let edges = project_rows(facts::annotate(self.conn(), edges.query_map([], row_json)?.collect::<Result<Vec<_>>>()?)?);
        // `scopes` is one current receipt per scope. `history` remains separately
        // bounded so the UI never mistakes historical rows for current coverage.
        let latest_sql = "SELECT run_id,scope_id,status,observed_at,counters,failures FROM topology_observations current WHERE NOT EXISTS (SELECT 1 FROM topology_observations newer WHERE newer.scope_id=current.scope_id AND (newer.observed_at>current.observed_at OR (newer.observed_at=current.observed_at AND newer.run_id>current.run_id))) ORDER BY observed_at DESC,run_id DESC";
        let mut scopes = self.conn().prepare(latest_sql)?;
        let scopes = receipt_rows(scopes.query_map([], row_json)?.collect::<Result<Vec<_>>>()?);
        let mut history = self.conn().prepare(
            "SELECT run_id,scope_id,status,observed_at,counters,failures FROM topology_observations ORDER BY observed_at DESC,run_id DESC LIMIT 200",
        )?;
        let history = receipt_rows(history.query_map([], row_json)?.collect::<Result<Vec<_>>>()?);
        let mut changes = self.conn().prepare("SELECT changes.run_id,changes.scope_id,changes.id,changes.kind,changes.change_kind,runs.at FROM topology_changes changes JOIN topology_runs runs ON runs.run_id=changes.run_id ORDER BY runs.at DESC,changes.scope_id,changes.id LIMIT 100")?;
        let changes = changes.query_map([], row_json)?.collect::<Result<Vec<_>>>()?;
        let mut view = json!({"schema":"katala_topology.v1","entities":entities,"edges":edges,"scopes":scopes,"history":history,"changes":changes});
        if include_candidates {
            view["candidates"] = Value::Array(candidates::read_candidates(self.conn(), crate::db::now_ms())?);
        }
        read.commit()?;
        Ok(view)
    }

    pub fn topology_receipts(&self, scope_id: &str) -> Result<Vec<Value>> {
        let mut statement = self.conn().prepare(
            "SELECT run_id,scope_id,status,observed_at,counters,failures FROM topology_observations WHERE scope_id=? ORDER BY observed_at DESC,run_id DESC",
        )?;
        statement.query_map([scope_id], row_json)?.collect()
    }

    pub fn topology_latest_success(&self, scope_id: &str) -> Result<Option<Value>> {
        self.topology_latest(scope_id, "status='complete'")
    }

    pub fn topology_latest_failure(&self, scope_id: &str) -> Result<Option<Value>> {
        self.topology_latest(scope_id, "status IN ('partial','failed')")
    }

    fn topology_latest(&self, scope_id: &str, clause: &str) -> Result<Option<Value>> {
        let sql = format!(
            "SELECT run_id,scope_id,status,observed_at,counters,failures FROM topology_observations WHERE scope_id=? AND {clause} ORDER BY observed_at DESC,run_id DESC LIMIT 1"
        );
        self.conn().query_row(&sql, [scope_id], row_json).optional()
    }

    /// Retention follows the recorded observation time, then run id only as a
    /// deterministic tie breaker. A run hash never describes chronology.
    pub fn prune_topology_history(&self, keep_per_scope: i64) -> Result<()> {
        self.tx(|db| prune_history_inner(db, keep_per_scope))
    }

    pub fn acquire_topology_lease_fenced(&self, holder: &str, now: i64, ttl_ms: i64) -> Result<Option<TopologyLease>> {
        if holder.is_empty() || ttl_ms <= 0 || now.checked_add(ttl_ms).is_none() {
            return Err(rusqlite::Error::InvalidParameterName("invalid topology lease".into()));
        }
        self.conn().execute_batch("BEGIN IMMEDIATE")?;
        let answer = (|| -> Result<Option<TopologyLease>> {
            let current: Option<(String, i64, i64)> = self
                .conn()
                .query_row("SELECT holder, generation, expires_at FROM topology_refresh_lease WHERE singleton = 1", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?;
            match current {
                Some((old, _generation, expires)) if old != holder && expires > now => Ok(None),
                Some((old, generation, _)) => {
                    let next = if old == holder {
                        generation
                    } else {
                        generation.checked_add(1).ok_or_else(|| rusqlite::Error::InvalidParameterName("topology lease generation overflow".into()))?
                    };
                    self.conn().execute(
                        "UPDATE topology_refresh_lease SET holder=?, generation=?, heartbeat_at=?, expires_at=? WHERE singleton=1",
                        params![holder, next, now, now + ttl_ms],
                    )?;
                    Ok(Some(TopologyLease { holder: holder.into(), generation: next }))
                }
                None => {
                    self.conn().execute(
                        "INSERT INTO topology_refresh_lease (singleton,holder,generation,heartbeat_at,expires_at) VALUES (1,?,1,?,?)",
                        params![holder, now, now + ttl_ms],
                    )?;
                    Ok(Some(TopologyLease { holder: holder.into(), generation: 1 }))
                }
            }
        })();
        match answer {
            Ok(value) => match self.conn().execute_batch("COMMIT") {
                Ok(()) => Ok(value),
                Err(error) => {
                    let _ = self.conn().execute_batch("ROLLBACK");
                    Err(error)
                }
            },
            Err(error) => {
                let _ = self.conn().execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Renew only the exact generation. An expiry equal to `now` is already stale.
    pub fn heartbeat_topology_lease(&self, lease: &TopologyLease, now: i64, ttl_ms: i64) -> Result<bool> {
        if lease.holder.is_empty() || ttl_ms <= 0 || now.checked_add(ttl_ms).is_none() {
            return Err(rusqlite::Error::InvalidParameterName("invalid topology lease heartbeat".into()));
        }
        let changed = self.conn().execute(
            "UPDATE topology_refresh_lease SET heartbeat_at=?, expires_at=? WHERE singleton=1 AND holder=? AND generation=? AND expires_at>?",
            params![now, now + ttl_ms, lease.holder, lease.generation, now],
        )?;
        Ok(changed == 1)
    }

    pub fn record_topology_fenced(&self, lease: &TopologyLease, run_id: &str, snapshot: &TopologySnapshot, at: i64) -> Result<Vec<Value>> {
        self.tx(|db| {
            assert_lease(db, lease, at)?;
            let changes = record_inner(db, run_id, snapshot, at)?;
            prune_history_inner(db, TOPOLOGY_HISTORY_KEEP)?;
            Ok(changes)
        })
    }

    /// Store normalized parse facts. `fetched_at_ms` is immutable for an
    /// existing key so a TTL cache hit cannot impersonate a fresh fetch.
    pub fn save_source_cache_fenced(&self, lease: &TopologyLease, entry: &SourceCacheEntry, now: i64) -> Result<()> {
        self.tx(|db| {
            assert_lease(db, lease, now)?;
            save_source_cache_inner(db, entry)?;
            prune_source_cache_inner(db, TOPOLOGY_SOURCE_CACHE_KEEP)
        })
    }

    pub fn source_cache(&self, key: &SourceCacheKey) -> Result<Option<SourceCacheEntry>> {
        self.conn()
            .query_row(
                "SELECT revision,fetched_at,facts FROM topology_source_cache WHERE source_id=? AND content_sha256=? AND parser_version=? AND kind=?",
                params![key.source_id, key.content_sha256, key.parser_version, key.kind],
                |row| {
                    let facts: String = row.get(2)?;
                    let facts = serde_json::from_str(&facts)
                        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error)))?;
                    Ok(SourceCacheEntry { key: key.clone(), revision: row.get(0)?, fetched_at_ms: row.get(1)?, facts })
                },
            )
            .optional()
    }

    pub fn prune_source_cache(&self, keep: i64) -> Result<()> {
        self.tx(|db| prune_source_cache_inner(db, keep))
    }

    /// GithubCache's type intentionally excludes raw API payloads and source
    /// text. It is fenced with graph/cache saves from the same refresh.
    pub fn save_github_cache_fenced(&self, lease: &TopologyLease, cache_key: &str, cache: &GithubCache, fetched_at_ms: i64, now: i64) -> Result<()> {
        if cache_key.is_empty() {
            return Err(rusqlite::Error::InvalidParameterName("empty github cache key".into()));
        }
        let encoded = serde_json::to_string(cache).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        self.tx(|db| {
            assert_lease(db, lease, now)?;
            db.execute("INSERT INTO topology_github_cache(cache_key,fetched_at,stored_at,cache) VALUES(?,?,?,?) ON CONFLICT(cache_key) DO UPDATE SET fetched_at=excluded.fetched_at,stored_at=excluded.stored_at,cache=excluded.cache", params![cache_key, fetched_at_ms, now, encoded])?;
            prune_github_cache_inner(db, TOPOLOGY_GITHUB_CACHE_KEEP)
        })
    }

    pub fn github_cache(&self, cache_key: &str) -> Result<Option<GithubCache>> {
        self.conn()
            .query_row("SELECT cache FROM topology_github_cache WHERE cache_key=?", [cache_key], |row| {
                let value: String = row.get(0)?;
                serde_json::from_str(&value).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))
            })
            .optional()
    }

    /// A bounded, opaque continuation cursor for fair local discovery. The
    /// caller owns path/config validation before it can resume a scan.
    pub fn topology_progress(&self, scope_id: &str) -> Result<Option<Value>> {
        self.conn()
            .query_row("SELECT cursor FROM topology_progress WHERE scope_id=?", [scope_id], |row| {
                let cursor: String = row.get(0)?;
                serde_json::from_str(&cursor).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))
            })
            .optional()
    }

    pub fn save_topology_progress_fenced(&self, lease: &TopologyLease, scope_id: &str, cursor: &Value, now: i64) -> Result<()> {
        if scope_id.is_empty() {
            return Err(rusqlite::Error::InvalidParameterName("empty topology progress scope".into()));
        }
        let encoded = serde_json::to_string(cursor).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        if encoded.len() > TOPOLOGY_PROGRESS_MAX_BYTES || progress_has_raw_field(cursor) {
            return Err(rusqlite::Error::InvalidParameterName("invalid topology progress cursor".into()));
        }
        self.tx(|db| {
            assert_lease(db, lease, now)?;
            db.execute(
                "INSERT INTO topology_progress(scope_id,cursor,updated_at) VALUES(?,?,?) ON CONFLICT(scope_id) DO UPDATE SET cursor=excluded.cursor,updated_at=excluded.updated_at",
                params![scope_id, encoded, now],
            )?;
            Ok(())
        })
    }

    /// Keep the row and its generation to prevent release/reacquire ABA.
    pub fn release_topology_lease_fenced(&self, lease: &TopologyLease) -> Result<()> {
        self.conn().execute(
            "UPDATE topology_refresh_lease SET expires_at=0 WHERE singleton=1 AND holder=? AND generation=?",
            params![lease.holder, lease.generation],
        )?;
        Ok(())
    }
}

fn assert_lease(db: &Connection, lease: &TopologyLease, now: i64) -> Result<()> {
    let valid: Option<i64> = db
        .query_row(
            "SELECT 1 FROM topology_refresh_lease WHERE singleton=1 AND holder=? AND generation=? AND expires_at>?",
            params![lease.holder, lease.generation, now],
            |row| row.get(0),
        )
        .optional()?;
    valid.ok_or_else(|| rusqlite::Error::InvalidParameterName("expired topology lease".into())).map(|_| ())
}

fn progress_has_raw_field(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(progress_has_raw_field),
        Value::Object(values) => {
            values.iter().any(|(key, value)| matches!(key.as_str(), "raw" | "body" | "content" | "manifest" | "config") || progress_has_raw_field(value))
        }
        _ => false,
    }
}

fn save_source_cache_inner(db: &Connection, entry: &SourceCacheEntry) -> Result<()> {
    let facts = serde_json::to_string(&entry.facts).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    db.execute(
        "INSERT INTO topology_source_cache(source_id,content_sha256,parser_version,kind,revision,fetched_at,facts) VALUES(?,?,?,?,?,?,?) ON CONFLICT(source_id,content_sha256,parser_version,kind) DO UPDATE SET revision=excluded.revision,facts=excluded.facts",
        params![entry.key.source_id, entry.key.content_sha256, entry.key.parser_version, entry.key.kind, entry.revision, entry.fetched_at_ms, facts],
    )?;
    Ok(())
}

fn prune_history_inner(db: &Connection, keep_per_scope: i64) -> Result<()> {
    if keep_per_scope < 1 {
        return Err(rusqlite::Error::InvalidParameterName("invalid topology history retention".into()));
    }
    let scopes =
        db.prepare("SELECT DISTINCT scope_id FROM topology_observations")?.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>>>()?;
    for scope_id in scopes {
        let receipts = db
            .prepare("SELECT run_id,status FROM topology_observations WHERE scope_id=? ORDER BY observed_at DESC,run_id DESC")?
            .query_map(params![scope_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>>>()?;
        // Preserve the most recent successful receipt alongside the current
        // graph when a later failure streak would otherwise prune it.
        let mut retained = std::collections::BTreeSet::new();
        if let Some((run_id, _)) = receipts.iter().find(|(_, status)| status == "complete") {
            retained.insert(run_id.clone());
        }
        for (run_id, _) in &receipts {
            if retained.len() >= keep_per_scope as usize {
                break;
            }
            retained.insert(run_id.clone());
        }
        for (run_id, _) in receipts {
            if retained.contains(&run_id) {
                continue;
            }
            db.execute("DELETE FROM topology_changes WHERE run_id=? AND scope_id=?", params![run_id, scope_id])?;
            db.execute("DELETE FROM topology_scopes WHERE run_id=? AND scope_id=?", params![run_id, scope_id])?;
            db.execute("DELETE FROM topology_observations WHERE run_id=? AND scope_id=?", params![run_id, scope_id])?;
            db.execute("DELETE FROM topology_runs WHERE run_id=? AND NOT EXISTS(SELECT 1 FROM topology_scopes WHERE run_id=?)", params![run_id, run_id])?;
        }
    }
    Ok(())
}

fn prune_source_cache_inner(db: &Connection, keep: i64) -> Result<()> {
    if keep < 1 {
        return Err(rusqlite::Error::InvalidParameterName("invalid source cache retention".into()));
    }
    db.execute("DELETE FROM topology_source_cache WHERE (source_id,content_sha256,parser_version,kind) IN (SELECT source_id,content_sha256,parser_version,kind FROM topology_source_cache ORDER BY fetched_at DESC,source_id,content_sha256,parser_version,kind LIMIT -1 OFFSET ?)", [keep])?;
    Ok(())
}

fn prune_github_cache_inner(db: &Connection, keep: i64) -> Result<()> {
    if keep < 1 {
        return Err(rusqlite::Error::InvalidParameterName("invalid github cache retention".into()));
    }
    db.execute("DELETE FROM topology_github_cache WHERE cache_key IN (SELECT cache_key FROM topology_github_cache ORDER BY fetched_at DESC,cache_key LIMIT -1 OFFSET ?)", [keep])?;
    Ok(())
}

fn record_inner(db: &Connection, run_id: &str, snapshot: &TopologySnapshot, at: i64) -> Result<Vec<Value>> {
    let mut changes = Vec::new();
    db.execute("INSERT OR IGNORE INTO topology_runs (run_id, at) VALUES (?, ?)", params![run_id, at])?;
    db.execute("INSERT INTO topology_scopes (run_id, scope_id, status) VALUES (?, ?, ?)", params![run_id, snapshot.scope_id, status(snapshot.status)])?;
    let failures = serde_json::to_string(&snapshot.failures).unwrap_or_else(|_| "[]".into());
    db.execute(
        "INSERT INTO topology_observations (run_id,scope_id,status,observed_at,counters,failures) VALUES (?,?,?,?,?,?)",
        params![
            run_id,
            snapshot.scope_id,
            status(snapshot.status),
            snapshot.observed_at_ms,
            serde_json::to_string(&snapshot.counters).unwrap_or_else(|_| "{}".into()),
            failures
        ],
    )?;
    candidates::record_attempt(db, run_id, snapshot, at)?;
    if snapshot.status != ScopeStatus::Complete {
        return Ok(changes);
    }
    let mut old = std::collections::BTreeMap::new();
    let mut query = db.prepare("SELECT id, kind, label, attributes FROM topology_entities WHERE scope_id = ?")?;
    for row in query.query_map([&snapshot.scope_id], |r| {
        let attributes: String = r.get(3)?;
        Ok((
            r.get::<_, String>(0)?,
            format!("{}:{}:{}", r.get::<_, String>(1)?, r.get::<_, String>(2)?, semantic_json(&serde_json::from_str(&attributes).unwrap_or(Value::Null))),
        ))
    })? {
        let (id, value) = row?;
        old.insert(id, value);
    }
    for entity in &snapshot.entities {
        let value = format!("{}:{}:{}", entity.kind.slug(), entity.label, semantic_json(&entity.attributes));
        match old.remove(&entity.id) {
            None => changes.push(json!({"id":entity.id,"kind":"entity","change":"add"})),
            Some(previous) if previous != value => changes.push(json!({"id":entity.id,"kind":"entity","change":"update"})),
            _ => {}
        }
    }
    changes.extend(old.keys().map(|id| json!({"id":id,"kind":"entity","change":"remove"})));
    let mut old_edges = std::collections::BTreeMap::new();
    let mut query = db.prepare("SELECT id, kind, from_id, to_id, attributes FROM topology_edges WHERE scope_id = ?")?;
    for row in query.query_map([&snapshot.scope_id], |r| {
        let attributes: String = r.get(4)?;
        Ok((
            r.get::<_, String>(0)?,
            format!(
                "{}:{}:{}:{}",
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                semantic_json(&serde_json::from_str(&attributes).unwrap_or(Value::Null))
            ),
        ))
    })? {
        let (id, value) = row?;
        old_edges.insert(id, value);
    }
    for edge in &snapshot.edges {
        let value = format!("{}:{}:{}:{}", edge.kind.slug(), edge.from, edge.to, semantic_json(&edge.attributes));
        match old_edges.remove(&edge.id) {
            None => changes.push(json!({"id":edge.id,"kind":"edge","change":"add"})),
            Some(previous) if previous != value => changes.push(json!({"id":edge.id,"kind":"edge","change":"update"})),
            _ => {}
        }
    }
    changes.extend(old_edges.keys().map(|id| json!({"id":id,"kind":"edge","change":"remove"})));
    db.execute("DELETE FROM topology_entities WHERE scope_id = ?", [&snapshot.scope_id])?;
    db.execute("DELETE FROM topology_edges WHERE scope_id = ?", [&snapshot.scope_id])?;
    for entity in &snapshot.entities {
        db.execute(
            "INSERT INTO topology_entities (scope_id,id,kind,label,attributes,updated_at) VALUES (?,?,?,?,?,?)",
            params![snapshot.scope_id, entity.id, entity.kind.slug(), entity.label, entity_json(entity), at],
        )?;
    }
    for edge in &snapshot.edges {
        db.execute(
            "INSERT INTO topology_edges (scope_id,id,kind,from_id,to_id,attributes,updated_at) VALUES (?,?,?,?,?,?,?)",
            params![snapshot.scope_id, edge.id, edge.kind.slug(), edge.from, edge.to, edge_json(edge), at],
        )?;
    }
    facts::record(db, run_id, snapshot)?;
    for change in &changes {
        db.execute(
            "INSERT INTO topology_changes (run_id,scope_id,id,kind,change_kind) VALUES (?,?,?,?,?)",
            params![run_id, snapshot.scope_id, change["id"].as_str(), change["kind"].as_str(), change["change"].as_str()],
        )?;
    }
    Ok(changes)
}

fn semantic_json(value: &Value) -> String {
    fn strip(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(strip).collect()),
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .filter(|(key, _)| !matches!(key.as_str(), "observed_at_ms" | "fetched_at_ms" | "freshness" | "last_seen_at_ms"))
                    .map(|(key, value)| (key.clone(), strip(value)))
                    .collect(),
            ),
            value => value.clone(),
        }
    }
    strip(value).to_string()
}
