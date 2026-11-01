'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { analyzeHours } = require('../lib/hourly-baseline');
const H = 3600000;
const D = 24 * H;
const END = Date.UTC(2026, 9, 26, 12);
const NOW = END + 30 * 60000;
const opts = (metric = 'mem') => ({ metric, now: NOW });
const series = (value = () => 20) => Array.from({ length: 672 }, (_, i) => {
  const at = END - 28 * D + i * H;
  return { at, mean: value(at, i), minutes: 60, resolution: 'hour' };
});
const recent = (points, fn) => points.map(p => p.at >= END - D ? { ...p, mean: fn((p.at - END + D) / H) } : p);

test('weekly and four-hour workload patterns stay within their matched baseline', () => {
  const points = series(at => {
    const date = new Date(at);
    return ([0, 6].includes(date.getUTCDay()) ? 10 : date.getUTCHours() >= 8 && date.getUTCHours() < 16 ? 80 : 20) + date.getUTCHours() % 4;
  });
  const r = analyzeHours(points, opts());
  assert.equal(r.state, 'within_baseline');
  assert.deepEqual(r.observations, []);
  assert.equal(r.coverage.history_hours, 648);
  assert.equal(r.coverage.matched_hours, 24);
  assert.equal(r.reference.until, END - D);
  const profile = (weekday, slot) => r.reference.profiles.find(p => p.weekday === weekday && p.slot === slot);
  assert.equal(profile(1, 0).median, 21.5);
  assert.equal(profile(1, 2).median, 81.5);
  assert.equal(profile(0, 2).median, 11.5);
});

test('a sustained step exceeds p90 without training itself into the reference', () => {
  const points = recent(series(), i => i >= 18 ? 50 : 20);
  const r = analyzeHours(points, opts());
  assert.equal(r.state, 'elevated');
  assert.equal(r.evidence.elevated_hours, 6);
  assert.equal(r.evidence.trend.slope_pp_per_hour, 0);
  assert.ok(r.reference.profiles.every(p => p.median === 20));
});

test('gradual memory growth is detected before the elevation threshold', () => {
  const r = analyzeHours(recent(series(), i => 20 + i * 0.5), opts());
  assert.equal(r.state, 'rising');
  assert.equal(r.evidence.elevated_hours, 0);
  assert.equal(r.evidence.trend.slope_pp_per_hour, 0.5);
  assert.equal(r.evidence.trend.change_pp, 9);
  const both = analyzeHours(recent(series(), i => 20 + i * 0.5 + (i >= 18 ? 25 : 0)), opts());
  assert.equal(both.state, 'elevated_and_rising');
});

test('CPU receives pressure elevation assessment but no leak/trend verdict', () => {
  const r = analyzeHours(recent(series(), i => 20 + i * 0.5), opts('cpu'));
  assert.equal(r.state, 'within_baseline');
  assert.equal(r.evidence.trend.assessed, false);
});

test('sparse weekday profiles with plentiful global data withhold observations', () => {
  const points = series().filter(p => p.at >= END - 20 * D);
  const r = analyzeHours(points, opts());
  assert.equal(r.state, 'insufficient');
  assert.ok(r.coverage.history_hours >= 72);
  assert.ok(r.reasons.includes('profile_coverage'));
  assert.deepEqual(r.observations, []);
});

test('missing/null/Boolean readings never substitute for actual zero', () => {
  assert.equal(analyzeHours(series(() => 0), opts()).state, 'within_baseline');
  for (const missing of [null, false, '0', NaN, Infinity]) {
    const r = analyzeHours(recent(series(), () => missing), opts());
    assert.equal(r.state, 'insufficient');
    assert.equal(r.coverage.recent_hours, 0);
    assert.deepEqual(r.observations, []);
  }
});

test('actual time gaps preserve trend units and endpoint coverage', () => {
  const points = recent(series(), i => 20 + i * 0.5).filter(p => p.at < END - D || ![3, 7, 11, 15, 19, 23].includes((p.at - END + D) / H));
  const r = analyzeHours(points, opts());
  assert.equal(r.state, 'rising');
  assert.equal(r.evidence.trend.slope_pp_per_hour, 0.5);
  assert.equal(r.evidence.trend.change_pp, 9.25);
  assert.equal(r.coverage.first_hours, 5);
  assert.equal(r.coverage.last_hours, 4);
});

test('missing endpoints with enough total recent hours do not imply normal', () => {
  const r = analyzeHours(series().filter(p => p.at < END - D || p.at >= END - 18 * H), opts());
  assert.equal(r.coverage.recent_hours, 18);
  assert.equal(r.state, 'insufficient');
  assert.ok(r.reasons.includes('endpoint_coverage'));
});

test('identical duplicates deduplicate and conflicting hours are order independent', () => {
  const points = [...series(), { ...series().at(-1), mean: 99 }];
  const a = analyzeHours(points, opts());
  const b = analyzeHours(points.toReversed(), opts());
  assert.deepEqual(a, b);
  assert.equal(a.coverage.conflicting_hours, 1);
  assert.equal(a.coverage.matched_hours, 23);
  assert.deepEqual(a.observations, []);
  const same = analyzeHours([...series(), { ...series().at(-1) }], opts());
  assert.equal(same.coverage.matched_hours, 24);
});

