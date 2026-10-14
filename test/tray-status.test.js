'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

const source = fs.readFileSync(require.resolve('../main.js'), 'utf8');
const start = source.indexOf('function statusCounts()');
const end = source.indexOf('\n// ---- 分析・ログ取り込み・自動スキャン ----', start);
assert.notEqual(start, -1, 'main.js にトレイ用 statusCounts が必要');
assert.notEqual(end, -1, 'statusCounts の終端が必要');

function trayCounts(rows, nodes) {
  const context = { db: { checks: () => rows }, cfg: { nodes }, health: require('../lib/health'), netsec: require('../lib/netsec') };
  vm.runInNewContext(`${source.slice(start, end)}\nglobalThis.statusCounts = statusCounts;`, context);
  return context.statusCounts();
}

test('トレイの件数は現行台帳だけを数え、保存済みの旧行とprivacy対象を変えない', () => {
  const rows = [
    { scope: 'removed', id: 'probe', status: 'fail' },
    { scope: 'current', id: 'probe', status: 'warn' },
    { scope: '_app', id: 'db', status: 'ok' },
    { scope: 'current', id: 'sec-login', status: 'fail' },
  ];
  const before = structuredClone(rows);
  assert.deepEqual(trayCounts(rows, [{ id: 'current', network_logins: false }]), { ok: 1, warn: 1, fail: 0, unknown: 0 });
  assert.deepEqual(rows, before);
});
