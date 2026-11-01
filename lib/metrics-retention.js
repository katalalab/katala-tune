// Numeric history only. This module never starts a collector or touches host settings.
'use strict';
const HOUR = 3600000;
const DAY = 24 * HOUR;
const KEYS = ['cpu', 'mem', 'swap', 'commit', 'dr', 'dw', 'rx', 'tx', 'power_cpu_w', 'power_soc_w', 'power_platform_w', 'power_gpu_w', 'gpu', 'gmem', 'gtemp'];
const clock = (x) => Number.isSafeInteger(x) && x >= 0;
const nodeOk = (x) => typeof x === 'string' && x.length > 0 && x.isWellFormed() && Buffer.byteLength(x) <= 256;
const finite = (x) => typeof x === 'number' && Number.isFinite(x) && x >= 0;
const order = (a, b) => a.bucket - b.bucket || Buffer.compare(Buffer.from(a.node_id), Buffer.from(b.node_id));

// Counts are valid MINUTES for each key, never total samples or total hours.
function mergeStats(values) {
  const out = {};
  for (const v of values) {
    for (const k of KEYS) {
      if (v[k] == null) continue;
      const x = v[k];
      if (!Array.isArray(x) || x.length !== 3 || !finite(x[0]) || !Number.isSafeInteger(x[1]) || x[1] <= 0 || !finite(x[2]) || x[2] > 1e12 || x[0] / x[1] > x[2] + Math.max(1, x[2]) * 1e-9) throw new Error('invalid numeric history stats');
      const old = out[k] || [0, 0, 0];
      const sum = old[0] + x[0];
      const count = old[1] + x[1];
      if (!Number.isFinite(sum) || !Number.isSafeInteger(count)) throw new Error('numeric history overflow');
      out[k] = [sum, count, Math.max(old[2], x[2])];
    }
  }
  return out;
}

// Input is an authoritative set of unique (node_id, minute) rows from SQLite.
function aggregateMinutes(rows) {
  const groups = new Map();
  let omittedRows = 0;
  for (const r of rows) {
    if (!nodeOk(r.node_id) || !clock(r.minute) || !Number.isSafeInteger(r.n) || r.n <= 0) { omittedRows++; continue; }
    const v = {};
    for (const k of KEYS) {
      const x = r.v?.[k];
      if (Array.isArray(x) && x.length === 2 && finite(x[0]) && finite(x[1]) && x[0] <= x[1] && x[1] <= 1e12) v[k] = [x[0], 1, x[1]];
    }
    if (!Object.keys(v).length) { omittedRows++; continue; }
    const bucket = Math.floor(r.minute / HOUR) * HOUR;
    const key = JSON.stringify([r.node_id, bucket]);
    const old = groups.get(key);
    groups.set(key, { node_id: r.node_id, bucket, v: mergeStats([old?.v || {}, v]) });
  }
  return { rows: [...groups.values()].sort(order), omittedRows };
}


