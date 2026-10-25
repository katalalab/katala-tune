// Issue #31: GPU 計算中の停止・電力操作を、提案と実行直前の両方で止める。
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { analyze } = require('../lib/rules');
const { plan, execute } = require('../lib/actions');
const { actionExchange } = require('../lib/collect');
const { makeRunner } = require('../lib/runner');
const { loadConfig } = require('../lib/nodes');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');
const { EventEmitter } = require('node:events');
const net = require('node:net');
const { createRequire } = require('node:module');
const { PassThrough } = require('node:stream');
const vm = require('node:vm');
const probe = fs.readFileSync(path.join(__dirname, '..', 'probes', 'win_probe.ps1'), 'utf8');
const windowsPowerShell = process.env.KT_GPU_POWERSHELL || 'powershell.exe';

async function exchangeAfterRequest(exchange, script, options = {}) {
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  let timeout;
  const barrier = new Promise((resolve, reject) => {
    timeout = setTimeout(() => reject(new Error('request barrier unavailable')), 1000);
    server.once('connection', (socket) => {
      socket.once('error', reject);
      socket.once('data', (data) => { clearTimeout(timeout); resolve(data.toString('ascii')); });
    });
  });
  const pending = exchange(script(port), options);
  try {
    const marker = await barrier;
    return { marker, result: await pending };
  } finally { server.close(); }
}

const node = { id: 'gpu-node', alias: 'mock-gpu', os: 'windows', hold: false, shared: false };
const now = Date.now();
const worker = () => ({
  pid: 4242, ppid: 1, name: 'TrainerHost', app: 'TrainerHost', cpu: 8, avg_core: 80, start: '20261021010101000',
  decision: {
    evidence_state: 'observed', reason_status: 'confirmed', process_class: 'duplicate_candidate', collector_self: false,
    owner_observed: true, service_match: true, scheduler_match: true, workload_match: true, protect_ledger_readable: true,
    evidence: [{ run: 'r1', scope: 'process', source_sha256: 'a'.repeat(64), observed_at: now - 1000, rowref: 'p4242' }],
    cpu_samples: [
      { pid: 4242, start: '20261021010101000', value: 90, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: now - 2000 },
      { pid: 4242, start: '20261021010101000', value: 91, unit: 'percent_of_core', definition: 'cpu_time_delta', sample_window_ms: 1000, at: now - 1000 },
    ],
  },
});
const snapshot = (gpu_compute, p = worker()) => ({ probe: 'windows', at: now, host: { cores: 20 }, gpus: [{ util: 0 }], gpu_compute, processes: { top_cpu: [p], apps: [], apps_cpu: [] } });

test('GPU compute PID（使用メモリ N/A を含む）と GPU の親子印は、名前に関わらず終了候補にしない', () => {
  for (const p of [worker(), { ...worker(), gpu_compute: true }]) {
    const s = snapshot({ availability: 'available', apps: [{ pid: 4242, used_memory_mb: null }] }, p);
    const finding = analyze(s, node).find((f) => f.id === 'runaway-TrainerHost-4242');
    assert.equal(finding.action, undefined);
    assert.equal(finding.decision.proposed_action, null);
    assert.equal(finding.decision.reason.code, 'protected_workload');
  }
});

test('GPU 使用率または新規 probe の取得不能は PID を特定せず機体全体の停止候補を保留し、空の成功結果は従来どおり通す', () => {
  for (const s of [
    snapshot({ availability: 'available', apps: [] }, worker()),
    { ...snapshot({ availability: 'absent', apps: [] }), gpus: [{ util: null }] },
    { ...snapshot({ availability: 'unavailable', apps: [], reason: 'query-failed' }), gpus: [{ util: null }] },
    { ...snapshot({ availability: 'available', apps: [] }), gpus: [{ util: 5 }] },
    { ...snapshot({ availability: 'available' }), gpus: [{ util: 0 }] },
    { ...snapshot({ availability: 'available', apps: [] }), gpus: [{ util: null }] },
    { ...snapshot({ availability: 'available', apps: [] }), gpus: [{ util: -1 }] },
  ]) {
    const finding = analyze(s, node).find((f) => f.id === 'runaway-TrainerHost-4242');
    if (s.gpu_compute.availability === 'absent' || (s.gpu_compute.availability === 'available' && Array.isArray(s.gpu_compute.apps) && s.gpu_compute.apps.length === 0 && s.gpus[0].util === 0)) assert.ok(finding.action);
    else assert.equal(finding.action, undefined);
  }
});

