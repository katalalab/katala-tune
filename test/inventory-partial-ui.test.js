'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const UI = require('../renderer/ui');

test('一部取得できなかった種類と前回の一覧が残ることを画面に示す', async () => {
  let html = '';
  const data = { nodes: [{ id: 'fixture', os: 'macos', count: 1, error: null, partial_sources: ['brew'] }], groups: [], events: [], sources: { brew: 'Homebrew' }, dogu: null, inventorying: false };
  const window = { tune: { inventory: async () => data } };
  const context = vm.createContext({ window, UI, ticket: () => 1, stale: () => false, setCrumbs: () => {}, page: s => { html = s; }, state: {}, $: () => null, $$: () => [], fmtTime: String, ago: String });
  vm.runInContext(fs.readFileSync(path.join(__dirname, '../renderer/tools.js'), 'utf8'), context);
  await window.KT_VIEWS[0].render();
  assert.match(html, /一部/);
  assert.match(html, /fixture/);
  assert.match(html, /Homebrew/);
  assert.match(html, /前回/);
  assert.doesNotMatch(html, /まだ棚卸ししていません/);
});
