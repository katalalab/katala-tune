#!/usr/bin/env node
// ネットワークとセキュリティの JS 版（lib/netsec.js と、それを呼ぶ lib/rules.js・lib/health.js・lib/logs.js のログイン）を仕様として、
// Rust 側のテスト（tests/parity_netsec.rs）から渡された入力を評価して返す。
// 使い方: node eval_netsec.js <cases.json>  → 標準出力に結果の JSON。例外は { __throw: メッセージ }
'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const LIB = path.join(__dirname, '..', '..', '..', '..', 'lib');
const netsec = require(path.join(LIB, 'netsec'));
const rules = require(path.join(LIB, 'rules'));
const health = require(path.join(LIB, 'health'));
const logs = require(path.join(LIB, 'logs'));
const { openDb } = require(path.join(LIB, 'db'));

const cases = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const safe = (fn) => { try { return fn(); } catch (e) { return { __throw: String((e && e.message) || e) }; } };
const plain = (v) => JSON.parse(JSON.stringify(v === undefined ? null : v));
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'kt-parity-ns-'));
const each = (k, fn) => (cases[k] || []).map((c) => safe(() => plain(fn(c))));

const out = {};
out.addr = each('addr', (a) => netsec.addrInfo(a));
out.isPublic = each('addr', (a) => netsec.isPublic(a));
out.enabled = each('enabled', (n) => netsec.enabled(n));
out.normalize = each('normalize', (c) => netsec.normalize(c.raw, c.probe));
out.annotate = each('annotate', (c) => netsec.annotate(c.ns, c.prev, c.at));
out.strip = each('strip', (c) => netsec.strip(c));
out.prepare = each('prepare', (c) => netsec.prepare(c.raw, c.probe, c.prev, c.at));
out.listenKey = each('listenKey', (l) => netsec.listenKey(l));
out.persistKinds = each('persistKinds', (c) => netsec.persistKinds(c));
out.diff = each('diff', (c) => netsec.diffPersist(c.prev, c.items, { kinds: c.kinds, failed: c.failed, baselined: c.baselined }));
out.peers = each('peers', (c) => netsec.classifyPeers(c.known, c.sample, { learning: c.learning }));
out.findings = each('findings', (c) => netsec.findings(c.ns, c.node));
out.checks = each('checks', (c) => netsec.checks(c.ns, c.findings, c.ctx, c.node));
out.analyze = each('analyze', (c) => rules.analyze(c.snap, c.node));
out.nodeChecks = each('nodeChecks', (c) => health.nodeChecks(c.node, c.snap, c.findings, c.ctx));
out.logFindings = each('logFindings', (c) => {
  const db = openDb(tmp());
  for (const l of c.logs) db.insertLogs(c.node_id, l.source, logs.normalize(l.source, l.rows), c.now);
  return logs.logFindings(db, c.node_id, c.now);
});
// 途中で切れたサロゲート対は U+FFFD にする（Rust の String と比べるため）
process.stdout.write(JSON.stringify(out, (_k, v) => (typeof v === 'string' ? v.toWellFormed() : v)));
