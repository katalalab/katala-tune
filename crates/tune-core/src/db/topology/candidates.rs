use super::{Result, Store, TopologyLease, assert_lease};
use crate::topology::{
    TopologySnapshot,
    candidates::{AdmittedDockerPartial, CandidateAttempt, DockerCandidatePayload, validate},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
const MAX_SCOPES: i64 = 64;
const MAX_TOTAL_BYTES: i64 = 64 * 1024 * 1024;
const RESERVATIONS: i64 = 512;
const MAX_LEDGER_SCOPES: i64 = 4096;
fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.into())
}
fn token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 160 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn has_tables(db: &Connection) -> Result<bool> {
    // Candidate storage is optional on legacy/read-only/partially migrated DBs.
    // Fixed PRAGMA metadata distinguishes an unknown shape from SQL/IO failure.
    for (table, expected) in [
        (
            "docker_candidates",
            &[
                ("scope_id", "TEXT"),
                ("attempt_id", "TEXT"),
                ("ordinal", "INTEGER"),
                ("writer_generation", "INTEGER"),
                ("payload", "TEXT"),
                ("digest", "TEXT"),
                ("bytes", "INTEGER"),
            ][..],
        ),
        (
            "docker_candidate_scopes",
            &[("scope_id", "TEXT"), ("latest_ordinal", "INTEGER"), ("latest_attempt", "TEXT"), ("latest_status", "TEXT"), ("complete_ordinal", "INTEGER")][..],
        ),
    ] {
        let mut statement = db.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(5)?)))?
            .collect::<Result<Vec<_>>>()?;
        if columns.len() != expected.len()
            || columns.iter().zip(expected).enumerate().any(|(index, ((name, kind, not_null, pk), (expected_name, expected_kind)))| {
                name != expected_name || kind != expected_kind || *not_null != 1 || *pk != i64::from(index == 0)
            })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

