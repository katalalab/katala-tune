'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const childProcess = require('node:child_process');
for (const method of ['spawn', 'spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync', 'fork']) {
  childProcess[method] = () => { throw new Error('unexpected_process_start'); };
}
const { probeNode } = require('../lib/collect');

const node = Object.freeze({
  id: 'synthetic',
  alias: 'synthetic.invalid',
  os: 'macos',
  local: false,
  shared: false,
});

const envelope = Object.freeze({
  probe: 'mac',
  probe_version: 2,
  host: {
    hostname: 'synthetic',
    os: 'macos',
    model: null,
    cpu: null,
    cores: null,
    p_cores: null,
    e_cores: null,
    uptime_h: null,
  },
  load: [],
});

const cases = [
  ['mac_nonzero_object', 9, envelope, false, null],
  ['mac_unfinished_object', null, envelope, false, 'timeout'],
  ['mac_zero_empty_object', 0, {}, false, null],
  ['mac_zero_unrelated_object', 0, { unrelated: true }, false, null],
  ['mac_zero_null', 0, null, false, null],
  ['mac_zero_builtin_null_metrics', 0, envelope, true, null],
  ['mac_zero_builtin_omitted_metrics', 0,
    { probe: 'mac', probe_version: 2, host: { hostname: 'synthetic', os: 'macos' }, load: [] },
    true, null],
  ['mac_timeout_after_zero', 0, envelope, false, 'timeout'],
  ['mac_incomplete_after_zero', 0, envelope, false, 'incomplete'],
  ['mac_completion_missing', 0, envelope, false, 'missing'],
  ['mac_completion_unknown', 0, envelope, false, 'unknown'],
  ['mac_wrong_probe', 0, { ...envelope, probe: 'other' }, false, null],
  ['mac_unknown_version', 0, { ...envelope, probe_version: 3 }, false, null],
  ['mac_missing_host', 0, { probe: 'mac', probe_version: 2 }, false, null],
  ['mac_null_host', 0, { ...envelope, host: null }, false, null],
  ['mac_array_host', 0, { ...envelope, host: [] }, false, null],
  ['mac_optional_invalid', 0, { ...envelope, optional_feature: { value: false } }, true, null],
];

for (const [name, code, body, expected, failure] of cases) {
  test(name, async () => {
    let calls = 0;
    let strictRequested = false;
    const result = await probeNode(node, {
      run: async (...args) => {
        calls += 1;
        strictRequested = args[2]?.strictCompletion === true;
        const base = { code, out: JSON.stringify(body), err: 'synthetic-sentinel' };
        return failure === 'missing' ? base : { ...base, completion_failure: failure };
      },
      probeExchange: () => { throw new Error('unexpected_probe_exchange'); },
      localShell: () => { throw new Error('unexpected_local_shell'); },
    });
    assert.equal(calls, 1, 'fake_runner_call_count');
    assert.equal(strictRequested, true, 'strict_completion_requested');
    assert.equal(typeof result.ok, 'boolean', 'result_shape');
    assert.equal(result.ok, expected, 'completion_integrity');
    if (!expected) {
      assert.equal(result.reason, failure === 'timeout' ? 'timeout' : 'error', 'failure_reason');
      assert.equal(JSON.stringify(result).includes('synthetic-sentinel'), false, 'failure_redaction');
    }
  });
}

