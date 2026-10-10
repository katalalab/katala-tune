'use strict';
const { summarize } = require('./power');
const LIMITS = { base_w: [0, 10000], psu_efficiency: [Number.MIN_VALUE, 1], hours: [0, 8784], rate_per_kwh: [0, 1000000] };
function validate(patch) {
  if (!patch || Array.isArray(patch) || typeof patch !== 'object') throw new Error('計算条件はオブジェクトで指定してください');
  for (const [k, v] of Object.entries(patch)) {
    if (k === 'currency') { if (v !== null && (typeof v !== 'string' || !/^[A-Z]{3}$/.test(v))) throw new Error('通貨は3文字のコードで指定してください'); }
    else if (!LIMITS[k] || (v !== null && (typeof v !== 'number' || !Number.isFinite(v) || v < LIMITS[k][0] || v > LIMITS[k][1]))) throw new Error(`計算条件が不正です: ${k}`);
  }
  return { ...patch };
}
function report(nodes, snapshots, settings, now = Date.now()) {
  const rows = nodes.map((n) => {
    const r = snapshots.find((s) => s.node_id === n.id && s.ok);
    const age = r && Number.isFinite(r.at) ? now - r.at : null;
    return { node_id: n.id, at: r?.at ?? null, fresh: age !== null && age >= -5000 && age <= 300000, settings: settings[n.id] || {}, data: r?.data || {}, summary: summarize(r?.data || {}, settings[n.id] || {}) };
  });
  const available = rows.filter((r) => r.fresh && r.summary.total_w !== null);
  const sum = available.reduce((v, r) => v + r.summary.total_w, 0);
  return { nodes: rows, fleet: { expected: nodes.length, available: available.length, available_w: available.length ? sum : null, total_w: nodes.length && available.length === nodes.length ? sum : null } };
}
module.exports = { validate, report };
