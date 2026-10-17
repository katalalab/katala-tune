// WSL の中の観測: 所見（lib/wsl.js・rules.analyze 経由）と、probes/win_probe.ps1 の安全性（停止中の distro に触れない構造）。
// 実機の wsl.exe は呼ばない。PowerShell 側の振る舞い（偽の wsl.exe）は test/win-wsl-regression.ps1（Windows PowerShell 5.1）で確かめる。
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const wsl = require('../lib/wsl');
const { analyze } = require('../lib/rules');

const PROBE = path.join(__dirname, '..', 'probes', 'win_probe.ps1');
const running = (name, availMb, totalMb = 24000, extra = {}) => ({ name, state: 'running', observed: true, reason: null, inside: { mem_total_mb: totalMb, mem_available_mb: availMb, ...extra } });
const stopped = (name) => ({ name, state: 'stopped_unobserved', observed: false, reason: 'not-running', inside: null });
const win = (wslData, vmMb = null) => ({
  probe: 'windows', host: { cores: 16 }, memory: { total_gb: 64, available_pct: 60, commit_pct: 40 }, disk: [], jobs: [],
  processes: { top_cpu: [], apps: vmMb ? [{ app: 'vmmemWSL', mem_mb: vmMb, count: 1 }] : [], apps_cpu: [] },
  wsl: { config: true, memory: '24GB', ...wslData },
});
const ids = (snap) => analyze(snap).map((f) => f.id);
const find = (snap, id) => analyze(snap).find((f) => f.id === id);

test('内側のメモリが 90% 以上の running distro は警告。PSI と上限を根拠に書く', () => {
  const f = find(win({ distros: [running('dev-a', 1000, 24000, { psi_some_avg10: 12.5 }), running('dev-b', 20000)] }), 'wsl-inside-memory');
  assert.equal(f.severity, 'warn');
  assert.match(f.title, /96%.*dev-a/);
  assert.doesNotMatch(f.title, /dev-b/);
  assert.match(f.detail, /22\.5 GB \/ WSL の VM の上限 23\.4 GB/);
  assert.match(f.detail, /PSI some 12\.5%/);
});

test('Windows 側の vmmem が内側の実使用より多く持っていれば提案。distro は VM を共有するので足さずに最大を使う', () => {
  const snap = win({ distros: [running('dev-a', 20000), running('dev-b', 19800)] }, 20000);
  const f = find(snap, 'wsl-reclaim');
  assert.equal(f.severity, 'info');
  assert.match(f.title, /約 15\.\d GB 多く保持/);
  assert.match(f.detail, /vmmem 19\.5 GB、内側の使用 4.1 GB/);
  // 小さい差・小さい VM・観測できていない distro があるときは言わない
  assert.ok(!ids(win({ distros: [running('dev-a', 20000)] }, 6000)).includes('wsl-reclaim'));
  assert.ok(!ids(win({ distros: [running('dev-a', 5000)] }, 20000)).includes('wsl-reclaim'));
  assert.ok(!ids(win({ distros: [running('dev-a', 20000), { name: 'dev-b', state: 'running', observed: false, reason: 'timeout', inside: null }] }, 20000)).includes('wsl-reclaim'));
});

test('停止中・一覧不明・観測失敗は「観測していない」として所見に出る（正常でも空でもない）', () => {
  const f = find(win({ distros: [running('dev-a', 20000), stopped('old'), { name: 'dev-c', state: 'running', observed: false, reason: 'timeout', inside: null }, { name: 'dev-d', state: 'unknown', observed: false, reason: 'running-list-failed', inside: null }] }), 'wsl-unobserved');
  assert.equal(f.severity, 'info');
  assert.match(f.title, /3 件は中を観測していない/);
  assert.match(f.detail, /停止中（起動しないので未観測）: old/);
  assert.match(f.detail, /dev-c（時間切れ）/);
  assert.match(f.detail, /dev-d（動いているかを読めなかった/);
  assert.match(f.advice, /起動はしない/);
});

test('Docker Desktop の内部 distro と、distro が無い／観測が正常な場合は何も言わない', () => {
  const internal = { name: 'docker-desktop', state: 'running', observed: false, reason: 'internal-distro-skipped', inside: null };
  for (const distros of [[], [running('dev-a', 20000)], [running('dev-a', 20000), internal]]) {
    assert.deepEqual(ids(win({ distros })).filter((i) => i.startsWith('wsl-') && i !== 'wsl-limit'), []);
  }
  assert.deepEqual(wsl.findings({ wsl: { distros: 'x' } }), []);
  assert.deepEqual(wsl.findings({}), []);
  assert.equal(wsl.usedMb({ mem_total_mb: 0, mem_available_mb: 1 }), null);
});

test('従来の snapshot（distros 無し）の所見は変わらない', () => {
  const before = ids(win({}));
  const withEmpty = ids(win({ installed: true, listed: true, distros: [] }));
  assert.deepEqual(withEmpty, before);
});

// ---- probe の構造（停止中の distro に触れない） ----
const source = fs.readFileSync(PROBE);
const text = source.subarray(3).toString('latin1');
const region = text.slice(text.indexOf('# --- WSL observation (read-only) begin ---'), text.indexOf('# --- WSL observation (read-only) end ---'));
const code = region.split('\n').filter((l) => !/^\s*#/.test(l)).join('\n');

test('win_probe.ps1 は ASCII・BOM 付きで、WSL の観測区間がある', () => {
  assert.deepEqual([...source.subarray(0, 3)], [0xef, 0xbb, 0xbf]);
  assert.ok(source.subarray(3).every((b) => b <= 0x7f), 'ASCII 以外が含まれる');
  assert.ok(region.length > 500);
});

test('WSL の内部観測は ObserveWsl の明示 opt-in が無ければ実行されない', () => {
  assert.match(text, /param\(\[switch\]\$NoNetwork,\s*\[switch\]\$ObserveWsl\)/);
  assert.match(region, /function Get-KtWslObservation\(\[bool\]\$Enabled = \$false\)/);
  assert.match(region, /if \(-not \$Enabled\) \{ return \$r \}/);
  assert.match(text, /Get-KtWslObservation -Enabled:\$ObserveWsl/);
});

test('WSL の観測区間は wsl.exe を1か所でしか起動せず、変更系の操作を含まない', () => {
  assert.equal((code.match(/ProcessStartInfo/g) || []).length, 1);
  assert.equal((code.match(/--exec/g) || []).length, 1, '中でコマンドを実行する呼び出しは1つだけ');
  for (const bad of ['--shutdown', '--terminate', '--unregister', '--import', '--export', '--install', '--update', '--set-', '--mount', '--unmount', 'Stop-Service', 'Start-Service', 'Restart-Service']) {
    assert.ok(!code.toLowerCase().includes(bad.toLowerCase()), `${bad} を含む`);
  }
  // distro の中に入る呼び出しは、走っている distro の分岐（else）の中だけ
  const exec = code.indexOf('--exec');
  assert.ok(code.lastIndexOf("elseif ($runNames -notcontains $name)", exec) >= 0);
  assert.ok(code.indexOf("$d.state = 'stopped_unobserved'") < exec);
  assert.ok(code.indexOf('Invoke-KtWsl @(\'-d\'') > code.lastIndexOf("$d.state = 'stopped_unobserved'"));
});