const windows_action_helper = { schema: 'katala-tune/action-helper/v2', source_sha: 'a'.repeat(40), tune_sha256: 'b'.repeat(64), manifest_sha256: 'c'.repeat(64), run_id: 1, attempt: 1 };
const windows_synthetic_cases = [
  { id: 'windows_empty_object', code: 0, data: {}, expected: false },
  { id: 'windows_unrelated_object', code: 0, data: { unrelated: true }, expected: false },
  { id: 'windows_wrong_probe', code: 0, data: { probe: 'mac', probe_version: 2, host: {} }, expected: false },
  { id: 'windows_unknown_version', code: 0, data: { probe: 'windows', probe_version: 999, host: {} }, expected: false },
  { id: 'windows_missing_host', code: 0, data: { probe: 'windows', probe_version: 2 }, expected: false },
  { id: 'windows_null', code: 0, data: null, expected: false },
  { id: 'windows_nonzero', code: 1, data: { probe: 'windows', probe_version: 2, host: {} }, expected: false },
  { id: 'windows_valid_partial', code: 0, data: { probe: 'windows', probe_version: 2, host: {}, metric: null }, expected: true },
  { id: 'windows_valid_omitted', code: 0, data: { probe: 'windows', probe_version: 2, host: {} }, expected: true },
];
for (const scenario of windows_synthetic_cases) {
  test(scenario.id, async () => {
    let calls = 0;
    const node = { id: 'synthetic', os: 'windows', local: false, alias: 'synthetic', action_helper: windows_action_helper };
    const result = await probeNode(node, {
      run: async () => { throw new Error('unexpected run'); },
      probeExchange: async () => { calls += 1; return { code: scenario.code, out: JSON.stringify(scenario.data), err: 'synthetic-private-sentinel' }; },
    });
    assert.equal(calls, 1, 'windows_fake_exchange');
    assert.equal(result.ok, scenario.expected, scenario.id);
    if (!scenario.expected) assert.equal(JSON.stringify(result).includes('synthetic-private-sentinel'), false, 'windows_redaction');
  });
}
const windows_failure_reasons = [
  { id: 'windows_auth_reason', err: 'permission denied synthetic-private-sentinel', reason: 'auth' },
  { id: 'windows_unreachable_reason', err: 'connection refused synthetic-private-sentinel', reason: 'unreachable' },
];
for (const scenario of windows_failure_reasons) {
  test(scenario.id, async () => {
    const node = { id: 'synthetic', os: 'windows', local: false, alias: 'synthetic', action_helper: windows_action_helper };
    const result = await probeNode(node, { probeExchange: async () => ({ code: 1, out: '', err: scenario.err }) });
    assert.equal(result.ok, false);
    assert.equal(result.reason, scenario.reason);
    assert.equal(result.error, scenario.reason);
    assert.equal(JSON.stringify(result).includes('synthetic-private-sentinel'), false);
  });
}

test('unsupported_os_never_dispatches', async () => {
  let calls = 0;
  const node = { id: 'synthetic', os: 'unsupported', local: false, alias: 'synthetic', action_helper: windows_action_helper };
  const result = await probeNode(node, { run: async () => { calls++; throw new Error('forbidden'); }, probeExchange: async () => { calls++; return { code: 0, out: JSON.stringify({ probe: 'windows', probe_version: 2, host: {} }), err: '' }; } });
  assert.equal(calls, 0);
  assert.equal(result.ok, false);
});

test('mac_trailing_broken_object_rejected', async () => {
  const node = { id: 'synthetic', os: 'macos', local: false, alias: 'synthetic' };
  const valid = JSON.stringify({ probe: 'mac', probe_version: 2, host: {} });
  const result = await probeNode(node, { run: async () => ({ code: 0, out: valid + '\n{"incomplete":', err: '', completion_failure: null }) });
  assert.equal(result.ok, false);
});

test('windows_trailing_broken_object_rejected', async () => {
  const node = { id: 'synthetic', os: 'windows', local: false, alias: 'synthetic', action_helper: windows_action_helper };
  const valid = JSON.stringify({ probe: 'windows', probe_version: 2, host: {} });
  const result = await probeNode(node, { probeExchange: async () => ({ code: 0, out: valid + '\n{"incomplete":', err: '' }) });
  assert.equal(result.ok, false);
});

