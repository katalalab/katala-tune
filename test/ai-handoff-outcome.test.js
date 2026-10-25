'use strict';
// Run the actual renderer handler; only the native bridge and DOM boundary are synthetic.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

async function outcome(result) {
  const toast = { textContent: '', classList: { add() {}, remove() {} } };
  const calls = [];
  const element = { addEventListener() {} };
  const context = {
    UI: { esc: String, icon() {} }, JobResults: { isFailing() {} },
    window: { tune: {
      onChecksUpdated() {}, onNavigate() {}, onProbeResult() {},
      config: () => new Promise(() => {}),
      aiHandoff: async (...args) => { calls.push(args); return result; },
    } },
    document: {
      querySelector: (s) => s === '#toast' ? toast : element,
      body: { classList: { add() {}, remove() {}, toggle() {} } },
      addEventListener() {},
    },
    navigator: { userAgent: '' }, location: { search: '' }, URLSearchParams,
    setInterval() {}, setTimeout() {}, clearTimeout() {},
  };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(require.resolve('../renderer/app.js'), 'utf8'), context);
  await vm.runInContext('handoff({id:"a"}, {id:"mem-pressure"}, "codex")', context);
  assert.deepEqual(calls, [['a', 'mem-pressure', 'codex']], 'an uncertain result must not retry the native call');
  return toast.textContent;
}

test('an incomplete audit after a launch request warns against retry and never claims that nothing opened', async () => {
  const message = await outcome({ ok: false, launch_requested: true, audit_incomplete: true, error: '結果の実行記録を書けません' });
  assert.match(message, /記録/);
  assert.match(message, /再実行/);
  assert.doesNotMatch(message, /開きませんでした|記録に残しました|undefined/);
});

test('cancellation shows audit write failure as well as the cancellation', async () => {
  const message = await outcome({ ok: false, cancelled: true, audit_error: '取消の記録を書けません' });
  assert.match(message, /取り消しました/);
  assert.match(message, /記録/);
});

test('a recorded success retains the normal handoff notification', async () => {
  assert.match(await outcome({ ok: true }), /Codex.*ターミナル.*記録に残しました/);
});