test('daily summaries and insufficient history cannot create baseline findings', () => {
  const empty = analyzeHours([], opts());
  assert.equal(empty.state, 'insufficient');
  assert.ok(empty.reasons.includes('history_hours'));
  assert.deepEqual(empty.observations, []);
  const daily = analyzeHours(series().map(p => ({ ...p, resolution: 'day' })), opts());
  assert.equal(daily.state, 'insufficient');
  assert.ok(daily.reasons.includes('hourly_data_required'));
});

test('partial current and future hours never contaminate the assessment', () => {
  const base = analyzeHours(series(), opts());
  const r = analyzeHours([...series(), ...Array.from({length:6}, (_, i) => ({at:END+i*H,mean:100,minutes:60,resolution:'hour'}))], opts());
  assert.equal(r.state, base.state);
  assert.deepEqual(r.reference, base.reference);
  assert.deepEqual(r.observations, base.observations);
});

test('input cap withholds evidence instead of silently trimming the sample', () => {
  const r = analyzeHours(Array(4097).fill(series()[0]), opts());
  assert.equal(r.state, 'insufficient');
  assert.deepEqual(r.reasons, ['input_limit']);
  assert.equal(r.coverage.history_hours, null);
  assert.deepEqual(r.observations, []);
});

test('elevation requires four hours and strictly more than the percentage-point margin', () => {
  const atMargin = analyzeHours(recent(series(), i => i >= 18 ? 35 : 20), opts('cpu'));
  assert.equal(atMargin.state, 'within_baseline');
  const onlyThree = analyzeHours(recent(series(), i => i >= 21 ? 50 : 20), opts('cpu'));
  assert.equal(onlyThree.state, 'within_baseline');
  const four = analyzeHours(recent(series(), i => i >= 20 ? 35.001 : 20), opts('cpu'));
  assert.equal(four.state, 'elevated');
});

test('trend needs both the inclusive slope and endpoint-change thresholds', () => {
  const exact = analyzeHours(recent(series(), i => 20 + 0.25 * i + (i >= 18 ? 3.5 : 0)), opts());
  assert.equal(exact.evidence.trend.slope_pp_per_hour, 0.25);
  assert.equal(exact.evidence.trend.change_pp, 8);
  assert.equal(exact.state, 'rising');
  const tooLittleChange = analyzeHours(recent(series(), i => 20 + 0.25 * i + (i >= 18 ? 3.49 : 0)), opts());
  assert.equal(tooLittleChange.state, 'within_baseline');
  const tooSlow = analyzeHours(recent(series(), i => 20 + 0.249 * i + (i >= 18 ? 3.6 : 0)), opts());
  assert.ok(tooSlow.evidence.trend.change_pp > 8);
  assert.equal(tooSlow.state, 'within_baseline');
});

test('represented-minute coverage is explicit and invalid percentages remain unknown', () => {
  assert.equal(analyzeHours(series().map(p => ({ ...p, minutes: 30 })), opts()).state, 'within_baseline');
  for (const patch of [{ minutes: 29 }, { minutes: 61 }, { mean: -1 }, { mean: 101 }]) {
    const r = analyzeHours(series().map(p => p.at >= END - D ? { ...p, ...patch } : p), opts());
    assert.equal(r.state, 'insufficient');
    assert.deepEqual(r.observations, []);
  }
});

test('pure report is independent of host timezone and never echoes identifiers', () => {
  const { execFileSync } = require('node:child_process');
  const path = require('node:path');
  const points = series().map(p => ({ ...p, node_id: 'do-not-echo-fixture', raw: 'canary-only' }));
  const code = `const fs=require('node:fs'); const {analyzeHours}=require(process.argv[1]); const x=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify(analyzeHours(x.points,x.opts)));`;
  const modulePath = path.resolve(__dirname, '../lib/hourly-baseline.js');
  const run = TZ => execFileSync(process.execPath, ['-e', code, modulePath], {input:JSON.stringify({points,opts:opts()}),env:{...process.env,TZ},encoding:'utf8',timeout:5000,maxBuffer:1024*1024});
  assert.equal(run('Pacific/Kiritimati'), run('America/Los_Angeles'));
  const report = JSON.stringify(analyzeHours(points, opts()));
  assert.ok(!report.includes('do-not-echo-fixture') && !report.includes('canary-only'));
});


test('nonconstant profile quantiles interpolate and elevation uses p90 rather than median', () => {
  let historicalValue = 0;
  const points = series().map(p => {
    const date = new Date(p.at);
    if (date.getUTCDay() !== 1 || date.getUTCHours() < 8 || date.getUTCHours() >= 12) return p;
    return { ...p, mean: p.at < END - D ? historicalValue++ : 24.8 };
  });
  const r = analyzeHours(points, opts('cpu'));
  const ref = r.reference.profiles.find(p => p.weekday === 1 && p.slot === 2);
  // Twelve historical values 0..11: position=(n-1)q gives 1.1, 5.5 and 9.9.
  assert.equal(ref.hours, 12);
  assert.equal(ref.p10, 1.1);
  assert.equal(ref.median, 5.5);
  assert.equal(ref.p90, 9.9);
  assert.equal(r.state, 'within_baseline'); // 24.8 > median+15 but <= p90+15.
  const above = analyzeHours(points.map(p => p.at >= END - 4 * H ? { ...p, mean: 25 } : p), opts('cpu'));
  assert.equal(above.state, 'elevated');
  assert.equal(above.evidence.elevated_hours, 4);
});