test('probe_all_exception_is_redacted', async () => {
  const node = { id: 'synthetic', os: 'macos', local: false, alias: 'synthetic' };
  const all = await require('../lib/collect').probeAll([node], () => {}, { run: async () => { throw new Error('synthetic-private-sentinel'); } });
  assert.equal(JSON.stringify(all).includes('synthetic-private-sentinel'), false);
  assert.equal(all[0].ok, false);
  assert.equal(all[0].reason, 'error');
});
const synthetic_bench_separator = '@@KATALA_TUNE_BENCH@@';
for (const os of ['macos', 'windows']) {
  for (const scenario of [
    { id: 'trailing_null', suffix: 'null' },
    { id: 'trailing_array', suffix: '[]' },
    { id: 'trailing_text', suffix: 'synthetic trailing text' },
  ]) {
    test(os + '_' + scenario.id, async () => {
      const node = { id: 'synthetic', os, local: false, alias: 'synthetic', action_helper: windows_action_helper };
      const valid = JSON.stringify({ probe: os === 'macos' ? 'mac' : 'windows', probe_version: 2, host: {} });
      const out = valid + '\n' + scenario.suffix;
      const result = await probeNode(node, { run: async () => ({ code: 0, out, err: '', completion_failure: null }), probeExchange: async () => ({ code: 0, out, err: '' }) });
      assert.equal(result.ok, false);
    });
  }
  test(os + '_prefix_banner_allowed', async () => {
    const node = { id: 'synthetic', os, local: false, alias: 'synthetic', action_helper: windows_action_helper };
    const valid = JSON.stringify({ probe: os === 'macos' ? 'mac' : 'windows', probe_version: 2, host: {} });
    const out = 'synthetic banner\n' + valid;
    const result = await probeNode(node, { run: async () => ({ code: 0, out, err: '', completion_failure: null }), probeExchange: async () => ({ code: 0, out, err: '' }) });
    assert.equal(result.ok, true);
  });
  test(os + '_separate_benchmark_allowed', async () => {
    const node = { id: 'synthetic', os, local: false, alias: 'synthetic', action_helper: windows_action_helper, benchmark: true };
    const valid = JSON.stringify({ probe: os === 'macos' ? 'mac' : 'windows', probe_version: 2, host: {} });
    const out = valid + '\n' + synthetic_bench_separator + '\n' + JSON.stringify({ synthetic_score: 1 });
    const result = await probeNode(node, { run: async () => ({ code: 0, out, err: '', completion_failure: null }), probeExchange: async () => ({ code: 0, out, err: '' }) });
    assert.equal(result.ok, true);
    assert.equal(result.data.bench.synthetic_score, 1);
  });
}
const mac_nonzero_failure_reasons = [
  { id: 'mac_nonzero_auth_reason', err: 'permission denied synthetic-private-sentinel', reason: 'auth' },
  { id: 'mac_nonzero_unreachable_reason', err: 'connection refused synthetic-private-sentinel', reason: 'unreachable' },
];
for (const scenario of mac_nonzero_failure_reasons) {
  test(scenario.id, async () => {
    const node = { id: 'synthetic', os: 'macos', local: false, alias: 'synthetic' };
    const valid = JSON.stringify({ probe: 'mac', probe_version: 2, host: {} });
    const result = await probeNode(node, { run: async () => ({ code: 1, out: valid, err: scenario.err, completion_failure: null }) });
    assert.equal(result.ok, false);
    assert.equal(result.reason, scenario.reason);
    assert.equal(result.error, scenario.reason);
    assert.equal(JSON.stringify(result).includes('synthetic-private-sentinel'), false);
  });
}
test('mac_direct_runner_rejection_redacted', async () => {
  const node = { id: 'synthetic', os: 'macos', local: false, alias: 'synthetic' };
  const result = await probeNode(node, { run: async () => { throw new Error('synthetic-private-sentinel'); } });
  assert.equal(result.ok, false);
  assert.equal(result.reason, 'error');
  assert.equal(JSON.stringify(result).includes('synthetic-private-sentinel'), false);
});

