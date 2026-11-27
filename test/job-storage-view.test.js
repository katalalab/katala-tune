'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const UI = require('../renderer/ui.js');

const cold = {
  kind: 'job-storage', schema_version: 1, job_id: 'cold', status: 'completed', volume: '<volume>', interval_ms: 100,
  samples: [{ elapsed_ms: 0, free_bytes: 8192 }, { elapsed_ms: 100, free_bytes: null }, { elapsed_ms: 200, free_bytes: 9216 }],
  peak_bytes: 0, net_bytes: -1024, residual_bytes: null,
  preflight: { os_reserve_bytes: 32, other_reserved_bytes: 0, expected_peak_bytes: 128 },
  categories: [{ kind: 'log', logical_delta_bytes: 24 }, { kind: 'artifact', logical_delta_bytes: null }],
};

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
  await v.select(file(cold), file({ ...cold, job_id: 'warm', residual_bytes: -256, future_field: true }, '<warm>.json'));
  assert.match(v.html, /cold\.json/);
  assert.match(v.html, /&lt;warm&gt;\.json/);
  assert.match(v.html, /&lt;volume&gt;/);
  assert.match(v.html, /欠測 1 回/);
  assert.match(v.html, /-1 KiB/);
  assert.match(v.html, /0 B/);
  assert.match(v.html, /-256 B/);
  assert.match(v.html, /未取得/);
  assert.match(v.html, /未判定/);
  assert.match(v.html, /記録なし/);
  v.clear();
  assert.doesNotMatch(v.html, /cold\.json/);
});

test('blocked and compacted reports preserve status and missing counts', async () => {
  const v = open();
  await v.select(file({ ...cold, status: 'blocked', reason: 'insufficient_space', samples: [cold.samples[0]],
    peak_bytes: null, net_bytes: null }));
  await v.select(file({ ...cold, sample_count: 9, missing_sample_count: 2 }, 'compact.json'));
  assert.match(v.html, /開始保留（必要量に対して空き不足）/);
  assert.match(v.html, /9 回（欠測 2 回、保存 3 点）/);
});

test('rejects malformed, oversized and excess inputs; already selected reports survive', async () => {
  const v = open();
  const invalid = [
    { ...cold, samples: [{ elapsed_ms: 0, free_bytes: -1 }] },
    { ...cold, net_bytes: '0' }, { ...cold, net_bytes: 0 }, { ...cold, peak_bytes: 4 }, { ...cold, categories: [{ kind: '__proto__', logical_delta_bytes: 1 }] },
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
