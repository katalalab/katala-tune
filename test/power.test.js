'use strict';
const assert = require('node:assert/strict');
const test = require('node:test');
const fixture = require('./fixtures/power.json');
const { integrate, summarize } = require('../lib/power');

for (const row of fixture.summary) {
  test(`summarize: ${row.name}`, () => {
    assert.deepEqual(summarize(row.snapshot, row.settings), row.expected);
  });
}

for (const row of fixture.integration) {
  test(`integrate: ${row.name}`, () => {
    assert.deepEqual(integrate(row.points, row.interval_seconds), row.expected);
  });
}