test('Windows の終了と GPU 電力変更は、直前に GPU 使用率・compute PID・親子を再確認する', () => {
  const kill = plan(node, { type: 'kill-process', params: { pid: 4242, name: 'TrainerHost', start: '20261021010101000' } }, { protect: [] });
  const power = plan(node, { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } }, { protect: [] });
  for (const p of [kill, power]) {
    assert.match(p.script, /--query-gpu=uuid,utilization\.gpu/);
    assert.match(p.script, /--query-compute-apps=pid,used_gpu_memory/);
    assert.match(p.script, /exit 9/);
    assert.ok(!p.script.includes("'"));
  }
  assert.match(kill.script, /Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,CreationDate/);
  assert.match(power.script, /operation refused.*Test-KatalaActionExpiry; \$script:KatalaGpuMutationStarted = \$true; \$applyRows = @\(Invoke-KatalaGpuSmi/);
  assert.match(power.script, /GPU power apply outcome unverified/);
  assert.doesNotMatch(power.script, /; & nvidia-smi\.exe --id=/);
  assert.match(power.script, /No running processes found/);
  assert.match(kill.script, /\[double\]::IsNaN\(\$util\).*\[double\]::IsInfinity\(\$util\)/);
  assert.match(kill.script, /\[int\]::TryParse\(\$part\[0\]\.Trim\(\), \[ref\]\$gpuPid\)/);
  assert.match(kill.script, /Get-CimInstance Win32_VideoController -Property Name,PNPDeviceID/);
  assert.match(probe, /gpu_compute = \[pscustomobject\]\$gpuCompute/);
  assert.match(probe, /No running processes found/);
  assert.match(probe, /used_memory_mb = \$used/);
});

test('GPU SMI の外部 subprocess は両 pipe を同時に 64 KiB まで読み、終了 ACK を確認する', () => {
  const kill = plan(node, { type: 'kill-process', params: { pid: 4242, name: 'TrainerHost', start: '20261021010101000' } }, { protect: [] });
  const power = plan(node, { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } }, { protect: [] });
  for (const p of [kill, power]) {
    assert.match(p.script, /BaseStream\.ReadAsync\(\$outBuffer, 0, \$outBuffer\.Length\)/);
    assert.match(p.script, /BaseStream\.ReadAsync\(\$errBuffer, 0, \$errBuffer\.Length\)/);
    assert.match(p.script, /\$stdout\.Length \+ \$outCount -gt 65536/);
    assert.match(p.script, /\$stderrCount \+ \$errCount -gt 65536/);
    assert.match(p.script, /\[Threading\.Tasks\.Task\]::WaitAny/);
    assert.match(p.script, /\$killAck = \$Process\.WaitForExit\(\[Math\]::Min\(1000, \$left\)\); if \(-not \$killAck -or -not \$Process\.HasExited\)/);
    assert.match(p.script, /\$script:KatalaGpuSmiTransportFailed/);
    assert.match(p.script, /\$script:KatalaGpuSmiCleanupAttempted/);
    assert.match(p.script, /if \(\$failed\) \{ \$script:KatalaGpuSmiTransportFailed = \$true;/);
    assert.match(p.script, /\$p\.StartInfo = \$si; \$smiDeadline = \[Math\]::Min\(9000, \[int\]\$script:KatalaGpuGuardClock\.ElapsedMilliseconds \+ 4000\); \[void\]\$p\.Start\(\);/);
    assert.match(p.script, /if \(\$script:KatalaGpuMutationStarted\) \{ Write-Output "GPU power outcome unverified"; exit 7 \}/);
    assert.doesNotMatch(p.script, /ReadToEndAsync/);
  }
});

