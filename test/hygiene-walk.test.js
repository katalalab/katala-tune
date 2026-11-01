'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const hygiene = require('../lib/hygiene');

test('探索失敗を打切りと区別し、確認できた結果を保つ', () => {
  const data = hygiene.normalize({
    probe: 'repo_hygiene', error: 'walk_incomplete', truncated: false, skipped: 0,
    repos: [{ state: 'ok', path: 'fixture/project', tracked: 1, untracked: 0, conflicted: 0, upstream: false, detached: false }],
  });
  const findings = hygiene.analyze(data);
  const missing = findings.find((finding) => finding.id === 'repo-unobserved');
  assert.equal(data.error, 'walk_incomplete');
  assert.equal(data.repos[0].tracked, 1);
  assert.match(missing.detail, /探索先の一部を確認できない/);
  assert.doesNotMatch(missing.detail, /時間または件数で打ち切った/);
  assert.equal(findings.some((finding) => finding.id === 'repo-dirty'), true);
});

test('探索失敗と時間・件数の打切りが同時なら両方を示す', () => {
  const data = hygiene.normalize({ probe: 'repo_hygiene', error: 'walk_incomplete', truncated: true, repos: [] });
  const missing = hygiene.analyze(data).find((finding) => finding.id === 'repo-unobserved');
  assert.match(missing.detail, /探索先の一部を確認できない/);
  assert.match(missing.detail, /探索を時間または件数で打ち切った/);
});
