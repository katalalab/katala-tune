'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { compare } = require('../scripts/cpu-reference');
const script = path.join(__dirname, '..', 'scripts', 'cpu-reference.js');

function run(started_ms, ticks) {
  return {
    schema: 'katala-tune.cpu-reference.v1',
    context: { hostname: 'fixture', platform: 'test-os', arch: 'test-arch', os_release: '1', logical_cpus: 4, cpu_model: 'test-cpu', runtime: 'v24',
      sampler: 'node-os-cpus-v1', interval_ms: 1000, workload: 'fixed-job', power: 'AC' },
    started_ms, ended_ms: started_ms + ticks.length * 1000,
    windows: ticks.map(([busy_ticks, total_ticks]) => ({ elapsed_ms: 1000, busy_ticks, total_ticks }))
  };
}

test('CPU change is relative to observed ticks and constant baselines remain usable', () => {
  const result = compare(run(0, [[0, 100], [0, 100]]), run(2000, [[10, 100], [40, 150]]));
  assert.equal(result.comparable, true);
  assert.equal(result.delta_percentage_points, 20);
});

test('context mismatches are explicit; missing context is invalid', () => {
  const baseline = run(0, [[20, 100]]);
  for (const [key, value] of Object.entries({ workload: 'other', power: 'battery', hostname: 'host-b', os_release: '2', logical_cpus: 8, runtime: 'v25' })) {
    const current = run(1000, [[90, 100]]);
    current.context[key] = value;
    const result = compare(baseline, current);
    assert.deepEqual(result.mismatches, [key]);
    assert.deepEqual(result.reasons, ['context_mismatch']);
    assert.equal(result.delta_percentage_points, null);
  }
  const invalid = run(1000, [[90, 100]]);
  delete invalid.context.sampler;
  assert.throws(() => compare(baseline, invalid), /context/);
});

test('comparison keeps missing windows and refuses overlapping observations', () => {
  const baseline = run(0, [[50, 100]]);
  const current = run(500, [[null, null], [10, 100]]);
  assert.deepEqual(compare(baseline, current).reasons, ['reference_not_prior', 'incomplete_observation']);
  baseline.windows[0].elapsed_ms = 2001;
  assert.equal(compare(baseline, current).baseline.cpu_pct, null);
  assert.throws(() => compare(baseline, run(5000, [[120, 100]])), /invalid CPU counters/);
});

test('real sample command yields a reusable observation, including through PowerShell UTF-16LE redirection', t => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cpu-ref-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const command = (...args) => spawnSync(process.execPath, [script, ...args], { encoding: 'utf8' });
  const observed = command('sample', 'fixed-job', 'ac', '1');
  assert.equal(observed.status, 0, observed.stderr);
  const sample = JSON.parse(observed.stdout);
  assert.equal(sample.windows.length, 1);
  assert.equal(sample.context.workload, 'fixed-job');
  const file = path.join(dir, 'sample.json');
  fs.writeFileSync(file, '\uFEFF' + observed.stdout, 'utf16le');
  const repeated = command('compare', file, file);
  assert.equal(repeated.status, 1);
  assert.ok(JSON.parse(repeated.stdout).reasons.includes('reference_not_prior'));
  assert.equal(command('sample', 'fixed-job', 'ac', '-2').status, 1);
});
