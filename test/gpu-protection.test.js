// Issue #31: GPU 計算中の停止・電力操作を、提案と実行直前の両方で止める。
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { analyze } = require('../lib/rules');
const { plan, execute, ACTION_RECEIVER, ACTION_RECEIVER_COMMAND } = require('../lib/actions');
const { makeRunner } = require('../lib/runner');
const { loadConfig } = require('../lib/nodes');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const probe = fs.readFileSync(path.join(__dirname, '..', 'probes', 'win_probe.ps1'), 'utf8');
const windowsPowerShell = process.env.KT_GPU_POWERSHELL || 'powershell.exe';

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
    const run = (mode, expected) => { fs.rmSync(spawnLog, { force: true }); return ps(`function Test-KatalaGpuDeadline {}; ${helper}; $script:KatalaGpuGuardClock = [Diagnostics.Stopwatch]::StartNew(); $script:KatalaGpuMutationStarted = $false; $script:KatalaGpuSmiSource = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(fixture)}")); $script:KatalaGpuSmiFunction = $false; $env:KT_GPU_CHILD_PID = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(childPid)}")); $env:KT_GPU_SPAWN_LOG = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(spawnLog)}")); $argsValue = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String("${b64(mode)}")); $rows = @(Invoke-KatalaGpuSmi $argsValue); if ([bool]$script:KatalaGpuSmiFailed -ne ${expected.failed ? '$true' : '$false'}) { exit 2 }; if ([bool]$script:KatalaGpuSmiTimedOut -ne ${expected.timedOut ? '$true' : '$false'}) { exit 3 }; if ([bool]$script:KatalaGpuSmiTransportFailed -ne ${expected.transport ? '$true' : '$false'}) { exit 4 }; ${expected.length == null ? '' : `if ($rows.Count -ne 1 -or $rows[0].Length -ne ${expected.length}) { exit 4 };`} ${expected.small ? 'if ($rows.Count -lt 1 -or $rows[0] -ne "GPU-test, 0") { exit 5 };' : ''} ${expected.reason ? `if ($script:KatalaGpuSmiFailureReason -notmatch "${expected.reason}") { exit 6 };` : ''} ${expected.twice ? 'Invoke-KatalaGpuSmi $argsValue | Out-Null; if (-not $script:KatalaGpuSmiFailed) { exit 8 };' : ''} exit 0`); };
    for (const [mode, expected] of [
      ['small', { failed: false, timedOut: false, transport: false, small: true }],
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

test('remote Windows GPU action は固定 receiver と完全 stdin 本文だけを executor に渡す', async () => {
  const action = { type: 'set-gpu-power-limit', params: { uuid: 'GPU-01234567-89ab-cdef-0123-456789abcdef', watts: 180, min: 100, max: 250, prev_w: 200 } };
  let call;
  await execute({ ...node, local: false }, action, { protect: [] }, async (...args) => { call = args; return { code: 9, out: '', err: '' }; });
  assert.equal(call[1], ACTION_RECEIVER_COMMAND);
  assert.equal(call[2], 30000);
  assert.ok(Buffer.isBuffer(call[3]));
  assert.match(call[3].subarray(0, 100).toString('ascii'), /^KT1 [0-9]+ [0-9a-f]{64} [0-9]+\n/);
  assert.match(call[3].toString('utf8'), /Stopwatch|nvidia-smi/);
  assert.doesNotMatch(call[1], /nvidia-smi|GPU-01234567/);
});

test('Windows receiver はヘッダー終端と EOF を一つの残り時間で拒否する', () => {
  assert.match(ACTION_RECEIVER, /\$headerDone/);
  assert.match(ACTION_RECEIVER, /if\(-not \$headerDone\)\{exit 9\}/);
  assert.match(ACTION_RECEIVER, /\$remaining=5000-\[int\]\$w\.ElapsedMilliseconds/);
  assert.doesNotMatch(ACTION_RECEIVER, /\.Wait\(500\)/);
  assert.ok(Buffer.byteLength(ACTION_RECEIVER_COMMAND) < 8191);
  assert.throws(() => require('../lib/actions').actionInput('x'.repeat(65537)), /長さ/);
});

test('Windows receiver は空・途中・余剰・改ざん・不正 UTF-8・期限切れ・held EOF を本文評価前に拒否する', { skip: process.platform !== 'win32' }, async () => {
  const receiver = Buffer.from(ACTION_RECEIVER, 'utf16le').toString('base64');
  const body = Buffer.from('$global:ktMutation = 1; Write-Output "MUTATED"', 'utf8');
  const crypto = require('node:crypto');
  const packet = (bytes = body, hash = crypto.createHash('sha256').update(bytes).digest('hex'), expiry = Date.now() + 30000, tail = Buffer.alloc(0)) => Buffer.concat([Buffer.from(`KT1 ${bytes.length} ${hash} ${expiry}\n`, 'ascii'), bytes, tail]);
  const run = (input) => require('node:child_process').spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', receiver], { input, encoding: 'utf8', timeout: 10000, windowsHide: true });
  const truncated = Buffer.concat([packet().subarray(0, packet().indexOf(10) + 1), body.subarray(0, 3)]);
  for (const input of [Buffer.alloc(0), truncated, packet(body, '0'.repeat(64)), packet(Buffer.from([0xc3, 0x28])), packet(body, undefined, Date.now() - 1), packet(body, undefined, undefined, Buffer.from('x'))]) {
    const r = run(input); assert.equal(r.status, 9, r.stderr || r.stdout); assert.doesNotMatch(r.stdout, /MUTATED/);
  }
  const ok = run(packet()); assert.equal(ok.status, 0, ok.stderr || ok.stdout); assert.match(ok.stdout, /MUTATED/);
  const held = await new Promise((resolve) => {
    const child = require('node:child_process').spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', receiver], { windowsHide: true });
    let out = '', err = '';
    child.stdout.on('data', (d) => { out += d; }); child.stderr.on('data', (d) => { err += d; });
    child.on('close', (status) => resolve({ status, out, err }));
    child.stdin.on('error', () => {});
    child.stdin.write(packet());
  });
  assert.equal(held.status, 9, held.err || held.out); assert.doesNotMatch(held.out, /MUTATED/);
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
