// Issue #19 の受入試験（mock SSH・一時ディレクトリ・偽の対象だけ。実機の ssh・launchd・タスクスケジューラは呼ばない）。
// Rust 側の同じ条件は crates/tune-core/tests/acceptance_issue19.rs。結果の記録は docs/acceptance/issue-19.md
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { probeAll, probeNode, classifyFailure, run } = require('../lib/collect');
const { execute, plan } = require('../lib/actions');
const { makeRunner } = require('../lib/runner');
const { openDb } = require('../lib/db');
const { loadConfig } = require('../lib/nodes');

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'kt-accept19-'));
const nodeOf = (id, extra = {}) => ({ id, alias: `mock-${id}`, os: 'windows', ...extra });

// ---- 偽の ssh（呼び出しを数え、台本どおりの結果を返す。本物の子プロセスは作らない） ----
function mockSsh(script) {
  const calls = [];
  const run = async (cmd, args) => {
    calls.push({ cmd, args });
    assert.equal(cmd, 'ssh', '偽の実行口は ssh 以外を受け付けない');
    const alias = args.find((a) => a.startsWith('mock-'));
    const r = script[alias];
    if (!r) throw new Error(`台本に無い host: ${alias}`);
    return r;
  };
  return { run, calls };
}

// ---- 条件1: read-only probe が host 別の成功・timeout・unreachable・認証失敗を分ける ----
test('条件1: probe は host 別に 成功・timeout・unreachable・認証失敗 を分けて返し、理由の文言も区別する', async () => {
  const ssh = mockSsh({
    'mock-ok': { code: 0, out: '{"probe":"windows","cpu_busy":3}\n', err: '' },
    'mock-slow': { code: null, out: '', err: '\ntimeout 120000ms' },
    'mock-gone': { code: 255, out: '', err: 'ssh: Could not resolve hostname mock-gone: Name or service not known\n' },
    'mock-down': { code: 255, out: '', err: 'ssh: connect to host mock-down port 22: Operation timed out\n' },
    'mock-key': { code: 255, out: '', err: 'user@mock-key: Permission denied (publickey).\n' },
    'mock-broken': { code: 1, out: 'not json\n', err: 'boom' },
  });
  const nodes = ['ok', 'slow', 'gone', 'down', 'key', 'broken'].map((id) => nodeOf(id));
  const seen = [];
  const results = await probeAll(nodes, (r) => seen.push(r.node_id), ssh);
  const by = Object.fromEntries(results.map((r) => [r.node_id, r]));

  assert.equal(by.ok.ok, true);
  assert.equal(by.ok.reason, undefined);
  assert.deepEqual(['slow', 'gone', 'down', 'key', 'broken'].map((id) => [by[id].ok, by[id].reason]), [
    [false, 'timeout'], [false, 'unreachable'], [false, 'unreachable'], [false, 'auth'], [false, 'error'],
  ]);
  // 画面に出す文言（reason_text）は種類ごとに違い、成功には付かない
  const texts = new Set(['slow', 'gone', 'key', 'broken'].map((id) => by[id].reason_text));
  assert.equal(texts.size, 4);
  assert.ok(by.slow.reason_text && by.gone.reason_text && by.key.reason_text);
  assert.equal(by.ok.reason_text, undefined);
  // 元のエラー文は消さない（既存の画面・状態が使う）
  assert.match(by.gone.error, /Could not resolve hostname/);
  assert.equal(seen.length, 6, 'host ごとに結果が1回ずつ届く');
  // すべて偽の ssh を通り、BatchMode（パスワードを聞かない）・接続の打ち切りが付く
  assert.equal(ssh.calls.length, 6);
  for (const c of ssh.calls) assert.ok(c.args.includes('BatchMode=yes') && c.args.includes('ConnectTimeout=10'));
});

