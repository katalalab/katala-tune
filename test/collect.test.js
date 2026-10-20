'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { run, probeNode, SSH_OPTS, windowsSshCommand, macProbeArgs, macSshCommand } = require('../lib/collect');

test('SSH は既存の ControlPersist master を使わない', () => {
  assert.ok(SSH_OPTS.includes('-T'));
  assert.ok(SSH_OPTS.includes('ControlMaster=no'));
  assert.ok(SSH_OPTS.includes('ControlPath=none'));
});

test('子が終了した後も stdout を継承したプロセスで run が止まらない', { skip: process.platform === 'win32' && 'POSIX shell が必要' }, async () => {
  const started = Date.now();
  const result = await run('/bin/sh', ['-c', '(sleep 2) & printf ok'], { timeoutMs: 5000 });
  assert.deepEqual({ code: result.code, out: result.out }, { code: 0, out: 'ok' });
  assert.ok(Date.now() - started < 1500, `elapsed=${Date.now() - started}ms`);
});

// Isolate timer ordering from OS scheduling: a real shell may not start within
// 100 ms during the parallel suite. The surrounding tests retain real processes.
async function exitBeforeTimeout({ regress = false, crlf = false } = {}) {
  const { EventEmitter } = require('node:events');
  const fs = require('node:fs');
  const path = require('node:path');
  const vm = require('node:vm');
  const moduleFile = require.resolve('../lib/collect');
  const realRequire = require('node:module').createRequire(moduleFile);
  const child = new EventEmitter();
  for (const name of ['stdin', 'stdout', 'stderr']) {
    child[name] = Object.assign(new EventEmitter(), { end() {}, destroy() {} });
  }
  let killed = false, now = 0, id = 0;
  const timers = new Map();
  child.kill = () => { killed = true; };
  const advance = (ms) => {
    now += ms;
    for (const [key, timer] of [...timers]) {
      if (timer.at <= now && timers.delete(key)) timer.fn();
    }
  };
  const module = { exports: {} };
  let source = fs.readFileSync(moduleFile, 'utf8');
  if (crlf) source = source.replace(/\r?\n/g, '\r\n');
  source = source.replace(/\r\n/g, '\n');
  if (regress) source = source.replace('clearTimeout(timer);\n        closeTimer', 'closeTimer');
  vm.runInNewContext(source, {
    module, __dirname: path.dirname(moduleFile), process, Buffer, TextDecoder,
    require: (name) => name === 'node:child_process' ? { spawn: () => child } : realRequire(name),
    setTimeout: (fn, ms) => { const key = ++id; timers.set(key, { fn, at: now + ms }); return key; },
    clearTimeout: (key) => timers.delete(key),
  });
  const pending = module.exports.run('fixture', [], { timeoutMs: 100 });
  child.stdout.emit('data', Buffer.from('ok'));
  child.emit('exit', 0);
  advance(100);
  advance(400);
  const result = await pending;
  return { code: result.code, out: result.out, err: result.err, killed };
}

test('子の終了後の close 猶予を実行timeoutと誤認しない', async () => {
  assert.deepEqual(await exitBeforeTimeout(), { code: 0, out: 'ok', err: '', killed: false });
});

test('close 猶予の回帰試験は終了時の実行タイマー解除を外すと検知する', async () => {
  for (const crlf of [false, true]) {
    const broken = await exitBeforeTimeout({ regress: true, crlf });
    assert.equal(broken.killed, true);
    assert.match(broken.err, /timeout 100ms/);
  }
});

test('close 猶予中も10MiBの出力を保持する', { skip: process.platform === 'win32' && 'POSIX shell が必要' }, async () => {
  const result = await run('/bin/sh', ['-c', '(sleep 2) & dd if=/dev/zero bs=1048576 count=10 2>/dev/null'], { timeoutMs: 5000 });
  assert.equal(result.code, 0);
  assert.equal(Buffer.byteLength(result.out), 10 * 1024 * 1024);
});


test('benchmark:false では Windows に固定計算を送らない', () => {
  const enabled = windowsSshCommand();
  assert.match(enabled, /KATALA_TUNE_BENCH/);
  assert.match(enabled, /3000000/);
  const disabled = windowsSshCommand(false);
  assert.match(disabled, /probe\.ps1/);
  assert.doesNotMatch(disabled, /KATALA_TUNE_BENCH|3000000|command -v python/);
});

test('WSL 内部観測は明示 opt-in の機体だけに渡す', async () => {
  const calls = [];
  const localShell = {
    async powershellFile(_script, params) { calls.push(params); return { code: 0, out: '{"ok":true}\n', err: '' }; },
  };
  for (const node of [
    { id: 'default', os: 'windows', local: true, benchmark: false },
    { id: 'disabled', os: 'windows', local: true, benchmark: false, wsl: false },
    { id: 'enabled', os: 'windows', local: true, benchmark: false, network: false, wsl: true },
  ]) await probeNode(node, { localShell });
  assert.doesNotMatch(calls[0], /ObserveWsl/);
  assert.doesNotMatch(calls[1], /ObserveWsl/);
  assert.equal(calls[2], '-NoNetwork -ObserveWsl');
});

test('WSL 内部観測はリモート Windows にも明示 opt-in でだけ渡す', () => {
  const disabled = windowsSshCommand(false, true, false);
  const enabled = windowsSshCommand(false, false, true);
  assert.doesNotMatch(disabled, /ObserveWsl/);
  assert.match(enabled, /probe\.ps1\)" -NoNetwork -ObserveWsl;/);
});

test('benchmark:false をローカル・リモート Mac に同じく渡す', () => {
  assert.deepEqual(macProbeArgs(true), ['python3', '-']);
  assert.deepEqual(macProbeArgs(false), ['python3', '-', '--skip-benchmark']);
  assert.doesNotMatch(macSshCommand(true), /skip-benchmark/);
  assert.equal(macSshCommand(false).match(/--skip-benchmark/g).length, 2);
});
