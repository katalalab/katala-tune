'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { run, SSH_OPTS, windowsSshCommand, macProbeArgs, macSshCommand } = require('../lib/collect');

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

test('子の終了後の close 猶予を実行timeoutと誤認しない', { skip: process.platform === 'win32' && 'POSIX shell が必要' }, async () => {
  const result = await run('/bin/sh', ['-c', '(sleep 2) & printf ok'], { timeoutMs: 100 });
  assert.deepEqual({ code: result.code, out: result.out, err: result.err }, { code: 0, out: 'ok', err: '' });
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

test('benchmark:false をローカル・リモート Mac に同じく渡す', () => {
  assert.deepEqual(macProbeArgs(true), ['python3', '-']);
  assert.deepEqual(macProbeArgs(false), ['python3', '-', '--skip-benchmark']);
  assert.doesNotMatch(macSshCommand(true), /skip-benchmark/);
  assert.equal(macSshCommand(false).match(/--skip-benchmark/g).length, 2);
});
