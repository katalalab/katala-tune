'use strict';
const { execFileSync } = require('node:child_process');
const path = require('node:path');

function keys(object, expected) {
  if (!object || Array.isArray(object) || typeof object !== 'object' ||
      Object.keys(object).sort().join(',') !== [...expected].sort().join(',')) throw Error('unexpected object keys');
}
function number(value, min, max) {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < min || value > max) throw Error('invalid number');
}
function integer(value, min, max = Number.MAX_SAFE_INTEGER) {
  number(value, min, max);
  if (!Number.isSafeInteger(value)) throw Error('invalid integer');
}
const reasons = new Set(['invalid_value', 'unavailable_or_invalid', 'unavailable_or_unsupported',
  'unsupported_platform', 'not_collected', 'permission_denied', 'unsupported', 'library_unavailable',
  'collection_error', 'no_devices', 'device_limit_exceeded']);
function reading(value, validate) {
  keys(value, ['value', 'status', 'reason']);
  if (value.status === 'unknown') {
    if (value.value !== null || !reasons.has(value.reason)) throw Error('invalid unknown');
  } else if (value.status === 'observed') {
    if (value.reason !== null) throw Error('invalid observed reason');
    validate(value.value);
  } else throw Error('invalid status');
}
function memory(value) {
  keys(value, ['usedBytes', 'totalBytes']);
  integer(value.totalBytes, 1);
  integer(value.usedBytes, 0, value.totalBytes);
}
function validateSnapshot(value, now = Date.now()) {
  keys(value, ['schemaVersion', 'kind', 'observedAtUnixMs', 'cpuWindowMs', 'cpuPercent', 'memory', 'gpus', 'hostTemperatureC', 'sources']);
  if (value.schemaVersion !== 1 || value.kind !== 'hybrid-canary') throw Error('invalid schema');
  integer(value.observedAtUnixMs, now - 10_000, now + 1000);
  integer(value.cpuWindowMs, 250, 1000);
  reading(value.cpuPercent, x => number(x, 0, 100));
  reading(value.memory, memory);
  reading(value.hostTemperatureC, x => number(x, 0, 150));
  if (value.hostTemperatureC.status !== 'unknown' || value.hostTemperatureC.reason !== 'not_collected') throw Error('host sensor not supported by this canary');
  reading(value.gpus, devices => {
    if (!Array.isArray(devices) || !devices.length || devices.length > 16) throw Error('invalid GPU count');
    devices.forEach((device, index) => {
      keys(device, ['index', 'utilizationPercent', 'memory', 'temperatureC']);
      if (device.index !== index) throw Error('invalid GPU index');
      reading(device.utilizationPercent, x => number(x, 0, 100));
      reading(device.memory, memory);
      reading(device.temperatureC, x => integer(x, 0, 150));
    });
  });
  keys(value.sources, ['host', 'gpu']);
  if (value.sources.host !== 'sysinfo-0.37.2' || value.sources.gpu !== 'nvml-wrapper-0.11.0') throw Error('unrecognized source');
  return value;
}

// One direct, read-only probe process; never a shell or process tree. SIGKILL is
// intentional: a stuck native driver must not extend the diagnostic lifetime by
// ignoring SIGTERM. Node terminates the child unconditionally on Windows too.
function runBounded(binary, args = [], timeoutMs = 10_000) {
  integer(timeoutMs, 1, 10_000);
  return execFileSync(binary, args, {
    encoding: 'utf8', timeout: timeoutMs, killSignal: 'SIGKILL', maxBuffer: 24 * 1024,
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
  });
}

if (require.main === module) {
  try {
    if (process.argv.length !== 3) throw Error('usage: node scripts/check-hybrid-canary.js PATH_TO_BUILT_EXAMPLE');
    const started = performance.now();
    const output = runBounded(path.resolve(process.argv[2]));
    const value = validateSnapshot(JSON.parse(output));
    // Local diagnostic only. Never makes an HTTP request or persists identity.
    console.log(JSON.stringify({ valid: true, elapsedMs: performance.now() - started, snapshot: value }));
  } catch (error) {
    // Do not print a driver error, local executable path, or buffered output.
    console.error(error.code === 'ETIMEDOUT' ? 'hybrid canary timed out' : 'hybrid canary failed validation or execution');
    process.exitCode = 1;
  }
}
module.exports = { validateSnapshot, runBounded };
