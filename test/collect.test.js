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
  assert.match(enabled, /windows-probe-v1/);
  assert.match(enabled, /--benchmark 1/);
  const disabled = windowsSshCommand(false);
  assert.match(disabled, /--benchmark 0/);
  assert.doesNotMatch(disabled, /KATALA_TUNE_BENCH|3000000|command -v python|probe\.ps1/);
});

test('WSL 内部観測は明示 opt-in の機体だけに渡す', async () => {
  const calls = [];
  const action_helper = { schema: 'katala-tune/action-helper/v2', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 };
  for (const node of [
    { id: 'default', os: 'windows', local: true, benchmark: false, action_helper },
    { id: 'disabled', os: 'windows', local: true, benchmark: false, wsl: false, action_helper },
    { id: 'enabled', os: 'windows', local: true, benchmark: false, network: false, wsl: true, action_helper },
  ]) await probeNode(node, { probeExchange: async (...args) => { calls.push(args); return { code: 0, out: '{"ok":true}\n', err: '' }; } });
  assert.equal(calls[0].at(-1), false);
  assert.equal(calls[1].at(-1), false);
  assert.deepEqual(calls[2].slice(-3), [false, false, true]);
});

test('WSL 内部観測はリモート Windows にも明示 opt-in でだけ渡す', () => {
  const disabled = windowsSshCommand(false, true, false);
  const enabled = windowsSshCommand(false, false, true);
  assert.match(disabled, /--observe-wsl 0/);
  assert.match(enabled, /windows-probe-v1 .*--benchmark 0 --network 0 --observe-wsl 1 --budget-ms 120000$/);
});

test('benchmark:false をローカル・リモート Mac に同じく渡す', () => {
  assert.deepEqual(macProbeArgs(true), ['python3', '-']);
  assert.deepEqual(macProbeArgs(false), ['python3', '-', '--skip-benchmark']);
  assert.doesNotMatch(macSshCommand(true), /skip-benchmark/);
  assert.equal(macSshCommand(false).match(/--skip-benchmark/g).length, 2);
});

test('Windows probe は v2 helper と固定 named transport だけを使い、stdin は空である', async () => {
  const hash = 'a'.repeat(64);
  const helper = { schema: 'katala-tune/action-helper/v2', source_sha: 'b'.repeat(40), tune_sha256: hash, manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 };
  for (const local of [false, true]) {
    let genericCalls = 0;
    let call;
    const result = await probeNode(
      { id: `w-${local}`, alias: 'fixture', os: 'windows', local, benchmark: false, network: false, wsl: true, action_helper: helper },
      {
        run: async () => { genericCalls++; return { code: 0, out: '{"wrong":true}\n', err: '' }; },
        probeExchange: async (...args) => {
          call = args;
          return { code: 0, out: '{"probe":true}\n', err: '' };
        },
      },
    );
    assert.equal(result.ok, true);
    assert.equal(genericCalls, 0);
    assert.deepEqual(call.slice(1), [hash, false, false, true]);
  }
});

test('Windows probe は v1・欠落・余分な helper を transport 前に拒否する', async () => {
  const valid = { schema: 'katala-tune/action-helper/v2', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 };
  for (const action_helper of [undefined, { ...valid, schema: 'katala-tune/action-helper/v1' }, { ...valid, extra: true }]) {
    let calls = 0;
    const result = await probeNode(
      { id: 'w', alias: 'fixture', os: 'windows', local: false, benchmark: false, action_helper },
      { run: async () => { calls++; return { code: 0, out: '{"wrong":true}\n', err: '' }; }, probeExchange: async () => { calls++; return { code: 0, out: '{"wrong":true}\n', err: '' }; } },
    );
    assert.equal(result.ok, false);
    assert.equal(calls, 0);
  }
});

test('local Windows probe uses isolated QA helpers instead of the live HOME helper', async () => {
  const { EventEmitter } = require('node:events');
  const path = require('node:path');
  const { probeExchange } = require('../lib/collect');
  const previous = process.env.KATALA_TUNE_DATA_DIR;
  const isolated = path.join(require('node:os').tmpdir(), 'katala-qa-fixture');
  process.env.KATALA_TUNE_DATA_DIR = isolated;
  let program;
  try {
    const result = await probeExchange({ local: true }, 'a'.repeat(64), false, false, false, {
      spawnChild(command) {
        program = command;
        const child = new EventEmitter();
        child.stdin = Object.assign(new EventEmitter(), { end() { process.nextTick(() => child.emit('close', 0)); } });
        child.stdout = new EventEmitter(); child.stderr = new EventEmitter();
        return child;
      },
    });
    assert.equal(result.code, 0);
    assert.equal(program, path.join(isolated, 'bin', 'tune.exe'));
  } finally {
    if (previous === undefined) delete process.env.KATALA_TUNE_DATA_DIR;
    else process.env.KATALA_TUNE_DATA_DIR = previous;
  }
});
