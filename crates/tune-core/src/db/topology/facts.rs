//! Timestamp binding for the graph actually adopted by a Complete write.
use super::{Result, TopologySnapshot};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub(super) fn record(db: &Connection, run_id: &str, snapshot: &TopologySnapshot) -> Result<()> {
    db.execute("INSERT INTO topology_accepted_facts (scope_id,accepted_run_id,facts_observed_at_ms,version) VALUES (?,?,?,1) ON CONFLICT(scope_id) DO UPDATE SET accepted_run_id=excluded.accepted_run_id,facts_observed_at_ms=excluded.facts_observed_at_ms,version=1",params![snapshot.scope_id,run_id,(snapshot.observed_at_ms>=0).then_some(snapshot.observed_at_ms)])?;
    Ok(())
}
pub(super) fn annotate(db: &Connection, rows: Vec<Value>) -> Result<Vec<Value>> {
    annotate_inner(db, rows, false)
}
pub(super) fn annotate_selected(db: &Connection, rows: Vec<Value>) -> Result<Vec<Value>> {
    annotate_inner(db, rows, true)
}
fn annotate_inner(db: &Connection, rows: Vec<Value>, selected: bool) -> Result<Vec<Value>> {
    let mut fields = db.prepare("PRAGMA table_info(topology_accepted_facts)")?;
    let columns = fields
        .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(5)?)))?
        .collect::<Result<Vec<_>>>()?;
    let expected = [("scope_id", "TEXT", 1, 1), ("accepted_run_id", "TEXT", 1, 0), ("facts_observed_at_ms", "INTEGER", 0, 0), ("version", "INTEGER", 1, 0)];
    let known = columns.len() == expected.len()
        && columns.iter().zip(expected).all(|((name, kind, not_null, pk), (n, k, nn, p))| name == n && kind == k && *not_null == nn && *pk == p);
    let mut metadata = std::collections::BTreeMap::new();
    if known {
        let scopes = rows.iter().filter_map(|row| row["scope_id"].as_str()).collect::<std::collections::BTreeSet<_>>();
        let restriction = if selected {
            format!(" AND scope_id IN ({})", scopes.iter().map(|s| format!("'{}'", s.replace('\'', "''"))).collect::<Vec<_>>().join(","))
        } else {
            String::new()
        };
        let mut statement=db.prepare(&format!("SELECT scope_id,accepted_run_id,facts_observed_at_ms FROM topology_accepted_facts WHERE typeof(scope_id)='text' AND typeof(accepted_run_id)='text' AND length(accepted_run_id) BETWEEN 1 AND 160 AND typeof(version)='integer' AND version=1 AND typeof(facts_observed_at_ms)='integer' AND facts_observed_at_ms>=0{restriction}"))?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))?;
        for row in rows {
            match row {
                Ok((scope, run, at)) => {
                    metadata.insert(scope, (run, at));
                }
                Err(
                    rusqlite::Error::InvalidColumnType(..)
                    | rusqlite::Error::FromSqlConversionFailure(..)
                    | rusqlite::Error::IntegralValueOutOfRange(..)
                    | rusqlite::Error::Utf8Error(..),
                ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(rows
        .into_iter()
        .map(|mut row| {
            let binding = row["scope_id"].as_str().and_then(|scope| metadata.get(scope));
            let (run, at) = binding.map_or((Value::Null, Value::Null), |(run, at)| (json!(run), json!(at)));
            row["accepted_run_id"] = run;
            row["facts_observed_at_ms"] = at;
            row
        })
        .collect())
}