test('GPU SMI の native Framework fixture は pipe 境界・held EOF・timeout を direct helper で検証する', { skip: process.platform !== 'win32' }, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-gpu-smi-framework-'));
  const fixture = path.join(dir, 'fixture.exe');
  const childPid = path.join(dir, 'held-eof.pid');
  const spawnLog = path.join(dir, 'spawn.log');
  const source = `using System;
using System.Diagnostics;
using System.IO;
using System.Threading;
public static class KatalaGpuFixture {
  static void Write(Stream stream, int count) { byte[] chunk = new byte[4096]; for (int i = 0; i < chunk.Length; i++) chunk[i] = 120; while (count > 0) { int n = Math.Min(count, chunk.Length); stream.Write(chunk, 0, n); count -= n; } stream.Flush(); }
  public static int Main(string[] args) {
    string mode = args.Length == 0 ? "" : args[0];
    string log = Environment.GetEnvironmentVariable("KT_GPU_SPAWN_LOG");
    if (mode != "sleep" && !String.IsNullOrEmpty(log)) File.AppendAllText(log, mode + "\\n");
    if (mode == "small") { Console.Write("GPU-test, 0\\n"); return 0; }
    if (mode == "crlf") { Console.Write("GPU-test, 0\\r\\n\\r\\n"); return 0; }
    if (mode == "malformed") { Console.Write("GPU-test, 0\\r\\ninvalid\\r\\n"); return 0; }
    if (mode == "stdout-65536") { Write(Console.OpenStandardOutput(), 65536); return 0; }
    if (mode == "stderr-65536") { Console.Write("GPU-test, 0\\n"); Write(Console.OpenStandardError(), 65536); return 0; }
    if (mode == "stdout-65537") { Write(Console.OpenStandardOutput(), 65537); return 0; }
    if (mode == "stderr-65537") { Console.Write("GPU-test, 0\\n"); Write(Console.OpenStandardError(), 65537); return 0; }
    if (mode == "both-65537") { Write(Console.OpenStandardOutput(), 65537); Write(Console.OpenStandardError(), 65537); return 0; }
    if (mode == "held-eof") { Process child = new Process(); child.StartInfo.FileName = Process.GetCurrentProcess().MainModule.FileName; child.StartInfo.Arguments = "sleep"; child.StartInfo.UseShellExecute = false; child.StartInfo.CreateNoWindow = true; child.Start(); File.WriteAllText(Environment.GetEnvironmentVariable("KT_GPU_CHILD_PID"), child.Id.ToString()); Console.Write("GPU-test, 0\\n"); return 0; }
    if (mode == "sleep" || mode == "timeout") { Thread.Sleep(5500); return 0; }
    return 1;
  }
}`;
  const b64 = (value) => Buffer.from(value, 'utf8').toString('base64');
  const ownEnv = { ...process.env, TEMP: dir, TMP: dir };
  const ps = (script, timeout = 9000) => spawnSync(windowsPowerShell, ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')], { encoding: 'utf8', timeout, windowsHide: true, env: ownEnv });
  const helperFor = (script) => {
    const start = script.indexOf('function Stop-KatalaGpuSmiProcess') >= 0 ? script.indexOf('function Stop-KatalaGpuSmiProcess') : script.indexOf('function Invoke-KatalaGpuSmi');
    return script.slice(start, script.indexOf('; $smi ='));
  };
  const waitForOwnChild = () => {
    if (!fs.existsSync(childPid)) return;
    const pid = Number(fs.readFileSync(childPid, 'utf8'));
    assert.match(String(pid), /^\d+$/);
    const stop = Date.now() + 7000;
    const wait = new Int32Array(new SharedArrayBuffer(4));
    while (Date.now() < stop) {
      try { process.kill(pid, 0); } catch { return; }
      Atomics.wait(wait, 0, 0, 50);
    }
    throw new Error(`owned held-EOF child ${pid} did not exit`);
  };
  try {
    const compiler = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(`$source = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(source)}")); $target = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(fixture)}")); Add-Type -TypeDefinition $source -OutputAssembly $target -OutputType ConsoleApplication -ErrorAction Stop`, 'utf16le').toString('base64')], { encoding: 'utf8', timeout: 30000, windowsHide: true, env: ownEnv });
    assert.equal(compiler.status, 0, compiler.stderr || compiler.stdout);
    const power = plan(node, { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } }, { protect: [] });
    const helper = helperFor(power.script);
    const run = (mode, expected) => { fs.rmSync(spawnLog, { force: true }); return ps(`function Test-KatalaGpuDeadline {}; ${helper}; $script:KatalaGpuGuardClock = [Diagnostics.Stopwatch]::StartNew(); $script:KatalaGpuMutationStarted = $false; $script:KatalaGpuSmiSource = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(fixture)}")); $script:KatalaGpuSmiFunction = $false; $env:KT_GPU_CHILD_PID = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(childPid)}")); $env:KT_GPU_SPAWN_LOG = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(spawnLog)}")); $argsValue = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(mode)}")); $rows = @(Invoke-KatalaGpuSmi $argsValue); if ([bool]$script:KatalaGpuSmiFailed -ne ${expected.failed ? '$true' : '$false'}) { exit 2 }; if ([bool]$script:KatalaGpuSmiTimedOut -ne ${expected.timedOut ? '$true' : '$false'}) { exit 3 }; if ([bool]$script:KatalaGpuSmiTransportFailed -ne ${expected.transport ? '$true' : '$false'}) { exit 4 }; ${expected.length == null ? '' : `if ($rows.Count -ne 1 -or $rows[0].Length -ne ${expected.length}) { exit 4 };`} ${expected.malformed ? 'if ($rows.Count -ne 2 -or $rows[1] -ne "invalid") { exit 6 };' : ''} ${expected.small ? 'if ($rows.Count -ne 1 -or $rows[0] -ne "GPU-test, 0") { exit 5 };' : ''} ${expected.reason ? `if ($script:KatalaGpuSmiFailureReason -notmatch "${expected.reason}") { exit 6 };` : ''} ${expected.twice ? 'Invoke-KatalaGpuSmi $argsValue | Out-Null; if (-not $script:KatalaGpuSmiFailed) { exit 8 };' : ''} exit 0`); };
    for (const [mode, expected] of [
      ['small', { failed: false, timedOut: false, transport: false, small: true }],
      ['crlf', { failed: false, timedOut: false, transport: false, small: true }],
      ['malformed', { failed: false, timedOut: false, transport: false, malformed: true }],
      ['stdout-65536', { failed: false, timedOut: false, transport: false, length: 65536 }],
      ['stderr-65536', { failed: false, timedOut: false, transport: false }],
      ['stdout-65537', { failed: true, timedOut: false, transport: true, reason: 'stdout exceeded' }],
      ['stderr-65537', { failed: true, timedOut: false, transport: true, reason: 'stderr exceeded' }],
      ['both-65537', { failed: true, timedOut: false, transport: true }],
      ['held-eof', { failed: true, timedOut: true, transport: true, twice: true, reason: 'timed out or held a pipe' }],
      ['timeout', { failed: true, timedOut: true, transport: true, reason: 'timed out or held a pipe' }],
    ]) {
      const result = run(mode, expected);
      assert.equal(result.status, 0, `${mode}: ${result.stderr || result.stdout}`);
      if (mode === 'held-eof') assert.equal(fs.readFileSync(spawnLog, 'utf8'), 'held-eof\n');
    }
  } finally {
    try { waitForOwnChild(); } finally { fs.rmSync(dir, { recursive: true, force: true }); }
  }
});


