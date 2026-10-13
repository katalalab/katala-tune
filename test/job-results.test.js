'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

test('ジョブ結果の共通判定は Node と画面で同じで、最新202を根拠付きで示す', () => {
  const results = require('../renderer/job-results');
  const task = { kind: 'schtask', id: '\\nefarius_HidHide_Updater', name: 'nefarius_HidHide_Updater', state: 'ready', last_result: 202 };
  const context = { task };
  vm.runInNewContext(fs.readFileSync(require.resolve('../renderer/job-results'), 'utf8') + '\nglobalThis.answer = [JobResults.isFailing(task), JobResults.label(task)];', context);
  assert.deepEqual(Array.from(context.answer), [results.isFailing(task), results.label(task)]);
  assert.equal(results.label(task), '最新（202）');
  assert.equal(results.label({ ...task, last_result: 201 }), '失敗 201');
});
