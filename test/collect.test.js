'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { run, SSH_OPTS } = require('../lib/collect');

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