test('JS と Rust は同じ bounded GPU SMI helper を生成する', () => {
  const power = plan(node, { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } }, { protect: [] });
  const jsHelper = power.script.match(/function Stop-KatalaGpuSmiProcess[\s\S]*?(?=; \$smi =)/)[0];
  const rust = fs.readFileSync(path.join(__dirname, '..', 'crates', 'tune-core', 'src', 'actions.rs'), 'utf8');
  const rustHelper = rust.match(/fn gpu_smi_helper\(\) -> String \{\s*r#"([\s\S]*?)"#\.to_string\(\)/)[1];
  assert.equal(jsHelper, rustHelper);
});


test('GPU SMI の kill ACK 未確認は synthetic Process で false として扱う', { skip: process.platform !== 'win32' }, () => {
  const power = plan(node, { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } }, { protect: [] });
  const start = power.script.indexOf('function Stop-KatalaGpuSmiProcess') >= 0 ? power.script.indexOf('function Stop-KatalaGpuSmiProcess') : power.script.indexOf('function Invoke-KatalaGpuSmi');
  const helper = power.script.slice(start, power.script.indexOf('; $smi ='));
  const mock = 'function Test-KatalaGpuDeadline {}; $script:KatalaGpuGuardClock = [Diagnostics.Stopwatch]::StartNew(); $p = [pscustomobject]@{ HasExited = $false }; Add-Member -InputObject $p -MemberType ScriptMethod -Name Kill -Value {}; Add-Member -InputObject $p -MemberType ScriptMethod -Name WaitForExit -Value { param($milliseconds); return $false }; if (Stop-KatalaGpuSmiProcess $p) { exit 1 }; if (-not $script:KatalaGpuSmiTransportFailed) { exit 2 }; $script:KatalaGpuSmiFunction = $true; $global:spawned = 0; function nvidia-smi.exe { $global:spawned += 1 }; $script:KatalaGpuMutationStarted = $false; $rows = @(Invoke-KatalaGpuSmi "ignored"); if (-not $script:KatalaGpuSmiFailed -or $global:spawned -ne 0) { exit 3 }; exit 0';
  const r = spawnSync(windowsPowerShell, ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(`${helper}; ${mock}`, 'utf16le').toString('base64')], { encoding: 'utf8', timeout: 5000, windowsHide: true });
  assert.equal(r.status, 0, r.stderr || r.stdout);
  const mutation = spawnSync(windowsPowerShell, ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(`function Test-KatalaGpuDeadline {}; ${helper}; $script:KatalaGpuGuardClock = [Diagnostics.Stopwatch]::StartNew(); $script:KatalaGpuSmiTransportFailed = $true; $script:KatalaGpuMutationStarted = $true; Invoke-KatalaGpuSmi "ignored"; Write-Output "AFTER-MUTATION"`, 'utf16le').toString('base64')], { encoding: 'utf8', timeout: 5000, windowsHide: true });
  assert.equal(mutation.status, 7, mutation.stderr || mutation.stdout);
  assert.doesNotMatch(mutation.stdout, /AFTER-MUTATION/);
});

test('Windows PowerShell の GPU 直前ガードは NaN・PID 桁あふれを拒否し、NVIDIA 非搭載は通す', { skip: process.platform !== 'win32' }, () => {
  const kill = plan(node, { type: 'kill-process', params: { pid: 4242, name: 'TrainerHost', min_cpu: 0 } }, { protect: [] });
  const guard = kill.script.slice(0, kill.script.indexOf('; try { Stop-Process'));
  const processMock = 'function Get-Process { param([int]$Id); $p = [pscustomobject]@{ Id = $Id; ProcessName = "TrainerHost"; CPU = 1 }; Add-Member -InputObject $p -MemberType ScriptMethod -Name Refresh -Value {}; return $p }; function Get-CimInstance { [CmdletBinding()] param([string]$ClassName, [string[]]$Property, [int]$OperationTimeoutSec); if ($ClassName -ne "Win32_Process") { throw "unexpected CIM class" }; return @([pscustomobject]@{ ProcessId = 4242; ParentProcessId = 4; CreationDate = [DateTime]"2026-01-01T00:00:00Z" }, [pscustomobject]@{ ProcessId = 4; ParentProcessId = 0; CreationDate = [DateTime]"2025-01-01T00:00:00Z" }) }';
  const run = (mock) => spawnSync(windowsPowerShell, ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(`${processMock}; ${mock}; ${guard}; exit 0`, 'utf16le').toString('base64')], { encoding: 'utf8', timeout: 15000, windowsHide: true });
  const nvidia = (gpu, compute, fail = false) => `function nvidia-smi.exe { if ($args -contains "--query-gpu=uuid,utilization.gpu") { $global:LASTEXITCODE = ${fail ? 1 : 0}; if (-not ${fail ? '$true' : '$false'}) { Write-Output "GPU-test, ${gpu}" }; return }; if ($args -contains "--query-compute-apps=pid,used_gpu_memory") { $global:LASTEXITCODE = 0; Write-Output "${compute}"; return } }`;
  const idle = run(nvidia('0', 'No running processes found'));
  assert.equal(idle.status, 0, idle.stderr || idle.stdout);
  const empty = run(nvidia('0', ''));
  assert.equal(empty.status, 0, empty.stderr || empty.stdout);
  const active = run(nvidia('5', 'No running processes found'));
  assert.equal(active.status, 9); assert.match(active.stdout, /GPU activity is active/);
  const nan = run(nvidia('NaN', 'No running processes found'));
  assert.equal(nan.status, 9); assert.match(nan.stdout, /GPU activity unreadable/);
  const overflow = run(nvidia('0', '2147483648, N\/A'));
  assert.equal(overflow.status, 9); assert.match(overflow.stdout, /GPU compute process list unreadable/);
  const queryFail = run(nvidia('0', 'No running processes found', true));
  assert.equal(queryFail.status, 9); assert.match(queryFail.stdout, /GPU SMI exited unsuccessfully; operation refused/);
  const absent = 'function Get-Command { param($Name); if ($Name -eq "nvidia-smi.exe") { return $null } }; function Get-CimInstance { [pscustomobject]@{ Name = "Intel UHD Graphics"; PNPDeviceID = "PCI\\VEN_8086" } }';
  assert.equal(run(absent).status, 0);
});

