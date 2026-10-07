'use strict';
// ライブ表示の画面側（renderer/live.js の貯め方と、charts.js のスパークライン・コアの棒）
const test = require('node:test');
const assert = require('node:assert/strict');
global.Charts = require('../renderer/charts');
global.UI = require('../renderer/ui');
const Live = require('../renderer/live');

const now = 1_800_000_000_000;
const pt = (s, v = {}) => ({ t: now - s * 1000, cpu: 10, ...v });

test('Electron 版（window.tune.liveStart が無い）では何も出さない', () => {
  assert.equal(Live.supported, false);
  assert.equal(Live.toggle('resources'), '');
  assert.equal(Live.panel('resources'), '');
  assert.doesNotThrow(() => { Live.mount('resources', ['a'], []); Live.route('overview'); });
});

test('イベントを機体ごとに貯める: 差分は後ろに足し、開始時の全体は時刻で重ね、古い点は捨てる', () => {
  const store = {};
  assert.deepEqual(Live.merge(store, { nodes: { a: { state: 'running', points: [pt(3), pt(2)], cores: [1, 2] } } }, now), ['a']);
  assert.equal(store.a.points.length, 2);
  // 同じ時刻・古い時刻の差分は重ねない
  Live.merge(store, { nodes: { a: { state: 'running', points: [pt(2), pt(1)] } } }, now);
  assert.deepEqual(store.a.points.map((p) => p.t), [now - 3000, now - 2000, now - 1000]);
  assert.deepEqual(store.a.cores, [1, 2], '来なかった値は前のまま');
  // 開始時の全体（full）は前の分と合わせて並べ直す
  Live.merge(store, { nodes: { a: { state: 'connecting', full: true, points: [pt(10), pt(2, { cpu: 99 })] } } }, now);
  assert.deepEqual(store.a.points.map((p) => p.t), [now - 10000, now - 3000, now - 2000, now - 1000]);
  assert.equal(store.a.state, 'connecting');
  // 5 分より古い点は持たない
  Live.merge(store, { nodes: { a: { points: [pt(0)] } } }, now + 300_000);
  assert.deepEqual(store.a.points.map((p) => p.t), [now]);
  // 止まった理由
  Live.merge(store, { nodes: { a: { state: 'stopped', reason: 'idle', detail: '自動で止めた' } } }, now);
  assert.deepEqual([store.a.state, store.a.reason], ['stopped', 'idle']);
});

test('速度の表記', () => {
  assert.equal(Live.rate(0), '0 B/s');
  assert.equal(Live.rate(999), '999 B/s');
  assert.equal(Live.rate(1500), '1.5 KB/s');
  assert.equal(Live.rate(12_345_678), '12.3 MB/s');
  assert.equal(Live.rate(250e6), '250 MB/s');
  assert.equal(Live.rate(null), '–');
});

test('順序の乱れ・重複・全体の再送も、従来の時刻マージと一致する', () => {
  const store = {};
  let expected = [];
  let seed = 42;
  const rand = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0);
  for (let i = 0; i < 1000; i++) {
    const at = now + i * 1000;
    const full = i % 17 === 0;
    const points = Array.from({ length: 1 + rand() % 5 }, () => ({ t: at - (rand() % 450) * 1000, cpu: rand() % 100 }));
    const last = expected.at(-1)?.t ?? -Infinity;
    const byT = new Map((full ? [...expected, ...points] : expected.concat(points.filter((p) => p.t > last))).map((p) => [p.t, p]));
    expected = [...byT.values()].sort((a, b) => a.t - b.t).filter((p) => p.t >= at - 300_000);
    Live.merge(store, { nodes: { a: { full, points } } }, at);
    assert.deepEqual(store.a.points, expected, `event ${i}`);
  }
  Live.merge(store, { nodes: { a: { state: 'stopped' } } }, now + 2_000_000);
  assert.deepEqual(store.a.points, [], '点の無い状態イベントでも保持期限を守る');
});

test('スパークライン: 時刻で横に並べ、間が空いたら線を切る', () => {
  const points = [0, 1, 2, 10, 11].map((s) => ({ t: now - 60_000 + s * 1000, v: s * 10 }));
  const svg = Charts.spark([{ points, tone: 'info' }], { window: 60_000, now, min: 0, max: 200, width: 600, height: 40 });
  const line = /class="sp-line tone-info" d="([^"]+)"/.exec(svg)[1];
  assert.equal((line.match(/M/g) || []).length, 2, '2〜10 秒の間（5 秒より長い）で切る');
  assert.ok(line.startsWith('M0 '), '窓の左端が x=0');
  assert.match(svg, /preserveAspectRatio="none"/);
  assert.match(svg, /vector-effect="non-scaling-stroke"/);
  // 上限を省くと値から切りの良い上限を決める
  assert.match(Charts.spark([{ points, tone: 'info' }], { window: 60_000, now }), /data-max="150"/);
  // 2 本目は塗らない。値の無い系列でも壊れない
  const two = Charts.spark([{ points, tone: 'info' }, { points: points.map((p) => ({ t: p.t, v: null })), tone: 'accent' }], { window: 60_000, now });
  assert.equal((two.match(/sp-area/g) || []).length, 1);
  assert.match(Charts.spark([], {}), /<svg class="spark/);
});

test('コアごとの棒: しきい値の色と、値の無いコア', () => {
  const html = Charts.bars([10, 70, 95, null], { warn: 60, crit: 85 });
  assert.equal((html.match(/<i /g) || []).length, 4);
  assert.match(html, /lv-warn[^>]*title="2: 70%"/);
  assert.match(html, /lv-crit[^>]*title="3: 95%"/);
  assert.match(html, /title="4: -"><b style="height:0%">/);
});
