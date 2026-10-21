// 台帳の運用状態（status）: 到達不能（online）と退役（retired など）を分け、運用対象外には接続も変更もしない。
// 偽の実行口と架空の台帳（test/fixtures/ledger）だけを使う。実機の ssh は呼ばない。
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ledger = require('../lib/ledger');
const { loadConfig } = require('../lib/nodes');
const { probeAll, probeNode, execOn } = require('../lib/collect');
const { inventoryNode } = require('../lib/inventory');
const { syncAll, syncNode } = require('../lib/logs');
const { nodeChecks } = require('../lib/health');
const { plan, execute } = require('../lib/actions');

const FIXTURE = path.join(__dirname, 'fixtures', 'ledger', 'nodes.json');
const EXAMPLE = path.join(__dirname, '..', 'config', 'nodes.example.json');
const cfg = () => loadConfig(FIXTURE);
const byId = (id) => cfg().nodes.find((n) => n.id === id);
const EXCLUDED = ['old-tower', 'locked-down', 'on-hold', 'someone-else', 'typo-node', 'retired-local'];

function mockSsh(script = {}) {
  const calls = [];
  const run = async (cmd, args) => {
    calls.push({ cmd, args });
    const alias = args.find((a) => typeof a === 'string' && Object.hasOwn(script, a));
    return script[alias] || { code: 0, out: '{"probe":"windows","cpu_busy":1}\n', err: '' };
  };
  const probeExchange = async (node) => run('ssh', ['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', node.alias]);
  return { run, probeExchange, calls };
}
const noLocal = new Proxy({}, { get: (_t, k) => () => { throw new Error(`ローカル実行を呼んだ: ${String(k)}`); } });

test('status が無い既存の台帳は壊れず、全機体が online として読める（後方互換）', () => {
  // status を持たない従来の台帳（見本から status 関連の項目を除いたもの）
  const raw = JSON.parse(fs.readFileSync(EXAMPLE, 'utf8'));
  for (const n of raw.nodes) { delete n.status; delete n.status_reason; delete n.status_date; }
  const example = { nodes: raw.nodes };
  assert.ok(example.nodes.length >= 3);
  assert.equal(loadConfig(FIXTURE).nodes.length, 8);
  for (const n of example.nodes) {
    const st = ledger.opState(n);
    assert.equal(st.state, 'online');
    assert.equal(st.active, true);
  }
  assert.equal(ledger.partition(example.nodes).excluded.length, 0);
  assert.equal(ledger.partition(example.nodes).targets.length, example.nodes.length);
});

test('status の読み方: 大文字小文字・アンダースコアを許し、読めない値は安全側（対象外）に倒す', () => {
  assert.equal(ledger.opState(byId('on-hold')).state, 'paused');
  assert.equal(ledger.opState(byId('someone-else')).state, 'out-of-scope');
  assert.equal(ledger.opState(byId('explicit-online')).state, 'online');
  const typo = ledger.opState(byId('typo-node'));
  assert.deepEqual([typo.state, typo.active, typo.valid], ['invalid', false, false]);
  assert.match(typo.reason, /retried/);
  assert.equal(ledger.opState({ status: null }).state, 'online');
  assert.equal(ledger.opState({ status: 3 }).active, false);
  const old = ledger.opState(byId('old-tower'));
  assert.equal(old.reason, '2026-09 に解体');
  assert.equal(old.date, '2026-09-30');
});

test('retired / no-ssh / paused / out-of-scope / 不明な status には ssh も probe も実行しない', async () => {
  const ssh = mockSsh();
  const seen = [];
  const nodes = cfg().nodes.map((node) => node.os === 'windows' && ledger.opState(node).active ? {
    ...node,
    action_helper: { schema: 'katala-tune/action-helper/v2', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 },
  } : node);
  const results = await probeAll(nodes, (r) => seen.push(r.node_id), { run: ssh.run, probeExchange: ssh.probeExchange, localShell: noLocal });
  assert.deepEqual(results.map((r) => r.node_id).sort(), ['active-mac', 'explicit-online']);
  assert.deepEqual(seen.sort(), ['active-mac', 'explicit-online']);
  assert.equal(ssh.calls.length, 2);
  for (const c of ssh.calls) assert.ok(c.args.some((a) => ['active-mac', 'explicit-online'].includes(a)), 'online 以外の alias に接続した');
  for (const id of EXCLUDED) assert.ok(!ssh.calls.some((c) => c.args.includes(id)), `${id} に接続した`);
});

test('probeNode を直接呼んでも、対象外の機体は ssh もローカル実行もせず「対象外」で返す（失敗の分類にしない）', async () => {
  for (const id of EXCLUDED) {
    const ssh = mockSsh();
    // この機体（local）を退役にした場合も、ローカル実行しない
    for (const node of [byId(id), { ...byId(id), local: true }]) {
      await probeNode(node, { run: ssh.run, localShell: noLocal });
      assert.equal((await execOn(node, 'echo x')).code, null);
    }
    const r = await probeNode(byId(id), { run: ssh.run, localShell: noLocal });
    assert.equal(ssh.calls.length, 0, id);
    assert.equal(r.ok, false);
    assert.equal(r.skipped, true);
    assert.equal(r.reason, 'excluded');
    assert.notEqual(r.reason, 'unreachable');
  }
});

