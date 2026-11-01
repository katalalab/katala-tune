'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createRequire } = require('node:module');
const cases = require('./fixtures/hygiene-process-result.json');

function fakeAudit(results) {
  const file = require.resolve('../lib/hygiene');
  const realRequire = createRequire(file);
  const collect = realRequire('./collect');
  const calls = [];
  const module = { exports: {} };
  const fakeRun = async (cmd, args, options) => {
    calls.push({ cmd, args, options });
    return results[Math.min(calls.length - 1, results.length - 1)];
  };
  vm.runInNewContext(fs.readFileSync(file, 'utf8'), {
    module, __dirname: path.dirname(file), process, Buffer, TextDecoder,
    require: (name) => name === './collect' ? { ...collect, run: fakeRun } : realRequire(name),
  });
  return { audit: module.exports.auditNode, calls };
}

test('監査は正常終了と正しい JSON の両方を要求し、部分出力を成功にしない', async () => {
  for (const fixture of cases.results) {
    for (const err of ['fixture-private-stderr', 'Python was not found @@KATALA_TUNE_NO_PYTHON@@']) {
      const result = { code: fixture.code, out: cases.outputs[fixture.output], err, completion_failure: fixture.failure };
      const { audit, calls } = fakeAudit([result]);
      const actual = await audit({ id: 'node-a', alias: 'node-a', os: 'macos', local: true }, {});
      assert.equal(calls.length, 1, fixture.name);
      assert.equal(calls[0].options.strictCompletion, true, fixture.name);
      assert.equal(actual.ok, fixture.ok, fixture.name);
      assert.equal(actual.error ?? null, fixture.error, fixture.name);
      if (!fixture.ok) assert.equal(Object.hasOwn(actual, 'repos'), false, fixture.name);
      assert.ok(!JSON.stringify(actual).includes(err), fixture.name);
    }
  }
});

test('未完了は別 interpreter で上書きせず、起動不能だけ次候補を試す', async () => {
  const success = { code: 0, out: cases.outputs.valid, err: '' };
  for (const failure of ['timeout', 'incomplete']) {
    const { audit, calls } = fakeAudit([{ code: null, out: '', err: '', completion_failure: failure }, success]);
    const result = await audit({ id: 'node-a', alias: 'node-a', os: 'windows', local: true }, {});
    assert.equal(result.ok, false, failure);
    assert.equal(result.error, failure, failure);
    assert.equal(calls.length, 1, failure);
  }
  const missing = { code: null, out: '', err: '', completion_failure: 'spawn_missing' };
  const { audit, calls } = fakeAudit([missing, success]);
  assert.equal((await audit({ id: 'node-a', alias: 'node-a', os: 'windows', local: true }, {})).ok, true);
  assert.equal(calls.length, 2);
  const absent = fakeAudit([missing]);
  assert.equal((await absent.audit({ id: 'node-a', alias: 'node-a', os: 'windows', local: true }, {})).error, 'no_python');
  assert.equal(absent.calls.length, 3);
  const denied = fakeAudit([{ code: null, out: '', err: '', completion_failure: 'spawn_failed' }, success]);
  assert.equal((await denied.audit({ id: 'node-a', alias: 'node-a', os: 'windows', local: true }, {})).error, 'incomplete');
  assert.equal(denied.calls.length, 1);
  const alias = fakeAudit([{ code: 9009, out: '', err: 'Python was not found' }, success]);
  assert.equal((await alias.audit({ id: 'node-a', alias: 'node-a', os: 'windows', local: true }, {})).error, 'probe_failed');
  assert.equal(alias.calls.length, 1);
});
