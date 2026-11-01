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

const audit = () => ({ roots: [{ path: '~/fixture', source: 'opt' }], repos: [], enumerated: 0, skipped: 0, truncated: false });

test('集計の欠損・不正値は null と未観測の所見として残す', () => {
  for (const field of ['enumerated', 'skipped', 'truncated']) {
    const invalid = field === 'truncated' ? [undefined, null, 0, 'false', {}] : [undefined, null, '0', false, -1, 0.5, Number.MAX_SAFE_INTEGER + 1, {}];
    for (const value of invalid) {
      const raw = { ...audit(), [field]: value };
      if (value === undefined) delete raw[field];
      const normalized = hygiene.normalize(raw);
      assert.equal(normalized[field], null, field);
      assert.ok(hygiene.analyze(normalized).some((finding) => finding.id === 'repo-unobserved'), field);
    }
  }
  const good = hygiene.normalize(audit());
  assert.deepEqual([good.enumerated, good.skipped, good.truncated], [0, 0, false]);
  assert.deepEqual(hygiene.analyze(good), []);
  for (const field of ['enumerated', 'skipped']) {
    const n = hygiene.normalize({ ...audit(), [field]: Number.MAX_SAFE_INTEGER });
    assert.equal(n[field], Number.MAX_SAFE_INTEGER);
  }
});

test('所見 API へ直接渡した入力も、欠損した集計情報を未観測にする', () => {
  for (const input of [{}, { repos: [] }, { repos: [observed()] }]) {
    assert.ok(hygiene.analyze(input).some((finding) => finding.id === 'repo-unobserved'));
  }
  assert.deepEqual(hygiene.analyze(audit()), []);
});

test('一覧の欠損・不正は配列の互換性を保ちつつ不明として残す', () => {
  for (const field of ['roots', 'repos']) {
    for (const value of [undefined, null, 'x', {}]) {
      const raw = { ...audit(), [field]: value };
      if (value === undefined) delete raw[field];
      const normalized = hygiene.normalize(raw);
      assert.deepEqual(normalized[field], []);
      assert.ok(normalized.incomplete_fields.includes(field));
      assert.ok(hygiene.analyze(normalized).some((finding) => finding.id === 'repo-unobserved'));
      assert.ok(hygiene.normalize(normalized).incomplete_fields.includes(field));
    }
  }
  const partial = hygiene.normalize({ ...audit(), roots: [{ path: '~/fixture', source: 'opt' }, null] });
  assert.ok(partial.incomplete_fields.includes('roots'));
  assert.equal(partial.roots[0].source, 'opt');
  for (const root of [{ path: '', source: 'opt' }, { path: '~/fixture', source: '' }]) {
    const normalized = hygiene.normalize({ ...audit(), roots: [root] });
    assert.ok(normalized.incomplete_fields?.includes('roots'));
  }
  assert.ok(!hygiene.normalize({ ...audit(), roots: [{ path: ' ', source: 'opt' }] }).incomplete_fields);
});

test('不正な不完全ラベルは生値を残さず固定ラベルに畳む', () => {
  for (const value of [null, 'roots', false, {}, ['unknown-marker'], ['roots', 'unknown-marker']]) {
    const normalized = hygiene.normalize({ ...audit(), incomplete_fields: value });
    assert.ok(normalized.incomplete_fields?.includes('metadata'));
    assert.ok(!JSON.stringify(normalized).includes('unknown-marker'));
    assert.ok(hygiene.analyze(normalized).some((finding) => finding.id === 'repo-unobserved'));
    assert.ok(hygiene.normalize(normalized).incomplete_fields.includes('metadata'));
  }
  assert.ok(!hygiene.normalize({ ...audit(), incomplete_fields: [] }).incomplete_fields);
});

test('集計不明でも既存 error と部分結果を保持する', () => {
  const raw = { ...audit(), enumerated: null, error: 'walk_incomplete', repos: [{ ...observed(), tracked: 1 }] };
  const normalized = hygiene.normalize(raw);
  assert.equal(normalized.error, 'walk_incomplete');
  assert.equal(normalized.repos[0].tracked, 1);
  const ids = hygiene.analyze(normalized).map((finding) => finding.id);
  assert.ok(ids.includes('repo-unobserved'));
  assert.ok(ids.includes('repo-dirty'));
});

