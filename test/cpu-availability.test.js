'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { analyze } = require('../lib/rules');
const { nodeChecks } = require('../lib/health');
const fixtures = require('./fixtures/cpu-availability.json');
const now = 1800000000000;
for (const f of fixtures) {
  test(`CPU availability consumers: ${f.name}`, () => {
    const findings = analyze(f.data);
    const cpu = nodeChecks({ id: 'test-node' }, { at: now, data: f.data }, findings, { now }).find(x => x.id === 'cpu');
    assert.equal(cpu.status, f.status);
    assert.equal(cpu.detail, f.detail);
    if (f.finding) assert.equal(findings.find(x => x.id === 'cpu-saturated').detail, f.finding);
  });
}
test('non-finite and non-numeric host CPU remain unknown', () => {
  for (const cpu_busy of [NaN, Infinity, '10']) {
    const data = { probe: 'windows', cpu_busy };
    const cpu = nodeChecks({ id: 'test-node' }, { at: now, data }, [], { now }).find(x => x.id === 'cpu');
    assert.equal(cpu.status, 'unknown');
    assert.equal(cpu.detail, 'CPU 使用率は未取得');
  }
});
