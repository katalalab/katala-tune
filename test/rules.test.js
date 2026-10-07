'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { analyze, score, compare, HIGH_PERF_GUID } = require('../lib/rules');

const mac = (over = {}) => ({
  probe: 'mac', host: { cores: 10, uptime_h: 10 }, cpu_busy: 20,
  memory: { total_gb: 16, available_pct: 50, pressure: 'normal', swap_used_gb: 0, swap_total_gb: 0 },
  processes: { top_cpu: [], apps: [], apps_cpu: [], agent_processes: 3 },
  disk: [{ mount: '/', total_gb: 500, free_gb: 200, free_pct: 40 }],
  power: {}, containers: {}, caches: [], bench: { runs_ms: [100, 101, 99], median_ms: 100 }, ...over,
});
const win = (over = {}) => ({
  probe: 'windows', host: { cores: 20, uptime_h: 10 }, cpu_busy: 10, cpu_perf_pct: 100,
  memory: { total_gb: 64, available_pct: 60, commit_pct: 40 },
  processes: { top_cpu: [], apps: [], apps_cpu: [], agent_processes: 1 },
  disk: [{ mount: 'C:', total_gb: 1000, free_gb: 500, free_pct: 50 }],
  power: { plan_guid: HIGH_PERF_GUID, plan_name: '高パフォーマンス' }, wsl: { config: true, memory: '16GB' },
  stability_7d: { bugcheck_1001: 0, kernel_power_41: 0, unexpected_6008: 0 }, defender: { realtime: true }, ...over,
});

test('健全な機体は所見なしで 100 点', () => {
  assert.deepEqual(analyze(mac()), []);
  assert.equal(score(analyze(win())), 100);
});

test('mac: 1コアを使い切るアプリは終了を提案し、システムは提案しない', () => {
  const f = analyze(mac({ processes: { top_cpu: [
    { pid: 500, name: 'Google Drive', app: 'Google Drive', cpu: 105 },
    { pid: 414, name: 'WindowServer', app: 'WindowServer', cpu: 90 },
    { pid: 900, name: 'claude', app: 'claude', cpu: 120 },
  ], apps: [], apps_cpu: [] } }));
  const drive = f.find((x) => x.id.startsWith('runaway-Google Drive'));
  assert.equal(drive.action.type, 'kill-process');
  assert.deepEqual(drive.action.params, { pid: 500, name: 'Google Drive', start: null, min_cpu: 50 });
  assert.match(drive.advice, /Google Drive/);
  assert.equal(f.find((x) => x.id.startsWith('runaway-WindowServer')).action, undefined);
  assert.equal(f.find((x) => x.id.startsWith('runaway-claude')).action, undefined);
});

test('windows: 瞬間値だけ高いプロセスは暴走扱いしない', () => {
  const spike = { pid: 10, name: 'ChatGPT', cpu: 8, avg_core: 5 };   // 8% x 20 = 160%/core だが平均は 5%
  const real = { pid: 11, name: 'PresentMon_x64', cpu: 8, avg_core: 74 };
  const f = analyze(win({ processes: { top_cpu: [spike, real], apps: [], apps_cpu: [] } }));
  assert.equal(f.filter((x) => x.id.startsWith('runaway-')).length, 1);
  assert.ok(f.find((x) => x.id === 'runaway-PresentMon_x64-11'));
});

test('システムディスクの空き 5% 未満は重大で、掃除コマンドを添える', () => {
  const f = analyze(mac({ disk: [{ mount: '/', total_gb: 460, free_gb: 16, free_pct: 3.5 }], caches: [{ path: '~/.npm/_cacache', gb: 4 }] }));
  const d = f.find((x) => x.id === 'disk-/');
  assert.equal(d.severity, 'critical');
  assert.ok(d.commands.includes('npm cache clean --force'));
});

test('電源プランがバランスなら高パフォーマンスを提案し、元の GUID を残す', () => {
  const bal = '381b4222-f694-41f0-9685-ff5bb260df2e';
  const f = analyze(win({ power: { plan_guid: bal, plan_name: 'バランス' } }));
  const p = f.find((x) => x.id === 'power-plan');
  assert.deepEqual(p.action.params, { guid: HIGH_PERF_GUID, prev_guid: bal });
});

test('共用機では実行を止めて提案だけにする', () => {
  const f = analyze(win({ power: { plan_guid: '381b4222-f694-41f0-9685-ff5bb260df2e', plan_name: 'バランス' } }), { shared: true });
  assert.ok(f.find((x) => x.id === 'power-plan').action.blocked);
});

test('予期しない停止が多い機体は重大', () => {
  const f = analyze(win({ stability_7d: { bugcheck_1001: 6, kernel_power_41: 6, unexpected_6008: 7 } }));
  assert.equal(f[0].id, 'stability');
  assert.equal(f[0].severity, 'critical');
});

test('WSL が大きく上限なしなら .wslconfig を提案する', () => {
  const f = analyze(win({ wsl: { config: false, memory: null }, processes: { top_cpu: [], apps: [{ app: 'vmmemWSL', mem_mb: 20000, count: 1 }], apps_cpu: [] } }));
  const w = f.find((x) => x.id === 'wsl-limit');
  assert.equal(w.severity, 'warn');
  assert.match(w.commands[0], /memory=32GB/);
});

test('前回比: 15% 以上の変化だけを遅い／速いと判定する', () => {
  assert.equal(compare({ bench: { median_ms: 100 } }, { bench: { median_ms: 120 } }).verdict, 'slower');
  assert.equal(compare({ bench: { median_ms: 100 } }, { bench: { median_ms: 80 } }).verdict, 'faster');
  assert.equal(compare({ bench: { median_ms: 100 } }, { bench: { median_ms: 105 } }).verdict, 'same');
  assert.equal(compare(null, { bench: { median_ms: 1 } }), null);
});

test('電源プラン: 機体に高パフォーマンスが無ければ提案しない', () => {
  const f = analyze(win({ power: { plan_guid: '381b4222-f694-41f0-9685-ff5bb260df2e', plan_name: 'バランス', plans: ['381b4222-f694-41f0-9685-ff5bb260df2e'] } }));
  assert.equal(f.find((x) => x.id === 'power-plan'), undefined);
});

test('ディスク: 実効の空き（自動で空く分を含む）で判定する', () => {
  // statvfs では 19GB（4%）でも、実効の空きが 108GB（23%）なら問題にしない（実測した例）
  const f = analyze(mac({ disk: [{ mount: '/', total_gb: 460, free_gb: 108, free_pct: 23.4, raw_free_gb: 19 }] }));
  assert.equal(f.find((x) => x.id.startsWith('disk-')), undefined);
});
