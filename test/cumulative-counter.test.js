'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { step } = require('../lib/cumulative-counter');
const fixture = require('./fixtures/cumulative-counter.json');

// Literal oracles catch rounding, zero-filling and stale-baseline bridges.
for (const c of fixture.cases) test(c.name, () => {
  let baseline = null;
  for (let i = 0; i < c.samples.length; i++) {
    const got = step(baseline, c.samples[i], c.window);
    const { baseline: wantBaseline, ...want } = c.expected[i];
    assert.deepEqual({ reason: got.reason, rate: got.rate }, want, `step ${i}`);
    assert.deepEqual(got.baseline, wantBaseline ? c.samples[i] : null, `baseline ${i}`);
    baseline = got.baseline;
  }
});

test('baseline owns its values instead of aliasing mutable input', () => {
  const sample = structuredClone(fixture.cases[0].samples[0]);
  const got = step(null, sample);
  sample.count = '999';
  sample.descriptor.source = 'changed';
  assert.equal(got.baseline.count, '100');
  assert.equal(got.baseline.descriptor.source, 's1');
});

test('a malformed previous baseline cannot create an accepted rate', () => {
  const sample = fixture.cases[0].samples[0];
  const got = step({ count: '0' }, sample);
  assert.deepEqual(got, { baseline: sample, reason: 'invalid_baseline', rate: null });
});