test('条件1: 失敗の分類表（Rust の classify_failure と同じ入力・同じ答え）', () => {
  const table = [
    [{ code: null, out: '', err: '\ntimeout 90000ms' }, 'timeout'],
    [{ code: 255, out: '', err: 'ssh: connect to host x port 22: Connection refused' }, 'unreachable'],
    [{ code: 255, out: '', err: 'ssh: connect to host x port 22: No route to host' }, 'unreachable'],
    [{ code: 255, out: '', err: 'kex_exchange_identification: Connection closed by remote host' }, 'unreachable'],
    [{ code: 255, out: '', err: 'Host key verification failed.' }, 'auth'],
    [{ code: 255, out: '', err: 'Connection closed by authenticating user x port 22 [preauth]\nPermission denied' }, 'auth'],
    [{ code: null, out: '', err: 'Error: spawn ssh ENOENT' }, 'error'],
    [{ code: 3, out: '', err: '' }, 'error'],
  ];
  for (const [res, kind] of table) assert.equal(classifyFailure(res), kind, JSON.stringify(res));
});

test('条件1: ssh を呼べない・例外でも host の失敗として返り、他の host の結果は届く', async () => {
  const ssh = mockSsh({ 'mock-ok': { code: 0, out: '{"probe":"windows"}\n', err: '' } });
  const results = await probeAll([nodeOf('ok'), nodeOf('nodata')], null, ssh);
  assert.equal(results[0].ok, true);
  assert.equal(results[1].ok, false);
  assert.match(results[1].error, /台本に無い host/);
  // 例外で落ちた host にも、他の失敗と同じ形で種類と文言が付く
  assert.equal(results[1].reason, 'error');
  assert.ok(results[1].reason_text);
});

test('条件1: この機体（local）の調査も、渡された実行口を通り、本物のローカル実行に迂回しない', async () => {
  const calls = [];
  const localShell = {
    async powershellFile() { calls.push('powershellFile'); return { code: 0, out: '{"probe":"windows"}\n', err: '' }; },
    async python() { calls.push('python'); return { code: 0, out: '{"runs_ms":[1],"median_ms":1}\n', err: '' }; },
  };
  const ssh = mockSsh({});
  const local = nodeOf('here', { local_hostname: 'x' });
  local.local = true;
  const r = await probeNode(local, { run: ssh.run, localShell });
  assert.equal(r.ok, true);
  assert.deepEqual(calls, ['powershellFile', 'python']);
  assert.equal(ssh.calls.length, 0);
});

// ---- 偽の対象: 一時ディレクトリの「タスク」。state ファイルが実体 ----
function fakeTaskHost(dir) {
  const calls = [];
  const stateFile = (name) => path.join(dir, `${name}.state`);
  const exec = async (node, command) => {
    calls.push({ node: node.id, command });
    const m = /(Disable|Enable)-ScheduledTask -TaskPath "([^"]*)" -TaskName "([^"]*)"/.exec(command);
    if (!m) return { code: 1, out: '', err: 'unsupported' };
    const file = stateFile(m[3]);
    if (!fs.existsSync(file)) return { code: 1, out: '', err: 'no such task' };
    if (exec.failNext) { exec.failNext = false; return { code: 1, out: '', err: 'access denied' }; }
    fs.writeFileSync(file, m[1] === 'Disable' ? 'Disabled' : 'Ready');
    return { code: 0, out: `${fs.readFileSync(file, 'utf8')}\n`, err: '' };
  };
  return { exec, calls, stateFile, state: (name) => fs.readFileSync(stateFile(name), 'utf8') };
}

