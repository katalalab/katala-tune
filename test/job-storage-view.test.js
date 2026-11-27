'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const UI = require('../renderer/ui.js');

const cold = {
  kind: 'job-storage', schema_version: 1, job_id: 'cold', status: 'completed', volume: '<volume>', interval_ms: 100,
  samples: [{ elapsed_ms: 0, free_bytes: 8192 }, { elapsed_ms: 100, free_bytes: null }, { elapsed_ms: 200, free_bytes: 9216 }],
  peak_bytes: 0, net_bytes: -1024, unattributed_net_bytes: -1024, residual_bytes: null, exit_code: 0,
  preflight: { os_reserve_bytes: 32, other_reserved_bytes: 0, expected_peak_bytes: 128, basis: 'important_usage' },
  categories: [{ kind: 'log', logical_delta_bytes: 24 }, { kind: 'artifact', logical_delta_bytes: null }],
};
const blocked = { ...cold, status: 'blocked', reason: 'insufficient_space', samples: [cold.samples[0]],
  categories: [], exit_code: null, peak_bytes: null, net_bytes: null, unattributed_net_bytes: null };

function row(html, name) {
  return html.match(new RegExp(name + '</th>(.*?)</tr>'))[1];
}

function file(data, name = 'cold.json') {
  const source = JSON.stringify(data);
  return { name, size: Buffer.byteLength(source), text: async () => source };
}

function open() {
  const controls = new Map();
  let html;
  const ctx = {
    window: {}, UI, ticket() {}, setCrumbs() {}, page(value) { html = value; },
    document: { querySelector(id) { if (!controls.has(id)) controls.set(id, {}); return controls.get(id); } },
  };
  vm.runInNewContext(fs.readFileSync('renderer/job-storage.js', 'utf8'), ctx);
  const view = ctx.window.KT_VIEWS[0];
  view.render();
  return { view, get html() { return html; }, select: (...files) => controls.get('#storageFile').onchange({ target: { files } }),
    clear: () => controls.get('#storageClear').onclick() };
}

test('shows two measurements including signed, missing and measured zero values', async () => {
  const v = open();
  await v.select(file(cold), file({ ...cold, job_id: 'warm', future_field: true }, '<warm>.json'));
  assert.match(v.html, /cold\.json/);
  assert.match(v.html, /&lt;warm&gt;\.json/);
  assert.match(v.html, /&lt;volume&gt;/);
  assert.match(v.html, /欠測 1 回/);
  assert.match(v.html, /-1 KiB/);
  assert.match(v.html, /0 B/);
  assert.match(row(v.html, '分類先が未確定の実容量'), /-1 KiB/);
  assert.match(row(v.html, '物理量で照合した残差'), /未判定/);
  assert.match(v.html, /未取得/);
  assert.match(v.html, /未判定/);
  assert.match(v.html, /記録なし/);
  v.clear();
  assert.doesNotMatch(v.html, /cold\.json/);
});

test('blocked and compacted reports preserve status and missing counts', async () => {
  const v = open();
  await v.select(file(blocked));
  await v.select(file({ ...cold, sample_count: 9, missing_sample_count: 2 }, 'compact.json'));
  assert.match(v.html, /開始保留（必要量に対して空き不足）/);
  assert.match(v.html, /9 回（欠測 2 回、保存 3 点）/);
});

test('guest-only and failed-to-spawn jobs show the initial sample only', async () => {
  const v = open();
  await v.select(file({ ...blocked, reason: 'guest_capacity_only' }),
    file({ ...blocked, status: 'failed', reason: 'spawn_error' }), file({ ...blocked, reason: 'environment_unknown' }));
  assert.match(v.html, /Windows側で仮想ディスクのあるドライブを指定/);
  assert.match(v.html, /ジョブを起動できず/);
  assert.match(v.html, /実行環境を識別できず/);
  assert.equal((row(v.html, '開始時の空き').match(/8 KiB/g) || []).length, 3);
  assert.equal((row(v.html, '終了時の空き').match(/未取得/g) || []).length, 3);
  assert.equal((row(v.html, '最終観測までの経過時間').match(/未取得/g) || []).length, 3);
  assert.equal((row(v.html, '子プロセスの終了コード').match(/未取得/g) || []).length, 3);
});

test('reports actual exit codes, elapsed observation time and admission basis', async () => {
  const v = open();
  await v.select(file(cold), file({ ...cold, status: 'failed', reason: 'command_failed', exit_code: 75,
    preflight: { ...cold.preflight, basis: 'available_to_caller' } }),
    file({ ...cold, status: 'failed', reason: 'command_failed', exit_code: null, preflight: { ...cold.preflight, basis: undefined } }));
  assert.match(row(v.html, '子プロセスの終了コード'), />0<.*>75<.*>未取得</);
  assert.match(row(v.html, '最終観測までの経過時間'), />200 ms</);
  assert.match(row(v.html, '空きの算出方法'), /重要用途に使える空き.*実行ユーザーが直接使える空き.*未記録/);
});

test('an interrupted placeholder renders unknown measurements and no invented observations', async () => {
  const v = open();
  await v.select(file({ ...blocked, status: 'failed', reason: 'incomplete', samples: [] }));
  assert.match(v.html, /終了未確認/);
  assert.match(row(v.html, '最終観測までの経過時間'), /未取得/);
});

test('rejects malformed, oversized and excess inputs; already selected reports survive', async () => {
  const v = open();
  const invalid = [
    { ...cold, samples: [{ elapsed_ms: 0, free_bytes: -1 }] },
    { ...cold, net_bytes: '0' }, { ...cold, net_bytes: 0 }, { ...cold, peak_bytes: 4 }, { ...cold, categories: [{ kind: '__proto__', logical_delta_bytes: 1 }] },
    { ...cold, preflight: { ...cold.preflight, os_reserve_bytes: Number.MAX_SAFE_INTEGER + 1 } },
    { ...cold, preflight: { ...cold.preflight, free_bytes: Number.MAX_SAFE_INTEGER + 1 } },
    { ...cold, unattributed_net_bytes: -1 }, { ...cold, residual_bytes: -256 },
    { ...blocked, exit_code: 0 }, { ...blocked, categories: cold.categories }, { ...cold, exit_code: 75 }, { ...cold, reason: 'spawn_error' },
    { ...cold, exit_code: '0' }, { ...cold, preflight: { ...cold.preflight, basis: [] } },
    { ...cold, sample_count: 1 }, { ...cold, status: ['failed'] }, { ...cold, schema_version: 2 },
  ];
  for (const data of invalid) {
    await v.select(file(data));
    assert.match(v.html, /class="err"/);
    assert.doesNotMatch(v.html, /class="dbt"/);
  }
  await v.select(file(cold), { size: 9 * 1024 * 1024, name: 'large.json', text: () => assert.fail('oversized file read') });
  assert.match(v.html, /8 MiBを超えて/);
  assert.match(v.html, /cold\.json/);
  await v.select(file(cold), file(cold), file(cold), file(cold));
  assert.match(v.html, /一度に表示できるのは4件/);
});

test('leaving the view invalidates a pending local import', async () => {
  const v = open();
  let finish;
  const pending = v.select({ name: 'delayed.json', size: 1024, text: () => new Promise(resolve => { finish = resolve; }) });
  v.view.leave();
  v.view.render();
  finish(JSON.stringify(cold));
  await pending;
  assert.doesNotMatch(v.html, /delayed\.json|読み込み中/);
});