test('GPU の実行直前拒否は既存の runner 記録に理由として残り、GPU 学習保護属性は台帳再読時にも拒否する', async () => {
  const action = { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } };
  const direct = await execute(node, action, { protect: [] }, async () => ({ code: 9, out: 'GPU compute process is active; operation refused', err: '' }));
  assert.equal(direct.ok, false);
  assert.match(direct.outcome, /GPU/);
  const rows = [];
  const db = { addAction: (v) => rows.push(v), finishAction: (id, v) => { Object.assign(rows.find((x) => x.id === id), v); return true; } };
  const runner = makeRunner({ loadConfig: () => ({ nodes: [node], protect: [] }), confirm: async () => true, exec: async () => ({ code: 9, out: 'GPU activity is active; operation refused', err: '' }), db, newId: () => 'gpu-guard', now: () => 1 });
  const result = await runner.confirmAndRun(node.id, action, 'GPU power');
  assert.equal(result.ok, false);
  assert.match(rows[0].output, /GPU.*中止|中止.*GPU/);
  assert.throws(() => plan({ ...node, protect_gpu: true }, action, { protect: [] }), /GPU 学習保護/);
  assert.throws(() => plan({ ...node, protect_gpu: true }, { type: 'kill-process', params: { pid: 4242, name: 'TrainerHost' } }, { protect: [] }), /GPU 学習保護/);
});

test('GPU 電力の outcome-unverified は local/remote とも undo を出さず、既存電源操作の exit 7 undo は維持する', async () => {
  const gpu = { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } };
  const power = { type: 'set-power-plan', params: { guid: '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c', prev_guid: '381b4222-f694-41f0-9685-ff5bb260df2e' } };
  for (const n of [node, { ...node, local: true }]) assert.equal((await execute(n, gpu, { protect: [] }, async () => ({ code: 7, out: '', err: '' }))).undo, null);
  assert.ok((await execute(node, power, { protect: [] }, async () => ({ code: 7, out: '', err: '' }))).undo);
});

test('remote Windows GPU action は named KT2 exchange に本文だけを渡す', async () => {
  const action = { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } };
  let call;
  const action_helper = { schema: 'katala-tune/action-helper/v2', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 };
  const result = await execute({ ...node, local: false, action_helper }, action, { protect: [] }, async () => { throw new Error('generic transport must stay unused'); }, async (...args) => {
    call = args;
    return { code: 9, out: '', err: '' };
  });
  assert.equal(result.code, 9);
  assert.equal(call[1], action_helper.tune_sha256);
  assert.ok(Buffer.isBuffer(call[2]));
  assert.match(call[2].toString('utf8'), /Stopwatch|nvidia-smi/);
});

test('remote Windows action は approved helper record が無いか閉じた形式でなければ transport 前に拒否する', async () => {
  const action = { type: 'kill-process', params: { pid: 4242, name: 'Worker', start: '20261021010101000', min_cpu: 1 } };
  for (const action_helper of [undefined, { schema: 'katala-tune/action-helper/v1', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 }, { schema: 'katala-tune/action-helper/v2', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1, extra: true }]) {
    let calls = 0;
    const result = await execute({ ...node, local: false, action_helper }, action, { protect: [] }, async () => { throw new Error('generic transport must stay unused'); }, async () => { calls += 1; return { code: 0, out: '', err: '' }; });
    assert.equal(result.code, 9);
    assert.equal(calls, 0, 'helper record rejection happens before ssh spawn');
  }
});

