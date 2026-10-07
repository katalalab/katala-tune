'use strict';
// 画面（renderer/agents.js・tools.js）の IIFE を vm で読み、中の関数を取り出して確かめる
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
function load(file, exports, extra = {}) {
  const window = { tune: {} };
  const context = vm.createContext({ window, UI: { ICON: {}, esc: String }, ...extra });
  const source = fs.readFileSync(path.join(__dirname, '../renderer', file), 'utf8');
  vm.runInContext(source.replace(/\}\)\(\);\s*$/, `window.review = { ${exports} };})();`), context);
  return window.review;
}
test('AI の一覧を読めず囲みに置き換わったとき、無い検索・種類・ページ送りにハンドラを付けない', () => {
  const missing = new Set(['#aiQ', '#aiKind', '#aiPrev', '#aiNext']);
  const controls = new Map();
  const select = id => missing.has(id) ? null : (controls.get(id) || (controls.set(id, {}), controls.get(id)));
  const { bind } = load('agents.js', 'bind', { $: select, $$: () => [], bindSearch: id => assert.ok(select(id)) });
  assert.doesNotThrow(() => bind());
  assert.equal(typeof controls.get('#aiDays').onchange, 'function');
});
test('AI の一覧を読めたときは、ページ送りのハンドラを付ける', () => {
  const controls = new Map();
  const { bind } = load('agents.js', 'bind', { $: id => controls.get(id) || (controls.set(id, {}), controls.get(id)), $$: () => [], bindSearch: () => {} });
  bind();
  for (const id of ['#aiPrev', '#aiNext']) assert.equal(typeof controls.get(id).onclick, 'function');
});
test('道具の黄色の強調と多数決は、版の先頭の数字の並びを機体ごとに1回だけ数えて比べる', () => {
  const { cellOf } = load('tools.js', 'cellOf');
  const group = { drift: true, nodes: {
    a: { versions: ['2.51.0'], sources: [] },
    b: { versions: ['2.51.0.windows.1'], sources: [] },
    c: { versions: ['2.50.1'], sources: [] },
  } };
  assert.equal(cellOf({}, group, 'a').diff, false);
  assert.equal(cellOf({}, group, 'b').diff, false);
  assert.equal(cellOf({}, group, 'c').diff, true);
  const duplicates = { drift: true, nodes: {
    a: { versions: ['1.0', '1.0.windows.1', '1.0,build'], sources: [] },
    b: { versions: ['2.0'], sources: [] }, c: { versions: ['2.0'], sources: [] },
  } };
  assert.equal(cellOf({}, duplicates, 'a').diff, true);
  assert.equal(cellOf({}, duplicates, 'b').diff, false);
});
