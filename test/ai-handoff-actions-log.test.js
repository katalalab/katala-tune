'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

async function renderLog(rows) {
  const elements = new Map();
  const element = (s) => elements.get(s) || (elements.set(s, { innerHTML: '', addEventListener() {} }), elements.get(s));
  const context = {
    UI: {
      esc: (s) => String(s).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;'),
      icon: () => '', head: () => '', chip: (label) => `<i>${label}</i>`,
      table: ({ rows, row }) => rows.map(row).join(''), empty: () => '',
    },
    JobResults: { isFailing() {} },
    window: { tune: { onChecksUpdated() {}, onNavigate() {}, onProbeResult() {}, config: () => new Promise(() => {}), actionsLog: async () => rows } },
    document: { querySelector: element, querySelectorAll: () => [], body: { classList: { add() {}, remove() {}, toggle() {} } }, addEventListener() {} },
    navigator: { userAgent: '' }, location: { search: '' }, URLSearchParams,
    setInterval() {}, setTimeout() {}, clearTimeout() {},
  };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(require.resolve('../renderer/app.js'), 'utf8'), context);
  await vm.runInContext('renderActions()', context);
  return element('#page').innerHTML;
}

const record = (extra) => ({ id: '1', node_id: 'test-node', type: 'ai-handoff', label: 'Codex に渡す', state: 'ok', ok: true, undo: null, ...extra });

test('new and legacy handoff records describe a terminal request without claiming CLI startup', async () => {
  for (const output of [
    'Codex 用のターミナルの起動を要求した。CLI の起動は未確認（/tmp/test）',
    'Codex を新しいターミナルで開いた（/tmp/test）',
    'Claude Code を新しいターミナルで開いた（C:\\test\\<prompt>）',
  ]) {
    const row = record({ output });
    const html = await renderLog([row]);
    assert.match(html, /起動要求済み/);
    assert.match(html, /CLI の起動は未確認/);
    assert.doesNotMatch(html, /を新しいターミナルで開いた|<i>成功<\/i>/);
    assert.match(html, /(?:\/tmp\/test|&lt;prompt&gt;)/);
    assert.equal(row.output, output, 'stored audit record must not be changed');
  }
});

test('pending, failed and unrelated actions retain their original states and output', async () => {
  const html = await renderLog([
    record({ id: 'pending', state: 'incomplete', ok: false, output: '起動要求の結果は未確認' }),
    record({ id: 'failed', state: 'failed', ok: false, output: '取消（開いていない）' }),
    record({ id: 'other', type: 'other', output: 'Codex を新しいターミナルで開いた' }),
  ]);
  assert.match(html, /<i>未完了<\/i>/);
  assert.match(html, /<i>失敗<\/i>/);
  assert.match(html, /取消（開いていない）/);
  assert.match(html, /<i>成功<\/i>/);
  assert.match(html, /Codex を新しいターミナルで開いた/);
  assert.doesNotMatch(html, /起動要求済み/);
});
