'use strict';
// 同一コマンドの多重起動・孤児・プロセス木（issue 38・30）。probe の鍵作りと集計（mac_probe.py）と、所見（lib/rules.js）。
// 入力は架空の固定データだけ（機体の実データ・ユーザー名は使わない）。
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { analyze } = require('../lib/rules');

const ROOT = path.join(__dirname, '..');
const py = process.env.PYTHON || 'python3';

// mac_probe.py の関数を架空の入力で呼ぶ。code は JSON を print する
function runPy(code) {
  const r = spawnSync(py, ['-c', `import sys, json\nsys.path.insert(0, 'probes')\nimport mac_probe as m\n${code}`], { cwd: ROOT, encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  return JSON.parse(r.stdout);
}

const mac = (processes, totalGb = 64) => ({
  probe: 'mac', host: { cores: 10, uptime_h: 10 }, cpu_busy: 20,
  memory: { total_gb: totalGb, available_pct: 50, pressure: 'normal', swap_used_gb: 0, swap_total_gb: 0 },
  processes: { top_cpu: [], apps: [], apps_cpu: [], agent_processes: 0, ...processes },
  disk: [{ mount: '/', total_gb: 500, free_gb: 200, free_pct: 40 }], power: {}, containers: {}, caches: [], bench: { runs_ms: [100, 101, 99], median_ms: 100 },
});
const win = (processes, totalGb = 64) => ({
  probe: 'windows', host: { cores: 20, uptime_h: 10 }, cpu_busy: 10, cpu_perf_pct: 100,
  memory: { total_gb: totalGb, available_pct: 60, commit_pct: 40 },
  processes: { top_cpu: [], apps: [], apps_cpu: [], agent_processes: 0, ...processes },
  disk: [{ mount: 'C:', total_gb: 1000, free_gb: 500, free_pct: 50 }],
  power: { plan_guid: '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c', plan_name: 'high' }, wsl: { config: true, memory: '16GB' },
  stability_7d: { bugcheck_1001: 0, kernel_power_41: 0, unexpected_6008: 0 }, defender: { realtime: true },
});
const grp = (key, count, mem_mb, extra = {}) => ({ key, count, mem_mb, orphans: 0, orphan_mb: 0, parent: 'launcher', owner: 'user', ...extra });
const multi = (f) => f.filter((x) => /^(proc-multi|proc-orphan|mem-tree)-/.test(x.id));

test('probe: 鍵はスクリプト名だけで、引数・パス・ユーザー名は出ない', () => {
  const k = runPy(`
H = '/Use' + 'rs/alice'  # 架空のホーム（公開前提の検査に引っかからないよう連結で作る）
L = '/ho' + 'me/alice'
cases = [
  ('node', [H + '/.npm/_npx/ab12/node_modules/.bin/chrome-devtools-mcp', '--token', 'sk-SECRET']),
  ('node', ['--max-old-space-size=4096', '/opt/srv/mcp-pdf-server/dist/index.js', '--stdio']),
  ('node', ['/opt/srv/customer-secret/dist/index.js']),
  ('node', ['/opt/x/node_modules/@scope/pdf-mcp/dist/index.js']),
  ('node', [H + '/index.js']),
  ('node', [L + '/app/main.js']),
  ('python3', ['-m', 'some_module.sub', '--password=hunter2']),
  ('python3', ['-c', 'print("secret")']),
  ('python3.13', ['/srv/tools/watcher.py']),
  ('git', ['-c', 'core.fsmonitor=true', 'fsmonitor--daemon', 'run', '--detach']),
  ('npm', ['exec', 'chrome-devtools-mcp@latest', '--', '--api-key=zzz']),
  ('node', ['bad name with spaces']),
  ('Google Chrome Helper (Renderer)', ['--type=renderer', '--secret=1']),
  ('node', []),
  ('node', ['--token', 'ghp_' + 'A' * 36, 'app.js']),
  ('node', ['--max-old-space-size', '4096', 'server.js']),
  ('python3', ['-W', 'ignore', 'x.py']),
  ('npx', ['-y', 'some-mcp-server']),
]
print(json.dumps([m.proc_key(e, a) for e, a in cases]))
`);
  assert.deepEqual(k, [
    'node:chrome-devtools-mcp',
    'node',
    'node',
    'node:pdf-mcp',
    'node',
    'node',
    'python3:some_module',
    'python3',
    'python3.13:watcher',
    'git:fsmonitor--daemon',
    'npm:chrome-devtools-mcp',
    'node',
    'Google Chrome Helper (Renderer)',
    'node',
    'node',
    'node',
    'python3',
    'npx:some-mcp-server',
  ]);
  const text = JSON.stringify(k);
  assert.doesNotMatch(text, /alice|SECRET|hunter2|secret|zzz|ghp_|4096|ignore|mcp-pdf-server/);
});

test('probe(Windows): MxKey も値付きオプションの値を鍵に入れない', { skip: process.platform !== 'win32' }, () => {
  const fs = require('node:fs');
  const src = fs.readFileSync(path.join(ROOT, 'probes', 'win_probe.ps1')).subarray(3).toString('ascii');
  const start = src.indexOf('$mxInterp = ');
  const end = src.indexOf('$mxGroups = @()');
  assert.ok(start > 0 && end > start, 'MxKey の定義が見つからない');
  const cases = [
    ['node', ['--token', 'ghp_' + 'A'.repeat(36), 'app.js'], 'node'],
    ['node', ['--max-old-space-size', '4096', 'server.js'], 'node'],
    ['node', ['--max-old-space-size=4096', 'C:\\srv\\customer-secret\\dist\\index.js'], 'node'],
    ['node', ['C:\\x\\node_modules\\@scope\\pdf-mcp\\dist\\index.js'], 'node:pdf-mcp'],
    ['npx', ['-y', 'some-mcp-server'], 'npx:some-mcp-server'],
    ['git', ['-c', 'core.fsmonitor=true', 'fsmonitor--daemon', 'run'], 'git:fsmonitor--daemon'],
  ];
  const q = (v) => `'${v.replace(/'/g, "''")}'`;
  const calls = cases.map(([exe, args]) => `[Console]::WriteLine((MxKey ${q(exe)} @(${args.map(q).join(',')})))`).join('\n');
  const script = `${src.slice(start, end)}\n${calls}`;
  const enc = Buffer.from(script, 'utf16le').toString('base64');
  const r = spawnSync('powershell.exe', ['-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', enc], { encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.stdout.trim().split(/\r?\n/), cases.map((c) => c[2]));
});

test('probe: 本数・合計・孤児（PPID 1 / 親が居ない）・親・所有者を数える', () => {
  const r = runPy(`
rows = []
def add(pid, ppid, uid, kb, name, key=None):
    rows.append({'pid': pid, 'ppid': ppid, 'uid': uid, 'rss_kb': kb, 'name': name, 'key': key or name})
add(1, 0, 0, 1000, 'launchd')
add(100, 1, 501, 50000, 'claude')
for i in range(5): add(200 + i, 100, 501, 100 * 1024, 'node', 'node:chrome-devtools-mcp')        # 親が生きている
for i in range(3): add(300 + i, 1, 501, 100 * 1024, 'node', 'node:chrome-devtools-mcp')          # PPID 1 の孤児
for i in range(2): add(400 + i, 9999, 501, 100 * 1024, 'node', 'node:chrome-devtools-mcp')       # 親が居ない孤児
for i in range(3): add(500 + i, 1, 0, 10 * 1024, 'logd')                                         # システム所有
g, t = m.multiplication(rows)
print(json.dumps({'g': g, 't': t}))
`);
  const mcp = r.g.find((x) => x.key === 'node:chrome-devtools-mcp');
  assert.deepEqual(mcp, { key: 'node:chrome-devtools-mcp', count: 10, mem_mb: 1000, orphans: 5, orphan_mb: 500, parent: 'claude', owner: 'user' });
  const sys = r.g.find((x) => x.key === 'logd');
  assert.equal(sys.owner, 'system');
  assert.equal(sys.orphans, 0, 'システム所有の PPID 1 は孤児に数えない');
  // launchd の直下は根になり、launchd 自身は根に数えない。claude の木に子 5 本が入る
  const claude = r.t.find((x) => x.root === 'claude');
  assert.deepEqual(claude, { root: 'claude', pid: 100, owner: 'user', count: 6, children: 5, mem_mb: 549 });
});

test('probe: 子より後に起動した親は PID の使い回しとして孤児に数え、子の数にも入れない', () => {
  const r = runPy(`
rows = []
def add(pid, ppid, age, name, key=None):
    rows.append({'pid': pid, 'ppid': ppid, 'uid': 501, 'rss_kb': 1024, 'name': name, 'key': key or name, 'age': age})
add(50, 1, 100, 'newer')                        # 使い回された PID（子より新しい）
for i in range(3): add(60 + i, 50, 1000, 'node', 'node:x-mcp')
add(70, 1, 5000, 'older')                       # 本当の親（子より古い）
for i in range(3): add(80 + i, 70, 1000, 'node', 'node:y-mcp')
g, t = m.multiplication(rows)
print(json.dumps({'g': g, 't': t}))
`);
  const x = r.g.find((g) => g.key === 'node:x-mcp');
  const y = r.g.find((g) => g.key === 'node:y-mcp');
  assert.deepEqual([x.orphans, x.parent], [3, null]);
  assert.deepEqual([y.orphans, y.parent], [0, 'older']);
  assert.equal(r.t.find((t) => t.root === 'older').children, 3);
  assert.equal(r.t.some((t) => t.root === 'newer' && t.children > 0), false);
});

test('probe: 親子の循環があっても止まり、本数が少ない群は出さない', () => {
  const r = runPy(`
rows = [
  {'pid': 10, 'ppid': 11, 'uid': 501, 'rss_kb': 1024, 'name': 'a', 'key': 'a'},
  {'pid': 11, 'ppid': 10, 'uid': 501, 'rss_kb': 1024, 'name': 'b', 'key': 'b'},
]
print(json.dumps(m.multiplication(rows)))
`);
  assert.deepEqual(r[0], []);
  assert.ok(Array.isArray(r[1]));
});

test('所見: git fsmonitor が機体で増殖していれば本数・合計・親・提案（実行はしない）を出す', () => {
  // 観測例: 1,020 プロセス・約 12 GB（Windows）。同設定を切った機の 8 プロセスは出さない
  const f = multi(analyze(win({ groups: [grp('git:fsmonitor--daemon', 1020, 12 * 1024, { orphans: 1020, parent: null, owner: 'system' })] })));
  assert.equal(f.length, 1);
  assert.equal(f[0].id, 'proc-multi-git:fsmonitor--daemon');
  assert.match(f[0].detail, /所有者 システム（Windows はセッション 0・SSH を含む）/);
  assert.equal(f[0].severity, 'warn');
  assert.match(f[0].title, /1020 本.*12\.0 GB/);
  assert.match(f[0].detail, /親が終了済み.*1020 本/);
  assert.match(f[0].advice, /core\.fsmonitor false.*自動では変更しない/);
  assert.equal(f[0].action, undefined);
  assert.equal(f[0].commands, undefined);
  assert.deepEqual(multi(analyze(win({ groups: [grp('git:fsmonitor--daemon', 8, 90)] }))), []);
  const mid = multi(analyze(win({ groups: [grp('git:fsmonitor--daemon', 40, 480)] })));
  assert.equal(mid.length, 1);
  assert.equal(mid[0].severity, 'info', '39〜41 本・約 0.5 GB は info');
});

test('所見: MCP サーバのセッションごとの複製と孤児を出す', () => {
  const f = multi(analyze(mac({ groups: [
    grp('chrome-devtools-mcp', 13, 1424, { parent: 'claude' }),
    grp('node:mcp-pdf-server', 11, 552),
    grp('node:mcp', 34, 455, { parent: 'codex' }),
    grp('node:watchdog', 12, 602),
    grp('node:old-mcp', 2, 300, { orphans: 2, orphan_mb: 300, parent: null }),
  ] })));
  assert.deepEqual(f.map((x) => x.id).sort(), ['proc-multi-chrome-devtools-mcp', 'proc-multi-node:mcp', 'proc-multi-node:mcp-pdf-server', 'proc-orphan-node:old-mcp']);
  const byId = (id) => f.find((x) => x.id === id);
  const dev = byId('proc-multi-chrome-devtools-mcp');
  assert.match(dev.title, /13 本.*1\.4 GB/);
  assert.match(dev.detail, /親の多数派: claude/);
  assert.match(dev.advice, /MCP サーバ/);
  assert.match(byId('proc-multi-node:mcp-pdf-server').title, /552 MB/);
  const orphan = byId('proc-orphan-node:old-mcp');
  assert.equal(orphan.severity, 'warn');
  assert.match(orphan.title, /孤児が 2 本.*300 MB/);
  assert.match(orphan.detail, /親の多数派: 不明/);
  assert.match(orphan.advice, /取り残し/);
});

test('所見: 同名 renderer の大量起動は本数と合計の両方を超えたときだけ', () => {
  const f = multi(analyze(mac({ groups: [grp('Google Chrome Helper (Renderer)', 49, 7 * 1024)] })));
  assert.equal(f.length, 1);
  assert.match(f[0].title, /49 本.*7\.0 GB/);
  assert.equal(f[0].severity, 'warn', '実メモリの 10% 以上');
  assert.deepEqual(multi(analyze(mac({ groups: [grp('Google Chrome Helper (Renderer)', 12, 3000)] }))), [], '本数が少ない');
  assert.deepEqual(multi(analyze(mac({ groups: [grp('some-helper', 60, 500)] }))), [], '合計が小さい');
});

test('所見: 増殖の無い機体・OS の定常的な多重・システム所有の不明な群は出さない（偽陽性なし）', () => {
  const healthy = [
    grp('git:fsmonitor--daemon', 8, 85),
    grp('node:chrome-devtools-mcp', 3, 300),
    grp('Google Chrome Helper (Renderer)', 20, 2500),
    grp('svchost', 101, 2006, { owner: 'mixed' }),
    grp('com.apple.WebKit.WebContent', 80, 3000, { orphans: 80, parent: 'launchd' }),
    grp('mystery', 90, 5000, { owner: 'system' }),
    grp('conhost', 44, 337, { owner: 'mixed' }),
    grp('node', 40, 3000),
  ];
  assert.deepEqual(multi(analyze(win({ groups: healthy }))), []);
  assert.deepEqual(multi(analyze(mac({ groups: healthy }))), []);
  assert.deepEqual(multi(analyze(mac({}))), [], 'groups が無い古い snapshot でも壊れない');
  assert.deepEqual(multi(analyze(mac({ groups: null, trees: 'x' }))), []);
});

test('所見: プロセス木は群の合計・根・所有者・子の数を示し、単一プロセスの RSS で止める判断をしない', () => {
  const trees = [
    { root: 'BigApp', pid: 4242, owner: 'user', count: 40, children: 12, mem_mb: 15 * 1024 },
    { root: 'vmcompute', pid: 4, owner: 'system', count: 3, children: 1, mem_mb: 20 * 1024 },
    { root: 'Small', pid: 9, owner: 'user', count: 2, children: 1, mem_mb: 20 * 1024 },
    { root: 'Dup', pid: 8, owner: 'user', count: 9, children: 3, mem_mb: 20 * 1024 },
  ];
  const f = analyze(mac({ trees, apps: [{ app: 'Dup', mem_mb: 20 * 1024, count: 9, cpu: 1 }] }));
  const t = f.filter((x) => x.id.startsWith('mem-tree-'));
  assert.deepEqual(t.map((x) => x.id), ['mem-tree-BigApp'], 'システム所有・2 プロセス・mem-hog と重なる根は出さない');
  assert.match(t[0].title, /15\.0 GB.*23%.*40 プロセス/);
  assert.match(t[0].detail, /根: BigApp（PID 4242）.*直下の子 12 本.*所有者 ユーザー.*群の合計/);
  assert.match(t[0].advice, /単一プロセスの RSS だけで止める判断はしない/);
  assert.equal(t[0].action, undefined);
  assert.equal(f.some((x) => x.action && /^(proc-|mem-tree)/.test(x.id)), false, '終了の操作は付けない');
});

test('出力に本文・引数・環境変数・パスを持ち込まない: 所見は鍵・名前・数だけから作る', () => {
  const f = multi(analyze(mac({ groups: [grp('node:chrome-devtools-mcp', 13, 1424, { parent: 'claude' })] })));
  assert.doesNotMatch(JSON.stringify(f), /\/Users\/|--token|--password|API_KEY|=/);
});
