'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { nodeChecks, appChecks, failingJobs } = require('../lib/health');

const now = 1_800_000_000_000;

test('収集を無効にしたログイン元の旧cursorを失敗・鮮度判定に使わない', () => {
  for (const source of ['mac_auth', 'win_security']) {
    for (const node of [{ id: 'pc', network: false }, { id: 'pc', network_logins: false }, { id: 'pc', shared: true }]) {
      for (const disabled of [{ last_ok_at: now - 86_400_000 }, { last_error: 'denied', fail_streak: 4 }]) {
        const cursors = [{ node_id: 'pc', source: 'active', last_ok_at: now }, { node_id: 'pc', source, ...disabled }];
        const logs = nodeChecks(node, null, [], { now, cursors }).find(c => c.id === 'logs');
        assert.equal(logs.status, 'ok');
        assert.match(logs.detail, /^1 か所/);
        assert.equal(nodeChecks({ ...node, network: true, network_logins: true }, null, [], { now, cursors }).find(c => c.id === 'logs').status,
          disabled.last_error ? 'fail' : 'warn');
      }
    }
  }
});

test('無効なログイン元の旧cursorだけならログ取り込みは未実施', () => {
  const logs = nodeChecks({ id: 'pc', network: false }, null, [], { now, cursors: [{ node_id: 'pc', source: 'win_security', last_error: 'denied' }] }).find(c => c.id === 'logs');
  assert.equal(logs.status, 'unknown');
});
const snap = (data, ago = 5) => ({ at: now - ago * 60000, wall_s: 4, data });
const win = { probe: 'windows', host: { cores: 8 }, cpu_busy: 10, memory: { available_pct: 50, commit_pct: 40 }, disk: [{ mount: 'C:', free_gb: 300, free_pct: 30 }],
  stability_7d: { bugcheck_1001: 0, kernel_power_41: 0 }, defender: { realtime: true }, processes: { apps: [], top_cpu: [] },
  jobs: [
    { kind: 'schtask', id: '\\ok', name: 'ok', state: 'ready', last_result: 0 },
    { kind: 'schtask', id: '\\never', name: 'never', state: 'ready', last_result: 267011 },
    { kind: 'schtask', id: '\\bad', name: 'bad', state: 'ready', last_result: 1 },
    { kind: 'schtask', id: '\\off', name: 'off', state: 'disabled', last_result: 1 },
  ],
  third_party_services: [{ name: 'beszel-agent', state: 'running', start: 'auto' }, { name: 'stopped-svc', state: 'stopped', start: 'auto' }] };

test('失敗した定期処理: SCHED_S_* と無効のタスクは数えない', () => {
  assert.deepEqual(failingJobs(win.jobs).map((j) => j.name), ['bad']);
  assert.deepEqual(failingJobs([{ kind: 'launchd', scope: 'user', state: 'loaded', last_result: 2, name: 'x' }, { kind: 'launchd', scope: 'user', state: 'running', last_result: 255, name: 'y' }]).map((j) => j.name), ['x']);
});

test('HidHide updater の最新コードだけは正常とし、未知タスク・別コードを除外しない', () => {
  const task = { kind: 'schtask', id: '\\nefarius_HidHide_Updater', name: 'nefarius_HidHide_Updater', state: 'ready', last_result: 202 };
  assert.equal(failingJobs([task]).length, 0);
  assert.equal(failingJobs([{ ...task, name: 'unrelated', id: '\\unrelated' }]).length, 1);
  assert.equal(failingJobs([{ ...task, id: '\\custom\\nefarius_HidHide_Updater' }]).length, 1);
  assert.equal(failingJobs([{ ...task, last_result: 201 }]).length, 1);
  assert.equal(failingJobs([{ ...task, kind: 'launchd', scope: 'user' }]).length, 1);
});

test('機能チェック: 期待するサービス・定期処理・Defender・ディスク・取り込みを判定する', () => {
  const c = nodeChecks({ id: 'pc' }, snap(win), [], {
    now, expect: { services: ['beszel-agent', 'stopped-svc', 'missing'], jobs: ['\\bad'] },
    cursors: [{ node_id: 'pc', source: 'win_system', last_ok_at: now - 60000 }, { node_id: 'pc', source: 'neonmonitor', last_error: 'ssh timeout' }],
  });
  const st = Object.fromEntries(c.map((x) => [x.id, x.status]));
  assert.equal(st.probe, 'ok');
  assert.equal(st.jobs, 'warn');
  assert.equal(st['svc:beszel-agent'], 'ok');
  assert.equal(st['svc:stopped-svc'], 'fail');
  assert.equal(st['svc:missing'], 'fail');
  assert.equal(st['job:\\bad'], 'warn');
  assert.equal(st.logs, 'fail');
  assert.equal(st.defender, 'ok');
  assert.equal(st.disk, 'ok');
});

test('機能チェック: 分析が古い・失敗した・未実施を区別する', () => {
  assert.equal(nodeChecks({ id: 'a' }, snap(win, 500), [], { now, schedule: { probe_minutes: 60 } })[0].status, 'warn');
  assert.equal(nodeChecks({ id: 'a' }, snap(win), [], { now, lastError: 'ssh: timeout' })[0].status, 'fail');
  assert.equal(nodeChecks({ id: 'a' }, null, [], { now })[0].status, 'unknown');
});

test('アプリの状態: 台帳・DB・自動スキャン', () => {
  const c = appChecks({ now, nodeCount: 3, protectCount: 1, dbCheck: 'ok', dbBytes: 1e6, scheduler: { enabled: true, probe_minutes: 60, logs_minutes: 15, lastProbeAt: now - 3 * 3600e3 } });
  const st = Object.fromEntries(c.map((x) => [x.id, x.status]));
  assert.equal(st.config, 'ok');
  assert.equal(st.db, 'ok');
  assert.equal(st.scheduler, 'warn');
  assert.equal(appChecks({ now, configError: 'JSON の誤り', dbCheck: 'ok', dbBytes: 0, scheduler: {} })[0].status, 'fail');
});

test('定期処理: 台帳の expect.ignore_jobs（意図どおり非0で終わるもの）は失敗に数えず、既知の件数を根拠に書く', () => {
  const byId = (data, expect) => Object.fromEntries(nodeChecks({ id: 'pc' }, snap(data), [], { now, expect }).map((x) => [x.id, x]));
  assert.equal(byId(win).jobs.status, 'warn');
  const j = byId(win, { ignore_jobs: ['bad'] }).jobs;
  assert.equal(j.status, 'ok');
  assert.match(j.detail, /前回失敗なし（既知 1 件を除く）/);
  // id（\bad）でも名前（bad）でも外せる。外していないものは残る
  const two = { ...win, jobs: [...win.jobs, { kind: 'schtask', id: '\\bad2', name: 'bad2', state: 'ready', last_result: 2 }] };
  const c = byId(two, { ignore_jobs: ['\\bad'] }).jobs;
  assert.equal(c.status, 'warn');
  assert.equal(c.detail, '1 件が前回失敗: bad2（既知 1 件を除く）');
});