test('ログ取り込み・棚卸し・変更操作も、対象外の機体には接続せず DB にも書かない', async () => {
  const touchedDb = new Proxy({}, { get: (_t, k) => () => { throw new Error(`DB を触った: ${String(k)}`); } });
  for (const id of EXCLUDED) {
    const n = byId(id);
    const s = await syncNode(touchedDb, n);
    assert.equal(s.skipped, true);
    assert.deepEqual(s.sources, {});
    const inv = await inventoryNode(n);
    assert.equal(inv.skipped, true);
    assert.equal(inv.ok, false);
    const ex = await execOn(n, 'echo x');
    assert.equal(ex.code, null);
    assert.match(ex.err, /接続も変更もしない/);
    assert.throws(() => plan(n, { type: 'kill-process', params: { pid: 99999, name: 'x' } }, { protect: [] }), /接続も変更もしない/);
    let called = 0;
    await assert.rejects(execute(n, { type: 'kill-process', params: { pid: 99999, name: 'x' } }, { protect: [] }, async () => { called += 1; return { code: 0, out: '', err: '' }; }), /接続も変更もしない/);
    assert.equal(called, 0);
  }
  assert.deepEqual(await syncAll(touchedDb, cfg().nodes.filter((n) => EXCLUDED.includes(n.id))), []);
});

test('online の機体は unreachable でも online のまま。台帳（ファイルも読んだ内容も）を自動で変えない', async () => {
  const before = fs.readFileSync(FIXTURE);
  const config = cfg();
  const frozen = JSON.parse(JSON.stringify(config.nodes));
  for (const n of config.nodes) Object.freeze(n);
  const ssh = mockSsh({ 'active-mac': { code: 255, out: '', err: 'ssh: Could not resolve hostname active-mac: Name or service not known\n' } });
  const r = await probeNode(config.nodes.find((n) => n.id === 'active-mac'), { run: ssh.run, localShell: noLocal });
  assert.equal(r.ok, false);
  assert.equal(r.reason, 'unreachable');
  assert.notEqual(r.skipped, true);
  assert.deepEqual(config.nodes, frozen);
  assert.equal(ledger.opState(config.nodes[0]).state, 'online');
  assert.ok(before.equals(fs.readFileSync(FIXTURE)), '台帳ファイルが書き換わった');
  // 状態は異常（fail）側。退役とは別の扱い
  const checks = nodeChecks(config.nodes[0], null, [], { now: 1, lastError: r.error });
  assert.equal(checks.find((c) => c.id === 'probe').status, 'fail');
});

test('対象外の機体の状態（health）は異常にならず、分析・ログの失敗や古さも異常にしない', () => {
  const cursors = [{ node_id: 'old-tower', source: 'win_system', last_error: 'boom', fail_streak: 9, last_ok_at: 1 }];
  for (const id of EXCLUDED.filter((x) => x !== 'typo-node')) {
    const checks = nodeChecks(byId(id), { at: 1, data: {} }, [], { now: 9e12, lastError: 'ssh: timeout', cursors });
    assert.equal(checks.length, 1);
    assert.equal(checks[0].id, 'ledger');
    assert.equal(checks[0].status, 'unknown');
    assert.match(checks[0].detail, /対象外/);
  }
  assert.match(nodeChecks(byId('old-tower'), null, [])[0].detail, /退役.*2026-09 に解体.*2026-09-30/);
  const typo = nodeChecks(byId('typo-node'), null, [], { lastError: 'x' });
  assert.deepEqual([typo.length, typo[0].status], [1, 'warn']);
});

test('対象台帳: 対象・除外・未観測を理由つきで分ける。除外には判断日と理由が出る', () => {
  const nodes = cfg().nodes;
  const led = ledger.targetLedger(nodes, {
    lastOkAt: (id) => (id === 'active-mac' ? 1_800_000_000_000 : null),
    lastError: (id) => (id === 'explicit-online' ? 'ssh: connect to host explicit-online port 22: Operation timed out\n' : null),
  });
  const row = (id) => led.rows.find((r) => r.node_id === id);
  assert.equal(row('active-mac').status, 'target');
  assert.equal(row('explicit-online').status, 'unobserved');
  assert.match(row('explicit-online').reason, /Operation timed out/);
  assert.equal(row('explicit-online').state, 'online');
  assert.deepEqual([row('old-tower').status, row('old-tower').reason, row('old-tower').date], ['excluded', '2026-09 に解体', '2026-09-30']);
  assert.equal(row('someone-else').reason, '理由の記載なし');
  assert.equal(row('typo-node').state, 'invalid');
  assert.deepEqual(led.counts, { target: 1, unobserved: 1, excluded: 6, total: 8 });
  assert.equal(ledger.targetLedger([{ id: 'n', os: 'macos' }]).rows[0].reason, 'まだ分析していない');
});

test('refusal は運用中なら null、対象外なら理由を返す', () => {
  assert.equal(ledger.refusal(byId('active-mac')), null);
  assert.match(ledger.refusal(byId('old-tower')), /old-tower は運用状態が retired（退役）/);
});

test('見本の台帳: 退役の見本機体は対象外、それ以外は運用中。見本はそのまま読める', () => {
  const example = loadConfig(EXAMPLE);
  const { targets, excluded } = ledger.partition(example.nodes);
  assert.deepEqual(excluded.map((x) => [x.node_id, x.state]), [['old-pc', 'retired']]);
  assert.equal(targets.length, example.nodes.length - 1);
});