function setup({ nodes, protect = [] } = {}) {
  const dir = tmp();
  const ledger = path.join(dir, 'nodes.json');
  const writeLedger = (n = nodes, p = protect) => fs.writeFileSync(ledger, JSON.stringify({ protect: p, nodes: n }));
  writeLedger();
  const db = openDb(path.join(dir, 'data'));
  const host = fakeTaskHost(dir);
  fs.writeFileSync(host.stateFile('Backup'), 'Ready');
  let approve = true;
  let onConfirm = null;
  const confirmations = [];
  const runner = makeRunner({
    loadConfig: () => loadConfig(ledger),
    confirm: async (title, detail) => { confirmations.push({ title, detail }); await onConfirm?.(); return approve; },
    exec: host.exec,
    db,
  });
  return { dir, ledger, writeLedger, db, host, runner, confirmations, setApprove: (v) => { approve = v; }, setOnConfirm: (f) => { onConfirm = f; } };
}

const disable = { type: 'task-disable', params: { path: '\\Katala\\', name: 'Backup' } };

// ---- 条件2: 承認なしでは変更コマンドを実行しない ----
test('条件2: 承認されなければ実行器を一度も呼ばず、実行記録も作らない', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  t.setApprove(false);
  const r = await t.runner.confirmAndRun('w', disable, 'w: タスクを止める');
  assert.equal(r.cancelled, true);
  assert.equal(t.host.calls.length, 0);
  assert.equal(t.host.state('Backup'), 'Ready');
  assert.equal(t.db.actions().length, 0);
  assert.equal(t.confirmations.length, 1);
  assert.match(t.confirmations[0].detail, /Disable-ScheduledTask/, '承認の画面には実行するコマンドが出る');
});

test('条件2: 許可リスト外・共用機・保護対象・台帳なしは、確認ダイアログすら出さず実行器を呼ばない', async () => {
  const t = setup({ nodes: [nodeOf('w'), nodeOf('shared', { shared: true })], protect: ['Backup'] });
  const kill = { type: 'kill-process', params: { pid: 4242, name: 'Backup' } };
  for (const [id, action] of [['w', { type: 'format-disk', params: {} }], ['shared', disable], ['w', kill], ['nobody', disable]]) {
    const r = await t.runner.confirmAndRun(id, action, 't');
    assert.equal(r.ok, false);
    assert.ok(r.refused, JSON.stringify(r));
  }
  assert.equal(t.confirmations.length, 0);
  assert.equal(t.host.calls.length, 0);
  // 台帳が読めないときも実行しない
  fs.writeFileSync(t.ledger, '{broken');
  const r = await t.runner.confirmAndRun('w', disable, 't');
  assert.match(r.refused, /台帳を読めない/);
  assert.equal(t.host.calls.length, 0);
});

test('条件2: actions.execute を直接呼んでも、共用機・保護リスト未読では実行器に届かない', async () => {
  const calls = [];
  const exec = async (...a) => { calls.push(a); return { code: 0, out: '', err: '' }; };
  await assert.rejects(execute(nodeOf('s', { shared: true }), disable, { protect: [] }, exec), /共用機/);
  await assert.rejects(execute(nodeOf('w'), disable, {}, exec), /保護リスト/);
  assert.equal(calls.length, 0);
});

// ---- 条件3: 承認後も対象の同一性が合わなければ実行を止める ----
test('条件3: 承認のあいだに台帳の接続先・OS・機体が変わったら、実行器を呼ばず理由を記録する', async () => {
  const cases = [
    ['alias', (n) => [{ ...n, alias: 'mock-other' }], /接続先/],
    ['os', (n) => [{ ...n, os: 'macos' }], /接続先|Windows だけ/],
    ['local', (n) => [{ ...n, local_hostname: os.hostname().replace(/\.local$/, '') }], /接続先/],
    ['removed', () => [], /台帳に無い/],
  ];
  for (const [name, change, expected] of cases) {
    const t = setup({ nodes: [nodeOf('w')] });
    t.setOnConfirm(() => t.writeLedger(change(nodeOf('w'))));
    const r = await t.runner.confirmAndRun('w', disable, 'w: タスクを止める');
    assert.equal(r.ok, false, name);
    assert.match(r.refused, expected, name);
    assert.equal(t.host.calls.length, 0, `${name}: 実行器は呼ばれない`);
    assert.equal(t.host.state('Backup'), 'Ready', name);
    const [row, ...rest] = t.db.actions();
    assert.equal(rest.length, 0, name);
    assert.equal(row.ok, false);
    assert.equal(row.state, 'failed');
    assert.equal(row.node_id, 'w');
    assert.equal(row.type, 'task-disable');
    assert.match(row.output, /^中止（実行していない）: /, name);
    assert.equal(row.undo, null);
  }
});