test('KT2 exchange waits for READY before its only request and classifies terminal uncertainty', async () => {
  const body = Buffer.from('action body', 'utf8');
  const nonce = 'd'.repeat(64);
  const helper = 'b'.repeat(64);
  const fixture = String.raw`const crypto=require('node:crypto');let early=false, chunks=[];process.stdin.on('data',d=>{early=true;chunks.push(d)});setTimeout(()=>{if(early){process.exit(91);return}process.stdout.write('KT2 READY ${nonce}\n');process.stdin.on('end',()=>{const all=Buffer.concat(chunks);const line=all.subarray(0,all.indexOf(10)).toString('ascii').split(' ');const bytes=all.subarray(all.indexOf(10)+1);if(line.length!==5||line[0]!=='KT2'||line[1]!=='REQUEST'||line[2]!=='${nonce}'||Number(line[3])!==bytes.length||line[4]!==crypto.createHash('sha256').update(bytes).digest('hex')){process.exit(92);return}process.stdout.write('KT2 RESULT ${nonce} '+line[4]+' 0 2 0\nok')})},30);`;
  const node2 = { ...node, local: false };
  // Successful exchange includes OS/Node startup; short deadlines below test timeout behavior.
  const options = { command: { cmd: process.execPath, args: ['-e', fixture] }, timeoutMs: 5000 };
  const good = await actionExchange(node2, helper, body, options);
  assert.deepEqual(good, { code: 0, out: 'ok', err: '' });

  const badHash = fixture.replace("+' 0 2 0\\nok'", "+'x'.repeat(64)+' 0 2 0\\nok'");
  const bad = await actionExchange(node2, helper, body, { command: { cmd: process.execPath, args: ['-e', badHash] }, timeoutMs: 5000 });
  assert.equal(bad.code, 7);

  const exit255 = `${fixture}process.on('beforeExit',()=>{process.exitCode=255});`;
  const nonzero = await actionExchange(node2, helper, body, { command: { cmd: process.execPath, args: ['-e', exit255] }, timeoutMs: 5000 });
  assert.equal(nonzero.code, 7);

  const bodyHash = require('node:crypto').createHash('sha256').update(body).digest('hex');
  const heldEof = (port) => String.raw`const crypto=require('node:crypto'),net=require('node:net');let chunks=[];process.stdout.write('KT2 READY ${nonce}\n');process.stdin.on('data',d=>chunks.push(d));process.stdin.on('end',()=>{const all=Buffer.concat(chunks),at=all.indexOf(10),line=all.subarray(0,at).toString('ascii').split(' '),bytes=all.subarray(at+1);if(at<0||line.length!==5||line[0]!=='KT2'||line[1]!=='REQUEST'||line[2]!=='${nonce}'||Number(line[3])!==bytes.length||line[4]!==crypto.createHash('sha256').update(bytes).digest('hex')){process.exit(92);return}const socket=net.connect(${port},'127.0.0.1',()=>{socket.end('write-complete');process.stdout.write('KT2 RESULT ${nonce} ${bodyHash} 0 0 0\n');setTimeout(()=>{},2000)})});`;
  const held = await exchangeAfterRequest(
    (script, options = {}) => actionExchange(node2, helper, body, { command: { cmd: process.execPath, args: ['-e', script] }, timeoutMs: 5000, ...options }),
    heldEof,
    { timeoutMs: 1000 },
  );
  assert.equal(held.marker, 'write-complete', 'the held EOF case starts after the completed request write');
  assert.equal(held.result.code, 7);

  const noReady = 'setTimeout(()=>{},2000)';
  const beforeCommit = await actionExchange(node2, helper, body, { command: { cmd: process.execPath, args: ['-e', noReady] }, timeoutMs: 150 });
  assert.equal(beforeCommit.code, 9);

  const oversized = `process.stdout.write('KT2 READY ${nonce}\\n');process.stdin.resume();process.stdin.on('end',()=>{process.stdout.write('KT2 RESULT ${nonce} ${bodyHash} 0 65537 0\\n'+'x'.repeat(65537))});`;
  const over = await actionExchange(node2, helper, body, { command: { cmd: process.execPath, args: ['-e', oversized] }, timeoutMs: 5000 });
  assert.equal(over.code, 7);
});

