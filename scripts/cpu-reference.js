'use strict';

const fs = require('node:fs');
const os = require('node:os');
const { setTimeout: delay } = require('node:timers/promises');

const schema = 'katala-tune.cpu-reference.v1';
const intervalMs = 1000;
const maxCount = 300;
const contextKeys = ['hostname', 'platform', 'arch', 'os_release', 'logical_cpus', 'cpu_model', 'runtime', 'sampler', 'interval_ms', 'workload', 'power'];
const label = value => typeof value === 'string' && value.trim().length > 0 && value.length <= 256;
const countValid = n => Number.isInteger(n) && n >= 1 && n <= maxCount;

function counters(cpus) {
  let idle = 0;
  let total = 0;
  for (const { times } of cpus) {
    for (const value of Object.values(times)) total += value;
    idle += times.idle;
  }
  return { idle, total };
}

async function sample(workload, power, count) {
  if (!label(workload) || !label(power) || !countValid(count)) throw new Error('specify workload, power, and a sample count from 1 to 300');
  const cpus = os.cpus();
  if (!cpus.length) throw new Error('host CPU counters unavailable');
  const context = {
    hostname: os.hostname(), platform: os.platform(), arch: os.arch(), os_release: os.release(),
    logical_cpus: cpus.length, cpu_model: cpus[0].model, runtime: process.version, sampler: 'node-os-cpus-v1',
    interval_ms: intervalMs, workload, power
  };
  const started_ms = Date.now();
  const windows = [];
  let previous = counters(cpus);
  let previousTime = performance.now();
  for (let i = 0; i < count; i++) {
    await delay(intervalMs);
    const next = os.cpus();
    const now = performance.now();
    const elapsed_ms = now - previousTime;
    const current = counters(next);
    const total = current.total - previous.total;
    const idle = current.idle - previous.idle;
    // Dropping a whole scheduled window makes this run ineligible for comparison.
    if (next.length !== context.logical_cpus || elapsed_ms >= 2 * intervalMs || total <= 0 || idle < 0 || idle > total) {
      windows.push({ elapsed_ms, busy_ticks: null, total_ticks: null });
    } else {
      windows.push({ elapsed_ms, busy_ticks: total - idle, total_ticks: total });
    }
    previous = current;
    previousTime = now;
  }
  return { schema, context, started_ms, ended_ms: Date.now(), windows };
}

function summarize(run) {
  if (run?.schema !== schema || !Array.isArray(run.windows) || !countValid(run.windows.length)) {
    throw new Error('unsupported CPU observation or invalid window count');
  }
  const c = run.context;
  if (!c || !contextKeys.every(key => {
    if (key === 'logical_cpus') return Number.isSafeInteger(c[key]) && c[key] > 0;
    if (key === 'interval_ms') return c[key] === intervalMs;
    if (key === 'sampler') return c[key] === 'node-os-cpus-v1';
    return label(c[key]);
  })) throw new Error('missing or unsupported observation context');
  if (![run.started_ms, run.ended_ms].every(Number.isSafeInteger) || run.ended_ms <= run.started_ms) {
    throw new Error('invalid observation time range');
  }
  let busy = 0;
  let total = 0;
  let observed = 0;
  let elapsed = 0;
  for (const w of run.windows) {
    if (!w || !Number.isFinite(w.elapsed_ms) || w.elapsed_ms <= 0) throw new Error('invalid window duration');
    elapsed += w.elapsed_ms;
    if (w.busy_ticks === null && w.total_ticks === null) continue;
    if (![w.busy_ticks, w.total_ticks].every(Number.isSafeInteger) || w.total_ticks <= 0 || w.busy_ticks < 0 || w.busy_ticks > w.total_ticks) {
      throw new Error('invalid CPU counters');
    }
    if (w.elapsed_ms >= 2 * intervalMs) continue;
    busy += w.busy_ticks;
    total += w.total_ticks;
    observed++;
  }
  const complete = observed === run.windows.length;
  return { windows: run.windows.length, observed, elapsed_ms: elapsed, cpu_pct: complete ? 100 * busy / total : null };
}

function compare(reference, current) {
  const baseline = summarize(reference);
  const observation = summarize(current);
  const mismatches = contextKeys.filter(key => reference.context[key] !== current.context[key]);
  const reasons = [];
  if (mismatches.length) reasons.push('context_mismatch');
  if (current.started_ms < reference.ended_ms) reasons.push('reference_not_prior');
  if (baseline.cpu_pct === null || observation.cpu_pct === null) reasons.push('incomplete_observation');
  return { comparable: !reasons.length, reasons, mismatches, baseline, observation,
    delta_percentage_points: reasons.length ? null : observation.cpu_pct - baseline.cpu_pct };
}

function readRun(file) {
  const bytes = fs.readFileSync(file);
  // Windows PowerShell 5 uses UTF-16LE with a BOM for native output redirection.
  const encoding = bytes[0] === 0xff && bytes[1] === 0xfe ? 'utf16le' : 'utf8';
  return JSON.parse(bytes.toString(encoding).replace(/^\uFEFF/, ''));
}

async function main(args) {
  let result;
  if (args[0] === 'sample' && args.length === 4) {
    result = await sample(args[1], args[2], Number(args[3]));
  } else if (args[0] === 'compare' && args.length === 3) {
    result = compare(readRun(args[1]), readRun(args[2]));
    if (!result.comparable) process.exitCode = 1;
  } else {
    throw new Error('usage: cpu-reference.js sample <workload> <power> <count> | compare <baseline.json> <current.json>');
  }
  console.log(JSON.stringify(result));
}

module.exports = { compare };
if (require.main === module) main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