test('探索先が空なら既知のゼロを保持しつつ未確認にする', () => {
  const normalized = hygiene.normalize({ ...audit(), roots: [] });
  assert.deepEqual(normalized.roots, []);
  assert.equal(normalized.enumerated, 0);
  const finding = hygiene.analyze(normalized).find((f) => f.id === 'repo-unobserved');
  assert.ok(finding);
  assert.match(finding.detail, /探索先/);
  const result = hygiene.progress({ ...normalized, node_id: 'node-a', wall_s: 1 }, 1);
  assert.match(result.line, /見つけた 0 件・未観測 件数不明/);
  const knownRoot = hygiene.normalize(audit());
  assert.deepEqual(hygiene.analyze(knownRoot), []);
  const missingGit = hygiene.analyze({ ...audit(), roots: [], error: 'git_missing' });
  assert.match(missingGit[0].detail, /git_missing/);
  assert.doesNotMatch(missingGit[0].detail, /探索先がない/);
});

test('error が不正型なら固定エラーにし、既知の分類と null は保つ', () => {
  for (const error of [false, true, 0, {}, []]) {
    const normalized = hygiene.normalize({ ...audit(), error });
    assert.equal(normalized.error, 'unknown_error');
    assert.ok(normalized.incomplete_fields?.includes('error'));
    assert.ok(hygiene.analyze(normalized).some((finding) => finding.id === 'repo-unobserved'));
  }
  for (const error of [undefined, null, 'git_missing', 'walk_incomplete', 'no_python', 'timeout_or_unreachable', 'probe_failed', 'excluded', 'internal', 'unknown_error']) {
    const normalized = hygiene.normalize({ ...audit(), error });
    assert.equal(normalized.error, error ?? null);
    assert.ok(!normalized.incomplete_fields?.includes('error'));
  }
});

test('未知の error は合成の機密らしい値を残さず未確認にする', () => {
  const marker = 'fixture-only-sensitive-value';
  const values = ['', `https://unit-user:${marker}@example.test/r?token=${marker}`, { detail: marker }];
  for (const error of values) {
    const raw = { ...audit(), node_id: 'node-a', wall_s: 1, error };
    const normalized = hygiene.normalize(raw);
    const findings = hygiene.analyze(raw);
    const progress = hygiene.progress(raw, findings.length);
    assert.equal(normalized.error, 'unknown_error');
    assert.ok(normalized.incomplete_fields?.includes('error'));
    assert.ok(findings.some((finding) => finding.id === 'repo-unobserved'));
    assert.match(progress.line, /未観測 件数不明/);
    assert.ok(!JSON.stringify({ normalized, findings, progress }).includes(marker));
    assert.equal(hygiene.normalize(normalized).error, 'unknown_error');
  }
});

test('探索件数と結果・未観測件数が矛盾する場合は既知値を保って未観測にする', () => {
  for (const [enumerated, skipped] of [[0, 0], [3, 1], [1, 2], [0, null]]) {
    const normalized = hygiene.normalize({ ...audit(), repos: [observed()], enumerated, skipped });
    assert.equal(normalized.enumerated, enumerated);
    assert.equal(normalized.skipped, skipped);
    assert.ok(normalized.incomplete_fields.includes('counts'));
    assert.ok(hygiene.analyze(normalized).some((finding) => finding.id === 'repo-unobserved'));
  }
  const valid = hygiene.normalize({ ...audit(), repos: [observed()], enumerated: 2, skipped: 1 });
  assert.ok(!valid.incomplete_fields?.includes('counts'));
});

test('進捗は既知のゼロと件数不明を区別する', () => {
  assert.equal(typeof hygiene.progress, 'function');
  const complete = { ...hygiene.normalize(audit()), node_id: 'node-a', wall_s: 1 };
  assert.match(hygiene.progress(complete, 0).line, /見つけた 0 件・未観測 0 件/);
  for (const patch of [{ enumerated: null }, { skipped: null }, { truncated: null }, { truncated: true }, { error: 'git_missing' }, { incomplete_fields: ['roots'] }]) {
    const value = hygiene.progress({ ...complete, ...patch }, 1);
    assert.match(value.line, /件数不明/);
    assert.doesNotMatch(value.line, /未観測 0 件・/);
  }
  const noRepos = hygiene.progress({ ...complete, incomplete_fields: ['repos'] }, 1);
  assert.equal(noRepos.observed, null);
  const inconsistent = hygiene.progress({ ...complete, incomplete_fields: ['counts'] }, 1);
  assert.match(inconsistent.line, /見つけた 件数不明/);
});