test('KT2 controller は単調期限、厳密な結果枠、stdin/stdio 境界を一回の子で検証する', async () => {
  const body = Buffer.from('action body', 'utf8');
  const nonce = 'e'.repeat(64);
  const helper = 'b'.repeat(64);
  const hash = require('node:crypto').createHash('sha256').update(body).digest('hex');
  const node2 = { ...node, local: false };
  const exchange = (script, options = {}) => actionExchange(node2, helper, body, { command: { cmd: process.execPath, args: ['-e', script] }, timeoutMs: 5000, ...options });
  const result = (fields, payload = '', stderr = '') => `process.stdout.write('KT2 READY ${nonce}\\n');process.stdin.resume();process.stdin.on('end',()=>{process.stdout.write('KT2 RESULT '+${JSON.stringify(fields)}+'\\n'+${JSON.stringify(payload)});process.stderr.write(${JSON.stringify(stderr)})});`;

  let spawns = 0;
  const valid = await exchange(result(`${nonce} ${hash} 0 2 0`, 'ok'), { spawnChild: (...args) => { spawns += 1; return spawn(...args); } });
  assert.deepEqual(valid, { code: 0, out: 'ok', err: '' });
  assert.equal(spawns, 1, 'KT2 exchange does not retry or send a second request');

  for (const fields of [
    `${nonce} ${'0'.repeat(64)} 0 0 0`,
    `${'f'.repeat(64)} ${hash} 0 0 0`,
    `${nonce} ${hash} 0 00 0`,
    `${nonce} ${hash} 0 65537 0`,
    `${nonce} ${hash} 0 0 65537`,
  ]) assert.equal((await exchange(result(fields))).code, 7, fields);

  const duplicate = result(`${nonce} ${hash} 0 0 0`) + `process.stdin.on('end',()=>process.stdout.write('KT2 RESULT ${nonce} ${hash} 0 0 0\\n'));`;
  assert.equal((await exchange(duplicate)).code, 7);
  const exactFrame = `process.stdout.write('KT2 READY ${nonce}\\n');process.stdin.resume();process.stdin.on('end',()=>{process.stdout.write('KT2 RESULT ${nonce} ${hash} 0 65536 65536\\n');process.stdout.write(Buffer.alloc(65536,'o'));process.stdout.write(Buffer.alloc(65536,'e'));process.stderr.write(Buffer.alloc(65536,'z'))});`;
  const exact = await exchange(exactFrame);
  assert.equal(exact.code, 0);
  assert.equal(Buffer.byteLength(exact.out), 65536);
  assert.equal(Buffer.byteLength(exact.err), 65536);
  const overflowStderr = `process.stdout.write('KT2 READY ${nonce}\\n');process.stdin.resume();process.stdin.on('end',()=>{process.stdout.write('KT2 RESULT ${nonce} ${hash} 0 0 0\\n');process.stderr.write(Buffer.alloc(65537,'z'))});`;
  assert.equal((await exchange(overflowStderr)).code, 7);

  let spawnThrows = 0;
  const spawnFailure = await actionExchange(node2, helper, body, { spawnChild: () => { spawnThrows += 1; throw new Error('fixture spawn failure'); }, timeoutMs: 1000 });
  assert.equal(spawnFailure.code, 9, 'a synchronous spawn failure is pre-write refusal');
  assert.equal(spawnThrows, 1, 'spawn failure is not retried');

  const closedStdin = () => {
    const child = new EventEmitter();
    child.stdout = new PassThrough();
    child.stderr = new PassThrough();
    child.stdin = new EventEmitter();
    child.stdin.destroy = () => {};
    child.stdin.end = (_request, callback) => {
      callback(new Error('EPIPE'));
      child.stdout.end(`KT2 RESULT ${nonce} ${hash} 0 0 0\n`);
      child.emit('close', 0);
    };
    child.kill = () => {};
    queueMicrotask(() => child.stdout.write(`KT2 READY ${nonce}\n`));
    return child;
  };
  assert.equal((await actionExchange(node2, helper, body, { spawnChild: closedStdin, timeoutMs: 1000 })).code, 7, 'a failed post-READY write is outcome-unverified');
  const collectModule = require.resolve('../lib/collect');
  const source = fs.readFileSync(collectModule, 'utf8').replace(
    'const monotonicNow = () => performance.now();',
    'const monotonicNow = (() => { const ticks = [0, 0, 0, 101, 101]; return () => ticks.shift() ?? 101; })();',
  );
  const sandbox = { module: { exports: {} }, Buffer, TextDecoder, process, setTimeout, clearTimeout, require: createRequire(collectModule), __dirname: path.dirname(collectModule) };
  vm.runInNewContext(source, sandbox, { filename: collectModule });
  let preWriteSpawns = 0, preWriteEnds = 0;
  const expiresBeforeWrite = () => {
    const child = new EventEmitter();
    child.stdout = new PassThrough();
    child.stderr = new PassThrough();
    child.stdin = new EventEmitter();
    child.stdin.destroy = () => {};
    child.stdin.end = () => { preWriteEnds += 1; };
    child.kill = () => {};
    queueMicrotask(() => child.stdout.write(`KT2 READY ${nonce}\n`));
    return child;
  };
  const preWrite = await sandbox.module.exports.actionExchange(node2, helper, body, { timeoutMs: 100, spawnChild: (...args) => { preWriteSpawns += 1; return expiresBeforeWrite(...args); } });
  assert.equal(preWrite.code, 9, 'READY without a write attempt remains pre-commit');
  assert.equal(preWriteEnds, 0, 'the expired deadline never calls stdin.end');
  assert.equal(preWriteSpawns, 1, 'the pre-write deadline never retries');
  const zeroUnhandled = [];
  const onUnhandled = (reason) => { zeroUnhandled.push(String(reason)); };
  process.on('unhandledRejection', onUnhandled);
  try {
    let zeroSpawns = 0;
    const zero = await exchange('setTimeout(()=>{},1000)', { timeoutMs: 0, spawnChild: (...args) => { zeroSpawns += 1; return spawn(...args); } });
    await new Promise((resolve) => setTimeout(resolve, 25));
    assert.equal(zero.code, 9, 'an expired READY deadline cannot write a request');
    assert.equal(zeroSpawns, 1, 'the expired path spawns once and never retries');
    assert.deepEqual(zeroUnhandled, [], 'destroying an unopened reader cannot leak Premature close');
  } finally { process.removeListener('unhandledRejection', onUnhandled); }
  const afterReadyDeadline = (port) => `const net=require('node:net');process.stdout.write('KT2 READY ${nonce}\\n');process.stdin.resume();process.stdin.on('end',()=>{const socket=net.connect(${port},'127.0.0.1',()=>{socket.end('write-complete');setTimeout(()=>process.stdout.write('KT2 RESULT ${nonce} ${hash} 0 0 0\\n'),2000)})});`;
  let postWriteSpawns = 0;
  const afterReady = await exchangeAfterRequest(exchange, afterReadyDeadline, { timeoutMs: 1000, spawnChild: (...args) => { postWriteSpawns += 1; return spawn(...args); } });
  assert.equal(afterReady.marker, 'write-complete', 'the child parsed the completed request before the deadline path');
  assert.equal(afterReady.result.code, 7, 'an expired result deadline follows the one completed request write');
  assert.equal(postWriteSpawns, 1, 'the post-write deadline never retries');
  const heldStderr = (port) => `const cp=require('node:child_process'),net=require('node:net');process.stdout.write('KT2 READY ${nonce}\\n');process.stdin.resume();process.stdin.on('end',()=>{const socket=net.connect(${port},'127.0.0.1',()=>{socket.end('write-complete');process.stdout.write('KT2 RESULT ${nonce} ${hash} 0 0 0\\n');cp.spawn(process.execPath,['-e','setTimeout(()=>{},2000)'],{stdio:['ignore','ignore',process.stderr]})})});`;
  const held = await exchangeAfterRequest(exchange, heldStderr, { timeoutMs: 1000 });
  assert.equal(held.marker, 'write-complete', 'the held stderr case starts after the completed request write');
  assert.equal(held.result.code, 7);

  const oversizedHeader = `process.stdout.write('KT2 READY ${nonce}\\n');process.stdin.resume();process.stdin.on('end',()=>process.stdout.write('x'.repeat(131073)));`;
  assert.equal((await exchange(oversizedHeader)).code, 7, 'result headers never buffer payload-sized data before rejecting');
  assert.equal((await actionExchange(node2, helper, Buffer.alloc(65537), { timeoutMs: 1000 })).code, 9);
});