const fs = require('node:fs');
const path = require('node:path');
const MAX_ROWS = 50000;
const MAX_BYTES = 16 * 1024 * 1024;
const SIZE = 'length(CAST(node_id AS BLOB)) + length(CAST(v AS BLOB)) + 80';
const floor = (t, width) => Math.floor(Math.max(0, t) / width) * width;
function initialize(db) {
  db.exec(fs.readFileSync(path.join(__dirname, '../config/metrics-retention.sql'), 'utf8'));
}
function status(db) {
  const state = { ...db.prepare('SELECT * FROM metrics_retention_state WHERE singleton=1').get() };
  delete state.singleton;
  const totals = db.prepare(`SELECT count(*) AS rows, coalesce(sum(${SIZE}),0) AS logical_bytes, min(bucket) AS oldest_bucket, max(bucket) AS latest_bucket FROM metrics_history`).get();
  return { ...state, ...totals, hour_days: 90, day_days: 400 };
}
function query(db, nodeId, since, until, resolution) {
  if (!nodeOk(nodeId) || !clock(since) || !clock(until) || since > until) throw new Error('invalid history range');
  if (!['hour', 'day'].includes(resolution)) throw new Error('invalid history resolution');
  return db.prepare(`SELECT node_id,resolution,bucket,v FROM metrics_history WHERE node_id=? AND resolution=? AND bucket>=? AND bucket<? ORDER BY bucket LIMIT ${MAX_ROWS}`).all(nodeId, resolution, since, until).map(r => {
    const stats = mergeStats([JSON.parse(r.v)]);
    return { ...r, v: Object.fromEntries(Object.entries(stats).map(([k, [sum, n, max]]) => [k, { mean: sum / n, max, minutes: n }])) };
  });
}
function compact(db, before, now, budget = {}) {
  if (!clock(now) || !clock(before) || before > Math.max(0, now - 14 * DAY)) throw new Error('retention must preserve the most recent 14 days');
  const maxRows = budget.maxRows ?? MAX_ROWS;
  const maxBytes = budget.maxBytes ?? MAX_BYTES;
  if (!Number.isSafeInteger(maxRows) || maxRows < 1 || maxRows > MAX_ROWS || !Number.isSafeInteger(maxBytes) || maxBytes < 1 || maxBytes > MAX_BYTES) throw new Error('invalid history capacity budget');
  db.exec('BEGIN IMMEDIATE');
  try {
    const state = db.prepare('SELECT * FROM metrics_retention_state WHERE singleton=1').get();
    const anchor = Math.max(now, state.last_now);
    const sealed = Math.max(before, state.sealed_before);
    const get = db.prepare('SELECT v FROM metrics_history WHERE node_id=? AND resolution=? AND bucket=?');
    const up = db.prepare('INSERT OR REPLACE INTO metrics_history(node_id,resolution,bucket,v) VALUES (?,?,?,?)');
    const add = (r, resolution) => {
      const old = get.get(r.node_id, resolution, r.bucket);
      const v = mergeStats([old ? JSON.parse(old.v) : {}, r.v]);
      up.run(r.node_id, resolution, r.bucket, JSON.stringify(v));
    };
    let omitted = 0;
    let batch = [];
    const flush = () => {
      const rolled = aggregateMinutes(batch);
      omitted += rolled.omittedRows;
      for (const r of rolled.rows) add(r, 'hour');
      batch = [];
    };
    const late = db.prepare('SELECT count(*) AS n FROM metrics_minute WHERE minute<?').get(Math.min(before, state.sealed_before)).n;
    const source = db.prepare(`
      SELECT node_id,minute,
        CASE WHEN typeof(n)='integer' AND n BETWEEN 1 AND 9007199254740991 THEN n ELSE NULL END AS n,
        CASE WHEN length(CAST(v AS BLOB))<=65536 THEN v ELSE NULL END AS v
      FROM metrics_minute WHERE minute>=? AND minute<? ORDER BY minute,node_id
    `);
    for (const r of source.iterate(state.sealed_before, sealed)) {
      try { r.v = JSON.parse(r.v); } catch { r.v = null; }
      batch.push(r);
      if (batch.length === 5000) flush();
    }
    flush();
    const hourCut = floor(anchor - 90 * DAY, HOUR);
    for (const r of db.prepare("SELECT node_id,bucket,v FROM metrics_history WHERE resolution='hour' AND bucket<? ORDER BY bucket,node_id").iterate(hourCut)) {
      add({ node_id: r.node_id, bucket: floor(r.bucket, DAY), v: JSON.parse(r.v) }, 'day');
    }
    db.prepare("DELETE FROM metrics_history WHERE resolution='hour' AND bucket<?").run(hourCut);
    const expired = db.prepare("DELETE FROM metrics_history WHERE resolution='day' AND bucket<?").run(floor(anchor - 400 * DAY, DAY)).changes;
    const deleted = db.prepare('DELETE FROM metrics_minute WHERE minute<?').run(before).changes;
    const totals = db.prepare(`SELECT count(*) AS n, coalesce(sum(${SIZE}),0) AS bytes FROM metrics_history`).get();
    let capacity = 0;
    const remove = db.prepare('DELETE FROM metrics_history WHERE node_id=? AND resolution=? AND bucket=?');
    while (totals.n > maxRows || totals.bytes > maxBytes) {
      const oldest = db.prepare(`SELECT node_id,resolution,bucket,${SIZE} AS bytes FROM metrics_history ORDER BY bucket,resolution,node_id LIMIT 512`).all();
      for (const r of oldest) {
        if (totals.n <= maxRows && totals.bytes <= maxBytes) break;
        remove.run(r.node_id, r.resolution, r.bucket);
        totals.n--; totals.bytes -= r.bytes; capacity++;
      }
    }
    db.prepare(`UPDATE metrics_retention_state SET sealed_before=?, last_now=?, late_rows=min(9007199254740991,late_rows+?), omitted_rows=min(9007199254740991,omitted_rows+?), expired_rows=min(9007199254740991,expired_rows+?), capacity_rows=min(9007199254740991,capacity_rows+?), max_rows=?, max_bytes=? WHERE singleton=1`).run(sealed, anchor, late, omitted, expired, capacity, maxRows, maxBytes);
    const result = { deleted, ...status(db) };
    db.exec('COMMIT');
    return result;
  } catch (e) {
    try { db.exec('ROLLBACK'); } catch { /* Preserve the original archive/commit error. */ }
    throw e;
  }
}
module.exports = { aggregateMinutes, mergeStats, initialize, compact, query, status };