impl Store {
    pub fn reserve_docker_attempt_fenced(&self, lease: &TopologyLease, attempt_id: &str, now: i64) -> Result<CandidateAttempt> {
        if !token(attempt_id) || now < 0 {
            return Err(invalid("invalid candidate attempt"));
        }
        self.tx(|db| {
            assert_lease(db, lease, now)?;
            let current: i64 = db.query_row("SELECT ordinal FROM docker_candidate_sequence WHERE singleton=1", [], |row| row.get(0))?;
            if current < 0 {
                return Err(invalid("invalid candidate sequence"));
            }
            let ordinal = current.checked_add(1).ok_or_else(|| invalid("candidate ordinal exhausted"))?;
            db.execute(
                "INSERT INTO docker_candidate_attempts (attempt_id,ordinal,holder,writer_generation,reserved_at) VALUES (?,?,?,?,?)",
                params![attempt_id, ordinal, lease.holder, lease.generation, now],
            )?;
            db.execute("UPDATE docker_candidate_sequence SET ordinal=? WHERE singleton=1", [ordinal])?;
            db.execute("DELETE FROM docker_candidate_attempts WHERE ordinal<=?", [ordinal.saturating_sub(RESERVATIONS)])?;
            prune_ledgers(db)?;
            Ok(CandidateAttempt::reserved(attempt_id.into(), ordinal, lease.holder.clone(), lease.generation))
        })
    }
    pub fn record_docker_candidate_fenced(&self, lease: &TopologyLease, admitted: AdmittedDockerPartial, now: i64) -> Result<()> {
        let (attempt, payload, digest) = admitted.parts();
        let (attempt_id, attempt_ordinal, attempt_holder, attempt_generation) = attempt.binding();
        if attempt_holder != lease.holder || attempt_generation != lease.generation {
            return Err(invalid("candidate admission binding mismatch"));
        }
        self.tx(|db| {
            assert_lease(db,lease,now)?;
            let bytes = validate(&payload, now).map_err(invalid)?;
            if crate::db::sha256_hex(&bytes) != digest { return Err(invalid("candidate admission binding mismatch")); }
            let reservation:Option<(i64,String,i64,i64)>=db.query_row("SELECT ordinal,holder,writer_generation,reserved_at FROM docker_candidate_attempts WHERE attempt_id=?",[attempt_id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
            if !reservation.is_some_and(|(ordinal,holder,generation,at)| ordinal==attempt_ordinal && holder==attempt_holder && generation==attempt_generation && at<=payload.attempt_started_at_ms) {
                return Err(invalid("candidate reservation mismatch"));
            }
            let ledger:Option<(i64,String,i64)>=db.query_row("SELECT latest_ordinal,latest_status,complete_ordinal FROM docker_candidate_scopes WHERE scope_id=?",[&payload.scope_id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            if ledger.is_some_and(|(latest,status,complete)| latest>attempt_ordinal || complete>=attempt_ordinal || (latest==attempt_ordinal && status!="partial")) {
                return Err(invalid("candidate is superseded"));
            }
            let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM docker_candidates WHERE scope_id=? AND ordinal>=?)",params![payload.scope_id,attempt_ordinal],|row| row.get(0))?;
            if exists { return Err(invalid("candidate attempt already stored")); }
            db.execute("INSERT INTO docker_candidate_scopes (scope_id,latest_ordinal,latest_attempt,latest_status) VALUES (?,?,?,'partial') ON CONFLICT(scope_id) DO UPDATE SET latest_ordinal=excluded.latest_ordinal,latest_attempt=excluded.latest_attempt,latest_status=excluded.latest_status",params![payload.scope_id,attempt_ordinal,attempt_id])?;
            // Only candidate data is removed; accepted graph/cache/cursor/receipts are independent.
            db.execute("DELETE FROM docker_candidates WHERE scope_id IN (SELECT scope_id FROM docker_candidates WHERE CASE WHEN json_valid(payload) THEN coalesce(json_extract(payload,'$.facts_observed_at_ms'),-1)<=? ELSE 1 END)",[now.saturating_sub(crate::topology::candidates::CANDIDATE_TTL_MS)])?;
            db.execute("INSERT INTO docker_candidates (scope_id,attempt_id,ordinal,writer_generation,payload,digest,bytes) VALUES (?,?,?,?,?,?,?) ON CONFLICT(scope_id) DO UPDATE SET attempt_id=excluded.attempt_id,ordinal=excluded.ordinal,writer_generation=excluded.writer_generation,payload=excluded.payload,digest=excluded.digest,bytes=excluded.bytes",params![payload.scope_id,attempt_id,attempt_ordinal,attempt_generation,std::str::from_utf8(&bytes).map_err(|_| invalid("invalid candidate UTF8"))?,digest,bytes.len() as i64])?;
            loop {
                let (count,total):(i64,i64)=db.query_row("SELECT count(*),coalesce(sum(length(CAST(payload AS BLOB))),0) FROM docker_candidates",[],|row| Ok((row.get(0)?,row.get(1)?)))?;
                if count<=MAX_SCOPES && total<=MAX_TOTAL_BYTES { break; }
                db.execute("DELETE FROM docker_candidates WHERE scope_id=(SELECT scope_id FROM docker_candidates ORDER BY ordinal,scope_id LIMIT 1)",[])?;
            }
            super::ui::record(db, &payload, attempt_id, attempt_ordinal, attempt_generation, &digest)?;
            super::ui::prune(db)?;
            prune_ledgers(db)?;
            Ok(())
        })
    }
}
fn prune_ledgers(db: &Connection) -> Result<()> {
    // Once its reservation is gone, a token cannot replay even if its empty ledger is pruned.
    db.execute("DELETE FROM docker_candidate_scopes WHERE scope_id NOT IN (SELECT scope_id FROM docker_candidates) AND latest_attempt NOT IN (SELECT attempt_id FROM docker_candidate_attempts)",[])?;
    let count: i64 = db.query_row("SELECT count(*) FROM docker_candidate_scopes", [], |row| row.get(0))?;
    if count > MAX_LEDGER_SCOPES {
        let cutoff: i64 = db.query_row("SELECT latest_ordinal FROM docker_candidate_scopes WHERE scope_id NOT IN (SELECT scope_id FROM docker_candidates) ORDER BY latest_ordinal,scope_id LIMIT 1", [], |row| row.get(0))?;
        db.execute("DELETE FROM docker_candidate_attempts WHERE ordinal<=?", [cutoff])?;
        db.execute("DELETE FROM docker_candidate_scopes WHERE scope_id NOT IN (SELECT scope_id FROM docker_candidates) AND latest_attempt NOT IN (SELECT attempt_id FROM docker_candidate_attempts)", [])?;
    }
    Ok(())
}
pub(super) fn record_attempt(db: &Connection, run_id: &str, snapshot: &TopologySnapshot, at: i64) -> Result<()> {
    if !snapshot.scope_id.contains(":docker:engine:") || !has_tables(db)? {
        return Ok(());
    }
    let ordinal: Option<i64> = db.query_row("SELECT a.ordinal FROM docker_candidate_attempts a JOIN topology_refresh_lease l ON a.holder=l.holder AND a.writer_generation=l.generation WHERE a.attempt_id=? AND l.singleton=1 AND l.expires_at>?", params![run_id,at], |row| row.get(0)).optional()?;
    let Some(ordinal) = ordinal else {
        return Ok(());
    };
    let state = super::status(snapshot.status);
    db.execute("INSERT INTO docker_candidate_scopes (scope_id,latest_ordinal,latest_attempt,latest_status,complete_ordinal) VALUES (?,?,?,?,?) ON CONFLICT(scope_id) DO UPDATE SET latest_ordinal=max(latest_ordinal,excluded.latest_ordinal),latest_attempt=CASE WHEN excluded.latest_ordinal>=latest_ordinal THEN excluded.latest_attempt ELSE latest_attempt END,latest_status=CASE WHEN excluded.latest_ordinal>=latest_ordinal THEN excluded.latest_status ELSE latest_status END,complete_ordinal=max(complete_ordinal,excluded.complete_ordinal)",params![snapshot.scope_id,ordinal,run_id,state,if state=="complete" {ordinal} else {0}])?;
    if state == "complete" {
        super::ui::complete(db, &snapshot.scope_id, ordinal)?;
    }
    prune_ledgers(db)
}
pub(super) fn read_candidates(db: &Connection, now: i64) -> Result<Vec<Value>> {
    if !has_tables(db)? {
        return Ok(vec![]);
    }
    let mut statement = db.prepare(
        r#"
        SELECT c.payload,c.digest,c.attempt_id,c.ordinal,c.writer_generation,
               s.latest_ordinal,s.complete_ordinal,c.bytes,c.scope_id,s.latest_status
        FROM docker_candidates c JOIN docker_candidate_scopes s USING(scope_id)
        WHERE typeof(c.payload)='text' AND length(CAST(c.payload AS BLOB))<=8388608
          AND typeof(c.digest)='text' AND length(c.digest)=64
          AND typeof(c.attempt_id)='text' AND length(c.attempt_id) BETWEEN 1 AND 160
          AND typeof(c.scope_id)='text' AND length(c.scope_id) BETWEEN 1 AND 512
          AND typeof(c.ordinal)='integer' AND c.ordinal>0
          AND typeof(c.writer_generation)='integer' AND c.writer_generation>0
          AND typeof(c.bytes)='integer' AND c.bytes BETWEEN 1 AND 8388608
          AND typeof(s.latest_ordinal)='integer' AND s.latest_ordinal>=c.ordinal
          AND typeof(s.complete_ordinal)='integer' AND s.complete_ordinal>=0 AND c.ordinal>s.complete_ordinal
          AND typeof(s.latest_status)='text' AND s.latest_status IN ('complete','partial','failed','excluded')
          AND typeof(s.latest_attempt)='text' AND length(s.latest_attempt) BETWEEN 1 AND 160
          AND (c.ordinal<s.latest_ordinal OR c.attempt_id=s.latest_attempt)
        ORDER BY c.ordinal DESC,c.scope_id LIMIT 64
    "#,
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;
    let mut result = vec![];
    let mut total = 0usize;
    for row in rows {
        let (raw, digest, attempt_id, ordinal, generation, latest, complete, bytes, scope_id, latest_status) = match row {
            Ok(row) => row,
            Err(
                rusqlite::Error::InvalidColumnType(..)
                | rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::Utf8Error(..),
            ) => {
                continue;
            }
            Err(error) => return Err(error),
        };
        if raw.len() > crate::topology::candidates::CANDIDATE_MAX_BYTES
            || bytes != raw.len() as i64
            || ordinal <= complete
            || ordinal > latest
            || !["complete", "partial", "failed", "excluded"].contains(&latest_status.as_str())
            || (ordinal == latest && latest_status != "partial")
            || generation < 1
            || !token(&attempt_id)
        {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<DockerCandidatePayload>(&raw) else {
            continue;
        };
        if payload.scope_id != scope_id {
            continue;
        }
        let Ok(canonical) = validate(&payload, now) else {
            continue;
        };
        if canonical != raw.as_bytes() || crate::db::sha256_hex(&canonical) != digest {
            continue;
        }
        total += canonical.len();
        if total > MAX_TOTAL_BYTES as usize {
            break;
        }
        result.push(json!({"scope_id":payload.scope_id,"attempt_id":attempt_id,"status":"partial","facts_observed_at_ms":payload.facts_observed_at_ms,"facts_started_at_ms":payload.facts_started_at_ms,"attempt_started_at_ms":payload.attempt_started_at_ms,"attempt_finished_at_ms":payload.attempt_finished_at_ms,"is_latest_attempt":ordinal==latest,"entities":payload.entities,"edges":payload.edges}));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{
        ScopeStatus,
        adapters::docker::{DockerEngine, DockerPacket, engine_snapshots, parse_container},
        candidates::{DockerCandidatePayload, DockerSemanticGap},
    };
    use serde_json::json;
    fn payload(now: i64) -> DockerCandidatePayload {
        let c = parse_container(&json!({"id":"container","state":"exited","health":null,"log_driver":"local","mounts":[],"networks":["absent"],"ports":[]}))
            .unwrap();
        let packet = DockerPacket { engines: vec![DockerEngine { id: "e".repeat(64), containers: vec![c], networks: vec![], volumes: vec![] }] };
        let snapshot = engine_snapshots(packet.snapshot("fixture:native:docker".into(), now).unwrap()).remove(1);
        assert_eq!(snapshot.status, ScopeStatus::Partial);
        DockerCandidatePayload {
            version: 1,
            scope_id: snapshot.scope_id,
            engine_identity: "e".repeat(64),
            source_generation: "a".repeat(64),
            settings_generation: "b".repeat(64),
            facts_started_at_ms: now - 20,
            facts_observed_at_ms: now - 10,
            attempt_started_at_ms: now - 30,
            attempt_finished_at_ms: now,
            gaps: vec![DockerSemanticGap::UnobservedNetworkMembership],
            entities: snapshot.entities,
            edges: snapshot.edges,
        }
    }
    #[test]
    fn ui_projection_does_not_decode_archive_and_declares_selected_validation() {
        let (db, lease, now) = setup();
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", payload(now), now), now).unwrap();
        db.conn().execute("UPDATE docker_candidates SET payload='corrupt archive'", []).unwrap();
        let view = db.topology_ui_view(&super::super::TopologyUiRequest::default()).unwrap();
        assert_eq!(view["candidates"].as_array().unwrap().len(), 1, "UI must use its independent validated projection");
        assert_eq!(view["window"]["full_archive_checked"], false);
        assert_eq!(view["window"]["validation_scope"], "selected_projection_rows");
        assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
        db.conn().execute_batch("DROP TABLE docker_candidates;CREATE VIEW docker_candidates AS SELECT archive_probe_must_not_run() AS scope_id;").unwrap();
        assert_eq!(db.topology_ui_view(&Default::default()).unwrap()["candidates"].as_array().unwrap().len(), 1, "UI never even inspects canonical schema");
        assert!(db.topology_view().is_err());
    }
    #[test]
    fn ui_selected_row_corruption_hides_scope_but_unselected_and_archive_are_explicitly_unchecked() {
        let (db, lease, now) = setup();
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", payload(now), now), now).unwrap();
        db.conn().execute("UPDATE docker_ui_rows SET display='corrupt' WHERE row_type='entity' AND kind='docker_container'", []).unwrap();
        let request = super::super::TopologyUiRequest { kind: "docker_engine".into(), ..Default::default() };
        let unchecked = db.topology_ui_view(&request).unwrap();
        assert_eq!(unchecked["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(unchecked["window"]["unselected_rows_checked"], false);
        assert_eq!(unchecked["window"]["full_archive_checked"], false);
        let checked = db.topology_ui_view(&Default::default()).unwrap();
        assert!(checked["candidates"].as_array().unwrap().is_empty());
        assert_eq!(checked["window"]["corrupt_scopes_hidden"], 1);
        assert_eq!(db.topology_view().unwrap()["candidates"].as_array().unwrap().len(), 1, "explicit full API validates canonical archive independently");
        // Display, search, kind, endpoints and envelope identity all participate.
        for assignment in ["search='changed'", "kind='host'", "from_id='changed'", "digest='changed'"] {
            let (db, lease, now) = setup();
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "bound", payload(now), now), now).unwrap();
            db.conn().execute(&format!("UPDATE docker_ui_rows SET {assignment} WHERE row_type='entity' AND kind='docker_container'"), []).unwrap();
            assert!(db.topology_ui_view(&Default::default()).unwrap()["candidates"].as_array().unwrap().is_empty(), "{assignment}");
        }
    }
    #[test]
    fn ui_unknown_projection_schema_envelope_and_legacy_archive_never_fallback() {
        for mode in ["version", "schema", "missing", "legacy", "binding", "ledger"] {
            let (db, lease, now) = setup();
            let p = payload(now);
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", p.clone(), now), now).unwrap();
            db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "next", p, now), now).unwrap();
            let before = db.topology_accepted_view().unwrap();
            match mode {
                "version" => {
                    db.conn().execute("UPDATE docker_ui_envelopes SET envelope=json_set(envelope,'$.version',99)", []).unwrap();
                }
                "binding" => {
                    db.conn().execute("UPDATE docker_ui_envelopes SET digest='changed'", []).unwrap();
                }
                "missing" => {
                    db.conn().execute_batch("DROP TABLE docker_ui_rows").unwrap();
                }
                "ledger" => {
                    db.conn().execute("UPDATE docker_candidate_scopes SET latest_ordinal=x'00'", []).unwrap();
                }
                "schema" => {
                    db.conn().execute_batch("ALTER TABLE docker_ui_rows RENAME COLUMN digest TO unknown;").unwrap();
                }
                _ => {
                    db.conn().execute("DELETE FROM docker_ui_envelopes", []).unwrap();
                }
            }
            let changes = db.conn().total_changes();
            let ui = db.topology_ui_view(&Default::default()).unwrap();
            assert!(ui["candidates"].as_array().unwrap().is_empty(), "{mode}");
            assert!(!ui["window"]["candidate_unavailable_reasons"].as_array().unwrap().is_empty(), "{mode} is unavailable, not verified zero");
            assert!(!ui["entities"].as_array().unwrap().is_empty());
            assert_eq!(db.topology_accepted_view().unwrap(), before);
            assert_eq!(db.conn().total_changes(), changes, "view is read only");
        }
    }
    #[test]
    fn ui_budgets_are_global_fair_stable_filtered_literal_and_closed() {
        let (db, lease, now) = setup();
        let p = expanded_payload(now, 0, Some(200), 128);
        db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", p, now), now).unwrap();
        let ui = db.topology_ui_view(&Default::default()).unwrap();
        assert_eq!(ui["entities"].as_array().unwrap().len(), 100);
        assert_eq!(ui["candidates"][0]["entities"].as_array().unwrap().len(), 100);
        let mut entities = ui["entities"].as_array().unwrap().len();
        let mut edges = ui["edges"].as_array().unwrap().len();
        for layer in std::iter::once(&ui).chain(ui["candidates"].as_array().unwrap().iter()) {
            let ids = layer["entities"].as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap()).collect::<std::collections::HashSet<_>>();
            for edge in layer["edges"].as_array().unwrap() {
                assert!(ids.contains(edge["from"].as_str().unwrap()));
                assert!(ids.contains(edge["to"].as_str().unwrap()));
            }
            if layer != &ui {
                entities += layer["entities"].as_array().unwrap().len();
                edges += layer["edges"].as_array().unwrap().len();
            }
        }
        assert!(entities <= 200 && edges <= 200);
        assert!(ui.to_string().len() <= 1024 * 1024);
        assert_eq!(db.topology_ui_view(&Default::default()).unwrap(), ui, "stable order");
        for filter in ["%", "not-present_%", "\\"] {
            let r = super::super::TopologyUiRequest { filter: filter.into(), ..Default::default() };
            let v = db.topology_ui_view(&r).unwrap();
            assert!(v["entities"].as_array().unwrap().is_empty());
            assert!(v["candidates"].as_array().unwrap().is_empty());
        }
        let deps = db.topology_ui_view(&super::super::TopologyUiRequest { tab: "dependencies".into(), ..Default::default() }).unwrap();
        assert!(deps["candidates"].as_array().unwrap().is_empty());
        assert!(db.topology_ui_view(&super::super::TopologyUiRequest { filter: "x".repeat(257), ..Default::default() }).is_err());
        // Attributes are compact before Rust JSON decoding; null/zero remain distinct.
        for entity in ui["entities"].as_array().unwrap().iter().filter(|v| v["kind"] == "docker_container") {
            assert_eq!(entity["attributes"]["ports_total"], 128);
            assert_eq!(entity["attributes"]["ports_omitted"], 112);
            assert!(entity["attributes"]["ports"].as_array().unwrap().len() <= 16);
        }
    }
    #[test]
    fn ui_projection_complete_expiry_eviction_and_transaction_rollback() {
        let (db, lease, now) = setup();
        let p = payload(now);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", p.clone(), now), now).unwrap();
        drop(db.reserve_docker_attempt_fenced(&lease, "reserved-complete", now - 40).unwrap());
        db.conn().execute_batch("CREATE TRIGGER ui_rollback BEFORE DELETE ON docker_ui_rows BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;").unwrap();
        assert!(db.record_topology("reserved-complete", &receipt(&p, ScopeStatus::Complete, now), now).is_err());
        assert_eq!(db.topology_ui_view(&Default::default()).unwrap()["candidates"].as_array().unwrap().len(), 1);
        db.conn().execute_batch("DROP TRIGGER ui_rollback").unwrap();
        // Unreserved legacy receipts do not promote a token; mixed writers remain
        // a separate gate before production admission can be enabled.
        db.record_topology("legacy", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        assert_eq!(db.topology_ui_view(&Default::default()).unwrap()["candidates"].as_array().unwrap().len(), 1);
        db.record_topology("reserved-complete", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        assert_eq!(db.conn().query_row("SELECT count(*) FROM docker_ui_envelopes", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(db.conn().query_row("SELECT count(*) FROM docker_ui_rows", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "new", p, now), now).unwrap();
        let expiry = now - 10 + crate::topology::candidates::CANDIDATE_TTL_MS;
        assert_eq!(super::super::ui::envelopes_for_test(db.conn(), expiry - 1).unwrap(), 1);
        assert_eq!(super::super::ui::envelopes_for_test(db.conn(), expiry).unwrap(), 0);
    }
    #[test]
    fn ui_projection_storage_limit_evicts_whole_scope_and_store_rollback_is_atomic() {
        let (db, lease, now) = setup();
        let p = payload(now);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "old", p.clone(), now), now).unwrap();
        // SQL pressure precondition; this is a cleanup-boundary test, not a
        // production admission or write-performance claim.
        db.conn()
            .execute("UPDATE docker_ui_rows SET display=CAST(zeroblob(67108865) AS TEXT) WHERE row_type='entity' AND kind='docker_container'", [])
            .unwrap();
        let next = relocate(p.clone(), 1);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "next", next.clone(), now), now).unwrap();
        assert_eq!(db.conn().query_row("SELECT count(*) FROM docker_ui_envelopes", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(db.conn().query_row("SELECT count(*) FROM docker_ui_rows WHERE scope_id=?", [&p.scope_id], |r| r.get::<_, i64>(0)).unwrap(), 0);
        db.conn()
            .execute_batch("CREATE TRIGGER projection_rollback BEFORE INSERT ON docker_ui_rows BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;")
            .unwrap();
        let third = relocate(p, 2);
        assert!(db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "third", third.clone(), now), now).is_err());
        assert_eq!(db.conn().query_row("SELECT count(*) FROM docker_candidates WHERE scope_id=?", [third.scope_id], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(db.topology_ui_view(&Default::default()).unwrap()["candidates"][0]["scope_id"], next.scope_id);
    }
    #[test]
    fn ui_accepted_byte_budget_invalid_attributes_and_scope_limit_are_bounded_before_decode() {
        let (db, _, now) = setup();
        // Store graph path permits other adapters' arbitrary attributes. UI must
        // select bounded display fields, including when attributes are invalid.
        for index in 0..80 {
            let mut p = payload(now);
            p.scope_id = format!("scope-{index:03}");
            let mut s = receipt(&p, ScopeStatus::Complete, now);
            s.entities = s.entities.into_iter().take(1).collect();
            s.entities[0].id = format!("id-{index:03}");
            s.entities[0].label = "界".repeat(300);
            s.entities[0].attributes = json!({"unselected_large":"x".repeat(100_000),"memory_bytes":0,"ports":[],"log_unbounded_candidate":null});
            s.edges.clear();
            db.record_topology(&format!("record-{index}"), &s, now).unwrap();
        }
        db.conn().execute("UPDATE topology_entities SET attributes='broken' WHERE id='id-000'", []).unwrap();
        let before = db.conn().total_changes();
        let v = db.topology_ui_view(&Default::default()).unwrap();
        assert!(v.to_string().len() <= 1024 * 1024);
        assert_eq!(v["entities"].as_array().unwrap().len(), 64);
        assert!(v["scopes"].as_array().unwrap().len() <= 64);
        assert_eq!(v["window"]["truncated"], true);
        assert!(v["entities"][0]["attributes"]["memory_bytes"].is_null());
        assert_eq!(v["entities"][1]["attributes"]["memory_bytes"], 0);
        assert!(!v.to_string().contains("unselected_large"));
        assert_eq!(db.conn().total_changes(), before);
    }
    #[test]
    fn ui_one_mib_hard_cap_includes_aliases_metadata_and_preserves_closure_after_trim() {
        let (db, _, now) = setup();
        for index in 0..4 {
            let mut s = receipt(&payload(now), ScopeStatus::Complete, now);
            s.scope_id = format!("{index}{}", "s".repeat(500));
            s.entities.clear();
            s.edges.clear();
            for n in 0..200 {
                let mut e = crate::topology::Entity::new(crate::topology::EntityKind::DockerContainer, "fixture");
                e.id = format!("{n:03}{}", "e".repeat(150));
                e.label = "界".repeat(256);
                e.attributes = json!({"ports":vec![json!({"host_port":65535,"container_port":65535,"protocol":"sctp"});16]});
                for key in ["content_sha256", "value_hash", "revision", "state", "health", "log_driver", "log_max_size"] {
                    e.attributes[key] = json!("x".repeat(160));
                }
                let edge = crate::topology::Edge::new(crate::topology::EdgeKind::DockerContains, &e.id, &e.id);
                s.edges.push(edge);
                s.entities.push(e);
            }
            db.record_topology(&format!("large-{index}"), &s, now).unwrap();
        }
        let v = db.topology_ui_view(&Default::default()).unwrap();
        assert!(v.to_string().len() <= 1024 * 1024);
        assert!(v["entities"].as_array().unwrap().len() < 200, "bytecap actually exercised");
        assert_eq!(v["window"]["stored_totals"]["accepted_entities"], 200);
        assert_eq!(v["window"]["truncated"], true);
        let ids = v["entities"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect::<std::collections::HashSet<_>>();
        for edge in v["edges"].as_array().unwrap() {
            assert!(ids.contains(edge["from"].as_str().unwrap()));
            assert!(ids.contains(edge["to"].as_str().unwrap()));
        }
    }
    #[test]
    fn ui_count_mismatch_in_unselected_rows_omits_whole_scope() {
        for change in [
            "DELETE FROM docker_ui_rows WHERE kind='docker_container' AND row_type='entity'",
            "INSERT INTO docker_ui_rows SELECT scope_id,row_type,id||'extra',kind,search,from_id,to_id,display,digest FROM docker_ui_rows WHERE kind='docker_container' AND row_type='entity'",
            "DELETE FROM docker_ui_rows WHERE row_type='edge'",
        ] {
            let (db, lease, now) = setup();
            let p = payload(now);
            db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", p, now), now).unwrap();
            db.conn().execute(change, []).unwrap();
            let v = db.topology_ui_view(&super::super::TopologyUiRequest { kind: "docker_engine".into(), ..Default::default() }).unwrap();
            assert!(v["candidates"].as_array().unwrap().is_empty(), "unselected count mismatch: {change}");
            assert_eq!(v["window"]["corrupt_scopes_hidden"], 1);
            assert_eq!(v["window"]["stored_totals"]["candidate_entities"], 0);
            assert!(!v["entities"].as_array().unwrap().is_empty());
        }
    }
    #[test]
    fn ui_invalid_utf8_optional_candidate_metadata_and_selected_rows_do_not_hide_accepted() {
        for (table, column) in [
            ("docker_ui_envelopes", "envelope"),
            ("docker_ui_envelopes", "digest"),
            ("docker_candidate_scopes", "latest_attempt"),
            ("docker_candidate_scopes", "latest_status"),
            ("docker_ui_rows", "display"),
            ("docker_ui_rows", "digest"),
            ("docker_ui_rows", "search"),
            ("docker_ui_rows", "from_id"),
            ("docker_ui_rows", "to_id"),
            ("docker_ui_rows", "id"),
        ] {
            let (db, lease, now) = setup();
            let p = payload(now);
            db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", p, now), now).unwrap();
            let accepted = db.topology_accepted_view().unwrap();
            let selected = if table == "docker_ui_rows" { " WHERE row_type='entity' AND kind='docker_container'" } else { "" };
            db.conn().execute(&format!("UPDATE {table} SET {column}=CAST(x'{}' AS TEXT){selected}", "80".repeat(64)), []).unwrap();
            let writes = db.conn().total_changes();
            let v = db.topology_ui_view(&Default::default()).unwrap();
            assert!(v["candidates"].as_array().unwrap().is_empty(), "{table}.{column}");
            assert!(!v["entities"].as_array().unwrap().is_empty());
            assert_eq!(v["window"]["corrupt_scopes_hidden"], 1);
            assert_eq!(db.topology_accepted_view().unwrap(), accepted);
            assert_eq!(db.conn().total_changes(), writes);
        }
    }
    #[test]
    fn ui_delayed_complete_and_old_holder_cannot_remove_newer_projection() {
        let (db, lease, now) = setup();
        let p = payload(now);
        drop(db.reserve_docker_attempt_fenced(&lease, "old-complete", now - 40).unwrap());
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "newer", p.clone(), now), now).unwrap();
        db.record_topology("old-complete", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        assert_eq!(db.topology_view().unwrap()["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(db.topology_ui_view(&Default::default()).unwrap()["candidates"].as_array().unwrap().len(), 1, "older Complete cannot erase newer candidate");
        drop(db.reserve_docker_attempt_fenced(&lease, "current-complete", now - 40).unwrap());
        db.record_topology("current-complete", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
        assert!(db.topology_ui_view(&Default::default()).unwrap()["candidates"].as_array().unwrap().is_empty());
        drop(db.reserve_docker_attempt_fenced(&lease, "old-holder", now - 40).unwrap());
        db.release_topology_lease_fenced(&lease).unwrap();
        let replacement = db.acquire_topology_lease_fenced("replacement", now, 10_000).unwrap().unwrap();
        db.record_docker_candidate_fenced(&replacement, admit(&db, &replacement, "new-holder", p.clone(), now), now).unwrap();
        db.record_topology("old-holder", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        assert_eq!(db.topology_view().unwrap()["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(db.topology_ui_view(&Default::default()).unwrap()["candidates"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn ui_expired_and_superseded_projection_reasons_are_not_corruption() {
        let (db, lease, now) = setup();
        let p = payload(now);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", p, now), now).unwrap();
        let scan = super::super::ui::scan_status_for_test(db.conn(), now - 10 + crate::topology::candidates::CANDIDATE_TTL_MS).unwrap();
        assert_eq!(scan, (0, vec![], vec!["expired_projection"]));
        db.conn().execute("UPDATE docker_candidate_scopes SET complete_ordinal=latest_ordinal,latest_status='complete'", []).unwrap();
        let v = db.topology_ui_view(&Default::default()).unwrap();
        assert!(v["candidates"].as_array().unwrap().is_empty());
        assert_eq!(v["window"]["corrupt_scopes_hidden"], 0);
        assert_eq!(v["window"]["candidate_unavailable_reasons"], json!([]));
        assert_eq!(v["window"]["candidate_omission_reasons"], json!(["superseded_projection"]));
    }
    #[test]
    fn ui_same_column_view_is_unavailable_without_mutating_accepted_or_schema() {
        let (db, lease, now) = setup();
        let p = payload(now);
        db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", p, now), now).unwrap();
        let before = db.topology_accepted_view().unwrap();
        db.conn()
            .execute_batch("ALTER TABLE docker_ui_rows RENAME TO docker_ui_backing;CREATE VIEW docker_ui_rows AS SELECT * FROM docker_ui_backing;")
            .unwrap();
        let schema: String = db.conn().query_row("SELECT sql FROM sqlite_schema WHERE name='docker_ui_rows'", [], |r| r.get(0)).unwrap();
        let writes = db.conn().total_changes();
        let v = db.topology_ui_view(&Default::default()).unwrap();
        assert!(v["candidates"].as_array().unwrap().is_empty(), "matching column names do not make a view a known projection table");
        assert_eq!(v["window"]["candidate_unavailable_reasons"], json!(["unknown_projection_schema"]));
        assert_eq!(db.topology_accepted_view().unwrap(), before);
        assert_eq!(db.conn().total_changes(), writes);
        assert_eq!(db.conn().query_row("SELECT sql FROM sqlite_schema WHERE name='docker_ui_rows'", [], |r| r.get::<_, String>(0)).unwrap(), schema);
    }
    #[test]
    fn ui_unknown_projection_does_not_block_reserved_complete_or_mutate_unknown_objects() {
        for (table, ddl) in [
            ("docker_ui_rows", "CREATE TABLE docker_ui_rows(old TEXT);INSERT INTO docker_ui_rows VALUES ('preserved');"),
            ("docker_ui_rows", "CREATE VIEW docker_ui_rows AS SELECT 'preserved' AS old;"),
            ("docker_ui_envelopes", "CREATE TABLE docker_ui_envelopes(old TEXT);INSERT INTO docker_ui_envelopes VALUES ('preserved');"),
            ("docker_ui_envelopes", "CREATE VIEW docker_ui_envelopes AS SELECT 'preserved' AS old;"),
        ] {
            let (db, lease, now) = setup();
            let p = payload(now);
            db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ui", p.clone(), now), now).unwrap();
            db.conn().execute_batch(&format!("DROP TABLE {table};{ddl}")).unwrap();
            let schema: String = db.conn().query_row("SELECT sql FROM sqlite_schema WHERE name=?", [table], |r| r.get(0)).unwrap();
            // A Partial save cannot be admitted into unknown materialized storage;
            // all candidate/ledger writes in its transaction must roll back.
            assert!(db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "refused-partial", p.clone(), now), now).is_err());
            assert_eq!(db.topology_view().unwrap()["candidates"][0]["attempt_id"], "ui");
            let complete = db.reserve_docker_attempt_fenced(&lease, "complete", now - 40).unwrap();
            drop(complete);
            let mut fresh = receipt(&p, ScopeStatus::Complete, now - 5);
            for e in &mut fresh.entities {
                e.label = "updated accepted".into();
            }
            db.record_topology_fenced(&lease, "complete", &fresh, now).expect("optional unknown cleanup must not prevent an accepted Complete");
            assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty(), "canonical candidate superseded by ordinal");
            let accepted = db.topology_accepted_view().unwrap();
            assert!(accepted["entities"].as_array().unwrap().iter().all(|e| e["label"] == "updated accepted" && e["facts_observed_at_ms"] == now - 5));
            let (latest, complete): (i64, i64) = db
                .conn()
                .query_row("SELECT latest_ordinal,complete_ordinal FROM docker_candidate_scopes WHERE scope_id=?", [&p.scope_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(latest, complete);
            assert_eq!(db.conn().query_row("SELECT sql FROM sqlite_schema WHERE name=?", [table], |r| r.get::<_, String>(0)).unwrap(), schema);
            assert_eq!(db.conn().query_row(&format!("SELECT old FROM {table}"), [], |r| r.get::<_, String>(0)).unwrap(), "preserved");
            let writes = db.conn().total_changes();
            let v = db.topology_ui_view(&Default::default()).unwrap();
            assert!(v["candidates"].as_array().unwrap().is_empty());
            assert!(!v["entities"].as_array().unwrap().is_empty());
            assert_eq!(v["window"]["candidate_unavailable_reasons"], json!(["unknown_projection_schema"]));
            assert_eq!(db.conn().total_changes(), writes);
        }
    }
    #[test]
    fn synthetic_admission_saves_candidate_without_applying_graph() {
        let db = Store::open_in_memory().unwrap();
        let now = crate::db::now_ms();
        let lease = db.acquire_topology_lease_fenced("fixture-writer", now - 100, 10000).unwrap().unwrap();
        let attempt = db.reserve_docker_attempt_fenced(&lease, "attempt-1", now - 40).unwrap();
        db.record_docker_candidate_fenced(&lease, AdmittedDockerPartial::synthetic(attempt, payload(now)), now).unwrap();
        let view = db.topology_view().unwrap();
        assert_eq!(view["candidates"].as_array().unwrap().len(), 1);
        assert!(view["entities"].as_array().unwrap().is_empty());
        assert!(view["edges"].as_array().unwrap().is_empty());
        assert_eq!(view["candidates"][0]["is_latest_attempt"], true);
    }
    fn setup() -> (Store, TopologyLease, i64) {
        let db = Store::open_in_memory().unwrap();
        let now = crate::db::now_ms();
        let lease = db.acquire_topology_lease_fenced("fixture-writer", now - 100, 10000).unwrap().unwrap();
        (db, lease, now)
    }
    fn admit(db: &Store, lease: &TopologyLease, id: &str, p: DockerCandidatePayload, now: i64) -> AdmittedDockerPartial {
        let attempt = db.reserve_docker_attempt_fenced(lease, id, now - 40).unwrap();
        AdmittedDockerPartial::synthetic(attempt, p)
    }
    fn receipt(p: &DockerCandidatePayload, status: ScopeStatus, at: i64) -> TopologySnapshot {
        let mut entities = p.entities.clone();
        if status == ScopeStatus::Complete {
            for entity in &mut entities {
                if entity.kind == crate::topology::EntityKind::DockerContainer {
                    entity.attributes["networks"] = json!([]);
                }
            }
        }
        TopologySnapshot {
            scope_id: p.scope_id.clone(),
            status,
            entities,
            edges: p.edges.clone(),
            failures: vec![],
            counters: Default::default(),
            observed_at_ms: at,
        }
    }
    #[test]
    fn synthetic_candidate_keeps_accepted_cache_cursor_and_changes_unchanged() {
        let (db, lease, now) = setup();
        let p = payload(now);
        db.record_topology("old-good", &receipt(&p, ScopeStatus::Complete, now - 500), now - 500).unwrap();
        db.conn().execute("INSERT INTO topology_source_cache VALUES ('fixture','digest','parser','kind',NULL,1,'{}')", []).unwrap();
        db.conn().execute("INSERT INTO topology_progress VALUES ('fixture','{}',1)", []).unwrap();
        let before = db.topology_view().unwrap();
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "candidate", p, now), now).unwrap();
        let after = db.topology_view().unwrap();
        for key in ["entities", "edges", "changes", "scopes", "history"] {
            assert_eq!(before[key], after[key], "{key}");
        }
        for table in ["topology_source_cache", "topology_progress"] {
            let count: i64 = db.conn().query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0)).unwrap();
            assert_eq!(count, 1);
        }
    }
    #[test]
    fn reserved_ordinal_orders_failure_complete_and_delayed_candidates_not_clock_or_hash() {
        let (db, lease, now) = setup();
        let p = payload(now);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "z-first", p.clone(), now), now).unwrap();
        let pending = admit(&db, &lease, "pending-second", p.clone(), now);
        let failed = db.reserve_docker_attempt_fenced(&lease, "a-third", now - 50).unwrap();
        drop(failed);
        db.record_topology("a-third", &receipt(&p, ScopeStatus::Failed, now - 200), now - 20).unwrap();
        let view = db.topology_view().unwrap();
        assert_eq!(view["candidates"][0]["attempt_id"], "z-first");
        assert_eq!(view["candidates"][0]["facts_observed_at_ms"], now - 10);
        assert_eq!(view["candidates"][0]["is_latest_attempt"], false);
        let ui = db.topology_ui_view(&Default::default()).unwrap();
        assert_eq!(ui["candidates"][0]["attempt_id"], "z-first");
        assert_eq!(ui["candidates"][0]["is_latest_attempt"], false);
        assert!(db.record_docker_candidate_fenced(&lease, pending, now).is_err());
        let complete = db.reserve_docker_attempt_fenced(&lease, "complete-fourth", now - 40).unwrap();
        drop(complete);
        db.record_topology("complete-fourth", &receipt(&p, ScopeStatus::Complete, now - 300), now).unwrap();
        assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "partial-fifth", p, now), now).unwrap();
        assert_eq!(db.topology_view().unwrap()["candidates"][0]["is_latest_attempt"], true);
    }
    #[test]
    fn tampered_payload_source_settings_attempt_and_expired_writer_are_rejected() {
        for field in ["source", "settings", "scope", "facts", "attributes"] {
            let (db, lease, now) = setup();
            let mut a = admit(&db, &lease, "tamper", payload(now), now);
            a.tamper(|p| match field {
                "source" => p.source_generation = "c".repeat(64),
                "settings" => p.settings_generation = "d".repeat(64),
                "scope" => p.scope_id = format!("other:{}", p.scope_id),
                "facts" => p.facts_started_at_ms -= 1,
                _ => p.entities[0].label = "unapproved".into(),
            });
            assert!(db.record_docker_candidate_fenced(&lease, a, now).is_err(), "{field}");
            assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
        }
        let (db, lease, now) = setup();
        let a = admit(&db, &lease, "expired", payload(now), now);
        db.release_topology_lease_fenced(&lease).unwrap();
        let replacement = db.acquire_topology_lease_fenced("replacement-writer", now, 10000).unwrap().unwrap();
        assert_ne!(lease.generation, replacement.generation);
        assert!(db.record_docker_candidate_fenced(&replacement, a, now).is_err());
        let a = admit(&db, &replacement, "expired-clock", payload(now), now);
        assert!(db.record_docker_candidate_fenced(&replacement, a, now + 10000).is_err());
    }
    #[test]
    fn synthetic_invalid_projection_and_policy_matrix_never_stores_candidate() {
        type Change = fn(&mut DockerCandidatePayload);
        let cases: [(&str, Change); 17] = [
            ("version", |p| p.version = 2),
            ("source", |p| p.source_generation.clear()),
            ("settings", |p| p.settings_generation = "A".repeat(64)),
            ("engine", |p| p.engine_identity = "invalid".into()),
            ("other engine", |p| p.engine_identity = "f".repeat(64)),
            ("source scope", |p| p.scope_id = format!("other:{}", p.scope_id)),
            ("scope", |p| p.scope_id = "fixture:native".into()),
            ("interval", |p| p.facts_started_at_ms = p.attempt_started_at_ms - 1),
            ("future", |p| p.attempt_finished_at_ms += 1),
            ("gap", |p| p.gaps.clear()),
            ("duplicate gap", |p| p.gaps.push(p.gaps[0])),
            ("duplicate entity", |p| p.entities.push(p.entities[0].clone())),
            ("duplicate edge", |p| p.edges.push(p.edges[0].clone())),
            ("endpoint", |p| p.edges[0].to = "absent".into()),
            ("private label", |p| p.entities[0].label = "private-canary".into()),
            ("private attribute", |p| p.entities[0].attributes["Env"] = json!(["private-canary"])),
            ("non-Docker", |p| p.entities[0].kind = crate::topology::EntityKind::Package),
        ];
        for (name, change) in cases {
            let (db, lease, now) = setup();
            let mut p = payload(now);
            change(&mut p);
            assert!(db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "invalid", p, now), now).is_err(), "{name}");
            assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
        }
    }
    #[test]
    fn ttl_is_inclusive_and_reads_do_not_delete_expired_rows() {
        let (db, lease, now) = setup();
        let p = payload(now);
        let expiry = p.facts_observed_at_ms + crate::topology::candidates::CANDIDATE_TTL_MS;
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "ttl", p.clone(), now), now).unwrap();
        assert_eq!(read_candidates(db.conn(), expiry - 1).unwrap().len(), 1);
        assert!(read_candidates(db.conn(), expiry).unwrap().is_empty());
        let count: i64 = db.conn().query_row("SELECT count(*) FROM docker_candidates", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        assert!(validate(&p, expiry).is_err());
        assert!(validate(&p, expiry - 1).is_ok());
    }
    #[test]
    fn corrupt_or_unknown_payloads_fail_closed_and_old_database_reads_are_graceful() {
        let (db, lease, now) = setup();
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "corrupt", payload(now), now), now).unwrap();
        let raw: String = db.conn().query_row("SELECT payload FROM docker_candidates", [], |row| row.get(0)).unwrap();
        for raw in ["{bad".into(), raw.replace("\"version\":1", "\"version\":2"), raw + " "] {
            db.conn()
                .execute("UPDATE docker_candidates SET payload=?,digest=?,bytes=?", params![raw, crate::db::sha256_hex(raw.as_bytes()), raw.len() as i64])
                .unwrap();
            assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
        }
        db.conn()
            .execute_batch(
                "DROP TABLE docker_candidates; DROP TABLE docker_candidate_scopes; DROP TABLE docker_candidate_attempts; DROP TABLE docker_candidate_sequence;",
            )
            .unwrap();
        assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
        db.conn().execute_batch(super::super::SCHEMA).unwrap();
        db.conn().execute_batch(super::super::SCHEMA).unwrap();
        assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
    }
    #[test]
    fn reservation_history_is_bounded_and_evicted_tokens_cannot_replay() {
        let (db, lease, now) = setup();
        let old = admit(&db, &lease, "old", payload(now), now);
        for i in 0..512 {
            db.reserve_docker_attempt_fenced(&lease, &format!("next-{i}"), now - 40).unwrap();
        }
        let count: i64 = db.conn().query_row("SELECT count(*) FROM docker_candidate_attempts", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 512);
        assert!(db.record_docker_candidate_fenced(&lease, old, now).is_err());
        assert!(db.reserve_docker_attempt_fenced(&lease, "next-511", now).is_err());
        let last: i64 = db.conn().query_row("SELECT ordinal FROM docker_candidate_sequence", [], |row| row.get(0)).unwrap();
        assert_eq!(last, 513);
    }
    fn expanded_payload(now: i64, mounts: usize, network_members: Option<usize>, ports: usize) -> DockerCandidatePayload {
        use crate::topology::adapters::docker::{Mount, Network, PublishedPort};
        let mut c =
            parse_container(&json!({"id":"expanded","state":"exited","health":null,"log_driver":"local","mounts":[],"networks":[],"ports":[]})).unwrap();
        c.ports = vec![PublishedPort { container_port: 80, protocol: "tcp".into(), host_port: 8080 }; ports];
        let mut engine = DockerEngine { id: "e".repeat(64), containers: vec![], networks: vec![], volumes: vec![] };
        if network_members.is_some() {
            engine.networks = (0..32)
                .map(|i| Network { id: crate::db::sha256_hex(format!("network-{i}").as_bytes()), driver: Some("bridge".into()), internal: None })
                .collect();
        }
        let mut remaining_mounts = mounts;
        let mut remaining_members = network_members.unwrap_or(0);
        for i in 0..512 {
            let mut c = c.clone();
            c.id = crate::db::sha256_hex(format!("container-{i}").as_bytes());
            let count = remaining_mounts.min(7);
            remaining_mounts -= count;
            c.mounts =
                vec![
                    Mount { kind: "bind".into(), source_hash: Some("a".repeat(64)), target_hash: "b".repeat(64), volume_hash: None, writable: Some(false) };
                    count
                ];
            let count = remaining_members.min(32);
            remaining_members -= count;
            c.networks = engine.networks.iter().take(count).map(|n| n.id.clone()).collect();
            if i == 0 {
                c.networks.push("c".repeat(64));
            }
            engine.containers.push(c);
        }
        assert_eq!(remaining_mounts, 0);
        assert_eq!(remaining_members, 0);
        let snapshot = engine_snapshots(DockerPacket { engines: vec![engine] }.snapshot("fixture:native:docker".into(), now).unwrap()).remove(1);
        let mut p = payload(now);
        p.entities = snapshot.entities;
        p.edges = snapshot.edges;
        p
    }
    #[test]
    fn exact_entity_and_edge_limits_accept_closed_graphs_and_reject_one_more_atomically() {
        let (db, lease, now) = setup();
        let p = expanded_payload(now, 3581, None, 0);
        assert_eq!(p.entities.len(), 4096);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "entities-limit", p, now), now).unwrap();
        let too_many = expanded_payload(now, 3582, None, 0);
        assert_eq!(too_many.entities.len(), 4097);
        assert!(db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "entities-over", too_many, now), now).is_err());
        let p = expanded_payload(now, 0, Some(15838), 0);
        assert_eq!(p.edges.len(), 16384);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "edges-limit", p, now), now).unwrap();
        let too_many = expanded_payload(now, 0, Some(15839), 0);
        assert_eq!(too_many.edges.len(), 16385);
        assert!(db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "edges-over", too_many, now), now).is_err());
        let saved: String = db.conn().query_row("SELECT attempt_id FROM docker_candidates", [], |row| row.get(0)).unwrap();
        assert_eq!(saved, "edges-limit");
    }
    fn relocate(mut p: DockerCandidatePayload, index: usize) -> DockerCandidatePayload {
        // Rebuild anchors for a distinct synthetic source; never join candidate scopes.
        let base = format!("fixture-{index}:native:docker");
        let mut engine = DockerEngine { id: p.engine_identity.clone(), containers: vec![], networks: vec![], volumes: vec![] };
        for e in &p.entities {
            match e.kind {
                crate::topology::EntityKind::DockerContainer => engine.containers.push(serde_json::from_value(e.attributes.clone()).unwrap()),
                crate::topology::EntityKind::DockerNetwork => engine.networks.push(serde_json::from_value(e.attributes.clone()).unwrap()),
                crate::topology::EntityKind::DockerVolume => engine.volumes.push(serde_json::from_value(e.attributes.clone()).unwrap()),
                _ => {}
            }
        }
        let snapshot = engine_snapshots(DockerPacket { engines: vec![engine] }.snapshot(base, p.facts_observed_at_ms).unwrap()).remove(1);
        p.scope_id = snapshot.scope_id;
        p.entities = snapshot.entities;
        p.edges = snapshot.edges;
        p
    }
    #[test]
    fn global_scope_limit_evicts_only_old_candidates_and_keeps_last_good() {
        let (db, lease, now) = setup();
        let p = payload(now);
        db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now - 500), now - 500).unwrap();
        let before = db.topology_entities(&p.scope_id).unwrap();
        for i in 0..65 {
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, &format!("scope-{i}"), relocate(p.clone(), i), now), now).unwrap();
        }
        let count: i64 = db.conn().query_row("SELECT count(*) FROM docker_candidates", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 64);
        let first: bool = db.conn().query_row("SELECT EXISTS(SELECT 1 FROM docker_candidates WHERE attempt_id='scope-0')", [], |row| row.get(0)).unwrap();
        assert!(!first);
        assert_eq!(db.topology_entities(&p.scope_id).unwrap(), before);
    }
    #[test]
    fn utf8_eight_mib_encoding_is_inclusive_and_oversize_rejection_preserves_candidate() {
        use crate::topology::candidates::{CANDIDATE_MAX_BYTES, encode};
        let exact = "é".repeat((CANDIDATE_MAX_BYTES - 2) / 2);
        assert_eq!(encode(&exact).unwrap().len(), CANDIDATE_MAX_BYTES);
        assert!(encode(&(exact + "x")).is_err());
        let (db, lease, now) = setup();
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "good", payload(now), now), now).unwrap();
        let mut p = payload(now);
        p.entities[0].label = "x".repeat(CANDIDATE_MAX_BYTES);
        assert!(db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "too-large", p, now), now).is_err());
        let attempt: String = db.conn().query_row("SELECT attempt_id FROM docker_candidates", [], |row| row.get(0)).unwrap();
        assert_eq!(attempt, "good");
    }
    #[test]
    fn global_byte_limit_evicts_oldest_valid_candidates_without_touching_accepted() {
        let (db, lease, now) = setup();
        let p = expanded_payload(now, 3581, None, 100);
        let size = validate(&p, now).unwrap().len();
        assert!(size > 4 * 1024 * 1024 && size <= 8 * 1024 * 1024);
        db.record_topology("accepted", &receipt(&payload(now), ScopeStatus::Complete, now - 500), now - 500).unwrap();
        let before = db.topology_entities(&p.scope_id).unwrap();
        let iterations = 64 * 1024 * 1024 / size + 2;
        for i in 0..iterations {
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, &format!("bytes-{i}"), relocate(p.clone(), i), now), now).unwrap();
        }
        let (count, bytes): (i64, i64) =
            db.conn().query_row("SELECT count(*),sum(bytes) FROM docker_candidates", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert!(bytes <= 64 * 1024 * 1024);
        assert!((count as usize) < iterations);
        let first: bool = db.conn().query_row("SELECT EXISTS(SELECT 1 FROM docker_candidates WHERE attempt_id='bytes-0')", [], |row| row.get(0)).unwrap();
        assert!(!first);
        assert_eq!(db.topology_entities(&p.scope_id).unwrap(), before);
    }
    #[test]
    fn scope_ledger_pressure_invalidates_old_reservations_before_pruning_replay_history() {
        let (db, lease, now) = setup();
        let p = payload(now);
        let old = admit(&db, &lease, "old-scope", p.clone(), now);
        // Synthetic unit precondition; this does not benchmark 4097 receipt/prune cycles.
        db.tx(|conn| {
            for i in 0..4096 {
                conn.execute("INSERT INTO docker_candidate_scopes VALUES (?,1,'old-scope','failed',0)", [format!("fixture-{i}:docker:engine:opaque")])?;
            }
            Ok(())
        })
        .unwrap();
        db.record_topology("old-scope", &receipt(&p, ScopeStatus::Failed, now), now).unwrap();
        let count: i64 = db.conn().query_row("SELECT count(*) FROM docker_candidate_scopes", [], |row| row.get(0)).unwrap();
        assert!(count <= 4096);
        assert!(db.record_docker_candidate_fenced(&lease, old, now).is_err());
    }
    #[test]
    fn read_only_legacy_database_does_not_migrate_or_create_candidate_tables() {
        let path = std::env::temp_dir().join(format!(
            "katala-candidate-legacy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let db = Store::open(&path).unwrap();
        db.conn().execute_batch("DROP TABLE docker_candidates;DROP TABLE docker_candidate_scopes;DROP TABLE docker_candidate_attempts;DROP TABLE docker_candidate_sequence;PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        drop(db);
        let before = std::fs::read(path.join(crate::db::DB_FILE)).unwrap();
        let db = Store::open_read_only(&path).unwrap();
        assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
        assert!(!has_tables(db.conn()).unwrap());
        drop(db);
        assert_eq!(std::fs::read(path.join(crate::db::DB_FILE)).unwrap(), before);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn missing_volume_is_an_exact_typed_gap_but_not_a_manufactured_endpoint() {
        let (db, lease, now) = setup();
        let mut p = payload(now);
        let c=parse_container(&json!({"id":"container","state":"exited","health":null,"log_driver":"local","mounts":[{"kind":"volume","source":"/fixture","target":"/data","name":null,"rw":false}],"networks":[],"ports":[]})).unwrap();
        let snapshot = engine_snapshots(
            DockerPacket { engines: vec![DockerEngine { id: p.engine_identity.clone(), containers: vec![c], networks: vec![], volumes: vec![] }] }
                .snapshot("fixture:native:docker".into(), now)
                .unwrap(),
        )
        .remove(1);
        p.entities = snapshot.entities;
        p.edges = snapshot.edges;
        p.gaps = vec![crate::topology::candidates::DockerSemanticGap::UnobservedVolumeTarget];
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "volume", p, now), now).unwrap();
        let view = db.topology_view().unwrap();
        assert!(!view["candidates"][0]["entities"].as_array().unwrap().iter().any(|e| e["kind"] == "docker_volume"));
    }
    #[test]
    fn reservation_binding_and_expired_candidate_cleanup_are_transactional() {
        let (db, lease, now) = setup();
        let p = payload(now);
        let a = admit(&db, &lease, "wrong-reservation", p.clone(), now);
        db.conn().execute("UPDATE docker_candidate_attempts SET holder='other-writer' WHERE attempt_id='wrong-reservation'", []).unwrap();
        assert!(db.record_docker_candidate_fenced(&lease, a, now).is_err());
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "old", p.clone(), now), now).unwrap();
        let expiry = p.facts_observed_at_ms + crate::topology::candidates::CANDIDATE_TTL_MS;
        let a = admit(&db, &lease, "expired-payload", p.clone(), now);
        assert!(db.record_docker_candidate_fenced(&lease, a, expiry).is_err());
        let next_lease = db.acquire_topology_lease_fenced("future-writer", expiry, 10000).unwrap().unwrap();
        let next = relocate(payload(expiry + 100), 1);
        db.record_docker_candidate_fenced(&next_lease, admit(&db, &next_lease, "new", next, expiry + 100), expiry + 100).unwrap();
        let count: i64 = db.conn().query_row("SELECT count(*) FROM docker_candidates", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        let exists: bool = db.conn().query_row("SELECT EXISTS(SELECT 1 FROM docker_candidates WHERE attempt_id='old')", [], |row| row.get(0)).unwrap();
        assert!(!exists);
    }
    #[test]
    fn corrupt_candidate_or_ledger_metadata_is_skipped_without_hiding_accepted_or_writing() {
        let cases = [
            ("docker_candidates", "digest", "x'00'"),
            ("docker_candidates", "ordinal", "'not-an-integer'"),
            ("docker_candidates", "writer_generation", "'not-an-integer'"),
            ("docker_candidates", "bytes", "'not-an-integer'"),
            ("docker_candidates", "attempt_id", "x'00'"),
            ("docker_candidates", "scope_id", "x'00'"),
            ("docker_candidate_scopes", "latest_ordinal", "'not-an-integer'"),
            ("docker_candidate_scopes", "complete_ordinal", "'not-an-integer'"),
            ("docker_candidate_scopes", "latest_status", "x'00'"),
            ("docker_candidate_scopes", "latest_attempt", "x'00'"),
            ("docker_candidate_scopes", "latest_status", "'unknown'"),
            ("docker_candidates", "ordinal", "-1"),
            ("docker_candidates", "writer_generation", "-1"),
            ("docker_candidates", "bytes", "-1"),
        ];
        for (table, column, value) in cases {
            let (db, lease, now) = setup();
            let p = payload(now);
            db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now - 500), now - 500).unwrap();
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "candidate", p, now), now).unwrap();
            let accepted = db.topology_view().unwrap();
            db.conn().execute(&format!("UPDATE {table} SET {column}={value}"), []).unwrap();
            let saved: String = db.conn().query_row(&format!("SELECT quote({column}) FROM {table}"), [], |row| row.get(0)).unwrap();
            let changes = db.conn().total_changes();
            let read = db.topology_view().unwrap();
            assert!(read["candidates"].as_array().unwrap().is_empty(), "{table}.{column}");
            for key in ["entities", "edges", "scopes", "history", "changes"] {
                assert_eq!(read[key], accepted[key], "{table}.{column}/{key}");
            }
            assert_eq!(db.conn().total_changes(), changes);
            let after: String = db.conn().query_row(&format!("SELECT quote({column}) FROM {table}"), [], |row| row.get(0)).unwrap();
            assert_eq!(saved, after);
        }
        let (db, lease, now) = setup();
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "invalid-utf8", payload(now), now), now).unwrap();
        let bytes = vec![0x80u8; 64];
        db.conn().execute("UPDATE docker_candidates SET digest=CAST(? AS TEXT)", [bytes]).unwrap();
        assert!(db.topology_view().unwrap()["candidates"].as_array().unwrap().is_empty());
    }
    #[test]
    fn old_reservation_cannot_change_ledger_under_a_new_live_holder() {
        let (db, lease, now) = setup();
        let p = payload(now);
        db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "candidate", p.clone(), now), now).unwrap();
        db.reserve_docker_attempt_fenced(&lease, "old-fenced", now - 40).unwrap();
        db.reserve_docker_attempt_fenced(&lease, "old-legacy", now - 40).unwrap();
        let before = db.topology_view().unwrap()["candidates"].clone();
        db.release_topology_lease_fenced(&lease).unwrap();
        let next = db.acquire_topology_lease_fenced("new-holder", now, 10000).unwrap().unwrap();
        db.record_topology_fenced(&next, "old-fenced", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        assert_eq!(db.topology_view().unwrap()["candidates"], before);
        db.record_topology("old-legacy", &receipt(&p, ScopeStatus::Complete, now), now).unwrap();
        assert_eq!(db.topology_view().unwrap()["candidates"], before);
        assert!(!db.topology_entities(&p.scope_id).unwrap().is_empty(), "ordinary accepted path remains available");
    }
    #[test]
    fn partial_candidate_schema_in_read_only_database_keeps_accepted_visible() {
        for (case, ddl) in [
            ("missing-scopes", "DROP TABLE docker_candidate_scopes"),
            ("missing-column", "ALTER TABLE docker_candidate_scopes RENAME COLUMN latest_status TO unknown_status"),
            ("unknown-column", "ALTER TABLE docker_candidates ADD COLUMN unknown_column TEXT"),
        ] {
            let path = std::env::temp_dir().join(format!(
                "katala-candidate-schema-{case}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            let db = Store::open(&path).unwrap();
            let now = crate::db::now_ms();
            let lease = db.acquire_topology_lease_fenced("fixture", now - 100, 10000).unwrap().unwrap();
            let p = payload(now);
            db.record_topology("accepted", &receipt(&p, ScopeStatus::Complete, now - 500), now - 500).unwrap();
            db.record_docker_candidate_fenced(&lease, admit(&db, &lease, "candidate", p, now), now).unwrap();
            let before = db.topology_view().unwrap();
            db.conn().execute_batch(ddl).unwrap();
            db.conn().execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
            drop(db);
            let bytes = std::fs::read(path.join(crate::db::DB_FILE)).unwrap();
            let db = Store::open_read_only(&path).unwrap();
            let changes = db.conn().total_changes();
            let read = db.topology_view().unwrap();
            assert!(read["candidates"].as_array().unwrap().is_empty(), "{case}");
            for key in ["entities", "edges", "scopes", "history", "changes"] {
                assert_eq!(read[key], before[key], "{case}/{key}");
            }
            assert_eq!(db.conn().total_changes(), changes);
            drop(db);
            assert_eq!(std::fs::read(path.join(crate::db::DB_FILE)).unwrap(), bytes);
            std::fs::remove_dir_all(path).unwrap();
        }
    }
}
