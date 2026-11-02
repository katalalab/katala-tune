'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createRequire } = require('node:module');
const cases = require('./fixtures/hygiene-process-result.json');

function fakeRunner(strictCompletion) {
  const file = require.resolve('../lib/collect');
  const realRequire = createRequire(file);
  const child = new EventEmitter();
  for (const stream of ['stdin', 'stdout', 'stderr']) {
    child[stream] = Object.assign(new EventEmitter(), { end() {}, destroy() {} });
    child[stream].on('error', () => {});
  }
  child.kill = () => {};
  let now = 0, id = 0;
  const timers = new Map();
  const advance = (ms) => {
    now += ms;
    for (const [key, timer] of [...timers]) if (timer.at <= now && timers.delete(key)) timer.fn();
  };
  const module = { exports: {} };
  vm.runInNewContext(fs.readFileSync(file, 'utf8'), {
    module, __dirname: path.dirname(file), process, Buffer, TextDecoder,
    require: (name) => name === 'node:child_process' ? { spawn: () => child } : realRequire(name),
    setTimeout: (fn, ms) => { const key = ++id; timers.set(key, { fn, at: now + ms }); return key; },
    clearTimeout: (key) => timers.delete(key),
  });
  return { child, advance, pending: module.exports.run('fixture', [], { input: 'fixture', timeoutMs: 100, strictCompletion }) };
}

test('厳格完了は終了・送信・両 EOF の合成状態を確認し、既定動作は保つ', async () => {
  for (const fixture of cases.completions) {
    for (const strict of [false, true]) {
      const { child, advance, pending } = fakeRunner(strict);
      child.emit('spawn');
      if (fixture.timed_out) advance(100);
      else child.emit('exit', fixture.code);
      if (fixture.stdin_complete) child.stdin.emit('finish');
      if (fixture.stdout_eof) child.stdout.emit('end');
      if (fixture.stderr_eof) child.stderr.emit('end');
      child.emit('close', fixture.code);
      const result = await pending;
      assert.equal(result.code, strict ? fixture.strict : fixture.legacy, fixture.name);
      if (strict) assert.equal(result.completion_failure, fixture.failure, fixture.name);
      else assert.deepEqual(Object.keys(result).sort(), ['code', 'err', 'out']);
    }
  }
});

test('厳格完了は stdin・stdout・stderr のエラーを完了後も忘れない', async () => {
  for (const name of ['stdin', 'stdout', 'stderr']) {
    const { child, pending } = fakeRunner(true);
    child.emit('spawn');
    child[name].emit('error', new Error('fixture'));
    child.stdin.emit('finish');
    child.stdout.emit('end');
    child.stderr.emit('end');
    child.emit('exit', 0);
    child.emit('close', 0);
    const result = await pending;
    assert.equal(result.code, null, name);
    assert.equal(result.completion_failure, 'incomplete', name);
  }
});

test('close 猶予切れで EOF が欠けた場合は厳格時だけ未完了にする', async () => {
  for (const strict of [false, true]) {
    const { child, advance, pending } = fakeRunner(strict);
    child.emit('spawn');
    child.stdin.emit('finish');
    child.stdout.emit('end');
    child.emit('exit', 0);
    advance(500);
    const result = await pending;
    assert.equal(result.code, strict ? null : 0);
    if (strict) assert.equal(result.completion_failure, 'incomplete');
  }
});

test('厳格完了は実行体不存在とその他の起動不能を区別する', async () => {
  for (const [code, expected] of [['ENOENT', 'spawn_missing'], ['EACCES', 'spawn_failed'], ['ENOMEM', 'spawn_failed']]) {
    const { child, pending } = fakeRunner(true);
    child.emit('error', Object.assign(new Error('fixture'), { code }));
    const result = await pending;
    assert.equal(result.code, null);
    assert.equal(result.completion_failure, expected, code);
  }
  const { child, pending } = fakeRunner(true);
  child.emit('spawn');
  child.emit('error', Object.assign(new Error('fixture'), { code: 'ENOENT' }));
  assert.equal((await pending).completion_failure, 'incomplete');
});

test('strict_exit_before_eof_is_proven', async () => {
  const { child, pending } = fakeRunner(true);
  child.emit('exit', 0);
  child.stdin.emit('finish');
  child.stderr.emit('end');
  child.stdout.emit('end');
  child.emit('close', 0);
  const result = await pending;
  assert.equal(result.code, 0);
  assert.equal(result.completion_failure, null);
});

test('strict_late_success_after_timeout_is_rejected', async () => {
  const { child, pending, advance } = fakeRunner(true);
  advance(100);
  child.stdin.emit('finish');
  child.stdout.emit('end');
  child.stderr.emit('end');
  child.emit('exit', 0);
  child.emit('close', 0);
  advance(501);
  const result = await pending;
  assert.equal(result.completion_failure, 'timeout');
});

test('default_result_shape_remains_unchanged', async () => {
  const { child, pending } = fakeRunner(false);
  child.stdin.emit('finish');
  child.stdout.emit('end');
  child.stderr.emit('end');
  child.emit('exit', 0);
  child.emit('close', 0);
  const result = await pending;
  assert.equal(result.code, 0);
  assert.equal(Object.prototype.hasOwnProperty.call(result, 'completion_failure'), false);
});