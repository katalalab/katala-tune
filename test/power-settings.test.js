'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { validate, report } = require('../lib/power-settings');

test('calibration validates units and preserves explicit zero', () => {
  assert.deepEqual(validate({ base_w: 0, psu_efficiency: 1, hours: 0, rate_per_kwh: 0, currency: 'JPY' }), { base_w: 0, psu_efficiency: 1, hours: 0, rate_per_kwh: 0, currency: 'JPY' });
  assert.deepEqual(validate({ base_w: null }), { base_w: null });
  for (const p of [{ psu_efficiency: 0 }, { psu_efficiency: 1.1 }, { hours: -1 }, { base_w: NaN }, { rate_per_kwh: '30' }, { currency: '<b>' }, { wall_w: 100 }]) assert.throws(() => validate(p));
});

test('fleet report never presents missing or stale nodes as full fleet watts', () => {
  const nodes = [{ id: 'a' }, { id: 'b' }, { id: 'c' }];
  const snapshots = [{ node_id: 'a', ok: true, at: 1000, data: { power: { wall_w: 100 } } }, { node_id: 'b', ok: true, at: 0, data: { power: { wall_w: 200 } } }];
  const r = report(nodes, snapshots, {}, 301000);
  assert.equal(r.fleet.total_w, null);
  assert.equal(r.fleet.available_w, 100);
  assert.equal(r.fleet.available, 1);
  assert.equal(r.fleet.expected, 3);
  assert.equal(r.nodes[1].fresh, false);
  assert.equal(r.nodes[2].summary.total_w, null);
});