test('所要時間が欠損・不正なら 0 秒とせず時間不明を表示する', () => {
  const base = { ...audit(), node_id: 'node-a' };
  for (const value of [undefined, null, '1', false, -1, NaN, Infinity, {}, []]) {
    const input = { ...base, wall_s: value };
    if (value === undefined) delete input.wall_s;
    const result = hygiene.progress(input, 0);
    assert.match(result.line, /時間不明/);
    assert.doesNotMatch(result.line, /0\.0s/);
  }
  assert.match(hygiene.progress({ ...base, wall_s: 0 }, 0).line, /0\.0s/);
  assert.match(hygiene.progress({ ...base, wall_s: 1.25 }, 0).line, /1\.3s/);
});

test('進捗へ直接渡した不正な error も未確認として扱う', () => {
  for (const error of [false, true, 0, {}, []]) {
    const result = hygiene.progress({ ...audit(), node_id: 'node-a', wall_s: 1.25, error }, 1);
    assert.match(result.line, /^node-a: .*未観測 件数不明・1\.3s/);
    assert.equal(result.observed, 0);
  }
});

test('必須項目の欠損・不正値は正常ではなく git_failed として扱う', () => {
  const counts = [undefined, null, '0', false, -1, -0.5, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, {}, []];
  const flags = [undefined, null, 0, 1, 'false', {}, []];
  for (const [fields, values] of [[['tracked', 'untracked', 'conflicted'], counts], [['upstream', 'detached', 'linked'], flags]]) {
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

test('対象 path の欠損・空文字は未観測とし、非空の空白パスは残す', () => {
  for (const value of [undefined, null, '', 0, {}]) {
    const raw = { ...observed(), path: value };
    if (value === undefined) delete raw.path;
    const [row] = hygiene.normalize({ ...audit(), enumerated: 1, repos: [raw] }).repos;
    assert.equal(row.state, 'unobserved');
    assert.equal(row.reason, 'git_failed');
  }
  const [row] = hygiene.normalize({ ...audit(), enumerated: 1, repos: [{ ...observed(), path: ' ' }] }).repos;
  assert.equal(row.state, 'ok');
  assert.equal(row.path, ' ');
});

test('正常なゼロ・真偽値と任意項目の null は保つ', () => {
  const raw = observed();
  assert.deepEqual(hygiene.normalize({ repos: [raw] }).repos[0], { ...raw, reason: null });
  const other = { ...raw, linked: true, detached: true, upstream: true, tracked: 1, untracked: 2, conflicted: 1 };
  assert.deepEqual(hygiene.normalize({ repos: [other] }).repos[0], { ...other, reason: null });
  const largest = { ...raw, tracked: Number.MAX_SAFE_INTEGER };
  assert.equal(hygiene.normalize({ repos: [largest] }).repos[0].tracked, Number.MAX_SAFE_INTEGER);
});

test('任意件数の不正値は未観測にし、null と欠損は許す', () => {
  const fields = ['ahead', 'behind', 'local_only', 'stash'];
  const invalid = ['1', false, -1, -0.5, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, {}, []];
  for (const field of fields) {
    for (const value of invalid) {
      const [row] = hygiene.normalize({ repos: [{ ...observed(), [field]: value }] }).repos;
      assert.equal(row.state, 'unobserved', `${field}: ${String(value)}`);
      assert.equal(row.reason, 'git_failed');
    }
    const missing = observed();
    delete missing[field];
    const [row] = hygiene.normalize({ repos: [missing] }).repos;
    assert.equal(row.state, 'ok');
    assert.equal(row[field], null);
    for (const value of [0, 1.0, Number.MAX_SAFE_INTEGER]) {
      const [valid] = hygiene.normalize({ repos: [{ ...observed(), [field]: value }] }).repos;
      assert.equal(valid.state, 'ok');
      assert.equal(valid[field], value);
    }
  }
});

test('時刻のずれで負数になった経過秒は保つ', () => {
  const [row] = hygiene.normalize({ repos: [{ ...observed(), dirty_age_s: -10, head_age_s: -20 }] }).repos;
  assert.equal(row.state, 'ok');
  assert.equal(row.dirty_age_s, -10);
  assert.equal(row.head_age_s, -20);
});

test('正常な観測でも origin の認証情報は取り除く', () => {
  const raw = { ...observed(), origin: 'https://unit:token@example.com/acme/fixture.git?nonce=1' };
  const [row] = hygiene.normalize({ repos: [raw] }).repos;
  assert.equal(row.state, 'ok');
  assert.equal(row.origin, 'https://example.com/acme/fixture.git');
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