test('条件3: 承認のあいだに共用機になる・保護対象になる・台帳が壊れる場合も実行しない', async () => {
  const t1 = setup({ nodes: [nodeOf('w')] });
  t1.setOnConfirm(() => t1.writeLedger([nodeOf('w', { shared: true })]));
  assert.match((await t1.runner.confirmAndRun('w', disable, 't')).refused, /共用機/);
  assert.equal(t1.host.calls.length, 0);

  const t2 = setup({ nodes: [nodeOf('w')] });
  t2.setOnConfirm(() => t2.writeLedger([nodeOf('w')], ['Slow']));
  const kill = { type: 'kill-process', params: { pid: 4242, name: 'Slow' } };
  assert.match((await t2.runner.confirmAndRun('w', kill, 't')).refused, /保護対象/);
  assert.equal(t2.host.calls.length, 0);

  const t3 = setup({ nodes: [nodeOf('w')] });
  t3.setOnConfirm(() => fs.writeFileSync(t3.ledger, '{broken'));
  assert.match((await t3.runner.confirmAndRun('w', disable, 't')).refused, /台帳を読めない/);
  assert.equal(t3.host.calls.length, 0);
  for (const t of [t1, t2, t3]) assert.match(t.db.actions()[0].output, /^中止（実行していない）/);
});

test('条件3: 同一性が合っていれば（台帳が同じ内容で書き直されただけなら）実行する', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  t.setOnConfirm(() => t.writeLedger([nodeOf('w', { note: '説明を足しただけ' })]));
  const r = await t.runner.confirmAndRun('w', disable, 't');
  assert.equal(r.ok, true);
  assert.equal(t.host.calls.length, 1);
});

// ---- 条件4: 成功・失敗・rollback を実行記録へ。偽の対象で可逆操作を実際に動かし、前後状態と照合する ----
test('条件4: 偽のタスクを止めて（成功）→ 元に戻す（rollback 成功）。前後の状態と実行記録が一致する', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  assert.equal(t.host.state('Backup'), 'Ready'); // 実行前
  const r = await t.runner.confirmAndRun('w', disable, 'w: タスクを止める');
  assert.equal(r.ok, true);
  assert.equal(t.host.state('Backup'), 'Disabled'); // 実行後
  assert.equal(t.host.calls.length, 1);

  let [row] = t.db.actions();
  assert.deepEqual([row.state, row.ok, row.node_id, row.type], ['ok', true, 'w', 'task-disable']);
  assert.deepEqual(row.params, { path: '\\Katala\\', name: 'Backup' }); // 対象 ID
  assert.match(row.output, /Disabled/); // 実行後の状態
  assert.deepEqual(row.undo, { type: 'task-enable', params: { path: '\\Katala\\', name: 'Backup' } }); // 実行前の状態へ戻す操作

  const u = await t.runner.undo(row.id);
  assert.equal(u.ok, true);
  assert.equal(t.host.state('Backup'), 'Ready'); // 実行前と同じ
  const all = t.db.actions();
  assert.equal(all.length, 2);
  const undoRow = all.find((a) => a.undo_of === row.id);
  assert.deepEqual([undoRow.state, undoRow.type, undoRow.node_id], ['ok', 'task-enable', 'w']);
  assert.match(undoRow.output, /Ready/);
  // 二度は戻さない
  assert.match((await t.runner.undo(row.id)).refused, /すでに元に戻した/);
  assert.equal(t.host.calls.length, 2);
});

