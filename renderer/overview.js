'use strict';
// 概要の対象・鮮度と絞り込み。収集・設定変更は行わず、渡された結果だけを扱う。
const Overview = (() => {
  function summarize(nodes, results, schedule = {}, now = Date.now()) {
    const interval = Number(schedule?.probe_minutes);
    const freshMinutes = (Number.isFinite(interval) && interval > 0 ? interval : 60) * 3;
    const counts = { fresh: 0, stale: 0, failed: 0, missing: 0, excluded: 0 };
    const rows = nodes.map(node => {
      const result = results[node.id];
      const active = !node.status || node.status === 'online';
      const validTime = Number.isFinite(result?.at) && result.at > 0 && result.at <= now;
      const status = !active ? 'excluded' : !result ? 'missing' : !result.ok ? 'failed'
        : !validTime || now - result.at > freshMinutes * 60000 ? 'stale' : 'fresh';
      counts[status]++;
      const attention = active && (status !== 'fresh' || (result?.findings || []).some(f => ['critical', 'warn'].includes(f.severity)));
      return { node, result, status, attention };
    });
    const current = rows.filter(r => r.status === 'fresh').map(r => r.result);
    const scores = current.map(r => r.score).filter(Number.isFinite);
    return { rows, counts, current, freshMinutes, target: nodes.length - counts.excluded,
      average: scores.length ? Math.round(scores.reduce((s, v) => s + v, 0) / scores.length) : null };
  }
  function select(rows, { q = '', os = '', status = 'all' } = {}) {
    const query = q.trim().toLowerCase();
    return rows.filter(r => (!query || `${r.node.id} ${r.node.role || ''}`.toLowerCase().includes(query))
      && (!os || r.node.os === os)
      && (status === 'all' || (status === 'attention' ? r.attention : r.status === status)));
  }
  return { summarize, select };
})();
if (typeof module !== 'undefined' && module.exports) module.exports = Overview;