test('protect_gpu は任意の真偽値で、属性の無い既存台帳を変えない', () => {
  const file = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'kt-gpu-ledger-')), 'nodes.json');
  fs.writeFileSync(file, JSON.stringify({ nodes: [{ ...node }, { ...node, id: 'protected', protect_gpu: true }] }));
  const config = loadConfig(file);
  assert.equal(config.nodes[0].protect_gpu, undefined);
  assert.equal(config.nodes[1].protect_gpu, true);
  fs.writeFileSync(file, JSON.stringify({ nodes: [{ ...node, protect_gpu: 'true' }] }));
  assert.throws(() => loadConfig(file), /protect_gpu/);
  fs.writeFileSync(file, JSON.stringify({ nodes: [{ ...node, protect_gpu: null }] }));
  assert.throws(() => loadConfig(file), /protect_gpu/);
});

test('native GPU process annotation binds its target PID and protects related processes', { skip: process.platform !== 'win32' }, () => {
  const start = probe.indexOf('function Test-KtGpuRelated(');
  const end = probe.indexOf('\nforeach ($p in $list)', start);
  assert.ok(start >= 0 && end > start);
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-gpu-related-'));
  const script = path.join(dir, 'fixture.ps1');
  const source = `$ErrorActionPreference = 'Stop'\n${probe.slice(start, end)}\n` +
    `$map = @{100=[pscustomobject]@{pid=100;ppid=0;born=1};200=[pscustomobject]@{pid=200;ppid=100;born=2};250=[pscustomobject]@{pid=250;ppid=200;born=3};300=[pscustomobject]@{pid=300;ppid=0;born=1}}\n` +
    `foreach($target in 100,200,250){if(-not (Test-KtGpuRelated $target @(200) $map)){throw 'related process was not protected'}}\n` +
    `if(Test-KtGpuRelated 300 @(200) $map){throw 'unrelated process was marked'}\nWrite-Output 'related-ok'\n`;
  fs.writeFileSync(script, '\ufeff' + source, 'utf8');
  try {
    const result = spawnSync(windowsPowerShell, ['-NoProfile', '-NonInteractive', '-File', script], { encoding: 'utf8', timeout: 10000, windowsHide: true });
    assert.equal(result.status, 0, result.stderr || result.stdout);
    assert.match(result.stdout, /related-ok/);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test('local Windows GPU actions use the same approved finite receiver as remote actions', async () => {
  const action = { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } };
  const action_helper = { schema: 'katala-tune/action-helper/v2', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 };
  let calls = 0;
  const result = await execute({ ...node, local: true, action_helper }, action, { protect: [] }, async () => { throw new Error('generic local PowerShell must stay unused'); }, async (target, hash, body) => {
    calls++; assert.equal(target.local, true); assert.equal(hash, action_helper.tune_sha256); assert.match(body.toString('utf8'), /Invoke-KatalaGpuSmi/);
    return { code: 9, out: '', err: 'fixture refusal' };
  });
  assert.equal(calls, 1); assert.equal(result.code, 9);
});

test('local KT2 spawns the isolated helper directly with closed arguments', async () => {
  const path = require('node:path');
  const os = require('node:os');
  const { spawn } = require('node:child_process');
  const prior = process.env.KATALA_TUNE_DATA_DIR;
  const isolated = path.join(os.tmpdir(), 'katala-local-action-fixture');
  process.env.KATALA_TUNE_DATA_DIR = isolated;
  const hash = 'b'.repeat(64);
  let calls = 0;
  try {
    const result = await actionExchange({ ...node, local: true }, hash, Buffer.from('owned body'), {
      timeoutMs: 5000,
      spawnChild(cmd, args, options) {
        calls++;
        assert.equal(cmd, path.join(isolated, 'bin', 'tune.exe'));
        assert.deepEqual(args, ['windows-action-receiver-v2', '--helper-sha256', hash, '--budget-ms', '25000']);
        // Reject before READY; fixture proves there is no generic shell or SSH fallback.
        return spawn(process.execPath, ['-e', 'process.exit(9)'], options);
      },
    });
    assert.equal(calls, 1);
    assert.equal(result.code, 9);
  } finally {
    if (prior === undefined) delete process.env.KATALA_TUNE_DATA_DIR;
    else process.env.KATALA_TUNE_DATA_DIR = prior;
  }
});
