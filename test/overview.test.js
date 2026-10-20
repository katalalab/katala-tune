'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const Overview = require('../renderer/overview');
const now = 10 * 3600e3;
const nodes = [
  { id: 'workstation', os: 'macos', role: 'Design' },
  { id: 'builder', os: 'windows', role: 'Build' },
  { id: 'offline', os: 'windows' }, { id: 'new', os: 'macos' },
  { id: 'archive', status: 'retired', os: 'macos' },
];
const results = {
  workstation: { ok: true, at: now - 60000, score: 90, findings: [] },
  builder: { ok: true, at: now - 4 * 3600e3, score: 10, findings: [{ severity: 'critical' }] },
  offline: { ok: false, at: now, error: 'not reachable' },
  archive: { ok: true, at: now, score: 0, findings: [{ severity: 'critical' }] },
};
const model = (schedule) => Overview.summarize(nodes, results, schedule, now);

test('coverage partitions active targets and excludes historical scores of retired nodes', () => {
  const d = model();
  assert.deepEqual(d.counts, { fresh: 1, stale: 1, failed: 1, missing: 1, excluded: 1 });
  assert.equal(d.target, 4);
  assert.equal(d.average, 90);
  assert.deepEqual(d.current.map(r => r.score), [90]);
  assert.equal(Overview.select(d.rows, { status: 'attention' }).length, 3);
});
test('freshness uses three scan intervals, including its exact boundary', () => {
  const n = [{ id: 'node' }];
  const r = at => ({ node: { ok: true, at, score: 0 } });
  assert.equal(Overview.summarize(n, r(now - 90 * 60000), { probe_minutes: 30 }, now).rows[0].status, 'fresh');
  assert.equal(Overview.summarize(n, r(now - 90 * 60000 - 1), { probe_minutes: 30 }, now).rows[0].status, 'stale');
  assert.equal(Overview.summarize(n, r(now), {}, now).average, 0);
});
test('invalid and future timestamps never become current; invalid interval falls back to 60 minutes', () => {
  for (const at of [null, undefined, 0, NaN, Infinity, now + 1, String(now)]) {
    const d = Overview.summarize([{ id: 'node' }], { node: { ok: true, at, score: 100 } }, {}, now);
    assert.equal(d.rows[0].status, 'stale');
    assert.equal(d.average, null);
  }
  for (const probe_minutes of [0, -1, NaN, Infinity, 'bad']) assert.equal(model({ probe_minutes }).freshMinutes, 180);
});
test('search, OS and status filters combine without changing the source order or denominator', () => {
  const d = model();
  assert.deepEqual(Overview.select(d.rows, { q: ' BUILD ', os: 'windows', status: 'attention' }).map(x => x.node.id), ['builder']);
  assert.equal(Overview.select(d.rows, { q: 'no match' }).length, 0);
  assert.equal(Overview.select(d.rows, { status: 'excluded' })[0].node.id, 'archive');
  assert.equal(d.rows.length, 5);
});
test('attention includes fresh findings; non-finite scores do not poison the average', () => {
  const d = Overview.summarize([{ id: 'warn' }, { id: 'clean' }], {
    warn: { ok: true, at: now, score: NaN, findings: [{ severity: 'warn' }] },
    clean: { ok: true, at: now, score: 80, findings: [] },
  }, {}, now);
  assert.equal(d.average, 80);
  assert.deepEqual(Overview.select(d.rows, { status: 'attention' }).map(x => x.node.id), ['warn']);
});
test('empty roster has no average and invalid operation state is excluded', () => {
  assert.equal(Overview.summarize([], {}, {}, now).average, null);
  assert.equal(Overview.summarize([{ id: 'n', status: 'invalid' }], {}, {}, now).target, 0);
});