test('条件4: 実行が失敗したら失敗として残り、戻し操作は付かない。対象は変わらない', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  t.host.exec.failNext = true;
  const r = await t.runner.confirmAndRun('w', disable, 'w: タスクを止める');
  assert.equal(r.ok, false);
  const [row] = t.db.actions();
  assert.deepEqual([row.state, row.ok, row.undo], ['failed', false, null]);
  assert.match(row.output, /失敗（exit 1）/);
  assert.equal(t.host.state('Backup'), 'Ready');
});

test('条件4: rollback が失敗したら失敗として残り、元の記録は「戻し済み」にならない（やり直せる）', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  await t.runner.confirmAndRun('w', disable, 'w: タスクを止める');
  const [orig] = t.db.actions();
  t.host.exec.failNext = true;
  const u = await t.runner.undo(orig.id);
  assert.equal(u.ok, false);
  assert.equal(t.host.state('Backup'), 'Disabled'); // 戻っていない
  const undoRow = t.db.actions().find((a) => a.undo_of === orig.id);
  assert.deepEqual([undoRow.state, undoRow.ok], ['failed', false]);
  // 失敗した戻しは「戻し済み」と見なされない
  const again = await t.runner.undo(orig.id);
  assert.equal(again.ok, true);
  assert.equal(t.host.state('Backup'), 'Ready');
});

test('条件4: 実行器が例外を投げたら失敗として残す（未完了のまま放置しない）', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  const runner = makeRunner({
    loadConfig: () => loadConfig(t.ledger), confirm: async () => true, db: t.db,
    exec: async () => { throw new Error('ssh を起動できない'); },
  });
  const r = await runner.confirmAndRun('w', disable, 't');
  assert.equal(r.ok, false);
  const [row] = t.db.actions();
  assert.deepEqual([row.state, row.ok], ['failed', false]);
  assert.match(row.output, /ssh を起動できない/);
});

test('条件4: 実行記録に書けないときは実行しない（記録が無いまま変更しない）', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  const brokenDb = { actions: () => [], addAction() { throw new Error('disk full'); }, finishAction() { throw new Error('disk full'); } };
  const runner = makeRunner({ loadConfig: () => loadConfig(t.ledger), confirm: async () => true, exec: t.host.exec, db: brokenDb });
  const r = await runner.confirmAndRun('w', disable, 't');
  assert.match(r.refused, /実行記録を書けない/);
  assert.equal(t.host.calls.length, 0);
  assert.equal(t.host.state('Backup'), 'Ready');
});

test('条件4: 結果の書き込みが「未更新」（false）を返したら、成功を返さず記録失敗として拒否する', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  // 実行中に、同じ記録へ別の手で結果が書かれた（未完了でなくなった）状態を作る
  const exec = async (...a) => {
    const [row] = t.db.actions();
    assert.equal(t.db.finishAction(row.id, { ok: false, output: '別の手', undo: null }), true);
    return t.host.exec(...a);
  };
  const runner = makeRunner({ loadConfig: () => loadConfig(t.ledger), confirm: async () => true, exec, db: t.db });
  const r = await runner.confirmAndRun('w', disable, 't');
  assert.equal(r.ok, false);
  assert.match(r.refused, /実行記録を書けなかった/);
  assert.equal(r.entry, undefined);
});

test('条件4: 結果を書けなかったら成功を返さず、記録は未完了のまま残る', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  const db = { actions: () => t.db.actions(), addAction: (a) => t.db.addAction(a), finishAction() { throw new Error('disk full'); } };
  const runner = makeRunner({ loadConfig: () => loadConfig(t.ledger), confirm: async () => true, exec: t.host.exec, db });
  const r = await runner.confirmAndRun('w', disable, 't');
  assert.equal(r.ok, false);
  assert.match(r.refused, /操作は実行済み/);
  const [row] = t.db.actions();
  assert.equal(row.state, 'incomplete');
});

