'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const { PassThrough, Writable } = require('node:stream');
const { probeNode, probeExchange } = require('../lib/collect');
const helper = { schema: 'katala-tune/action-helper/v2', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 };
const node = { id: 'probe-fixture', alias: 'mock-fixture', os: 'windows', action_helper: helper };

for (const [kind, stderr, code = 255] of [
  ['auth', 'fixture-private: Permission denied (publickey).'],
  ['unreachable', 'ssh: Could not resolve hostname fixture-private'],
  ['error', 'helper rejected fixture-private'],
  ['error', 'helper permission denied for fixture-private', 9],
]) test(`production Windows probe classifies ${kind} (exit ${code}) without exposing raw stderr`, async () => {
  const exchange = (...args) => probeExchange(...args, { spawnChild() {
    const child = new EventEmitter();
    child.stdout = new PassThrough(); child.stderr = new PassThrough();
    child.kill = () => true;
    child.stdin = new Writable({ write(_chunk, _encoding, callback) { callback(); }, final(callback) {
      callback();
      process.nextTick(() => { child.stdout.end(); child.stderr.end(stderr); child.emit('exit', code); child.emit('close', code); });
    } });
    return child;
  } });
  const r = await probeNode(node, { probeExchange: exchange });
  assert.equal(r.ok, false);
  assert.equal(r.reason, kind);
  assert.ok(r.reason_text);
  assert.doesNotMatch(r.error, /fixture-private/);
});

test('production Windows probe reports its deadline as timeout and discards partial output', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const exchange = (...args) => probeExchange(...args, { spawnChild() {
    const child = new EventEmitter();
    child.stdout = new PassThrough(); child.stderr = new PassThrough();
    child.kill = () => true;
    child.stdin = new Writable({ write(_chunk, _encoding, callback) { callback(); } });
    return child;
  } });
  const pending = probeNode(node, { probeExchange: exchange });
  t.mock.timers.tick(135000);
  const r = await pending;
  assert.equal(r.ok, false);
  assert.equal(r.reason, 'timeout');
  assert.equal(r.data, undefined);
});
