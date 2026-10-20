#!/usr/bin/env node
// 端末から全機体を調べる: node scripts/probe-all.js [node_id ...] > out.json
'use strict';
const { loadConfig } = require('../lib/nodes');
const { probeAll } = require('../lib/collect');
const { partition } = require('../lib/ledger');

const want = process.argv.slice(2);
const { nodes } = loadConfig();
const selected = want.length ? nodes.filter((n) => want.includes(n.id)) : nodes;
// 運用対象でない機体（retired など）には接続しない
const { targets, excluded } = partition(selected);
for (const x of excluded) process.stderr.write(`${x.node_id}: 対象外（${x.state}）${x.reason ? ` ${x.reason}` : ''}\n`);
probeAll(targets, (r) => process.stderr.write(`${r.node_id}: ${r.ok ? 'ok' : 'NG ' + r.error.split('\n').pop()} ${r.wall_s?.toFixed(1)}s\n`))
  .then((rs) => process.stdout.write(JSON.stringify(rs) + '\n'));
