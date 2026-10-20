'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const code = fs.readFileSync(require.resolve('../renderer/live.js'), 'utf8');

test('ネイティブ非表示の停止後は visibilitychange 無しで heartbeat から復帰する', async () => {
  let receive, heartbeat, starts = 0, shown = true;
  const button = { classList: { toggle() {} }, setAttribute() {} };
  const context = {
    module: { exports: {} },
    document: { visibilityState: 'visible', querySelectorAll: () => [button], getElementById: () => null, addEventListener() {} },
    window: { tune: {
      async liveStart() { starts++; return shown ? { started: ['a'] } : { hidden: true }; },
      async liveStop() {}, onLive(fn) { receive = fn; },
    } },
    setInterval(fn) { heartbeat = fn; return 1; }, clearInterval() {}, requestAnimationFrame() { return 1; },
  };
  vm.runInNewContext(code, context);
  const live = context.module.exports;
  live.mount('node', ['a'], []);
  button.onclick();
  await new Promise(setImmediate);
  assert.equal(starts, 1);
  shown = false;
  heartbeat();
  receive({ nodes: { a: { state: 'stopped', reason: 'hidden' } } });
  await new Promise(setImmediate);
  assert.notEqual(live.store.a.held, true);
  shown = true;
  heartbeat();
  await new Promise(setImmediate);
  assert.equal(starts, 3);
  receive({ nodes: { a: { state: 'stopped', reason: 'idle' } } });
  assert.equal(live.store.a.held, true, '通常の自動停止は手動再開を維持');
  heartbeat();
  await new Promise(setImmediate);
  assert.equal(starts, 3);
});
