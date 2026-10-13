'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const probe = fs.readFileSync(path.join(__dirname, '../probes/win_probe.ps1'));
const source = probe.toString('utf8');

function helper(name) {
  const match = source.match(new RegExp(`function ${name}\\([^]*?\\n\\}`));
  assert.ok(match, `missing ${name} helper`);
  return match[0];
}

test('CPU capture pairs a single cumulative read with adjacent monotonic ticks and start identity', () => {
  const capture = helper('ReadCpuSample');
  assert.match(capture, /\$start = \$p\.StartTime/);
  assert.match(capture, /\$seconds = \$p\.TotalProcessorTime\.TotalSeconds\s+\$ticks = \[Diagnostics\.Stopwatch\]::GetTimestamp\(\)/);
  assert.match(capture, /start = \$start\.ToUniversalTime\(\)\.Ticks/);
  assert.match(capture, /catch \{ return \$null \}/);
  assert.equal((source.match(/ReadCpuSample \$p/g) || []).length, 2);
  assert.match(source, /\$s2\[\$p\.Id\] = ReadCpuSample \$p[^]*?\$perf2 = Get-CimInstance/);
  assert.doesNotMatch(source.slice(source.indexOf('$list = foreach'), source.indexOf('# Memory')), /\$p\.CPU|\$p\.StartTime/);
});

test('CPU getter failure preserves process identity independently of numeric availability', () => {
  const capture = helper('ReadCpuSample');
  assert.match(capture, /catch \{ \$seconds = \$null; \$ticks = \$null \}/);
  assert.match(capture, /start_time = \$start/);
  assert.match(source, /\$null -ne \$sample\.cpu -and \$sample\.life -gt 60/);
});

test('CPU interval calculation includes provider delay and rejects reuse or unknown intervals, without clamp', () => {
  const calc = helper('ProcessCpuPercent');
  assert.match(calc, /\$before\.pid -ne \$after\.pid/);
  assert.match(calc, /\$before\.start -ne \$after\.start/);
  assert.match(calc, /\$null -eq \$sample\.cpu/);
  assert.match(calc, /\$elapsed = \(\$after\.ticks - \$before\.ticks\) \/ \$frequency/);
  assert.match(calc, /\$delta = \$after\.cpu - \$before\.cpu/);
  assert.match(calc, /\$delta -lt 0 -or \$elapsed -le 0/);
  assert.match(calc, /\$percent = \$delta \/ \$elapsed \/ \$cores \* 100/);
  assert.doesNotMatch(calc, /::Min|::Max/);
  assert.match(source, /ProcessCpuPercent \$s1\[\$p\.Id\] \$sample \$ncpu \(\[Diagnostics\.Stopwatch\]::Frequency\)/);
});

test('CPU calculation rejects non-finite inputs and arithmetic overflow', () => {
  const calc = helper('ProcessCpuPercent');
  assert.match(calc, /IsNaN\(\$value\)/);
  assert.match(calc, /IsInfinity\(\$value\)/);
  assert.match(calc, /@\(\$before\.cpu, \$after\.cpu, \$before\.ticks, \$after\.ticks, \$cores, \$frequency\)/);
  assert.match(calc, /@\(\$delta, \$elapsed, \$percent\)/);
});

test('host missing samples and app totals remain unknown instead of zero', () => {
  const host = helper('HostCpuSample');
  assert.match(host, /\$null -ne \$value/);
  assert.match(host, /\$values\.Count -eq 0/);
  assert.match(host, /availability = "unavailable"/);
  assert.match(host, /"partial"/);
  assert.match(source, /cpu_busy = \(R1 \$hostCpu\.value\)/);
  assert.match(source, /cpu_busy_availability = \$hostCpu\.availability/);
  assert.match(source, /cpu_busy_samples = \$hostCpu\.samples/);
  assert.doesNotMatch(source, /\[double\]\$perf[12]\.PercentProcessorTime/);
  assert.match(source, /if \(@\(\$_\.Group \| Where-Object \{ \$null -eq \$_\.cpu \}\)\.Count -eq 0\)/);
});

test('CPU fix preserves PS5.1 ASCII/BOM and NoNetwork switch', () => {
  assert.deepEqual([...probe.subarray(0, 3)], [0xef, 0xbb, 0xbf]);
  assert.ok(probe.subarray(3).every(byte => byte < 128));
  assert.match(source, /param\(\[switch\]\$NoNetwork\)/);
});

test('PS5.1 executes actual isolated CPU helpers with synthetic samples', { skip: process.platform !== 'win32' }, () => {
  const r = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-File', path.join(__dirname, 'win-cpu-sampling-regression.ps1')], { encoding: 'utf8', timeout: 15000, windowsHide: true });
  assert.equal(r.status, 0, r.stderr || r.stdout);
  assert.match(r.stdout, /CPU sampling regression: PASS/);
});
