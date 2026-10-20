'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { analyze, score, compare, processDecision, HIGH_PERF_GUID } = require('../lib/rules');

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

const cacheCases = require('./fixtures/cache-accounting.json');
const cacheInspect = 'du -sh ~/Library/Caches/* 2>/dev/null | sort -h | tail -15   # 大きいものを確認';
for (const c of cacheCases) test(`cache accounting: ${c.name}`, () => {
  const findings = analyze(c.snap);
  const cache = findings.find((f) => f.id === 'caches');
  const storage = findings.find((f) => f.id === 'vm-storage');
  if (c.cache_gib == null) assert.equal(cache, undefined);
  else {
    assert.ok(cache);
    assert.equal(cache.title, `再生成可能なキャッシュ候補が ${c.cache_gib} GiB`);
    assert.equal(cache.detail, c.cache_detail);
    assert.match(cache.advice, /確認/);
    assert.equal(cache.action, undefined);
  }
  if (c.storage_gib == null) assert.equal(storage, undefined);
  else {
    assert.ok(storage);
    assert.equal(storage.severity, 'info');
    assert.equal(storage.category, 'disk');
    assert.equal(storage.title, `VM・シミュレータの保存領域（削除候補ではない）: ${c.storage_gib} GiB`);
    assert.equal(storage.detail, `${c.storage_detail}。ユーザーデータを含むため、再生成可能なキャッシュには数えない。`);
    assert.equal(storage.commands, undefined);
    assert.equal(storage.action, undefined);
  }
  for (const f of findings) {
    assert.doesNotMatch((f.commands || []).join('\n'), /docker .*prune|simctl delete/);
    if (f.id === 'caches' || f.id === 'disk-/') {
      assert.match(f.advice, /確認/);
      assert.doesNotMatch(f.advice, /消しても|削除コマンドから始める/);
      if (c.cleanup_commands) assert.deepEqual(f.commands, [...c.cleanup_commands, cacheInspect]);
    }
  }
});

test('cache accounting: invalid or missing parents do not suppress a measured child', () => {
  for (const gb of [undefined, null, '20', -1, true, {}, [], NaN, Infinity, -Infinity]) {
    const f = analyze({ probe: 'mac', caches: [
      { path: '~/Library/Caches', gb }, { path: '~/Library/Caches/Homebrew', gb: 12 },
    ] }).find((f) => f.id === 'caches');
    assert.ok(f);
    assert.equal(f.detail, '~/Library/Caches/Homebrew 12 GiB');
  }
});

test('cache accounting: non-finite measurements never produce findings or cleanup commands', () => {
  for (const gb of [NaN, Infinity, -Infinity]) {
    const findings = analyze({ probe: 'mac', caches: [
      { path: '~/.npm/_cacache', gb }, { path: '~/.colima', gb },
    ], disk: [{ mount: '/', total_gb: 500, free_gb: 10, free_pct: 2 }] });
    assert.equal(findings.length, 1);
    assert.deepEqual(findings[0].commands, [cacheInspect]);
  }
});

test('健全な機体は所見なしで 100 点', () => {
  assert.deepEqual(analyze(mac()), []);
  assert.equal(score(analyze(win())), 100);
});

test('mac: CPU と名前だけでは終了を提案せず、システムは引き続き保護する', () => {
  const f = analyze(mac({ processes: { top_cpu: [
    { pid: 500, name: 'Google Drive', app: 'Google Drive', cpu: 105 },
    { pid: 414, name: 'WindowServer', app: 'WindowServer', cpu: 90 },
    { pid: 900, name: 'claude', app: 'claude', cpu: 120 },
  ], apps: [], apps_cpu: [] } }));
  const drive = f.find((x) => x.id.startsWith('runaway-Google Drive'));
  assert.equal(drive.action, undefined);
  assert.equal(drive.decision.proposed_action, null);
  assert.equal(drive.decision.remediation.class, 'manual_only');
  assert.ok(drive.decision.needed_observation.includes('pid_name_start_owner_and_parent'));
  assert.match(drive.advice, /CPU と名前だけでは不要/);
  assert.match(drive.advice, /Google Drive/);
  assert.equal(f.find((x) => x.id.startsWith('runaway-WindowServer')).action, undefined);
  assert.equal(f.find((x) => x.id.startsWith('runaway-claude')).action, undefined);
});

