'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { validateSnapshot, runBounded } = require('../scripts/check-hybrid-canary');
const now = 1_800_000_000_000;
const observed = value => ({ value, status: 'observed', reason: null });
const unknown = reason => ({ value: null, status: 'unknown', reason });
function fixture() {
  return { schemaVersion: 1, kind: 'hybrid-canary', observedAtUnixMs: now, cpuWindowMs: 250,
    cpuPercent: observed(0), memory: observed({ usedBytes: 1, totalBytes: 2 }),
    gpus: unknown('library_unavailable'), hostTemperatureC: unknown('not_collected'),
    sources: { host: 'sysinfo-0.37.2', gpu: 'nvml-wrapper-0.11.0' } };
}
test('hybrid host data remains valid without NVML', () => assert.doesNotThrow(() => validateSnapshot(fixture(), now)));
test('hybrid allows partial GPU readings', () => {
  const value = fixture();
  value.gpus = observed([{ index: 0, utilizationPercent: unknown('permission_denied'),
    memory: observed({ usedBytes: 2, totalBytes: 4 }), temperatureC: observed(45) }]);
  assert.doesNotThrow(() => validateSnapshot(value, now));
});
test('hybrid rejects stale, future, unprimed, malformed and identifying data', () => {
  for (const mutate of [
    x => { x.observedAtUnixMs = now - 10001; },
    x => { x.observedAtUnixMs = now + 1001; },
    x => { x.cpuWindowMs = 0; },
    x => { x.cpuPercent.value = NaN; },
    x => { x.memory.value.usedBytes = 3; },
    x => { x.memory.value.totalBytes = 0; },
    x => { x.gpus.value = []; },
    x => { x.gpus = observed([]); },
    x => { x.gpus.reason = 'raw local path'; },
    x => { x.hostname = 'private'; },
    x => { x.memory.value.username = 'private'; },
    x => { x.hostTemperatureC = observed(0); },
    x => { x.cpuPercent.reason = 'collection_error'; },
    x => { x.sources.host = 'unversioned'; },
  ]) {
    const value = fixture(); mutate(value);
    assert.throws(() => validateSnapshot(value, now));
  }
});

test('hybrid timeout kills a child even when it ignores SIGTERM', () => {
  const started = performance.now();
  assert.throws(() => runBounded(process.execPath,
    ['-e', "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)"], 200),
    error => error.code === 'ETIMEDOUT');
  assert.ok(performance.now() - started < 5000, 'ignored SIGTERM must not hang the harness');
});
test('hybrid bounds output and never forwards raw stderr', () => {
  const { spawnSync } = require('node:child_process');
  const child = spawnSync(process.execPath, ['-e', `
    const {runBounded} = require('./scripts/check-hybrid-canary');
    try { runBounded(process.execPath, ['-e', 'console.error("RAW_PRIVATE_SENTINEL"); process.exit(1)']); }
    catch { process.stdout.write('sanitized'); }
  `], { encoding: 'utf8', timeout: 5000, killSignal: 'SIGKILL' });
  assert.equal(child.status, 0);
  assert.equal(child.stdout, 'sanitized');
  assert.equal(child.stderr, '');
  assert.throws(() => runBounded(process.execPath,
    ['-e', "process.stdout.write('x'.repeat(100000))"]), error => error.code === 'ENOBUFS');
});
