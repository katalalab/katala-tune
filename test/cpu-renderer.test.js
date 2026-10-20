'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function render({ cpu, probe = 'windows', fleet = false }) {
  const source = fs.readFileSync(path.join(__dirname, '../renderer/app.js'), 'utf8');
  const start = source.indexOf('function wslHtml(');
  const machine = source.indexOf('\nfunction machineHtml(', start);
  const fleetStart = source.indexOf('function renderProcs(');
  const fleetEnd = source.indexOf('\n// ---- スケジュール', fleetStart);
  const p = { pid: 7, name: 'sample-process', app: 'sample-app', cpu, mem_mb: 128, avg_core: null, count: 1 };
  const data = { probe, host: { cores: 8 }, processes: { apps: [p], top_cpu: [p], top_mem: [], count: 1 } };
  let html = '';
  const UI = { section: () => '', head: () => '', search: () => '', select: () => '', seg: () => '', table: ({ rows, row }) => rows.map(row).join('') };
  const ctx = vm.createContext({
    UI, esc: String, fmtTime: String, Charts: { dataBar: (n) => { assert.ok(Number.isFinite(n), 'unknown CPU must not draw a numeric bar'); return ''; } },
    state: { nodes: [{ id: 'test-node' }], results: { 'test-node': { ok: true, data } }, procFilter: { sort: 'cpu', q: '', node_id: '' } },
    ticket: () => {}, setCrumbs: () => {}, nodeOptions: () => [], page: (s) => { html = s; },
    bindSearch: () => {}, $: () => ({}), $$: () => [], data,
  });
  vm.runInContext(source.slice(start, machine) + '\n' + source.slice(fleetStart, fleetEnd), ctx);
  if (fleet) vm.runInContext('renderProcs()', ctx);
  else html = vm.runInContext('procsHtml(data)', ctx);
  return html;
}

for (const fleet of [false, true]) {
  for (const probe of ['windows', 'mac']) {
    test(`${fleet ? 'fleet' : 'machine'} ${probe}: unknown CPU is not displayed as zero`, () => {
      for (const cpu of [null, undefined, NaN, Infinity]) {
        const html = render({ cpu, probe, fleet });
        assert.equal((html.match(/<td class="n">—<\/td>/g) || []).length, fleet ? 1 : 2);
        assert.doesNotMatch(html, /(?:NaN|Infinity|0)%/);
      }
    });
    test(`${fleet ? 'fleet' : 'machine'} ${probe}: real zero and measured CPU retain units`, () => {
      assert.match(render({ cpu: 0, probe, fleet }), /<td class="n">0%/);
      assert.match(render({ cpu: 12.5, probe, fleet }), new RegExp(`<td class="n">${probe === 'windows' ? 100 : 13}%`));
    });
  }
}