test('process decision: 確認済みの重複だけが承認必須の終了候補になる', () => {
  const now = Date.now();
  const p = {
    pid: 500, ppid: 1, name: 'ExampleWorker', cpu: 105, start: 'Thu Oct  1 08:12:03 2026',
    decision: {
      evidence_state: 'observed', reason_status: 'confirmed', process_class: 'duplicate_candidate', collector_self: false,
      owner_observed: true, service_match: true, scheduler_match: true, workload_match: true, protect_ledger_readable: true,
      evidence: [{ run: 'r1', scope: 'process', source_sha256: 'a'.repeat(64), observed_at: now - 1000, rowref: 'p500' }],
      cpu_samples: [
        { pid: 500, start: 'Thu Oct  1 08:12:03 2026', value: 90, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: now - 2000 },
        { pid: 500, start: 'Thu Oct  1 08:12:03 2026', value: 91, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: now - 1000 },
      ],
    },
  };
  const f = analyze(mac({ at: now, processes: { top_cpu: [p], apps: [], apps_cpu: [] } }), { hold: false, shared: false });
  const finding = f.find((x) => x.id === 'runaway-ExampleWorker-500');
  assert.deepEqual(finding.action.params, { pid: 500, name: 'ExampleWorker', start: 'Thu Oct  1 08:12:03 2026', min_cpu: 50 });
  assert.equal(finding.decision.remediation.class, 'requires_approval');
  assert.equal(finding.decision.remediation.can_auto_fix, false);
  assert.equal(finding.decision.symptom, 'repeated_high_cpu_samples');
  assert.notEqual(finding.decision.symptom, 'sustained_high_cpu_process');
  assert.match(finding.advice, /重複候補を確認/);
});

test('process decision: shared、collector、identity、CPU 根拠が明示されなければ停止候補を作らない', () => {
  const now = Date.now();
  const base = () => ({
    pid: 500, ppid: 1, name: 'ExampleWorker', start: 'Thu Oct  1 08:12:03 2026',
    decision: {
      evidence_state: 'observed', reason_status: 'confirmed', process_class: 'duplicate_candidate', owner_observed: true, collector_self: false,
      service_match: true, scheduler_match: true, workload_match: true, protect_ledger_readable: true,
      evidence: [{ run: 'r1', scope: 'process', source_sha256: 'a'.repeat(64), observed_at: now - 1000, rowref: 'p500' }],
      cpu_samples: [
        { pid: 500, start: 'Thu Oct  1 08:12:03 2026', value: 90, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: now - 2000 },
        { pid: 500, start: 'Thu Oct  1 08:12:03 2026', value: 91, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: now - 1000 },
      ],
    },
  });
  for (const node of [{ hold: false }, { hold: false, shared: null }, { hold: false, shared: true }, { hold: false, shared: 'false' }]) {
    const finding = processDecision(base(), node, false, now);
    assert.equal(finding.proposed_action, null);
    assert.ok(finding.needed_observation.includes('explicit_node_shared_clear'));
  }
  for (const collector_self of [undefined, null, true, 'false']) {
    const p = base();
    if (collector_self === undefined) delete p.decision.collector_self;
    else p.decision.collector_self = collector_self;
    const finding = processDecision(p, { hold: false, shared: false }, false, now);
    assert.equal(finding.proposed_action, null);
    assert.ok(finding.needed_observation.includes('collector_self_excluded'));
  }
  for (const patch of [{ ppid: null }, { ppid: 0 }, { ppid: -1 }, { name: null }, { name: 7 }, { name: 'unknown' }]) {
    const p = { ...base(), ...patch };
    const finding = processDecision(p, { hold: false, shared: false }, false, now);
    assert.equal(finding.proposed_action, null);
    assert.ok(finding.needed_observation.includes('pid_name_start_owner_and_parent'));
  }
  const missingName = base();
  delete missingName.name;
  const missingNameFinding = processDecision(missingName, { hold: false, shared: false }, false, now);
  assert.equal(missingNameFinding.proposed_action, null);
  assert.ok(missingNameFinding.needed_observation.includes('pid_name_start_owner_and_parent'));
  for (const values of [[0, 0], [49.9, 91], [10001, 91]]) {
    const p = base();
    p.decision.cpu_samples[0].value = values[0];
    p.decision.cpu_samples[1].value = values[1];
    const finding = processDecision(p, { hold: false, shared: false }, false, now);
    assert.equal(finding.proposed_action, null);
    assert.ok(finding.needed_observation.includes('two_cpu_time_samples'));
  }
  const positive = processDecision(base(), { hold: false, shared: false }, false, now);
  assert.equal(positive.proposed_action.params.min_cpu, 50);
});

test('process decision: 古い snapshot.at と同時期の observed 根拠でも入口で停止候補を作らない', () => {
  const staleAt = Date.now() - 300001;
  const p = {
    pid: 500, ppid: 1, name: 'ExampleWorker', cpu: 105, start: 'Thu Oct  1 08:12:03 2026',
    decision: {
      evidence_state: 'observed', reason_status: 'confirmed', process_class: 'duplicate_candidate', owner_observed: true,
      service_match: true, scheduler_match: true, workload_match: true, protect_ledger_readable: true,
      evidence: [{ run: 'r1', scope: 'process', source_sha256: 'a'.repeat(64), observed_at: staleAt, rowref: 'p500' }],
      cpu_samples: [
        { pid: 500, start: 'Thu Oct  1 08:12:03 2026', value: 90, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: staleAt - 1000 },
        { pid: 500, start: 'Thu Oct  1 08:12:03 2026', value: 91, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: staleAt },
      ],
    },
  };
  const finding = analyze(mac({ at: staleAt, processes: { top_cpu: [p], apps: [], apps_cpu: [] } }), { hold: false, shared: false }).find((x) => x.id === 'runaway-ExampleWorker-500');
  assert.equal(finding.action, undefined);
  assert.equal(finding.decision.proposed_action, null);
  assert.ok(finding.decision.needed_observation.includes('fresh_timestamp_and_clock_consistency'));
});

