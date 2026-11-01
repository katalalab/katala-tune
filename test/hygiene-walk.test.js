'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const hygiene = require('../lib/hygiene');

test('探索失敗を打切りと区別し、確認できた結果を保つ', () => {
  const data = hygiene.normalize({
    probe: 'repo_hygiene', error: 'walk_incomplete', truncated: false, skipped: 0, enumerated: 1,
    roots: [{ path: 'fixture', source: 'opt' }],
    repos: [{ state: 'ok', path: 'fixture/project', linked: false, tracked: 1, untracked: 0, conflicted: 0, upstream: false, detached: false }],
  });
  const findings = hygiene.analyze(data);
  const missing = findings.find((finding) => finding.id === 'repo-unobserved');
  assert.equal(data.error, 'walk_incomplete');
  assert.equal(data.repos[0].tracked, 1);
  assert.match(missing.detail, /探索先の一部を確認できない/);
  assert.doesNotMatch(missing.detail, /時間または件数で打ち切った/);
  assert.equal(findings.some((finding) => finding.id === 'repo-dirty'), true);
  assert.equal(hygiene.walkCoverageNote(data), '・探索未確認あり（件数不明）');
});

test('探索未確認でも既知の未観測数と未知の範囲を区別する', () => {
  const base = {
    node_id: 'fixture-node', wall_s: 1,
    roots: [{ path: 'fixture', source: 'opt' }], enumerated: 5, skipped: 3, truncated: false,
    repos: [
      { state: 'ok', path: 'fixture/ok', linked: false, tracked: 0, untracked: 0, conflicted: 0, upstream: false, detached: false },
      { state: 'unobserved', path: 'fixture/missing', linked: false, reason: 'git_failed' },
    ],
  };
  for (const [error, truncated] of [['walk_incomplete', false], [null, true], ['walk_incomplete', true]]) {
    const { observed, line } = hygiene.progress({ ...base, error, truncated }, 1);
    assert.equal(observed, 1);
    assert.match(line, /見つけた 5 件・未観測 既知 4 件・探索未確認あり（件数不明）/);
    assert.equal((line.match(/探索未確認あり/g) || []).length, 1);
  }
  for (const input of [{ ...base, enumerated: 4, truncated: true }, { ...base, skipped: null, truncated: true }]) {
    assert.match(hygiene.progress(input, 1).line, /未観測 件数不明/);
  }
});

test('探索失敗と時間・件数の打切りが同時なら両方を示す', () => {
  const data = hygiene.normalize({ probe: 'repo_hygiene', error: 'walk_incomplete', truncated: true, repos: [] });
  const missing = hygiene.analyze(data).find((finding) => finding.id === 'repo-unobserved');
  assert.match(missing.detail, /探索先の一部を確認できない/);
  assert.match(missing.detail, /探索を時間または件数で打ち切った/);
  assert.equal(hygiene.walkCoverageNote(data), '・探索未確認あり（件数不明）');
  assert.equal(hygiene.walkCoverageNote({ error: null, truncated: true }), '・探索未確認あり（件数不明）');
  assert.equal(hygiene.walkCoverageNote({ error: null }), '');
});
