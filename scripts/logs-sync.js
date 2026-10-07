#!/usr/bin/env node
// 端末からログを取り込む: node scripts/logs-sync.js [--db <dir>] [node_id ...]
'use strict';
const path = require('node:path');
const os = require('node:os');
const { loadConfig } = require('../lib/nodes');
const { openDb } = require('../lib/db');
const { syncAll } = require('../lib/logs');

const argv = process.argv.slice(2);
const i = argv.indexOf('--db');
const dir = i >= 0 ? argv.splice(i, 2)[1] : path.join(os.tmpdir(), 'katala-tune-cli');
const { nodes } = loadConfig();
const targets = argv.length ? nodes.filter((n) => argv.includes(n.id)) : nodes;
const db = openDb(dir);
const t0 = Date.now();
syncAll(db, targets, (r) => {
  const s = Object.entries(r.sources || {}).map(([k, v]) => `${k}:${v.error ? 'NG ' + v.error.slice(0, 60) : `+${v.inserted}/${v.fetched}${v.dropped ? ` drop${v.dropped}` : ''}`}`).join(' ');
  process.stderr.write(`${r.node_id}: ${r.error ? 'NG ' + r.error.split('\n').pop() : s}\n`);
}).then(() => process.stderr.write(`done ${((Date.now() - t0) / 1000).toFixed(1)}s db=${dir}\n`));
