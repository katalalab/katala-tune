// Pure observations of hourly percentage means, never a host-health/action verdict.
'use strict';
const HOUR = 3600000;
const DAY = 24 * HOUR;
const MAX_INPUT = 4096;
const METRICS = new Set(['cpu', 'mem', 'commit']);
const profileKey = at => {
  const d = new Date(at);
  return d.getUTCDay() * 6 + Math.floor(d.getUTCHours() / 4);
};
function quantile(values, q) {
  if (!values.length) return null;
  const xs = values.toSorted((a, b) => a - b);
  const pos = (xs.length - 1) * q;
  const lo = Math.floor(pos), hi = Math.ceil(pos);
  return xs[lo] + (xs[hi] - xs[lo]) * (pos - lo);
}

function analyzeHours(points, { metric, now } = {}) {
  if (!Array.isArray(points) || !METRICS.has(metric)) throw new TypeError('invalid hourly baseline input/metric');
  if (!Number.isSafeInteger(now) || now < 0 || now > 8640000000000000) throw new TypeError('invalid baseline clock');
  const end = now - now % HOUR;
  const since = Math.max(0, end - 28 * DAY);
  const until = Math.max(0, end - DAY);
  const report = {
    metric, timezone: 'UTC', state: 'insufficient', reasons: [], observations: [],
    coverage: { input_points: points.length, accepted_hours: null, history_hours: null, history_days: null, recent_hours: null, matched_hours: null, first_hours: null, last_hours: null, ignored_points: null, conflicting_hours: null, duplicate_points: null },
    reference: { since, until, profiles: [] },
    evidence: { elevation_assessed: false, elevated_hours: null, trend: { assessed: false, slope_pp_per_hour: null, change_pp: null } },
  };
  if (points.length > MAX_INPUT) {
    report.reasons = ['input_limit'];
    return report;
  }
  let ignored = 0, coarse = 0;
  const buckets = new Map();
  for (const p of points) {
    if (p?.resolution !== 'hour') { ignored++; if (p?.resolution === 'day') coarse++; continue; }
    if (!Number.isSafeInteger(p.at) || p.at < since || p.at >= end || p.at % HOUR !== 0 ||
        typeof p.mean !== 'number' || !Number.isFinite(p.mean) || p.mean < 0 || p.mean > 100 ||
        !Number.isInteger(p.minutes) || p.minutes < 30 || p.minutes > 60) { ignored++; continue; }
    const mean = p.mean === 0 ? 0 : p.mean;
    const key = `${mean}:${p.minutes}`;
    const b = buckets.get(p.at) || { variants: new Map(), count: 0 };
    b.variants.set(key, { at: p.at, mean, minutes: p.minutes });
    b.count++;
    buckets.set(p.at, b);
  }
  let conflicting = 0, duplicates = 0;
  const valid = [];
  for (const b of buckets.values()) {
    duplicates += b.count - b.variants.size;
    if (b.variants.size > 1) conflicting++;
    else valid.push(b.variants.values().next().value);
  }
  valid.sort((a, b) => a.at - b.at);
  const history = valid.filter(p => p.at < until);
  const recent = valid.filter(p => p.at >= until);
  const groups = new Map();
  for (const p of history) {
    const k = profileKey(p.at);
    const group = groups.get(k) || { values: [], days: new Set() };
    group.values.push(p.mean);
    group.days.add(Math.floor(p.at / DAY));
    groups.set(k, group);
  }
  const references = new Map();
  for (const [k, g] of [...groups].sort(([a], [b]) => a - b)) {
    const mature = g.values.length >= 8 && g.days.size >= 3;
    const p = { weekday: Math.floor(k / 6), slot: k % 6, hours: g.values.length, days: g.days.size, mature,
      p10: mature ? quantile(g.values, 0.1) : null, median: mature ? quantile(g.values, 0.5) : null, p90: mature ? quantile(g.values, 0.9) : null };
    references.set(k, p);
    report.reference.profiles.push(p);
  }
  const matched = recent.filter(p => references.get(profileKey(p.at))?.mature).map(p => {
    const ref = references.get(profileKey(p.at));
    return { at: p.at, residual: p.mean - ref.median, elevated: p.mean > ref.p90 + 15 };
  });
  const first = matched.filter(p => p.at < end - 18 * HOUR);
  const last = matched.filter(p => p.at >= end - 6 * HOUR);
  const historyDays = new Set(history.map(p => Math.floor(p.at / DAY))).size;
  report.coverage = { input_points: points.length, accepted_hours: valid.length, history_hours: history.length, history_days: historyDays,
    recent_hours: recent.length, matched_hours: matched.length, first_hours: first.length, last_hours: last.length,
    ignored_points: ignored, conflicting_hours: conflicting, duplicate_points: duplicates };
  if (history.length < 72) report.reasons.push('history_hours');
  if (historyDays < 7) report.reasons.push('history_days');
  if (recent.length < 18) report.reasons.push('recent_coverage');
  if (matched.length < 18) report.reasons.push('profile_coverage');
  if (first.length < 4 || last.length < 4) report.reasons.push('endpoint_coverage');
  if (!valid.length && coarse) report.reasons.push('hourly_data_required');
  if (report.reasons.length) return report;

  const elevatedHours = last.filter(p => p.elevated).length;
  const elevated = elevatedHours >= 4;
  let rising = false;
  report.evidence.elevation_assessed = true;
  report.evidence.elevated_hours = elevatedHours;
  if (metric !== 'cpu') {
    const slopes = [];
    for (let i = 0; i < matched.length; i++) for (let j = i + 1; j < matched.length; j++) {
      slopes.push((matched[j].residual - matched[i].residual) / ((matched[j].at - matched[i].at) / HOUR));
    }
    const slope = quantile(slopes, 0.5);
    const change = quantile(last.map(p => p.residual), 0.5) - quantile(first.map(p => p.residual), 0.5);
    rising = slope >= 0.25 && change >= 8;
    report.evidence.trend = { assessed: true, slope_pp_per_hour: slope, change_pp: change };
  }
  if (elevated) report.observations.push({ kind: 'elevated', metric });
  if (rising) report.observations.push({ kind: 'rising', metric });
  report.state = elevated && rising ? 'elevated_and_rising' : elevated ? 'elevated' : rising ? 'rising' : 'within_baseline';
  return report;
}
module.exports = { analyzeHours };
