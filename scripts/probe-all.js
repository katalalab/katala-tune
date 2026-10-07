#!/usr/bin/env node
// 端末から全機体を調べる: node scripts/probe-all.js [node_id ...] > out.json
'use strict';
const { loadConfig } = require('../lib/nodes');
const { probeAll } = require('../lib/collect');

const want = process.argv.slice(2);
const { nodes } = loadConfig();
const targets = want.length ? nodes.filter((n) => want.includes(n.id)) : nodes;
probeAll(targets, (r) => process.stderr.write(`${r.node_id}: ${r.ok ? 'ok' : 'NG ' + r.error.split('\n').pop()} ${r.wall_s?.toFixed(1)}s\n`))
  .then((rs) => process.stdout.write(JSON.stringify(rs) + '\n'));
