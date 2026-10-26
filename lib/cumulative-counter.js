'use strict';

// Pure, cumulative-only arithmetic. Descriptor equality does not prove a host
// or boot identity; callers must supply that evidence. No OS metric mapping.
const U64_MAX = '18446744073709551615';
const FIELDS = ['host', 'boot', 'source', 'definition', 'unit', 'scope', 'counter_epoch', 'clock', 'mode'];
const SAMPLE_FIELDS = ['count', 'tick', 'frequency', 'descriptor'];

function keys(value, fields) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  let count = 0;
  for (const key in value) if (Object.hasOwn(value, key)) {
    if (++count > fields.length || !fields.includes(key)) return false;
  }
  return count === fields.length;
}

function uint(value, positive = false) {
  // Bound grammar, length and range BEFORE constructing a BigInt.
  if (typeof value !== 'string' || value.length > 20 || !/^(0|[1-9][0-9]*)$/.test(value)
    || (value.length === 20 && value > U64_MAX) || (positive && value === '0')) return null;
  return BigInt(value);
}

function validate(sample) {
  if (!keys(sample, SAMPLE_FIELDS)) return 'invalid';
  if (uint(sample.count) === null || uint(sample.tick) === null || uint(sample.frequency, true) === null) return 'invalid';
  const d = sample.descriptor;
  if (!d || typeof d !== 'object' || Array.isArray(d)) return 'invalid';
  if (d.host == null || d.host === '' || d.boot == null || d.boot === '') return 'unknown_provenance';
  if (!keys(d, FIELDS)) return 'invalid';
  for (const field of FIELDS) {
    const v = d[field];
    if (typeof v !== 'string' || v.length < 1 || v.length > 128 || !/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/.test(v)) return 'invalid';
  }
  if (d.mode !== 'cumulative') return 'unsupported_mode';
  if (d.scope !== 'host') return 'unsupported_scope';
  return null;
}

function decimal(delta, ticks, frequency) {
  const numerator = delta * frequency;
  let whole = numerator / ticks;
  // Splitting the integer part also keeps the equivalent Rust u128 bounded.
  let fraction = ((numerator % ticks) * 1000000n + ticks / 2n) / ticks;
  if (fraction === 1000000n) { whole++; fraction = 0n; }
  return `${whole}.${fraction.toString().padStart(6, '0')}`;
}

function step(previous, sample, window = null) {
  const result = (reason, baseline = null, rate = null) => ({ baseline, reason, rate });
  if (window !== null && (!keys(window, ['min_elapsed_ticks', 'max_elapsed_ticks'])
    || uint(window.min_elapsed_ticks, true) === null || uint(window.max_elapsed_ticks, true) === null
    || BigInt(window.min_elapsed_ticks) > BigInt(window.max_elapsed_ticks))) return result('invalid_window');
  if (sample == null) return result('missing');
  const invalid = validate(sample);
  if (invalid) return result(invalid);
  // Copy validated primitive fields: caller mutation must not rewrite history.
  const current = { count: sample.count, tick: sample.tick, frequency: sample.frequency, descriptor: { ...sample.descriptor } };
  if (previous == null) return result('first', current);
  if (validate(previous)) return result('invalid_baseline', current);
  if (previous.descriptor.clock !== current.descriptor.clock) return result('clock_changed');
  if (previous.frequency !== current.frequency) return result('frequency_changed');
  if (FIELDS.some(field => previous.descriptor[field] !== current.descriptor[field])) return result('descriptor_changed', current);
  const ticks = BigInt(current.tick) - BigInt(previous.tick);
  if (ticks <= 0n) return result('invalid_time');
  if (window !== null && (ticks < BigInt(window.min_elapsed_ticks) || ticks > BigInt(window.max_elapsed_ticks))) return result('window');
  const delta = BigInt(current.count) - BigInt(previous.count);
  if (delta < 0n) return result('reset', current);
  return result('ok', current, {
    delta_count: delta.toString(), elapsed_ticks: ticks.toString(), frequency: current.frequency,
    decimal: decimal(delta, ticks, BigInt(current.frequency)),
  });
}

module.exports = { step };
