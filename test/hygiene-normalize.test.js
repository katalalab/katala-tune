'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const hygiene = require('../lib/hygiene');

const observed = () => ({
  path: '~/fixture', rel: 'example.com/acme/fixture', linked: false, state: 'ok',
  branch: null, detached: false, upstream: false, ahead: null, behind: null, local_only: null,
  tracked: 0, untracked: 0, conflicted: 0, dirty_age_s: null, head_age_s: null, stash: null,
  origin: null, origin_host: null, origin_path: null, has_origin: null,
});

test('必須項目の欠損・不正値は正常ではなく git_failed として扱う', () => {
  const counts = [undefined, null, '0', false, -1, -0.5, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, {}, []];
  const flags = [undefined, null, 0, 1, 'false', {}, []];
  for (const [fields, values] of [[['tracked', 'untracked', 'conflicted'], counts], [['upstream', 'detached'], flags]]) {
    for (const field of fields) {
      for (const value of values) {
        const raw = { ...observed(), [field]: value, reason: 'timeout' };
        if (value === undefined) delete raw[field];
        const [row] = hygiene.normalize({ repos: [raw] }).repos;
        assert.equal(row.state, 'unobserved', `${field}: ${String(value)}`);
        assert.equal(row.reason, 'git_failed', `${field}: ${String(value)}`);
        assert.equal('tracked' in row, false);
      }
    }
  }
});

test('正常なゼロ・真偽値と任意項目の null は保つ', () => {
  const raw = observed();
  assert.deepEqual(hygiene.normalize({ repos: [raw] }).repos[0], { ...raw, reason: null });
  const other = { ...raw, detached: true, upstream: true, tracked: 1, untracked: 2, conflicted: 1 };
  assert.deepEqual(hygiene.normalize({ repos: [other] }).repos[0], { ...other, reason: null });
  const largest = { ...raw, tracked: Number.MAX_SAFE_INTEGER };
  assert.equal(hygiene.normalize({ repos: [largest] }).repos[0].tracked, Number.MAX_SAFE_INTEGER);
});

test('既に未観測の行は従来どおり理由を保つ', () => {
  for (const reason of ['timeout', 'git_failed', 'over_limit']) {
    const [row] = hygiene.normalize({ repos: [{ state: 'unobserved', reason }] }).repos;
    assert.equal(row.reason, reason);
  }
});

test('衝突件数が変更件数を超える矛盾した行は未観測にする', () => {
  const [row] = hygiene.normalize({ repos: [{ ...observed(), tracked: 0, conflicted: 1 }] }).repos;
  assert.equal(row.state, 'unobserved');
  assert.equal(row.reason, 'git_failed');
});

test('必須項目が欠けた行は未観測の所見になり、変更ありと数えない', () => {
  const audit = hygiene.normalize({ repos: [{ ...observed(), tracked: null, untracked: 2 }] });
  const findings = hygiene.analyze(audit);
  assert.deepEqual(findings.map((finding) => finding.id), ['repo-unobserved']);
  assert.match(findings[0].detail, /git_failed 1/);
  assert.equal(hygiene.pathStatus(audit.repos[0]), 'unknown');
  assert.deepEqual(hygiene.matrix([{ node_id: 'node-a', ...audit }]), []);
});
