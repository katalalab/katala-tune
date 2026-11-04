'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const collect = require('../lib/collect');
const realRun = collect.run;
const realPowerShellFile = collect.localShell.powershellFile;
let result;
let strict;
collect.run = async (_cmd, _args, opts) => { strict = opts.requireComplete; return result; };
collect.localShell.powershellFile = async (_script, _params, _timeout, requireComplete) => { strict = requireComplete; return result; };
const { inventoryNode } = require('../lib/inventory');

test('failed or unconfirmed process cannot authorize inventory replacement', async () => {
  for (const code of [1, null]) {
    result = { code, out: '{"items":[]}', err: 'synthetic failure' };
    const got = await inventoryNode({ id: 'fixture', os: 'macos', local: true });
    assert.equal(got.ok, false);
    assert.equal(got.items, undefined);
  }
  result = { code: 0, out: '{"items":[{"source":"app","name":"fixture"}],"failed_sources":[]}', err: '' };
  const got = await inventoryNode({ id: 'fixture', os: 'macos', local: true });
  assert.equal(got.ok, true);
  assert.equal(got.items.length, 1);
});

test('incomplete inventory payload cannot authorize inventory replacement', async () => {
  for (const payload of [
    { items: [] },
    { items: [], failed_sources: null },
    { items: [], failed_sources: [null] },
    { items: [{ source: 'app' }], failed_sources: [] },
    { items: [{ source: 'app', name: '' }], failed_sources: [] },
  ]) {
    result = { code: 0, out: JSON.stringify(payload), err: '' };
    const got = await inventoryNode({ id: 'fixture', os: 'macos', local: true });
    assert.equal(got.ok, false, JSON.stringify(payload));
    assert.equal(got.items, undefined);
  }
});

test('all inventory transports require completed IO', async () => {
  result = { code: 0, out: '{"items":[],"failed_sources":[]}', err: '' };
  for (const node of [
    { id: 'fixture', os: 'macos', local: true }, { id: 'fixture', os: 'macos', local: false, alias: 'fixture' },
    { id: 'fixture', os: 'windows', local: true }, { id: 'fixture', os: 'windows', local: false, alias: 'fixture' },
  ]) {
    strict = false;
    assert.equal((await inventoryNode(node)).ok, true);
    assert.equal(strict, true);
  }
});

test('strict collection rejects output when an inherited pipe never reaches EOF', async () => {
  const { EventEmitter } = require('node:events');
  const fs = require('node:fs');
  const path = require('node:path');
  const vm = require('node:vm');
  const moduleFile = require.resolve('../lib/collect');
  const realRequire = require('node:module').createRequire(moduleFile);
  const child = new EventEmitter();
  for (const name of ['stdin', 'stdout', 'stderr']) child[name] = Object.assign(new EventEmitter(), { end() {}, destroy() {} });
  child.kill = () => {};
  const timers = new Map(); let id = 0;
  const module = { exports: {} };
  vm.runInNewContext(fs.readFileSync(moduleFile, 'utf8'), {
    module, __dirname: path.dirname(moduleFile), process, Buffer, TextDecoder,
    require: name => name === 'node:child_process' ? { spawn: () => child } : realRequire(name),
    setTimeout: (fn, ms) => { const key = ++id; timers.set(key, { fn, ms }); return key; },
    clearTimeout: key => timers.delete(key),
  });
  const pending = module.exports.run('fixture', [], { timeoutMs: 5000, requireComplete: true });
  child.stdout.emit('data', Buffer.from('{"items":[]}\n'));
  child.emit('exit', 0);
  for (const timer of [...timers.values()]) if (timer.ms === 500) timer.fn();
  const result = await pending;
  assert.equal(result.out.trim(), '{"items":[]}');
  assert.equal(result.code, null);
});

test('strict collection preserves verified success and nonzero status', async () => {
  for (const exit of [0, 7]) {
    const result = await realRun(process.execPath, ['-e', `process.stdin.resume(); process.stdin.on('end', () => { process.stdout.write('{"items":[]}\\n'); process.exitCode = ${exit}; });`], { input: 'fixture', timeoutMs: 5000, requireComplete: true });
    assert.equal(result.code, exit);
    assert.equal(result.out.trim(), '{"items":[]}');
  }
});

test('strict local Windows transport accepts a synthetic PowerShell response', { skip: process.platform !== 'win32' }, async () => {
  const script = Buffer.from('\ufeffWrite-Output \'{"items":[]}\'\r\n', 'utf8');
  const got = await realPowerShellFile(script, '', 5000, true);
  assert.equal(got.code, 0);
  assert.equal(got.out.trim(), '{"items":[]}');
});
