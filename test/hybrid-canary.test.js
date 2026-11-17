'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { validateSnapshot } = require('../scripts/check-hybrid-canary');
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
