//! Bounded, independently materialized UI facts. The canonical archive is checked
//! only by the explicit full API; a UI read checks its selected projection rows.
use super::{Result, Store, facts, project_rows, receipt_rows, row_json};
use crate::topology::candidates::DockerCandidatePayload;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

const ENTITIES: usize = 200;
const EDGES: usize = 200;
const RESPONSE_BYTES: usize = 1024 * 1024;
const PROJECTION_BYTES: i64 = 64 * 1024 * 1024;
const PROJECTION_ROWS: i64 = 64 * (4096 + 16384);
const ROW_BYTES: usize = 4096;
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TopologyUiRequest {
    pub tab: String,
    pub kind: String,
    pub filter: String,
}
impl TopologyUiRequest {
    fn checked(&self) -> Result<Self> {
        let tab = if self.tab.is_empty() { "all" } else { &self.tab };
        let kind = if self.kind.is_empty() { "all" } else { &self.kind };
        if !["all", "hosts", "dependencies", "cicd"].contains(&tab)
            || kind.len() > 48
            || !kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            || self.filter.len() > 256
            || self.filter.chars().any(char::is_control)
        {
            return Err(invalid("invalid topology UI filter"));
        }
        Ok(Self { tab: tab.into(), kind: kind.into(), filter: self.filter.to_ascii_lowercase() })
    }
}
fn invalid(s: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(s.into())
}
fn text(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(char::is_control)
}
fn short(v: &Value, max: usize) -> Value {
    v.as_str().filter(|s| text(s, max)).map_or(Value::Null, |s| json!(s))
}
fn attributes(a: &Value) -> Value {
    let mut out = json!({});
    for key in ["content_sha256", "value_hash", "revision", "state", "health", "log_driver", "log_max_size"] {
        out[key] = short(&a[key], 160);
    }
    for key in ["memory_bytes", "nano_cpus", "log_max_file", "restart_count"] {
        out[key] = a[key].as_u64().map_or(Value::Null, |n| json!(n));
    }
    out["log_unbounded_candidate"] = a["log_unbounded_candidate"].as_bool().map_or(Value::Null, |b| json!(b));
    out["attributes_truncated"] = json!(true);
    let ports_total = a["ports_total"].as_u64().or_else(|| a["ports"].as_array().map(|v| v.len() as u64));
    out["ports_total"] = ports_total.map_or(Value::Null, |n| json!(n));
    out["ports"] = json!(
        a["ports"]
            .as_array()
            .into_iter()
            .flatten()
            .take(16)
            .filter_map(|p| {
                let host = p["host_port"].as_u64()?;
                let container = p["container_port"].as_u64()?;
                let protocol = p["protocol"].as_str()?;
                (host <= 65535 && container <= 65535 && ["tcp", "udp", "sctp"].contains(&protocol))
                    .then(|| json!({"host_port":host,"container_port":container,"protocol":protocol}))
            })
            .collect::<Vec<_>>()
    );
    if !a["ports"].is_array() {
        out["ports"] = Value::Null;
    }
    out["ports_omitted"] = ports_total.map_or(Value::Null, |n| json!(n.saturating_sub(out["ports"].as_array().map_or(0, Vec::len) as u64)));
    out
}
fn display(mut row: Value, edge: bool) -> Option<Value> {
    let id = row["id"].as_str()?;
    let kind = row["kind"].as_str()?;
    if !text(id, 160) || id.is_empty() || !text(kind, 48) {
        return None;
    }
    if edge {
        let from = row["from"].as_str().or_else(|| row["from_id"].as_str())?;
        let to = row["to"].as_str().or_else(|| row["to_id"].as_str())?;
        if !text(from, 160) || !text(to, 160) {
            return None;
        }
        Some(json!({"id":id,"kind":kind,"from":from,"to":to}))
    } else {
        let label = row["label"].as_str()?.chars().filter(|c| !c.is_control()).take(256).collect::<String>();
        row = json!({"id":id,"kind":kind,"label":label,"attributes":attributes(&row["attributes"])});
        (row.to_string().len() <= ROW_BYTES).then_some(row)
    }
}
fn search(row: &Value, edge: bool) -> String {
    if edge {
        format!("{} {} {}", row["kind"].as_str().unwrap_or(""), row["from"].as_str().unwrap_or(""), row["to"].as_str().unwrap_or(""))
    } else {
        format!("{} {} {}", row["label"].as_str().unwrap_or(""), row["id"].as_str().unwrap_or(""), row["kind"].as_str().unwrap_or(""))
    }
    .to_ascii_lowercase()
}
fn tab(kind: &str, tab: &str, edge: bool) -> bool {
    match (tab, edge) {
        ("all", _) => true,
        ("hosts", false) => [
            "docker_engine",
            "docker_container",
            "docker_network",
            "docker_volume",
            "docker_mount",
            "host",
            "environment",
            "execution_environment",
            "repository",
            "checkout",
            "worktree",
        ]
        .contains(&kind),
        ("dependencies", false) => ["repository", "checkout", "workspace", "package"].contains(&kind),
        ("cicd", false) => ["repository", "workflow", "job", "artifact", "deployment"].contains(&kind),
        ("hosts", true) => [
            "docker_contains",
            "docker_network_member",
            "docker_mounts",
            "contains_environment",
            "contains_checkout",
            "checkout_of",
            "worktree_of",
            "contains_workspace",
        ]
        .contains(&kind),
        ("dependencies", true) => {
            ["contains_checkout", "contains_workspace", "contains_package", "workspace_member", "declares_dependency", "resolves_to"].contains(&kind)
        }
        ("cicd", true) => ["has_workflow", "has_job", "needs_job", "uses_action", "uses_workflow", "runs_on", "deploys_to", "stores_in"].contains(&kind),
        _ => false,
    }
}
fn allowed_kinds(request: &TopologyUiRequest, edge: bool) -> String {
    let kinds = if edge {
        vec![
            "docker_contains",
            "docker_network_member",
            "docker_mounts",
            "contains_environment",
            "contains_checkout",
            "checkout_of",
            "worktree_of",
            "contains_workspace",
            "contains_package",
            "workspace_member",
            "declares_dependency",
            "resolves_to",
            "has_workflow",
            "has_job",
            "needs_job",
            "uses_action",
            "uses_workflow",
            "runs_on",
            "reads_config",
            "installed_from",
            "deploys_to",
            "stores_in",
            "collects_from",
        ]
    } else {
        vec![
            "docker_engine",
            "docker_container",
            "docker_network",
            "docker_volume",
            "docker_mount",
            "host",
            "environment",
            "execution_environment",
            "repository",
            "checkout",
            "worktree",
            "workspace",
            "package",
            "configuration",
            "workflow",
            "job",
            "artifact",
            "deployment",
            "installed_artifact",
        ]
    };
    kinds
        .into_iter()
        .filter(|k| tab(k, &request.tab, edge) && (edge || request.kind == "all" || request.kind == *k))
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",")
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    scope_id: String,
    attempt_id: String,
    ordinal: i64,
    writer_generation: i64,
    source_generation: String,
    settings_generation: String,
    payload_digest: String,
    facts_started_at_ms: i64,
    facts_observed_at_ms: i64,
    attempt_started_at_ms: i64,
    attempt_finished_at_ms: i64,
    stored_entities: usize,
    stored_edges: usize,
}
fn hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn row_digest(binding: [&str; 9]) -> String {
    crate::db::sha256_hex(json!(binding).to_string().as_bytes())
}
pub(super) fn record(db: &Connection, p: &DockerCandidatePayload, attempt: &str, ordinal: i64, generation: i64, digest: &str) -> Result<()> {
    let env = Envelope {
        version: 1,
        scope_id: p.scope_id.clone(),
        attempt_id: attempt.into(),
        ordinal,
        writer_generation: generation,
        source_generation: p.source_generation.clone(),
        settings_generation: p.settings_generation.clone(),
        payload_digest: digest.into(),
        facts_started_at_ms: p.facts_started_at_ms,
        facts_observed_at_ms: p.facts_observed_at_ms,
        attempt_started_at_ms: p.attempt_started_at_ms,
        attempt_finished_at_ms: p.attempt_finished_at_ms,
        stored_entities: p.entities.len(),
        stored_edges: p.edges.len(),
    };
    let raw = serde_json::to_string(&env).map_err(|_| invalid("invalid UI envelope"))?;
    let binding = crate::db::sha256_hex(raw.as_bytes());
    db.execute("DELETE FROM docker_ui_rows WHERE scope_id=?", [&p.scope_id])?;
    db.execute("INSERT INTO docker_ui_envelopes VALUES (?,?,?,?) ON CONFLICT(scope_id) DO UPDATE SET envelope=excluded.envelope,digest=excluded.digest,ordinal=excluded.ordinal",params![p.scope_id,raw,binding,ordinal])?;
    for (ty, rows) in [("entity", serde_json::to_value(&p.entities)), ("edge", serde_json::to_value(&p.edges))] {
        for row in rows.map_err(|_| invalid("invalid UI row"))?.as_array().ok_or_else(|| invalid("invalid UI rows"))? {
            let row = display(row.clone(), ty == "edge").ok_or_else(|| invalid("invalid UI row"))?;
            let id = row["id"].as_str().unwrap();
            let kind = row["kind"].as_str().unwrap();
            let key = search(&row, ty == "edge");
            let from = row["from"].as_str().unwrap_or("");
            let to = row["to"].as_str().unwrap_or("");
            let raw = row.to_string();
            let hash = row_digest([&binding, &p.scope_id, ty, id, kind, &key, from, to, &raw]);
            db.execute("INSERT INTO docker_ui_rows VALUES (?,?,?,?,?,?,?,?,?)", params![p.scope_id, ty, id, kind, key, from, to, raw, hash])?;
        }
    }
    Ok(())
}
pub(super) fn prune(db: &Connection) -> Result<()> {
    db.execute("DELETE FROM docker_ui_envelopes WHERE scope_id NOT IN (SELECT scope_id FROM docker_candidates)", [])?;
    db.execute("DELETE FROM docker_ui_rows WHERE scope_id NOT IN (SELECT scope_id FROM docker_ui_envelopes)", [])?;
    loop {
        // UTF8 data columns, digests and duplicated indexed keys count toward this
        // logical storage budget. SQLite pages, indexes and WAL are separate.
        let (rows, bytes): (i64, i64) = db.query_row(
            "SELECT count(*),coalesce(sum(length(CAST(scope_id||row_type||id||kind||search||from_id||to_id||display||digest AS BLOB))),0) FROM docker_ui_rows",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (scopes, metadata): (i64, i64) =
            db.query_row("SELECT count(*),coalesce(sum(length(CAST(scope_id||envelope||digest AS BLOB))+8),0) FROM docker_ui_envelopes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if rows <= PROJECTION_ROWS && bytes + metadata <= PROJECTION_BYTES && scopes <= 64 {
            break;
        }
        db.execute("DELETE FROM docker_candidates WHERE scope_id=(SELECT scope_id FROM docker_ui_envelopes ORDER BY ordinal,scope_id LIMIT 1)", [])?;
        db.execute("DELETE FROM docker_ui_envelopes WHERE scope_id NOT IN (SELECT scope_id FROM docker_candidates)", [])?;
        db.execute("DELETE FROM docker_ui_rows WHERE scope_id NOT IN (SELECT scope_id FROM docker_ui_envelopes)", [])?;
    }
    Ok(())
}
pub(super) fn complete(db: &Connection, scope: &str, ordinal: i64) -> Result<()> {
    // Unknown optional storage cannot prevent an accepted Complete write.
    if !known(db)? {
        return Ok(());
    }
    db.execute(
        "DELETE FROM docker_ui_rows WHERE scope_id IN (SELECT scope_id FROM docker_ui_envelopes WHERE scope_id=? AND ordinal<=?)",
        params![scope, ordinal],
    )?;
    db.execute("DELETE FROM docker_ui_envelopes WHERE scope_id=? AND ordinal<=?", params![scope, ordinal])?;
    Ok(())
}
pub(super) fn initialize_index(db: &Connection) -> Result<()> {
    if known(db)? {
        db.execute_batch("CREATE INDEX IF NOT EXISTS docker_ui_rows_selection ON docker_ui_rows(row_type,kind,scope_id,id);")?;
    }
    Ok(())
}
fn known(db: &Connection) -> Result<bool> {
    for (name, expected) in [
        ("docker_ui_envelopes", vec![("scope_id", "TEXT"), ("envelope", "TEXT"), ("digest", "TEXT"), ("ordinal", "INTEGER")]),
        (
            "docker_ui_rows",
            vec![
                ("scope_id", "TEXT"),
                ("row_type", "TEXT"),
                ("id", "TEXT"),
                ("kind", "TEXT"),
                ("search", "TEXT"),
                ("from_id", "TEXT"),
                ("to_id", "TEXT"),
                ("display", "TEXT"),
                ("digest", "TEXT"),
            ],
        ),
        (
            "docker_candidate_scopes",
            vec![("scope_id", "TEXT"), ("latest_ordinal", "INTEGER"), ("latest_attempt", "TEXT"), ("latest_status", "TEXT"), ("complete_ordinal", "INTEGER")],
        ),
    ] {
        // A view can expose identical declared columns without being this table.
        if !db.prepare("SELECT 1 FROM sqlite_schema WHERE name=? AND type='table'")?.exists([name])? {
            return Ok(false);
        }
        let mut s = db.prepare(&format!("PRAGMA table_info({name})"))?;
        let columns = s.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>>>()?;
        if columns.len() != expected.len() || columns.iter().zip(expected).any(|((n, t), (en, et))| n != en || t != et) {
            return Ok(false);
        }
    }
    Ok(true)
}
struct Group {
    env: Envelope,
    binding: String,
    latest: bool,
    entities: Vec<Value>,
    edges: Vec<Value>,
    filtered: usize,
    edge_total: usize,
}
struct EnvelopeScan {
    groups: Vec<Group>,
    hidden: usize,
    unavailable: Vec<&'static str>,
    omitted: Vec<&'static str>,
}
fn bounded_text(row: &rusqlite::Row<'_>, index: usize, max: usize) -> Result<String> {
    let value = row.get_ref(index)?;
    if !matches!(value, rusqlite::types::ValueRef::Text(bytes) if bytes.len()<=max) {
        return Err(rusqlite::Error::InvalidColumnType(index, "projection".into(), value.data_type()));
    }
    row.get(index)
}
fn envelopes(db: &Connection, now: i64) -> Result<EnvelopeScan> {
    let mut scan = EnvelopeScan { groups: vec![], hidden: 0, unavailable: vec![], omitted: vec![] };
    if !known(db)? {
        scan.unavailable.push("unknown_projection_schema");
        return Ok(scan);
    }
    let mut s=db.prepare("SELECT e.scope_id,e.envelope,e.digest,e.ordinal,s.latest_ordinal,s.latest_attempt,s.latest_status,s.complete_ordinal FROM docker_ui_envelopes e LEFT JOIN docker_candidate_scopes s USING(scope_id) ORDER BY e.ordinal DESC,e.scope_id LIMIT 64")?;
    let rows = s.query_map([], |r| {
        Ok((
            bounded_text(r, 0, 512)?,
            bounded_text(r, 1, 8192)?,
            bounded_text(r, 2, 64)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            bounded_text(r, 5, 160)?,
            bounded_text(r, 6, 16)?,
            r.get::<_, i64>(7)?,
        ))
    })?;
    let mut seen = false;
    for row in rows {
        seen = true;
        let (scope, raw, binding, ordinal, latest, attempt, status, complete) = match row {
            Ok(row) => row,
            Err(
                rusqlite::Error::InvalidColumnType(..)
                | rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::Utf8Error(..),
            ) => {
                scan.hidden += 1;
                scan.unavailable.push("invalid_envelope_column");
                continue;
            }
            Err(error) => return Err(error),
        };
        let Ok(e) = serde_json::from_str::<Envelope>(&raw) else {
            scan.hidden += 1;
            scan.unavailable.push("invalid_envelope");
            continue;
        };
        if e.version != 1 {
            scan.hidden += 1;
            scan.unavailable.push("unsupported_projection_version");
            continue;
        }
        if e.scope_id != scope
            || !text(&scope, 512)
            || !text(&e.attempt_id, 160)
            || e.attempt_id.is_empty()
            || e.ordinal != ordinal
            || ordinal <= 0
            || e.writer_generation <= 0
            || !hex(&e.source_generation)
            || !hex(&e.settings_generation)
            || !hex(&e.payload_digest)
            || crate::db::sha256_hex(raw.as_bytes()) != binding
            || serde_json::to_string(&e).ok().as_deref() != Some(&raw)
            || e.stored_entities > 4096
            || e.stored_edges > 16384
            || e.attempt_started_at_ms < 0
            || e.attempt_started_at_ms > e.facts_started_at_ms
            || e.facts_started_at_ms > e.facts_observed_at_ms
            || e.facts_observed_at_ms > e.attempt_finished_at_ms
            || e.attempt_finished_at_ms > now
            || complete < 0
            || complete > latest
            || ordinal > latest
            || !["partial", "failed", "complete", "excluded"].contains(&status.as_str())
            || (ordinal == latest && e.attempt_id != attempt)
        {
            scan.hidden += 1;
            scan.unavailable.push("invalid_envelope");
            continue;
        }
        let Some(expiry) = e.facts_observed_at_ms.checked_add(crate::topology::candidates::CANDIDATE_TTL_MS) else {
            scan.hidden += 1;
            scan.unavailable.push("invalid_envelope");
            continue;
        };
        if now >= expiry {
            scan.omitted.push("expired_projection");
            continue;
        }
        if ordinal <= complete {
            scan.omitted.push("superseded_projection");
            continue;
        }
        if ordinal == latest && status != "partial" {
            scan.hidden += 1;
            scan.unavailable.push("invalid_envelope");
            continue;
        }
        // This checks cardinality only. Content/digests of unselected rows are
        // still unchecked; the totals describe stored projections, not validity.
        let (total, entities, edges): (i64, i64, i64) = db.query_row(
            "SELECT count(*),coalesce(sum(row_type='entity'),0),coalesce(sum(row_type='edge'),0) FROM docker_ui_rows WHERE scope_id=?",
            [&scope],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if entities != e.stored_entities as i64 || edges != e.stored_edges as i64 || total != entities + edges {
            scan.hidden += 1;
            scan.unavailable.push("projection_count_mismatch");
            continue;
        }
        scan.groups.push(Group { env: e, binding, latest: ordinal == latest, entities: vec![], edges: vec![], filtered: 0, edge_total: 0 });
    }
    if !seen {
        scan.unavailable.push("no_materialized_projection");
    }
    scan.unavailable.sort_unstable();
    scan.unavailable.dedup();
    scan.omitted.sort_unstable();
    scan.omitted.dedup();
    Ok(scan)
}
// A malformed selected row invalidates the whole scope, including its other rows.
fn candidate_rows(db: &Connection, g: &Group, r: &TopologyUiRequest, edge: bool, limit: usize, ids: &HashSet<String>) -> Result<Option<Vec<Value>>> {
    let kinds = allowed_kinds(r, edge);
    if kinds.is_empty() {
        return Ok(Some(vec![]));
    }
    let ty = if edge { "edge" } else { "entity" };
    let endpoint = if edge {
        format!(" AND from_id IN ({0}) AND to_id IN ({0})", ids.iter().map(|id| format!("'{}'", id.replace('\'', "''"))).collect::<Vec<_>>().join(","))
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT id,kind,search,from_id,to_id,display,digest FROM docker_ui_rows WHERE scope_id=? AND row_type=? AND kind IN ({kinds}) AND instr(search,?)>0{endpoint} ORDER BY id LIMIT ?"
    );
    let mut s = db.prepare(&sql)?;
    let mut rows = s.query(params![g.env.scope_id, ty, r.filter, limit as i64])?;
    let mut out = vec![];
    while let Some(row) = rows.next()? {
        // Inspect SQL types and byte sizes before allocating/decoding display text.
        let mut cols = vec![];
        for (i, max) in [160, 48, 1536, 160, 160, ROW_BYTES, 64].into_iter().enumerate() {
            let value = row.get_ref(i)?;
            let rusqlite::types::ValueRef::Text(bytes) = value else {
                return Ok(None);
            };
            if bytes.len() > max {
                return Ok(None);
            }
            let Ok(value) = std::str::from_utf8(bytes) else {
                return Ok(None);
            };
            cols.push(value.to_owned());
        }
        let [id, kind, key, from, to, raw, hash] = <&[String; 7]>::try_from(cols.as_slice()).unwrap();
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            return Ok(None);
        };
        if row_digest([&g.binding, &g.env.scope_id, ty, id, kind, key, from, to, raw]) != *hash
            || value["id"] != *id
            || value["kind"] != *kind
            || search(&value, edge) != *key
            || display(value.clone(), edge).as_ref() != Some(&value)
            || (edge && (value["from"] != *from || value["to"] != *to || !ids.contains(from) || !ids.contains(to)))
            || (!edge && (!from.is_empty() || !to.is_empty()))
        {
            return Ok(None);
        }
        out.push(value);
    }
    Ok(Some(out))
}
fn count_candidate(db: &Connection, g: &Group, r: &TopologyUiRequest, edge: bool) -> Result<usize> {
    let kinds = allowed_kinds(r, edge);
    if kinds.is_empty() {
        return Ok(0);
    }
    db.query_row(
        &format!("SELECT count(*) FROM docker_ui_rows WHERE scope_id=? AND row_type=? AND kind IN ({kinds}) AND instr(search,?)>0"),
        params![g.env.scope_id, if edge { "edge" } else { "entity" }, r.filter],
        |row| row.get::<_, i64>(0).map(|n| n as usize),
    )
}
// Accepted reads select the first stored scope BEFORE filtering, matching the
// full graph's attribute/facts adoption. SQL scans identity/search columns but
// never transfers full attributes to Rust or IPC.
fn accepted_sql(r: &TopologyUiRequest, edge: bool, ids: &HashSet<String>) -> String {
    let table = if edge { "topology_edges" } else { "topology_entities" };
    let kinds = allowed_kinds(r, edge);
    let search = if edge { "lower(c.kind||' '||c.from_id||' '||c.to_id)" } else { "lower(substr(c.label,1,256)||' '||c.id||' '||c.kind)" };
    let endpoints = if edge {
        let values = ids.iter().map(|id| format!("'{}'", id.replace('\'', "''"))).collect::<Vec<_>>().join(",");
        format!(" AND c.from_id IN ({values}) AND c.to_id IN ({values})")
    } else {
        String::new()
    };
    format!(
        "WITH first AS (SELECT id,min(scope_id) AS scope_id FROM {table} GROUP BY id) SELECT {{columns}} FROM {table} c JOIN first f USING(id,scope_id) WHERE typeof(c.id)='text' AND length(CAST(c.id AS BLOB)) BETWEEN 1 AND 160 AND typeof(c.scope_id)='text' AND length(CAST(c.scope_id AS BLOB))<=512 AND c.kind IN ({kinds}) AND instr({search},?)>0{endpoints}"
    )
}
fn accepted_attributes() -> String {
    let mut fields = vec![];
    for key in ["content_sha256", "value_hash", "revision", "state", "health", "log_driver", "log_max_size"] {
        fields.push(format!("'{key}',CASE WHEN json_type(c.attributes,'$.{key}')='text' THEN substr(json_extract(c.attributes,'$.{key}'),1,160) END"));
    }
    for key in ["memory_bytes", "nano_cpus", "log_max_file", "restart_count"] {
        fields.push(format!("'{key}',CASE WHEN json_type(c.attributes,'$.{key}')='integer' AND json_extract(c.attributes,'$.{key}')>=0 THEN json_extract(c.attributes,'$.{key}') END"));
    }
    fields.push("'log_unbounded_candidate',json(CASE json_type(c.attributes,'$.log_unbounded_candidate') WHEN 'true' THEN 'true' WHEN 'false' THEN 'false' ELSE 'null' END)".into());
    let ports=(0..16).map(|i|format!("json_object('host_port',CASE WHEN json_type(c.attributes,'$.ports[{i}].host_port')='integer' THEN json_extract(c.attributes,'$.ports[{i}].host_port') END,'container_port',CASE WHEN json_type(c.attributes,'$.ports[{i}].container_port')='integer' THEN json_extract(c.attributes,'$.ports[{i}].container_port') END,'protocol',CASE WHEN json_type(c.attributes,'$.ports[{i}].protocol')='text' THEN substr(json_extract(c.attributes,'$.ports[{i}].protocol'),1,8) END)")).collect::<Vec<_>>().join(",");
    fields.push(format!("'ports',CASE WHEN json_type(c.attributes,'$.ports')='array' THEN json_array({ports}) END"));
    fields.push("'ports_total',CASE WHEN json_type(c.attributes,'$.ports')='array' THEN json_array_length(c.attributes,'$.ports') END".into());
    format!("CASE WHEN json_valid(c.attributes) THEN json_object({}) ELSE '{{}}' END", fields.join(","))
}
fn accepted_rows(db: &Connection, r: &TopologyUiRequest, edge: bool, limit: usize, ids: &HashSet<String>) -> Result<Vec<Value>> {
    if allowed_kinds(r, edge).is_empty() {
        return Ok(vec![]);
    }
    let columns = if edge {
        "c.scope_id,c.id,c.kind,c.from_id,c.to_id,'{}' AS attributes,c.updated_at".into()
    } else {
        format!("c.scope_id,c.id,c.kind,substr(c.label,1,256) AS label,{} AS attributes,c.updated_at", accepted_attributes())
    };
    let sql = accepted_sql(r, edge, ids).replace("{columns}", &columns) + " ORDER BY c.id,c.scope_id LIMIT ?";
    let mut s = db.prepare(&sql)?;
    let raw = s.query_map(params![r.filter, limit as i64], row_json)?.collect::<Result<Vec<_>>>()?;
    let mut result = vec![];
    for row in project_rows(facts::annotate_selected(db, raw)?) {
        let mut decoded = row.clone();
        decoded["attributes"] = row["attributes"].clone();
        if let Some(mut compact) = display(decoded, edge) {
            for key in ["scope_id", "scope_ids", "facts_observed_at_ms"] {
                compact[key] = row[key].clone();
            }
            compact["last_attempt_at_ms"] = Value::Null;
            result.push(compact);
        }
    }
    // Only bounded scope IDs, never additional attributes, are read for aliases.
    let table = if edge { "topology_edges" } else { "topology_entities" };
    for row in &mut result {
        let mut s = db.prepare(&format!(
            "SELECT scope_id FROM {table} WHERE id=? AND typeof(scope_id)='text' AND length(CAST(scope_id AS BLOB))<=512 ORDER BY scope_id LIMIT 5"
        ))?;
        let mut scopes = s.query_map([row["id"].as_str().unwrap()], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>>>()?;
        row["scope_ids_truncated"] = json!(scopes.len() > 4);
        scopes.truncate(4);
        row["scope_ids"] = json!(scopes);
    }
    Ok(result)
}
fn accepted_count(db: &Connection, r: &TopologyUiRequest, edge: bool, ids: &HashSet<String>) -> Result<usize> {
    if allowed_kinds(r, edge).is_empty() {
        return Ok(0);
    }
    db.query_row(&accepted_sql(r, edge, ids).replace("{columns}", "count(*)"), [&r.filter], |r| r.get::<_, i64>(0).map(|n| n as usize))
}
fn metadata(db: &Connection, selected: &HashSet<String>) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>)> {
    let restriction = if selected.is_empty() {
        String::new()
    } else {
        format!(" AND scope_id IN ({})", selected.iter().map(|s| format!("'{}'", s.replace('\'', "''"))).collect::<Vec<_>>().join(","))
    };
    // Fixed columns only: no adapter message, raw failure, counters or attributes
    // can grow the response before its byte budget is applied.
    let mut cols="substr(run_id,1,160) AS run_id,substr(scope_id,1,512) AS scope_id,CASE WHEN status IN ('complete','partial','failed','excluded') THEN status ELSE 'failed' END AS status,CASE WHEN typeof(observed_at)='integer' AND observed_at>=0 THEN observed_at END AS observed_at".to_owned();
    let counters=["entries","files","bytes"].map(|k|format!("'{k}',CASE WHEN json_valid(counters) THEN CASE WHEN json_type(counters,'$.{k}')='integer' AND json_extract(counters,'$.{k}')>=0 THEN json_extract(counters,'$.{k}') END END")).join(",");
    let failures=(0..4).map(|i|format!("json_object('code',CASE WHEN json_valid(failures) THEN CASE WHEN json_extract(failures,'$[{i}].code') IN ('timeout','unreachable','auth_unavailable','permission_denied','tool_missing','parse_error','rate_limited','truncated','limit_reached','unsupported','internal') THEN json_extract(failures,'$[{i}].code') END END)")).collect::<Vec<_>>().join(",");
    cols += &format!(",json_object({counters}) AS counters,json_array({failures}) AS failures");
    let mut s=db.prepare(&format!("SELECT {cols} FROM topology_observations c WHERE 1=1{restriction} AND NOT EXISTS (SELECT 1 FROM topology_observations n WHERE n.scope_id=c.scope_id AND (n.observed_at>c.observed_at OR (n.observed_at=c.observed_at AND n.run_id>c.run_id))) ORDER BY observed_at DESC,run_id DESC LIMIT 64"))?;
    let scopes = receipt_rows(s.query_map([], row_json)?.collect::<Result<Vec<_>>>()?);
    let mut s = db.prepare(&format!("SELECT {cols} FROM topology_observations WHERE 1=1{restriction} ORDER BY observed_at DESC,run_id DESC LIMIT 64"))?;
    let history = receipt_rows(s.query_map([], row_json)?.collect::<Result<Vec<_>>>()?);
    let mut s=db.prepare(&format!("SELECT substr(c.scope_id,1,512) AS scope_id,substr(c.id,1,160) AS id,substr(c.kind,1,48) AS kind,substr(c.change_kind,1,48) AS change_kind,r.at FROM topology_changes c JOIN topology_runs r USING(run_id) WHERE 1=1{restriction} ORDER BY r.at DESC,c.scope_id,c.id LIMIT 64"))?;
    let changes = s.query_map([], row_json)?.collect::<Result<Vec<_>>>()?;
    Ok((scopes, history, changes))
}
fn ids(rows: &[Value]) -> HashSet<String> {
    rows.iter().filter_map(|v| v["id"].as_str().map(str::to_owned)).collect()
}
fn bound_scopes(rows: &mut Vec<Value>, candidate: &HashSet<String>) {
    let mut allowed = candidate.clone();
    rows.retain(|r| {
        let Some(scope) = r["scope_id"].as_str() else {
            return false;
        };
        if allowed.contains(scope) {
            true
        } else if allowed.len() < 64 {
            allowed.insert(scope.into());
            true
        } else {
            false
        }
    });
    for row in rows.iter() {
        for scope in row["scope_ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if allowed.len() < 64 {
                allowed.insert(scope.into());
            }
        }
    }
    for row in rows {
        let scopes = row["scope_ids"].as_array_mut().unwrap();
        let before = scopes.len();
        scopes.retain(|v| v.as_str().is_some_and(|s| allowed.contains(s)));
        if scopes.len() != before {
            row["scope_ids_truncated"] = json!(true);
        }
    }
}
fn closure(view: &mut Value) {
    let accepted = ids(view["entities"].as_array().unwrap());
    view["edges"].as_array_mut().unwrap().retain(|e| accepted.contains(e["from"].as_str().unwrap_or("")) && accepted.contains(e["to"].as_str().unwrap_or("")));
    for g in view["candidates"].as_array_mut().unwrap() {
        let selected = ids(g["entities"].as_array().unwrap());
        g["edges"].as_array_mut().unwrap().retain(|e| selected.contains(e["from"].as_str().unwrap_or("")) && selected.contains(e["to"].as_str().unwrap_or("")));
    }
}
impl Store {
    pub fn topology_ui_view(&self, request: &TopologyUiRequest) -> Result<Value> {
        let r = request.checked()?;
        let read = self.conn().unchecked_transaction()?;
        let db = self.conn();
        let scan = envelopes(db, crate::db::now_ms())?;
        let mut groups = scan.groups;
        let mut unavailable = scan.unavailable;
        let empty = HashSet::new();
        let accepted_filtered = accepted_count(db, &r, false, &empty)?;
        let accepted_stored: usize = db.query_row("SELECT count(DISTINCT id) FROM topology_entities", [], |row| row.get::<_, i64>(0).map(|n| n as usize))?;
        let accepted_stored_edges: usize = db.query_row("SELECT count(DISTINCT id) FROM topology_edges", [], |row| row.get::<_, i64>(0).map(|n| n as usize))?;
        let mut candidate_filtered = 0;
        let mut stored_entities = 0;
        let mut stored_edges = 0;
        for g in &mut groups {
            g.filtered = count_candidate(db, g, &r, false)?;
            g.edge_total = count_candidate(db, g, &r, true)?;
            candidate_filtered += g.filtered;
            stored_entities += g.env.stored_entities;
            stored_edges += g.env.stored_edges;
        }
        groups.retain(|g| g.filtered > 0);
        let scope_limit = if accepted_filtered > 0 { 32 } else { 64 };
        groups.truncate(scope_limit);
        let candidate_scopes = groups.iter().map(|g| g.env.scope_id.clone()).collect::<HashSet<_>>();
        // Reserve half for each nonempty layer, then give unused capacity back.
        let accepted_budget = if candidate_filtered == 0 { ENTITIES } else { (ENTITIES / 2).max(ENTITIES.saturating_sub(candidate_filtered)) };
        let mut accepted = accepted_rows(db, &r, false, accepted_budget, &empty)?;
        bound_scopes(&mut accepted, &candidate_scopes);
        let candidate_budget = ENTITIES - accepted.len();
        let mut used = 0;
        let mut checked = 0;
        let mut hidden = scan.hidden;
        let mut valid = vec![];
        for mut g in groups {
            let Some(rows) = candidate_rows(db, &g, &r, false, candidate_budget - used, &empty)? else {
                hidden += 1;
                unavailable.push("invalid_selected_projection_row");
                continue;
            };
            checked += rows.len();
            g.entities = rows;
            used += g.entities.len();
            valid.push(g);
        }
        if ENTITIES - used > accepted.len() {
            accepted = accepted_rows(db, &r, false, ENTITIES - used, &empty)?;
        }
        bound_scopes(&mut accepted, &candidate_scopes);
        let accepted_ids = ids(&accepted);
        let accepted_edge_filtered = accepted_count(db, &r, true, &accepted_ids)?;
        let mut candidate_edge_budget = if accepted_edge_filtered > 0 { EDGES / 2 } else { EDGES };
        let mut with_edges = vec![];
        for mut g in valid {
            let Some(rows) = candidate_rows(db, &g, &r, true, candidate_edge_budget, &ids(&g.entities))? else {
                hidden += 1;
                unavailable.push("invalid_selected_projection_row");
                continue;
            };
            checked += rows.len();
            candidate_edge_budget -= rows.len();
            g.edges = rows;
            with_edges.push(g);
        }
        let candidate_edges: usize = with_edges.iter().map(|g| g.edges.len()).sum();
        let mut edges = accepted_rows(db, &r, true, EDGES - candidate_edges, &accepted_ids)?;
        let mut response_scopes = candidate_scopes.clone();
        response_scopes.extend(accepted.iter().flat_map(|v| v["scope_ids"].as_array().into_iter().flatten()).filter_map(|v| v.as_str().map(str::to_owned)));
        edges.retain(|e| e["scope_id"].as_str().is_some_and(|scope| response_scopes.contains(scope)));
        for edge in &mut edges {
            edge["scope_ids"].as_array_mut().unwrap().retain(|v| v.as_str().is_some_and(|scope| response_scopes.contains(scope)));
        }
        let mut remaining = EDGES - edges.len();
        let candidates=with_edges.into_iter().filter(|g|g.filtered>0 || !g.entities.is_empty()).map(|mut g| {
            g.edges.truncate(remaining);remaining-=g.edges.len();
            json!({"scope_id":g.env.scope_id,"attempt_id":g.env.attempt_id,"status":"partial","facts_observed_at_ms":g.env.facts_observed_at_ms,"facts_started_at_ms":g.env.facts_started_at_ms,"attempt_started_at_ms":g.env.attempt_started_at_ms,"attempt_finished_at_ms":g.env.attempt_finished_at_ms,"is_latest_attempt":g.latest,"entities":g.entities,"edges":g.edges,"stored_totals":{"entities":g.env.stored_entities,"edges":g.env.stored_edges},"filtered_stored_totals":{"entities":g.filtered,"edges":g.edge_total},"display_values_truncated":true,"validation_scope":"selected_projection_rows","full_archive_checked":false})
        }).collect::<Vec<_>>();
        let mut selected_scopes = candidate_scopes;
        selected_scopes.extend(accepted.iter().flat_map(|v| v["scope_ids"].as_array().into_iter().flatten()).filter_map(|v| v.as_str().map(str::to_owned)));
        let (scopes, history, changes) = metadata(db, &selected_scopes)?;
        let mut view = json!({"schema":"katala_topology_ui.v1","entities":accepted,"edges":edges,"candidates":candidates,"scopes":scopes,"history":history,"changes":changes,
            "window":{"candidate_unavailable_reasons":unavailable,"candidate_omission_reasons":scan.omitted,"accepted_read_scope":"bounded_selected_fields","validation_layer":"candidates","available_kinds":allowed_kinds(&TopologyUiRequest {kind:"all".into(),..r.clone()},false).split(',').filter(|s|!s.is_empty()).map(|s|s.trim_matches('\'')).collect::<Vec<_>>(),"validation_scope":"selected_projection_rows","full_archive_checked":false,"unselected_rows_checked":false,"selected_rows_checked":checked,"corrupt_scopes_hidden":hidden,"stored_totals":{"accepted_entities":accepted_stored,"accepted_edges":accepted_stored_edges,"candidate_entities":stored_entities,"candidate_edges":stored_edges},"filtered_stored_totals":{"accepted_entities":accepted_filtered,"accepted_closed_edges":accepted_edge_filtered,"candidate_entities":candidate_filtered},"entity_limit":ENTITIES,"edge_limit":EDGES,"response_byte_limit":RESPONSE_BYTES,"search_scope":"bounded_display_columns","display_values_truncated":true,"truncated":false,"omission_reasons":["entity_budget","edge_budget_or_unselected_endpoints","bounded_metadata","bounded_display_values","scope_budget"]}});
        closure(&mut view);
        let mut byte_capped = false;
        while serde_json::to_vec(&view).map_err(|_| invalid("invalid UI response"))?.len() > RESPONSE_BYTES {
            byte_capped = true;
            let removed = view["candidates"].as_array_mut().unwrap().iter_mut().rev().any(|g| g["entities"].as_array_mut().unwrap().pop().is_some())
                || ["entities", "history", "changes", "scopes", "candidates"].iter().any(|key| view[*key].as_array_mut().unwrap().pop().is_some());
            if !removed {
                return Err(invalid("UI metadata exceeds budget"));
            }
            closure(&mut view);
        }
        // Reserve room for the final boolean by inserting it before checking.
        view["window"]["truncated"] = json!(
            byte_capped
                || accepted_filtered + candidate_filtered
                    > view["entities"].as_array().unwrap().len()
                        + view["candidates"].as_array().unwrap().iter().map(|g| g["entities"].as_array().unwrap().len()).sum::<usize>()
                || accepted_stored_edges + stored_edges
                    > view["edges"].as_array().unwrap().len()
                        + view["candidates"].as_array().unwrap().iter().map(|g| g["edges"].as_array().unwrap().len()).sum::<usize>()
        );
        if serde_json::to_vec(&view).map_err(|_| invalid("invalid UI response"))?.len() > RESPONSE_BYTES {
            return Err(invalid("UI response exceeds budget"));
        }
        read.commit()?;
        Ok(view)
    }
}

#[cfg(test)]
pub(super) fn envelopes_for_test(db: &Connection, now: i64) -> Result<usize> {
    Ok(envelopes(db, now)?.groups.len())
}

#[cfg(test)]
pub(super) fn scan_status_for_test(db: &Connection, now: i64) -> Result<(usize, Vec<&'static str>, Vec<&'static str>)> {
    let scan = envelopes(db, now)?;
    Ok((scan.hidden, scan.unavailable, scan.omitted))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ui_request_does_not_allow_caller_to_raise_limits() {
        assert!(serde_json::from_value::<TopologyUiRequest>(json!({"limit":201})).is_err());
        assert!(Store::open_in_memory().unwrap().topology_ui_view(&TopologyUiRequest { kind: "all OR 1=1".into(), ..Default::default() }).is_err());
    }
}
