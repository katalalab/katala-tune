'use strict';
// Local synthetic benchmark; an optional path allows measuring an earlier renderer.
const { performance } = require('node:perf_hooks');
const path = require('node:path');
const Live = require(path.resolve(process.argv[2] || 'renderer/live.js'));
const count = 20_000;
const timings = [];
for (let run = 0; run < 7; run++) {
  const store = {};
  const start = performance.now();
  for (let i = 0; i < count; i++) {
    const t = 1_800_000_000_000 + i * 1000;
    const nodes = Object.fromEntries(Array.from({ length: 6 }, (_, n) =>
      [`node-${n}`, { points: [{ t, cpu: 10, mem: 40 }] }]));
    Live.merge(store, { nodes }, t);
  }
  timings.push(performance.now() - start);
  if (store['node-0'].points.length !== 301) throw new Error('retention changed');
}
timings.sort((a, b) => a - b);
console.log(JSON.stringify({ events: count, nodes: 6, runs: timings.length, median_ms: timings[3], min_ms: timings[0] }));
