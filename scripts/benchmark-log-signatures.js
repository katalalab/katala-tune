#!/usr/bin/env node
'use strict';
// Synthetic data only. --assert-improvement is an opt-in performance experiment, not a CI timing test.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { performance } = require('node:perf_hooks');
const { openDb } = require('../lib/db');

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-signatures-bench-'));
const store = openDb(dir);
try {
  const varied = process.argv.includes('--varied');
  const signatureOf = (i) => varied ? i % 100 : i < 47500 ? 0 : 1 + i % 99;
  const rows = Array.from({ length: 50000 }, (_, i) => ({
    uid: String(i), ts: 1800000000000 + i, level: 'warn', provider: 'example', event_id: '1',
    // Flood-heavy workload: most rows repeat one diagnostic, with a smaller varied tail.
    message: `example diagnostic ${signatureOf(i)}`, fingerprint: `fp-${String(signatureOf(i)).padStart(3, '0')}`,
    occurrences: 1 + i % 3,
  }));
  store.insertLogs('node-a', 'example', rows);
  store.insertLogs('node-b', 'example', rows.slice(0, 1000));
  const reference = store.db.prepare(`SELECT l.fingerprint, s.level, s.provider, s.source, s.sample,
    sum(l.occurrences) AS n, count(DISTINCT l.node_id) AS nodes, group_concat(DISTINCT l.node_id) AS node_ids,
    max(l.ts) AS last_ts, s.total FROM logs l JOIN log_signatures s USING (fingerprint)
    WHERE l.ts >= ? GROUP BY l.fingerprint ORDER BY n DESC, l.fingerprint LIMIT ?`);
  const args = { since: 1800000000000, limit: 50 };
  const canonical = (items) => items.map(r => ({ ...r, node_ids: r.node_ids.split(',').sort().join(',') })).sort((a, b) => a.fingerprint.localeCompare(b.fingerprint));
  assert.deepEqual(canonical(store.signatures(args)), canonical(reference.all(args.since, args.limit)));
  const timings = { reference: [], current: [] };
  const run = (name) => {
    const start = performance.now();
    if (name === 'reference') reference.all(args.since, args.limit); else store.signatures(args);
    timings[name].push(performance.now() - start);
  };
  for (let i = 0; i < 15; i++) {
    for (const name of i % 2 ? ['current', 'reference'] : ['reference', 'current']) run(name);
  }
  const median = (values) => values.slice(2).sort((a, b) => a - b)[6];
  const oldMs = median(timings.reference), newMs = median(timings.current);
  console.log(JSON.stringify({ workload: varied ? 'varied' : 'flood', rows: 51000, signatures: 100, runs: 13, reference_ms: oldMs, current_ms: newMs, reduction_pct: 100 * (1 - newMs / oldMs), parity: true }));
  if (process.argv.includes('--assert-improvement')) assert.ok(newMs < oldMs * 0.9, 'signature query must be at least 10% faster in this experiment');
} finally {
  store.db.close();
  fs.rmSync(dir, { recursive: true, force: true });
}
