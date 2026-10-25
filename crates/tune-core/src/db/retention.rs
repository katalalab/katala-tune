//! Numeric monitor history with a dedicated additive schema, shared with JS.
//! Store initialization registers the tables; maintenance never starts a collector.
use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Result, params};
use serde_json::{Map, Value, json};

use crate::js;

pub const HOUR: i64 = 3_600_000;
pub const DAY: i64 = 24 * HOUR;
const SAFE: i64 = 9_007_199_254_740_991;
const MAX_ROWS: i64 = 50_000;
const MAX_BYTES: i64 = 16_777_216;
pub(super) const SCHEMA: &str = include_str!("../../../../config/metrics-retention.sql");
const SIZE: &str = "length(CAST(node_id AS BLOB)) + length(CAST(v AS BLOB)) + 80";
const KEYS: [&str; 15] =
    ["cpu", "mem", "swap", "commit", "dr", "dw", "rx", "tx", "power_cpu_w", "power_soc_w", "power_platform_w", "power_gpu_w", "gpu", "gmem", "gtemp"];

#[derive(Clone, Copy)]
pub struct Budget {
    pub max_rows: i64,
    pub max_bytes: i64,
}
impl Default for Budget {
    fn default() -> Self {
        Self { max_rows: MAX_ROWS, max_bytes: MAX_BYTES }
    }
}

pub struct Minute {
    pub node_id: String,
    pub minute: i64,
    pub n: Option<i64>,
    pub v: Value,
}
fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.into())
}
fn clock(t: i64) -> bool {
    (0..=SAFE).contains(&t)
}
fn node_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
}
fn nonnegative(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite() && *x >= 0.0)
}
fn floor(t: i64, width: i64) -> i64 {
    t.max(0).div_euclid(width) * width
}
fn parsed(s: &str) -> Result<Value> {
    serde_json::from_str(s).map_err(|_| invalid("invalid numeric history JSON"))
}

/// Count valid minutes independently for every metric, not samples or hours.
pub fn merge_stats(values: &[Value]) -> Result<Value> {
    let mut out = Map::new();
    for v in values {
        for k in KEYS {
            let x = &v[k];
            if x.is_null() {
                continue;
            }
            let a = x.as_array().filter(|a| a.len() == 3).ok_or_else(|| invalid("invalid numeric history stats"))?;
            let sum = nonnegative(&a[0]).ok_or_else(|| invalid("invalid numeric history stats"))?;
            let n = a[1].as_f64().filter(|n| n.fract() == 0.0 && *n > 0.0 && *n <= SAFE as f64).ok_or_else(|| invalid("invalid numeric history stats"))? as i64;
            let max = nonnegative(&a[2]).filter(|m| *m <= 1e12).ok_or_else(|| invalid("invalid numeric history stats"))?;
            if sum / n as f64 > max + max.max(1.0) * 1e-9 {
                return Err(invalid("invalid numeric history stats"));
            }
            let old = out.get(k).cloned().unwrap_or_else(|| json!([0, 0, 0]));
            let sum = old[0].as_f64().unwrap_or(0.0) + sum;
            let count = old[1].as_i64().unwrap_or(0) + n;
            if !sum.is_finite() || count > SAFE {
                return Err(invalid("numeric history overflow"));
            }
            out.insert(k.into(), json!([js::jnum(sum), count, js::jnum(old[2].as_f64().unwrap_or(0.0).max(max))]));
        }
    }
    Ok(Value::Object(out))
}

/// Authoritative, unique (node_id, minute) input rows, in source order.
pub fn aggregate_minutes(rows: &[Minute]) -> Result<Value> {
    let mut groups: BTreeMap<(i64, String), Value> = BTreeMap::new();
    let mut omitted = 0;
    for r in rows {
        if !node_ok(&r.node_id) || !clock(r.minute) || !r.n.is_some_and(|n| (1..=SAFE).contains(&n)) {
            omitted += 1;
            continue;
        }
        let mut stats = Map::new();
        for k in KEYS {
            let Some(a) = r.v[k].as_array().filter(|a| a.len() == 2) else { continue };
            let (Some(mean), Some(max)) = (nonnegative(&a[0]), nonnegative(&a[1])) else { continue };
            if mean <= max && max <= 1e12 {
                stats.insert(k.into(), json!([js::jnum(mean), 1, js::jnum(max)]));
            }
        }
        if stats.is_empty() {
            omitted += 1;
            continue;
        }
        let bucket = floor(r.minute, HOUR);
        let key = (bucket, r.node_id.clone());
        let old = groups.remove(&key).unwrap_or_else(|| json!({}));
        groups.insert(key, merge_stats(&[old, Value::Object(stats)])?);
    }
    let rows: Vec<Value> = groups.into_iter().map(|((bucket, node_id), v)| json!({"node_id":node_id,"bucket":bucket,"v":v})).collect();
    Ok(json!({"rows":rows,"omittedRows":omitted}))
}

pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(SCHEMA)
}

// Use the existing JS-compatible formatter so byte budgets have the same units
// in both writers (serde_json's 1.0 / exponent spellings can differ from JS).
fn stats_json(v: &Value) -> Result<String> {
    let checked = merge_stats(std::slice::from_ref(v))?;
    let fields: Vec<String> = KEYS
        .into_iter()
        .filter_map(|k| {
            let a = checked[k].as_array()?;
            Some(format!("\"{k}\":[{},{},{}]", js::num_str(a[0].as_f64()?), a[1].as_i64()?, js::num_str(a[2].as_f64()?)))
        })
        .collect();
    Ok(format!("{{{}}}", fields.join(",")))
}
fn add(db: &Connection, node: &str, resolution: &str, bucket: i64, v: &Value) -> Result<()> {
    let old: Option<String> = db
        .prepare_cached("SELECT v FROM metrics_history WHERE node_id=? AND resolution=? AND bucket=?")?
        .query_row(params![node, resolution, bucket], |r| r.get(0))
        .optional()?;
    let old = old.map(|s| parsed(&s)).transpose()?.unwrap_or_else(|| json!({}));
    let v = merge_stats(&[old, v.clone()])?;
    db.prepare_cached("INSERT OR REPLACE INTO metrics_history(node_id,resolution,bucket,v) VALUES (?,?,?,?)")?.execute(params![
        node,
        resolution,
        bucket,
        stats_json(&v)?
    ])?;
    Ok(())
}
fn flush(db: &Connection, batch: &mut Vec<Minute>) -> Result<i64> {
    let rolled = aggregate_minutes(batch)?;
    for r in rolled["rows"].as_array().ok_or_else(|| invalid("invalid rollup"))? {
        add(
            db,
            r["node_id"].as_str().ok_or_else(|| invalid("invalid rollup"))?,
            "hour",
            r["bucket"].as_i64().ok_or_else(|| invalid("invalid rollup"))?,
            &r["v"],
        )?;
    }
    batch.clear();
    Ok(rolled["omittedRows"].as_i64().unwrap_or(0))
}

