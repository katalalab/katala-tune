'use strict';
const { DatabaseSync } = require('node:sqlite');
const fs = require('node:fs');
const retention = require('../../../../lib/metrics-retention');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
let output;
if (!Array.isArray(input)) {
  const store = require('../../../../lib/db').openDb(input.store_dir);
  if (Object.hasOwn(input, 'maintenance')) store.setMeta('metricsRetentionMaintenance', input.maintenance);
  output = store.metricsRetention();
  store.db.close();
} else output = input.map(ops => {
  const db = new DatabaseSync(':memory:');
  db.exec('CREATE TABLE metrics_minute(node_id TEXT, minute INTEGER, n INTEGER, v TEXT, src TEXT, PRIMARY KEY(node_id,minute)) WITHOUT ROWID; CREATE INDEX metrics_minute_by_time ON metrics_minute(minute)');
  retention.initialize(db);
  const results = [];
  for (const op of ops) {
    if (op.type === 'put') db.prepare('INSERT OR REPLACE INTO metrics_minute VALUES (?,?,?,?,NULL)').run(op.node, op.at, typeof op.n === 'string' ? BigInt(op.n) : op.n, op.v_blob ? Buffer.from(op.v_blob) : JSON.stringify(op.v));
    else if (op.type === 'compact') results.push(retention.compact(db, op.before, op.now, { maxRows: op.maxRows, maxBytes: op.maxBytes }));
    else if (op.type === 'query') results.push(retention.query(db, op.node, 0, 700 * 86400000, op.resolution));
    else throw new Error('unknown fixture operation');
  }
  db.close();
  return results;
});
process.stdout.write(JSON.stringify(output));
