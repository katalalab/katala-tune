// 画面の見た目を確かめるための preload（scripts/ui-preview.js から使う。アプリ本体では使わない）。
// window.tune を test/fixtures/ui/*.json の架空データで差し替える。API の形は preload.js と同じ。
// フィクスチャの時刻は "@-12m"（今から12分前）のような相対表記で、読み込んだ瞬間の「今」に合わせて数値へ直す。
// "@s-12m" は秒単位（katala-fleet の last_seen など）。
'use strict';
const { contextBridge, ipcRenderer } = require('electron');
const fs = require('node:fs');
const path = require('node:path');

const DIR = path.join(__dirname, '..', 'test', 'fixtures', 'ui');
const NOW = Date.now();
const REL = /^@(s)?(-?\d+(?:\.\d+)?)m$/;
function revive(v) {
  if (Array.isArray(v)) return v.map(revive);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, revive(x)]));
  if (typeof v === 'string') {
    const m = REL.exec(v);
    if (m) { const ms = NOW + Number(m[2]) * 60000; return m[1] ? Math.round(ms / 1000) : Math.round(ms); }
  }
  return v;
}
const load = (name) => revive(JSON.parse(fs.readFileSync(path.join(DIR, name), 'utf8')));

const params = new URLSearchParams(globalThis.location?.search || '');
const config = { ...load('config.json'), platform: params.get('platform') || process.platform, accent: params.get('accent') ? `#${params.get('accent')}` : null };
const results = load('results.json');
const history = load('history.json');
const { logs, cursors } = load('logs.json');
const status = load('status.json');
const actions = load('actions.json');
const fleet = load('fleet.json');

const clone = (v) => JSON.parse(JSON.stringify(v));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const listeners = { probe: [], checks: [], logs: [] };

function counts() {
  const c = { ok: 0, warn: 0, fail: 0, unknown: 0 };
  for (const x of status.checks) c[x.status]++;
  return c;
}

// SQLite の FTS5 の代わり。語の並び（すべて含む）・OR（どれか）・"…"（語句）だけを扱う
function matcher(q) {
  if ((q.match(/"/g) || []).length % 2) throw new Error('fts5: syntax error near """');
  const terms = (s) => (s.match(/"[^"]*"|\S+/g) || []).map((t) => t.replace(/^"|"$/g, '').toLowerCase()).filter(Boolean);
  const alts = q.split(/\s+OR\s+/).map(terms);
  return (l) => {
    const hay = `${l.message} ${l.provider || ''}`.toLowerCase();
    return alts.some((all) => all.every((t) => hay.includes(t)));
  };
}

function queryLogs({ node_id, level, source, q, since, limit = 300 } = {}) {
  let rows = logs;
  if (q) rows = rows.filter(matcher(q));
  if (node_id) rows = rows.filter((l) => l.node_id === node_id);
  if (level) rows = rows.filter((l) => l.level === level);
  if (source) rows = rows.filter((l) => l.source === source);
  if (since) rows = rows.filter((l) => l.ts >= since);
  return rows.slice().sort((a, b) => b.ts - a.ts).slice(0, Math.min(limit, 2000));
}

function signatures({ since = 0, node_id, limit = 50 } = {}) {
  const by = new Map();
  for (const l of logs) {
    if (l.ts < since || (node_id && l.node_id !== node_id)) continue;
    const s = by.get(l.fingerprint) || { fingerprint: l.fingerprint, level: l.level, provider: l.provider, source: l.source, sample: l.message.slice(0, 300), n: 0, ids: new Set(), last_ts: 0 };
    s.n++; s.ids.add(l.node_id); s.last_ts = Math.max(s.last_ts, l.ts);
    by.set(l.fingerprint, s);
  }
  const total = (fp) => logs.filter((l) => l.fingerprint === fp).length;
  return [...by.values()].sort((a, b) => b.n - a.n).slice(0, limit)
    .map(({ ids, ...s }) => ({ ...s, nodes: ids.size, node_ids: [...ids].join(','), total: total(s.fingerprint) }));
}

contextBridge.exposeInMainWorld('tune', {
  config: async () => clone(config),
  last: async () => clone(results),
  probe: async (ids) => {
    const targets = results.filter((r) => !ids?.length || ids.includes(r.node_id));
    await wait(1500);
    for (const r of targets) {
      Object.assign(r, { at: Date.now(), stale: false });
      for (const fn of listeners.probe) fn(clone(r));
    }
    return { done: targets.length, auto: false };
  },
  history: async (id) => clone(history[id] || []),
  fleet: async () => clone(fleet),
  // 確認ダイアログで「やめる」を選んだのと同じ結果を返す（プレビューでは何も実行しない）
  action: async () => ({ ok: false, cancelled: true }),
  undo: async () => ({ ok: false, cancelled: true }),
  actionsLog: async () => clone(actions),
  logsSync: async (ids) => {
    await wait(800);
    const targets = config.nodes.filter((n) => !ids?.length || ids.includes(n.id));
    return { done: targets.length, ms: 800, results: targets.map((n) => (n.id === 'family-pc' ? { node_id: n.id, error: 'ssh: connect to host family-pc port 22: Operation timed out' } : { node_id: n.id, sources: { a: { inserted: 2, fetched: 2, dropped: 0 } } })) };
  },
  logsQuery: async (f) => { try { return { rows: clone(queryLogs(f || {})) }; } catch (e) { return { error: String(e.message || e) }; } },
  logsSignatures: async (f) => clone(signatures(f || {})),
  logsCursors: async () => clone(cursors),
  copy: async () => true,
  openDataDir: async () => '',
  openConfig: async () => '',
  status: async () => clone({ ...status, counts: counts() }),
  setSchedule: async (patch) => {
    if (typeof patch?.enabled === 'boolean') status.schedule.enabled = patch.enabled;
    for (const k of ['probe_minutes', 'logs_minutes']) if (Number.isInteger(patch?.[k])) status.schedule[k] = patch[k];
    return clone(status.schedule);
  },
  setLogin: async (on) => { status.openAtLogin = !!on; return status.openAtLogin; },
  onChecksUpdated: (fn) => { listeners.checks.push(fn); },
  onNavigate: (fn) => ipcRenderer.on('navigate', (_e, v) => fn(v)),
  onProbeResult: (fn) => { listeners.probe.push(fn); },
  onLogsSynced: (fn) => { listeners.logs.push(fn); },
});