test('mac_direct_source_failure_redacted', async () => {
  const fs = require('node:fs');
  const originalRead = fs.readFileSync;
  let runCalls = 0;
  fs.readFileSync = () => { throw new Error('synthetic-private-sentinel'); };
  try {
    const node = { id: 'synthetic', os: 'macos', local: false, alias: 'synthetic' };
    const result = await probeNode(node, { run: async () => { runCalls++; throw new Error('forbidden'); } });
    assert.equal(runCalls, 0);
    assert.equal(result.ok, false);
    assert.equal(result.reason, 'error');
    assert.equal(JSON.stringify(result).includes('synthetic-private-sentinel'), false);
  } finally {
    fs.readFileSync = originalRead;
  }
});
const synthetic_response_variants = [
  { id: 'missing_result', result: undefined },
  { id: 'null_result', result: null },
  { id: 'missing_out', result: { code: 0, err: '', completion_failure: null } },
  { id: 'null_out', result: { code: 0, out: null, err: '', completion_failure: null } },
  { id: 'nonstring_out', result: { code: 0, out: 123, err: '', completion_failure: null } },
  { id: 'null_err', result: { code: 0, out: '{}', err: null, completion_failure: null } },
  { id: 'missing_err', result: { code: 0, out: '{}', completion_failure: null } },
  { id: 'nonstring_err', result: { code: 0, out: '{}', err: 123, completion_failure: null } },
  { id: 'array_result', result: [] },
  { id: 'string_result', result: 'synthetic' },
];
for (const os of ['macos', 'windows']) for (const scenario of synthetic_response_variants) {
  test(os + '_' + scenario.id, async () => {
    const node = { id: 'synthetic', os, local: false, alias: 'synthetic', action_helper: windows_action_helper };
    const result = await probeNode(node, { run: async () => scenario.result, probeExchange: async () => scenario.result });
    assert.equal(result.ok, false);
    assert.equal(result.reason, 'error');
  });
}
test('mac_timeout_fixed_error', async () => {
  const node = { id: 'synthetic', os: 'macos', local: false, alias: 'synthetic' };
  const result = await probeNode(node, { run: async () => ({ code: null, out: '', err: 'synthetic-private-sentinel', completion_failure: 'timeout' }) });
  assert.equal(result.ok, false);
  assert.equal(result.reason, 'timeout');
  assert.equal(result.error, 'timeout');
  assert.equal(JSON.stringify(result).includes('synthetic-private-sentinel'), false);
});
const unproven_stderr_reason_cases = [
  { id: 'incomplete_auth_not_classified', failure: 'incomplete', err: 'permission denied' },
  { id: 'incomplete_unreachable_not_classified', failure: 'incomplete', err: 'connection refused' },
  { id: 'missing_proof_auth_not_classified', err: 'permission denied' },
  { id: 'missing_proof_unreachable_not_classified', err: 'connection refused' },
  { id: 'incomplete_null_exit_auth_not_classified', failure: 'incomplete', code: null, err: 'permission denied' },
  { id: 'incomplete_null_exit_unreachable_not_classified', failure: 'incomplete', code: null, err: 'connection refused' },
];
for (const scenario of unproven_stderr_reason_cases) {
  test('mac_' + scenario.id, async () => {
    const node = { id: 'synthetic', os: 'macos', local: false, alias: 'synthetic' };
    const result = { code: Object.prototype.hasOwnProperty.call(scenario, 'code') ? scenario.code : 1, out: '', err: scenario.err };
    if (scenario.failure) result.completion_failure = scenario.failure;
    const outcome = await probeNode(node, { run: async () => result });
    assert.equal(outcome.ok, false);
    assert.equal(outcome.reason, 'error');
    assert.equal(outcome.error, 'probe_failed');
  });
}
const synthetic_benchmark_analysis_cases = [false, true];
for (const os of ['macos', 'windows']) for (const enabled of synthetic_benchmark_analysis_cases) {
  test(os + '_benchmark_analysis_' + (enabled ? 'enabled' : 'disabled'), async () => {
    const node = { id: 'synthetic', os, local: false, alias: 'synthetic', action_helper: windows_action_helper, benchmark: enabled };
    const valid = JSON.stringify({ probe: os === 'macos' ? 'mac' : 'windows', probe_version: 2, host: {} });
    const out = valid + '\n' + synthetic_bench_separator + '\n' + JSON.stringify({ runs_ms: [1, 100, 1000], median_ms: 100 });
    const result = await probeNode(node, { run: async () => ({ code: 0, out, err: '', completion_failure: null }), probeExchange: async () => ({ code: 0, out, err: '' }) });
    assert.equal(result.ok, true);
    assert.equal(result.data.bench == null, !enabled);
    const findings = require('../lib/rules').analyze(result.data, node);
    assert.equal(findings.length, enabled ? 1 : 0);
  });
}