// ---- 条件5: アプリ再起動後に未完了操作を成功扱いしない ----
test('条件5: 実行中に止まった操作は、DB を開き直すと「未完了」であり、成功にも失敗にも数えず、戻しも出さない', async () => {
  const t = setup({ nodes: [nodeOf('w')] });
  // 実行器が返らないまま（= 結果を書く前にアプリが止まった状態）
  const hung = makeRunner({ loadConfig: () => loadConfig(t.ledger), confirm: async () => true, exec: () => new Promise(() => {}), db: t.db });
  hung.confirmAndRun('w', disable, 'w: タスクを止める');
  for (let i = 0; i < 50 && t.db.actions().length === 0; i++) await new Promise((r) => setTimeout(r, 10));
  assert.equal(t.db.actions().length, 1, '実行の前に記録が書かれている');

  // 再起動: 同じ DB ファイルを新しく開く
  const reopened = openDb(path.join(t.dir, 'data'));
  const rows = reopened.actions();
  assert.equal(rows.length, 1);
  const [row] = rows;
  assert.equal(row.state, 'incomplete');
  assert.equal(row.ok, false, '成功扱いにしない');
  assert.equal(row.undo, null);
  assert.match(row.output, /結果は未確認/);
  const count = (s) => rows.filter((a) => a.state === s).length;
  assert.deepEqual([count('ok'), count('failed'), count('incomplete')], [0, 0, 1]);

  // 未完了の記録は元に戻せない（状態が分からないものを戻さない）。あとから結果が書かれても、すでに結果のある記録は書き換えない
  const r2 = makeRunner({ loadConfig: () => loadConfig(t.ledger), confirm: async () => true, exec: t.host.exec, db: reopened });
  assert.match((await r2.undo(row.id)).refused, /元に戻せる記録が無い/);
  assert.equal(reopened.finishAction(row.id, { ok: true, output: 'x', undo: null }), true, '未完了の記録にだけ結果を書ける');
  assert.equal(reopened.finishAction(row.id, { ok: false, output: 'y', undo: null }), false, '書いた結果は上書きしない');
  assert.equal(reopened.actions()[0].state, 'ok');
});

// ---- 条件3 の機体側: 生成されるプロセス終了スクリプトは、名前・起動時刻・負荷が合わなければ kill に進まない ----
// 試験が自分で起こした子プロセス（sleep）にだけ、ローカルの sh で実行する。どの経路でも kill に届かないことを確かめる
test('条件3: プロセス終了スクリプトは、名前・起動時刻・負荷が合わなければ kill に進まない（対象は生きている）', { skip: process.platform === 'win32' && 'POSIX shell が必要' }, async () => {
  const { spawn, execFileSync } = require('node:child_process');
  const child = spawn('sleep', ['30'], { stdio: 'ignore' });
  try {
    const pid = child.pid;
    const lstart = execFileSync('ps', ['-p', String(pid), '-o', 'lstart=']).toString().trim();
    const mac = { id: 'm', alias: 'mock-m', os: 'macos' };
    const alive = () => { try { process.kill(pid, 0); return true; } catch { return false; } };
    const cases = [
      ['Slowproc', undefined, 50, 3],
      ['sleep', 'Mon Jan  1 00:00:00 2001', 50, 3],
      ['sleep', lstart, 50, 4],
    ];
    for (const [name, start, min_cpu, expect] of cases) {
      const p = plan(mac, { type: 'kill-process', params: { pid, name, start, min_cpu } }, { protect: [] });
      const r = await run('/bin/sh', ['-c', p.script], { timeoutMs: 10000 });
      assert.equal(r.code, expect, `${name}: ${r.out}${r.err}`);
      assert.ok(alive(), `${name}: 対象は生きている`);
    }
  } finally {
    child.kill('SIGKILL');
  }
});