test('process decision: observed でも stale、future、異種 CPU 標本では停止候補を作らない', () => {
  const p = {
    pid: 500, ppid: 1, name: 'ExampleWorker', cpu: 105, start: 'Thu Oct  1 08:12:03 2026',
    decision: {
      evidence_state: 'observed', reason_status: 'confirmed', process_class: 'duplicate_candidate', owner_observed: true,
      service_match: true, scheduler_match: true, workload_match: true, protect_ledger_readable: true,
      evidence: [{ run: 'r1', scope: 'process', source_sha256: 'a'.repeat(64), observed_at: 1760000001000, rowref: 'p500' }],
      cpu_samples: [
        { pid: 500, start: 'Thu Oct  1 08:12:03 2026', value: 90, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: 1760000000000 },
        { pid: 500, start: 'Thu Oct  1 08:12:03 2026', value: 91, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 2000, at: 1760000001000 },
      ],
    },
  };
  const finding = analyze(mac({ at: 1760000002000, processes: { top_cpu: [p], apps: [], apps_cpu: [] } }), { hold: false, shared: false }).find((x) => x.id === 'runaway-ExampleWorker-500');
  assert.equal(finding.action, undefined);
  assert.equal(finding.decision.proposed_action, null);
  assert.ok(finding.decision.needed_observation.includes('two_cpu_time_samples'));
});

test('process decision: private process fields never enter public finding JSON', () => {
  const p = { pid: 500, name: 'ExampleWorker', cpu: 105, argv: ['--token=secret'], owner: 'private-owner', path: '/private/path' };
  const finding = analyze(mac({ processes: { top_cpu: [p], apps: [], apps_cpu: [] } })).find((x) => x.id === 'runaway-ExampleWorker-500');
  assert.doesNotMatch(JSON.stringify(finding.decision), /secret|private-owner|\/private\/path/);
});

test('windows: 瞬間値だけ高いプロセスは暴走扱いしない', () => {
  const spike = { pid: 10, name: 'ChatGPT', cpu: 8, avg_core: 5 };   // 8% x 20 = 160%/core だが平均は 5%
  const real = { pid: 11, name: 'PresentMon_x64', cpu: 8, avg_core: 74 };
  const f = analyze(win({ processes: { top_cpu: [spike, real], apps: [], apps_cpu: [] } }));
  assert.equal(f.filter((x) => x.id.startsWith('runaway-')).length, 1);
  assert.ok(f.find((x) => x.id === 'runaway-PresentMon_x64-11'));
});

test('起動から2分未満のプロセス（すぐ終わるバッチ）は暴走扱いしない。経過が分からないものは数える', () => {
  const top_cpu = [
    { pid: 446, name: 'python3', cpu: 88, etime: '00:32' },
    { pid: 447, name: 'python3', cpu: 88, etime: '02:00' },
    { pid: 448, name: 'node', cpu: 95, etime: '1-02:03:04' },
    { pid: 449, name: 'ruby', cpu: 90 },
  ];
  const ids = analyze(mac({ processes: { top_cpu, apps: [], apps_cpu: [] } })).filter((x) => x.id.startsWith('runaway-')).map((x) => x.id);
  assert.deepEqual(ids, ['runaway-python3-447', 'runaway-node-448', 'runaway-ruby-449']);
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

test('launchd: 起動制約（CODESIGNING）で止められたジョブは登録し直しを提案する', () => {
  const plist = '/Users/me/Library/LaunchAgents/com.example.job.plist';
  const f = analyze(mac({
    launchd_failing: [
      { label: 'com.example.job', exit: '78', reason: 'OS_REASON_CODESIGNING | Launch Constraint Violation' },
      { label: 'com.example.other', exit: '1', reason: 'OS_REASON_EXIT' },
      { label: 'com.example.noplist', exit: '78', reason: 'OS_REASON_CODESIGNING' },
    ],
    jobs: [{ kind: 'launchd', id: 'com.example.job', name: 'com.example.job', scope: 'user', plist }],
  }));
  const c = f.find((x) => x.id === 'launchd-constraint-com.example.job');
  assert.equal(c.title, 'com.example.job が起動制約で止められている（実行ファイルの更新後）');
  assert.match(c.advice, /bootout → bootstrap/);
  assert.deepEqual(c.commands, [`launchctl bootout gui/$(id -u)/com.example.job\nlaunchctl bootstrap gui/$(id -u) ${plist}`]);
  assert.equal(f.find((x) => x.id === 'launchd-constraint-com.example.other'), undefined);
  assert.equal(f.find((x) => x.id === 'launchd-constraint-com.example.noplist').commands, undefined, 'plist が分からなければコマンドは添えない');
});
