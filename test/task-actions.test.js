'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { plan, execute } = require('../lib/actions');
const node = { id: 'test-task', os: 'windows', local: true };
const params = { path: '\\Fixture\\', name: 'OwnedFixture' };
const action = (type = 'task-disable', extra = {}) => ({ type, params: { ...params, ...extra } });
const result = (out, err = '') => ({ code: 0, out, err });

for (const [title, out, err] of [
  ['missing receipt', 'Ready', ''],
  ['unchanged requested state mismatch', 'KT_TASK_STATE 1 1', ''],
  ['opposite transition', 'KT_TASK_STATE 0 1', ''],
  ['extra stdout', 'noise\nKT_TASK_STATE 1 0', ''],
  ['nonterminating error', 'KT_TASK_STATE 1 0', 'access denied'],
  ['duplicate receipt', 'KT_TASK_STATE 1 0\nKT_TASK_STATE 1 0', ''],
]) test(`task result refuses ${title} instead of recording success`, async () => {
  const r = await execute(node, action(), { protect: [] }, async () => result(out, err));
  assert.equal(r.ok, false);
  assert.equal(r.undo, null);
});

test('task undo is derived from verified before/after; no-op produces no undo', async () => {
  const changed = await execute(node, action(), { protect: [] }, async () => result('KT_TASK_STATE 1 0'));
  assert.equal(changed.ok, true);
  assert.deepEqual(changed.undo, action('task-enable', { expected_enabled: false }));
  const noop = await execute(node, action(), { protect: [] }, async () => result('KT_TASK_STATE 0 0'));
  assert.equal(noop.ok, true);
  assert.equal(noop.undo, null);
  const stale = await execute(node, action('task-enable', { expected_enabled: false }), { protect: [] }, async () => result('KT_TASK_STATE 1 1'));
  assert.equal(stale.ok, false);
  assert.equal(stale.undo, null);
});

test('task expected state must be a boolean', () => {
  for (const value of [null, 'false', 0, {}]) assert.throws(() => plan(node, action('task-enable', { expected_enabled: value }), { protect: [] }));
});

const ps = process.platform === 'win32' ? 'powershell.exe' : 'pwsh';
const present = spawnSync(ps, ['-NoProfile', '-Command', 'exit 0']).status === 0;
if (process.platform === 'win32') assert.ok(present, 'Windows CI must execute the synthetic PowerShell cases');
// All task commands are functions, not real OS tasks; finally reports synthetic state even on exit.
function nativeTask(type, before, mode, extra = {}) {
  const p = plan(node, action(type, extra), { protect: [] });
  const setup = `$script:enabled=$${before}; $script:writes=0; $script:reads=0;
function Get-ScheduledTask { [CmdletBinding()]param($TaskPath,$TaskName); $script:reads++; if ("${mode}" -eq "missing" -or ("${mode}" -eq "readfail" -and $script:reads -gt 1)) { throw "read unavailable" }; @{Settings=@{Enabled=$script:enabled}} }
function Disable-ScheduledTask { [CmdletBinding()]param($TaskPath,$TaskName); $script:writes++; if ("${mode}" -eq "denied") { Write-Error "denied"; return }; if ("${mode}" -ne "unchanged") { $script:enabled=$false }; if ("${mode}" -eq "error-after-change") { Write-Error "changed then failed" } }
function Enable-ScheduledTask { [CmdletBinding()]param($TaskPath,$TaskName); $script:writes++; if ("${mode}" -eq "denied") { Write-Error "denied"; return }; if ("${mode}" -ne "unchanged") { $script:enabled=$true }; if ("${mode}" -eq "error-after-change") { Write-Error "changed then failed" } }
function Start-ScheduledTask { [CmdletBinding()]param($TaskPath,$TaskName); $script:writes++; Write-Error "denied" }
try { ${p.script} } finally { Write-Output ("TRACE:" + (@{enabled=$script:enabled;writes=$script:writes}|ConvertTo-Json -Compress)) }`;
  const loader = '$source=[Console]::In.ReadToEnd(); & ([ScriptBlock]::Create($source))';
  const r = spawnSync(ps, ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(loader, 'utf16le').toString('base64')], { input: setup, encoding: 'utf8', timeout: 12000 });
  const trace = r.stdout?.split(/\r?\n/).find(s => s.startsWith('TRACE:'));
  assert.ok(trace, `${r.stderr}\n${r.stdout}`);
  return { code: r.status, trace: JSON.parse(trace.slice(6)), out: r.stdout };
}

test('native task script rejects nonterminating changes, mismatched or missing readback', { skip: !present }, () => {
  for (const mode of ['denied', 'error-after-change', 'unchanged', 'missing', 'readfail']) {
    const r = nativeTask('task-disable', true, mode);
    assert.notEqual(r.code, 0, mode);
    assert.doesNotMatch(r.out, /KT_TASK_STATE/, mode);
    if (mode === 'error-after-change') assert.equal(r.trace.enabled, false);
  }
  assert.notEqual(nativeTask('task-run', true, 'denied').code, 0);
});

test('native task script preserves no-op and refuses stale undo before writing', { skip: !present }, () => {
  for (const [type, before] of [['task-disable', false], ['task-enable', true]]) {
    const r = nativeTask(type, before, 'success');
    assert.equal(r.code, 0);
    assert.equal(r.trace.enabled, before);
    assert.equal(r.trace.writes, 0);
  }
  const r = nativeTask('task-enable', true, 'success', { expected_enabled: false });
  assert.notEqual(r.code, 0);
  assert.equal(r.trace.writes, 0);
  const changed = nativeTask('task-disable', true, 'success');
  assert.equal(changed.code, 0);
  assert.equal(changed.trace.enabled, false);
  assert.match(changed.out, /KT_TASK_STATE 1 0/);
});

test('task-run cannot report success without a completed command receipt', async () => {
  for (const output of ['Ready', 'KT_TASK_RUN\nKT_TASK_RUN']) {
    const r = await execute(node, action('task-run'), { protect: [] }, async () => result(output));
    assert.equal(r.ok, false);
    assert.equal(r.undo, null);
  }
  const accepted = await execute(node, action('task-run'), { protect: [] }, async () => result('KT_TASK_RUN'));
  assert.equal(accepted.ok, true);
  assert.equal(accepted.undo, null);
});
