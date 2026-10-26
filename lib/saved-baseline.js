// Numeric saved-data reader only. Opening a normal Store can migrate; this method never does.
'use strict';
const { analyzeHours } = require('./hourly-baseline');
const HOUR = 3600000, DAY = 24 * HOUR, MINUTE = 60000;
const MAX_CLOCK = 8640000000000000;
const number = x => typeof x === 'number' && Number.isFinite(x);
const clock = x => Number.isSafeInteger(x) && x >= 0 && x <= MAX_CLOCK;
function parse(text) { try { return JSON.parse(text); } catch { return null; } }
function readBaseline(db, node, metric, now) {
  const empty = analyzeHours([], { metric, now }); // Validate the programming contract before SQL.
  if (typeof node !== 'string' || !node.length || !node.isWellFormed() || Buffer.byteLength(node) > 256) throw new TypeError('invalid baseline node');
  const end = now - now % HOUR, since = Math.max(0, end - 28 * DAY);
  const source = { state: 'insufficient', sealed_before: null, last_now: null, history_rows: null, raw_rows: null, omitted_raw_rows: null };
  const withheld = reason => {
    empty.reasons = [reason];
    for (const k of Object.keys(empty.coverage)) if (k !== 'input_points') empty.coverage[k] = null;
    return { ...empty, source };
  };
  db.exec('SAVEPOINT baseline_read');
  try {
    const read = () => {
      // This SELECT pins the snapshot even if maintenance commits on another connection.
      const tables = db.prepare("SELECT count(*) AS n FROM sqlite_schema WHERE type='table' AND name IN ('metrics_minute','metrics_history','metrics_retention_state')").get().n;
      if (tables !== 3) return withheld('storage_unavailable');
      const state = db.prepare(`SELECT
        CASE WHEN typeof(sealed_before)='integer' AND sealed_before BETWEEN 0 AND ${MAX_CLOCK} THEN sealed_before END AS seal,
        CASE WHEN typeof(last_now)='integer' AND last_now BETWEEN 0 AND ${MAX_CLOCK} THEN last_now END AS last
        FROM metrics_retention_state WHERE singleton=1`).get();
      if (!state) return withheld('storage_unavailable');
      source.sealed_before = state.seal;
      source.last_now = state.last;
      if (!clock(state.seal) || !clock(state.last) || state.seal > state.last) return withheld('source_invalid');
      if (state.last > now) return withheld('clock_rollback');
      const hours = db.prepare(`SELECT bucket, CASE WHEN typeof(v)='text' AND length(CAST(v AS BLOB))<=2048 THEN v END AS v
        FROM metrics_history WHERE node_id=? AND resolution='hour' AND bucket>=? AND bucket<? ORDER BY bucket LIMIT 673`).all(node, since, end);
      source.history_rows = hours.length;
      if (hours.length > 672) return withheld('source_limit');
      const buckets = new Map();
      for (const r of hours) {
        if (!clock(r.bucket) || r.bucket % HOUR || r.bucket >= state.seal) return withheld('source_invalid');
        const v = parse(r.v);
        if (!v || typeof v !== 'object' || Array.isArray(v)) return withheld('source_invalid');
        const a = v[metric];
        if (a == null) continue;
        if (!Array.isArray(a) || a.length !== 3 || !number(a[0]) || a[0] < 0 || !Number.isInteger(a[1]) || a[1] < 1 || a[1] > 60 ||
            !number(a[2]) || a[2] < 0 || a[2] > 100 || a[0] / a[1] > a[2] + Math.max(1, a[2]) * 1e-9 ||
            a[1] > Math.ceil(Math.min(HOUR, state.seal - r.bucket) / MINUTE)) return withheld('source_invalid');
        buckets.set(r.bucket, { sum: a[0], minutes: a[1] });
      }
      source.raw_rows = 0;
      source.omitted_raw_rows = 0;
      const rows = db.prepare(`SELECT minute,
        CASE WHEN typeof(n)='integer' AND n BETWEEN 1 AND 9007199254740991 THEN n END AS n,
        CASE WHEN typeof(v)='text' AND length(CAST(v AS BLOB))<=65536 THEN v END AS v
        FROM metrics_minute WHERE node_id=? AND minute>=? AND minute<? ORDER BY minute LIMIT 40321`);
      for (const r of rows.iterate(node, Math.max(since, state.seal), end)) {
        source.raw_rows++;
        if (source.raw_rows > 40320) return withheld('source_limit');
        const a = parse(r.v)?.[metric];
        if (!clock(r.minute) || r.minute % MINUTE || !Number.isSafeInteger(r.n) || r.n < 1 || !Array.isArray(a) || a.length !== 2 ||
            !number(a[0]) || !number(a[1]) || a[0] < 0 || a[0] > a[1] || a[1] > 100) { source.omitted_raw_rows++; continue; }
        const at = r.minute - r.minute % HOUR;
        const b = buckets.get(at) || { sum: 0, minutes: 0 };
        b.sum += a[0]; b.minutes++;
        if (b.minutes > 60) return withheld('source_invalid');
        buckets.set(at, b);
      }
      const points = [...buckets].sort(([a], [b]) => a - b).map(([at, b]) => ({ at, mean: b.sum / b.minutes, minutes: b.minutes, resolution: 'hour' }));
      source.state = 'ready';
      return { ...analyzeHours(points, { metric, now }), source };
    };
    const result = read();
    db.exec('RELEASE baseline_read');
    return result;
  } catch (e) {
    try { db.exec('ROLLBACK TO baseline_read; RELEASE baseline_read'); } catch { /* Preserve the original read/release error. */ }
    throw e;
  }
}
module.exports = { readBaseline };