pub fn status(db: &Connection) -> Result<Value> {
    let mut state = db.query_row("SELECT sealed_before,last_now,late_rows,omitted_rows,expired_rows,capacity_rows,max_rows,max_bytes FROM metrics_retention_state WHERE singleton=1", [], |r| {
        Ok(json!({"sealed_before":r.get::<_, i64>(0)?,"last_now":r.get::<_, i64>(1)?,"late_rows":r.get::<_, i64>(2)?,"omitted_rows":r.get::<_, i64>(3)?,"expired_rows":r.get::<_, i64>(4)?,"capacity_rows":r.get::<_, i64>(5)?,"max_rows":r.get::<_, i64>(6)?,"max_bytes":r.get::<_, i64>(7)?}))
    })?;
    let (rows, bytes, first, last): (i64, i64, Option<i64>, Option<i64>) =
        db.query_row(&format!("SELECT count(*),coalesce(sum({SIZE}),0),min(bucket),max(bucket) FROM metrics_history"), [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    for (k, v) in [
        ("rows", json!(rows)),
        ("logical_bytes", json!(bytes)),
        ("oldest_bucket", json!(first)),
        ("latest_bucket", json!(last)),
        ("hour_days", json!(90)),
        ("day_days", json!(400)),
    ] {
        state[k] = v;
    }
    Ok(state)
}

pub fn query(db: &Connection, node: &str, since: i64, until: i64, resolution: &str) -> Result<Value> {
    if !node_ok(node) || !clock(since) || !clock(until) || since > until {
        return Err(invalid("invalid history range"));
    }
    if !["hour", "day"].contains(&resolution) {
        return Err(invalid("invalid history resolution"));
    }
    let mut stmt =
        db.prepare_cached("SELECT bucket,v FROM metrics_history WHERE node_id=? AND resolution=? AND bucket>=? AND bucket<? ORDER BY bucket LIMIT 50000")?;
    let mut out = Vec::new();
    let mut rows = stmt.query(params![node, resolution, since, until])?;
    while let Some(r) = rows.next()? {
        let bucket: i64 = r.get(0)?;
        let v = merge_stats(&[parsed(&r.get::<_, String>(1)?)?])?;
        let mut metrics = Map::new();
        for k in KEYS {
            if let Some(a) = v[k].as_array() {
                metrics.insert(k.into(), json!({"mean":js::jnum(a[0].as_f64().unwrap_or(0.0)/a[1].as_f64().unwrap_or(1.0)),"max":a[2],"minutes":a[1]}));
            }
        }
        out.push(json!({"node_id":node,"resolution":resolution,"bucket":bucket,"v":metrics}));
    }
    Ok(Value::Array(out))
}

/// Owns the complete archive-and-delete transaction; never nest in Store::tx.
pub fn compact(db: &Connection, before: i64, now: i64, budget: Budget) -> Result<Value> {
    if !clock(now) || !clock(before) || before > (now - 14 * DAY).max(0) {
        return Err(invalid("retention must preserve the most recent 14 days"));
    }
    if !(1..=MAX_ROWS).contains(&budget.max_rows) || !(1..=MAX_BYTES).contains(&budget.max_bytes) {
        return Err(invalid("invalid history capacity budget"));
    }
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let (previous, previous_now): (i64, i64) =
            db.query_row("SELECT sealed_before,last_now FROM metrics_retention_state WHERE singleton=1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let sealed = before.max(previous);
        let anchor = now.max(previous_now);
        let late: i64 = db.query_row("SELECT count(*) FROM metrics_minute WHERE minute<?", [previous.min(before)], |r| r.get(0))?;
        let mut omitted = 0;
        {
            let mut source = db.prepare_cached("SELECT CASE WHEN typeof(node_id)='text' THEN node_id ELSE '' END,CASE WHEN typeof(minute)='integer' THEN minute ELSE NULL END,CASE WHEN typeof(n)='integer' AND n BETWEEN 1 AND 9007199254740991 THEN n ELSE NULL END,CASE WHEN typeof(v)='text' AND length(CAST(v AS BLOB))<=65536 THEN v ELSE NULL END FROM metrics_minute WHERE minute>=? AND minute<? ORDER BY minute,node_id")?;
            let mut rows = source.query(params![previous, sealed])?;
            let mut batch = Vec::new();
            while let Some(r) = rows.next()? {
                let text: Option<String> = r.get(3)?;
                batch.push(Minute {
                    node_id: r.get(0)?,
                    minute: r.get::<_, Option<i64>>(1)?.unwrap_or(-1),
                    n: r.get(2)?,
                    v: text.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null),
                });
                if batch.len() == 5000 {
                    omitted += flush(db, &mut batch)?;
                }
            }
            omitted += flush(db, &mut batch)?;
        }
        let hour_cut = floor(anchor - 90 * DAY, HOUR);
        {
            let mut source = db.prepare_cached("SELECT node_id,bucket,v FROM metrics_history WHERE resolution='hour' AND bucket<? ORDER BY bucket,node_id")?;
            let mut rows = source.query([hour_cut])?;
            while let Some(r) = rows.next()? {
                let node: String = r.get(0)?;
                let bucket: i64 = r.get(1)?;
                add(db, &node, "day", floor(bucket, DAY), &parsed(&r.get::<_, String>(2)?)?)?;
            }
        }
        db.execute("DELETE FROM metrics_history WHERE resolution='hour' AND bucket<?", [hour_cut])?;
        let expired = db.execute("DELETE FROM metrics_history WHERE resolution='day' AND bucket<?", [floor(anchor - 400 * DAY, DAY)])? as i64;
        let deleted = db.execute("DELETE FROM metrics_minute WHERE minute<?", [before])? as i64;
        let (mut count, mut bytes): (i64, i64) =
            db.query_row(&format!("SELECT count(*),coalesce(sum({SIZE}),0) FROM metrics_history"), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut capacity = 0;
        while count > budget.max_rows || bytes > budget.max_bytes {
            let oldest = {
                let mut stmt =
                    db.prepare_cached(&format!("SELECT node_id,resolution,bucket,{SIZE} FROM metrics_history ORDER BY bucket,resolution,node_id LIMIT 512"))?;
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))?
                    .collect::<Result<Vec<_>>>()?
            };
            for (node, resolution, bucket, size) in oldest {
                if count <= budget.max_rows && bytes <= budget.max_bytes {
                    break;
                }
                db.prepare_cached("DELETE FROM metrics_history WHERE node_id=? AND resolution=? AND bucket=?")?.execute(params![node, resolution, bucket])?;
                count -= 1;
                bytes -= size;
                capacity += 1;
            }
        }
        db.execute("UPDATE metrics_retention_state SET sealed_before=?,last_now=?,late_rows=min(9007199254740991,late_rows+?),omitted_rows=min(9007199254740991,omitted_rows+?),expired_rows=min(9007199254740991,expired_rows+?),capacity_rows=min(9007199254740991,capacity_rows+?),max_rows=?,max_bytes=? WHERE singleton=1", params![sealed, anchor, late, omitted, expired, capacity, budget.max_rows, budget.max_bytes])?;
        let mut result = status(db)?;
        result["deleted"] = json!(deleted);
        db.execute_batch("COMMIT")?;
        Ok(result)
    })();
    if result.is_err() {
        let _ = db.execute_batch("ROLLBACK");
    }
    result
}
