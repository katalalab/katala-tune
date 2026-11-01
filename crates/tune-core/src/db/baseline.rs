//! Saved numeric reader only: opening a normal Store can migrate; this method never does.
use super::{Result, Store};
use crate::{
    hourly_baseline::{HOUR, MAX_CLOCK, analyze_hours, integer},
    js::jnum,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
const DAY: i64 = 24 * HOUR;
fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.into())
}
fn clock(t: i64) -> bool {
    (0..=MAX_CLOCK).contains(&t)
}
fn finite(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}
fn withheld(mut report: Value, source: &Value, reason: &str) -> Value {
    report["reasons"] = json!([reason]);
    for (k, v) in report["coverage"].as_object_mut().unwrap() {
        if k != "input_points" {
            *v = Value::Null;
        }
    }
    report["source"] = source.clone();
    report
}
fn snapshot(db: &Connection, node: &str, metric: &str, now: i64, empty: Value) -> Result<Value> {
    let end = now - now % HOUR;
    let since = (end - 28 * DAY).max(0);
    let mut source = json!({"state":"insufficient","sealed_before":null,"last_now":null,"history_rows":null,"raw_rows":null,"omitted_raw_rows":null});
    let tables: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('metrics_minute','metrics_history','metrics_retention_state')",
        [],
        |r| r.get(0),
    )?;
    if tables != 3 {
        return Ok(withheld(empty, &source, "storage_unavailable"));
    }
    let state = db
        .query_row(
            "SELECT
        CASE WHEN typeof(sealed_before)='integer' AND sealed_before BETWEEN 0 AND 8640000000000000 THEN sealed_before END,
        CASE WHEN typeof(last_now)='integer' AND last_now BETWEEN 0 AND 8640000000000000 THEN last_now END
        FROM metrics_retention_state WHERE singleton=1",
            [],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .optional()?;
    let Some((seal, last)) = state else {
        return Ok(withheld(empty, &source, "storage_unavailable"));
    };
    source["sealed_before"] = json!(seal);
    source["last_now"] = json!(last);
    let (Some(seal), Some(last)) = (seal, last) else {
        return Ok(withheld(empty, &source, "source_invalid"));
    };
    if !clock(seal) || !clock(last) || seal > last {
        return Ok(withheld(empty, &source, "source_invalid"));
    }
    if last > now {
        return Ok(withheld(empty, &source, "clock_rollback"));
    }
    let mut st = db.prepare(
        "SELECT bucket,
        CASE WHEN typeof(v)='text' AND length(CAST(v AS BLOB))<=2048 THEN CASE WHEN json_valid(v) THEN v -> ? END END,
        CASE WHEN typeof(v)='text' AND length(CAST(v AS BLOB))<=2048 THEN CASE WHEN json_valid(v) THEN json_type(v)='object' ELSE 0 END ELSE 0 END
        FROM metrics_history WHERE node_id=? AND resolution='hour' AND bucket>=? AND bucket<? ORDER BY bucket LIMIT 673",
    )?;
    let hours = st
        .query_map(params![format!("$.{metric}"), node, since, end], |r| {
            Ok((r.get_ref(0)?.as_i64().ok(), r.get::<_, Option<String>>(1)?, r.get::<_, bool>(2)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    source["history_rows"] = json!(hours.len());
    if hours.len() > 672 {
        return Ok(withheld(empty, &source, "source_limit"));
    }
    let mut buckets: BTreeMap<i64, (f64, i64)> = BTreeMap::new();
    for (at, text, valid) in hours {
        let Some(at) = at else {
            return Ok(withheld(empty, &source, "source_invalid"));
        };
        if !clock(at) || at % HOUR != 0 || at >= seal {
            return Ok(withheld(empty, &source, "source_invalid"));
        }
        if !valid {
            return Ok(withheld(empty, &source, "source_invalid"));
        }
        let v = match text {
            None => Value::Null,
            Some(text) => match serde_json::from_str::<Value>(&text) {
                Ok(v) => v,
                Err(_) => return Ok(withheld(empty, &source, "source_invalid")),
            },
        };
        if v.is_null() {
            continue;
        }
        let Some(a) = v.as_array().filter(|a| a.len() == 3) else {
            return Ok(withheld(empty, &source, "source_invalid"));
        };
        let (sum, n, max) = (finite(&a[0]), integer(&a[1]), finite(&a[2]));
        let (Some(sum), Some(n), Some(max)) = (sum, n, max) else {
            return Ok(withheld(empty, &source, "source_invalid"));
        };
        if sum < 0.0
            || !(1..=60).contains(&n)
            || !(0.0..=100.0).contains(&max)
            || sum / n as f64 > max + max.max(1.0) * 1e-9
            || n > (HOUR.min(seal - at) + 59_999) / 60_000
        {
            return Ok(withheld(empty, &source, "source_invalid"));
        }
        buckets.insert(at, (sum, n));
    }
    let mut raw_rows = 0;
    let mut omitted = 0;
    source["raw_rows"] = json!(0);
    source["omitted_raw_rows"] = json!(0);
    let mut st = db.prepare(
        "SELECT minute,
        CASE WHEN typeof(n)='integer' AND n BETWEEN 1 AND 9007199254740991 THEN n END,
        CASE WHEN typeof(v)='text' AND length(CAST(v AS BLOB))<=65536 THEN CASE WHEN json_valid(v) THEN v -> ? END END
        FROM metrics_minute WHERE node_id=? AND minute>=? AND minute<? ORDER BY minute LIMIT 40321",
    )?;
    let mut rows = st.query(params![format!("$.{metric}"), node, since.max(seal), end])?;
    while let Some(r) = rows.next()? {
        raw_rows += 1;
        source["raw_rows"] = json!(raw_rows);
        if raw_rows > 40320 {
            return Ok(withheld(empty, &source, "source_limit"));
        }
        // SQLite REAL timestamps may be corrupt data, not an operational type error.
        let at = r.get_ref(0)?.as_i64().ok();
        let n: Option<i64> = r.get(1)?;
        let text: Option<String> = r.get(2)?;
        let v = text.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok());
        let pair = v.as_ref().and_then(Value::as_array).filter(|a| a.len() == 2).and_then(|a| Some((finite(&a[0])?, finite(&a[1])?)));
        let usable = match (at, n, pair) {
            (Some(at), Some(n), Some((mean, max))) if clock(at) && at % 60_000 == 0 && n > 0 && mean >= 0.0 && mean <= max && max <= 100.0 => Some((at, mean)),
            _ => None,
        };
        let Some((at, mean)) = usable else {
            omitted += 1;
            source["omitted_raw_rows"] = json!(omitted);
            continue;
        };
        let b = buckets.entry(at - at % HOUR).or_default();
        b.0 += mean;
        b.1 += 1;
        if b.1 > 60 {
            return Ok(withheld(empty, &source, "source_invalid"));
        }
    }
    let points: Vec<_> = buckets.into_iter().map(|(at, (sum, n))| json!({"at":at,"mean":jnum(sum/n as f64),"minutes":n,"resolution":"hour"})).collect();
    source["state"] = json!("ready");
    let mut result = analyze_hours(&json!(points), metric, now).map_err(invalid)?;
    result["source"] = source;
    Ok(result)
}
impl Store {
    /// Reads one snapshot of existing tables. Does not initialize, compact or repair them.
    pub fn metrics_baseline(&self, node: &str, metric: &str, now: i64) -> Result<Value> {
        let empty = analyze_hours(&json!([]), metric, now).map_err(invalid)?;
        if node.is_empty() || node.len() > 256 {
            return Err(invalid("invalid baseline node"));
        }
        self.conn.execute_batch("SAVEPOINT baseline_read")?;
        let result = snapshot(&self.conn, node, metric, now, empty).and_then(|r| {
            self.conn.execute_batch("RELEASE baseline_read")?;
            Ok(r)
        });
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK TO baseline_read; RELEASE baseline_read");
        }
        result
    }
}
