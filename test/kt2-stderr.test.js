'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const { PassThrough } = require('node:stream');
const { createHash } = require('node:crypto');
const { actionExchange } = require('../lib/collect');

const node = { id: 'fixture', alias: 'mock-target', os: 'windows', local: false };
const helper = 'a'.repeat(64);
const nonce = 'b'.repeat(64);
const body = Buffer.from('owned fixture body');
const hash = createHash('sha256').update(body).digest('hex');

// Only the OS child boundary is replaced. Real streams drive the production
// READY/REQUEST/RESULT parser; no SSH, PowerShell or GPU process is launched.
function exchange({ beforeReady, afterRequest, closeStderr = true } = {}) {
  const child = new EventEmitter();
  child.stdin = new PassThrough();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  let request = Buffer.alloc(0);
  let closed = false;
  const close = () => { if (!closed) { closed = true; child.emit('close', 0); } };
  child.kill = () => { setImmediate(close); return true; };
  child.stdin.on('data', (chunk) => { request = Buffer.concat([request, chunk]); });
  child.stdin.on('finish', () => {
    afterRequest?.(child);
    if (closeStderr && !child.stderr.destroyed) child.stderr.end();
    child.stdout.end(`KT2 RESULT ${nonce} ${hash} 0 2 0\nok`);
    setImmediate(close);
  });
  const pending = actionExchange(node, helper, body, {
    timeoutMs: 100,
    spawnChild: () => {
      queueMicrotask(() => {
        const ready = () => child.stdout.write(`KT2 READY ${nonce}\n`);
        if (beforeReady) beforeReady(child, ready);
        else ready();
      });
      return child;
    },
  });
  return { pending, request: () => request };
}

for (const [name, disturb] of [
  ['I/O error', (child) => child.stderr.destroy(new Error('synthetic stderr read failure'))],
  ['close without EOF', (child) => child.stderr.destroy()],
]) {
  test(`KT2 stderr ${name} before READY refuses without sending REQUEST`, async () => {
    const fixture = exchange({ beforeReady(child, ready) { child.stderr.once('close', ready); disturb(child); } });
    const result = await fixture.pending;
    assert.equal(result.code, 9);
    assert.equal(result.out, '');
    assert.equal(fixture.request().length, 0);
  });

  test(`KT2 stderr ${name} after REQUEST withholds a valid success payload`, async () => {
    const fixture = exchange({ afterRequest: disturb });
    const result = await fixture.pending;
    assert.equal(result.code, 7);
    assert.equal(result.out, '');
    assert.equal(fixture.request().toString(), `KT2 REQUEST ${nonce} ${body.length} ${hash}\n${body}`);
  });
}

test('KT2 child close cannot stand in for stderr EOF', async () => {
  const fixture = exchange({ closeStderr: false });
  const result = await fixture.pending;
  assert.equal(result.code, 7);
  assert.equal(result.out, '');
});

test('KT2 real stderr EOF before READY still permits a verified result', async () => {
  const fixture = exchange({ beforeReady(child, ready) { child.stderr.once('end', ready); child.stderr.end(); } });
  const result = await fixture.pending;
  assert.equal(result.code, 0);
  assert.equal(result.out, 'ok');
});

for (const [size, code, out] of [[0, 0, 'ok'], [65536, 0, 'ok'], [65537, 7, '']]) {
  test(`KT2 stderr ${size} bytes followed by real EOF returns ${code}`, async () => {
    const fixture = exchange({ afterRequest(child) { child.stderr.write(Buffer.alloc(size, 'z')); } });
    const result = await fixture.pending;
    assert.equal(result.code, code);
    assert.equal(result.out, out);
  });
}

test('KT2 stderr overflow before READY refuses without sending REQUEST', async () => {
  const fixture = exchange({ beforeReady(child, ready) { child.stderr.end(Buffer.alloc(65537, 'z')); ready(); } });
  assert.equal((await fixture.pending).code, 9);
  assert.equal(fixture.request().length, 0);
});
