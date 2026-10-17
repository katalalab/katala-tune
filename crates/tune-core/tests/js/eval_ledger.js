#!/usr/bin/env node
// 台帳の運用状態の JS 版（lib/ledger.js・lib/health.js）を仕様として、Rust 側のテスト（tests/parity_ledger.rs）から渡された入力を評価して返す。
// 使い方: node eval_ledger.js <cases.json>  → 標準出力に結果の JSON
'use strict';
const fs = require('node:fs');
const path = require('node:path');

const LIB = path.join(__dirname, '..', '..', '..', '..', 'lib');
const ledger = require(path.join(LIB, 'ledger'));
const health = require(path.join(LIB, 'health'));

const cases = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const plain = (v) => JSON.parse(JSON.stringify(v === undefined ? null : v));

const out = {};
out.opState = cases.nodes.map((n) => plain(ledger.opState(n)));
out.refusal = cases.nodes.map((n) => ledger.refusal(n));
out.partition = plain(ledger.partition(cases.nodes).excluded);
out.targets = ledger.partition(cases.nodes).targets.map((n) => n.id);
const obs = cases.obs || {};
out.targetLedger = plain(ledger.targetLedger(cases.nodes, { lastOkAt: (id) => obs[id]?.ok ?? null, lastError: (id) => obs[id]?.err ?? null }));
out.checks = cases.nodes.map((n) => plain(health.nodeChecks(n, null, [], { now: 1 })).filter((c) => c.id === 'ledger'));
process.stdout.write(JSON.stringify(out));
