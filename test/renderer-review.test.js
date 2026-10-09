'use strict';
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
test('AI一覧読取エラーでも存在しない検索・種類・pagerを操作しない', () => {
  const missing = new Set(['#aiQ', '#aiKind', '#aiPrev', '#aiNext']);
  const controls = new Map();
  const select = id => missing.has(id) ? null : (controls.get(id) || (controls.set(id, {}), controls.get(id)));
  const { bind } = load('agents.js', 'bind', { $: select, $$: () => [], bindSearch: id => assert.ok(select(id)) });
  assert.doesNotThrow(() => bind());
  assert.equal(typeof controls.get('#aiDays').onchange, 'function');
});
test('AI一覧成功時はpager操作を登録する', () => {
  const controls = new Map();
  const { bind } = load('agents.js', 'bind', { $: id => controls.get(id) || (controls.set(id, {}), controls.get(id)), $$: () => [], bindSearch: () => {} });
  bind();
  for (const id of ['#aiPrev', '#aiNext']) assert.equal(typeof controls.get(id).onclick, 'function');
});
test('道具の黄色表示も版の数字部分を機体単位で比較する', () => {
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

test('セキュリティの学習中表示も選択した機体だけに絞る', () => {
  const source = fs.readFileSync(path.join(__dirname, '../renderer/security.js'), 'utf8');
  assert.match(source, /const learning = pick\(d\.nodes\)\.filter\(\(n\) => n\.netsec\?\.peers\?\.learning\)/);
});

test('AI の「この数字はどこから」は、画面で選んでいるツールで絞って問い合わせる', async () => {
  const calls = [];
  const window = { tune: { aiTrace: async (f) => { calls.push(f); return { error: 'stub' }; } } };
  const context = vm.createContext({ window, UI: { ICON: {}, esc: String, section: () => '', chip: () => '' } });
  const source = fs.readFileSync(path.join(__dirname, '../renderer/agents.js'), 'utf8');
  vm.runInContext(source.replace(/\}\)\(\);\s*$/, 'window.review = { ai, traceHtml };})();'), context);
  const { ai, traceHtml } = window.review;
  ai.tool = 'codex';
  ai.trace = { node_id: 'n1', day: 123 };
  await traceHtml();
  assert.deepEqual(JSON.parse(JSON.stringify(calls[0])), { node_id: 'n1', day: 123, tool: 'codex' });
  // ファイルの区間は、1 つのファイルの話なのでツールで絞らない
  ai.trace = { node_id: 'n1', file: 'claude:p/a.jsonl' };
  await traceHtml();
  assert.deepEqual(JSON.parse(JSON.stringify(calls[1])), { node_id: 'n1', file: 'claude:p/a.jsonl' });
});
