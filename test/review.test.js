'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function loadModel() {
  const window = {};
  vm.runInNewContext(fs.readFileSync('renderer/review-model.js', 'utf8'), { window, Date, URL });
  return window.KT_REVIEW_MODEL;
}

test('task-completion.v1 を安全な表示用モデルへ変換し、申告を未照合として表す', () => {
  const model = loadModel();
  const result = model.parse({
    schema: 'task-completion.v1',
    tasks: [{
      id: 'review-screen',
      goal: '表示を確認する',
      target: 'repository',
      revision: 'abc123',
      delivery: 'repository',
      gates: { implementation: { status: 'pass', evidence: [{ ref: 'https://github.com/example/tune/pull/1', result: 'test', observed_at: '2026-10-19T00:00:00Z', revision: 'abc123', environment: 'repository' }] } },
      review: { status: 'accepted' },
    }],
    coverage: { expected: ['repository'], observed: ['repository'], unobserved: [] },
  }, new Date('2026-10-19T12:00:00Z'));

  assert.equal(result.ok, true);
  assert.equal(result.tasks[0].gates.implementation.statusLabel, '成功の申告（未照合）');
  assert.equal(result.tasks[0].review.acceptedLabel, '受入の申告（未照合）');
  assert.equal(result.tasks[0].gates.implementation.evidence[0].link, 'https://github.com/example/tune/pull/1');
  assert.deepEqual(Array.from(result.coverage.missing), []);
});

test('上限超過、型不正、重複IDを文書全体として拒否する', () => {
  const model = loadModel();
  const valid = { schema: 'task-completion.v1', tasks: [{ id: 'one' }] };
  for (const input of [
    { ...valid, tasks: [] },
    { ...valid, tasks: [{ id: 'same' }, { id: 'same' }] },
    { ...valid, tasks: [{ id: 'one', goal: 1 }] },
    { ...valid, tasks: [{ id: 'one', delivery: 'remote' }] },
    { ...valid, tasks: [{ id: 'one', gates: { implementation: { evidence: 'bad' } } }] },
    { ...valid, tasks: Array.from({ length: 1001 }, (_, i) => ({ id: String(i) })) },
  ]) assert.throws(() => model.parse(input), /./);
});

test('coverage の空配列は不足として表示し、実行環境との差は配布・受入段階だけに警告する', () => {
  const model = loadModel();
  const input = { schema: 'task-completion.v1', tasks: [{
    id: 'one', target: 'repository',
    gates: {
      verification: { evidence: [{ environment: 'local' }] },
      deployment: { evidence: [{ environment: 'local' }] },
    },
  }], coverage: { expected: [], observed: [], unobserved: [] } };
  const result = model.parse(input);
  const task = result.tasks[0];
  assert.deepEqual(Array.from(result.coverage.expected), []);
  assert.match(result.coverage.insufficient.join(' '), /想定/);
  assert.match(result.coverage.insufficient.join(' '), /観測/);
  assert.doesNotMatch(task.gates.verification.warnings.join(' '), /対象環境/);
  assert.match(task.gates.deployment.warnings.join(' '), /対象環境/);
});

test('表示しない未知キーを含めても、文字列長と深さの上限は全入力で検査する', () => {
  const model = loadModel();
  const deep = {}; let cursor = deep;
  for (let i = 0; i < 17; i++) cursor.next = cursor = {};
  assert.throws(() => model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'one', unknown: deep }] }));
  assert.throws(() => model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'one', unknown: 'x'.repeat(8193) }] }));
  assert.equal(model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'one', unknown: { command: 'ignore' } }] }).ok, true);
});

test('許可したGitHubのPR・Issueだけをリンクにし、証跡とcoverageの不足は警告として残す', () => {
  const model = loadModel();
  for (const url of [
    'javascript:alert(1)', 'data:text/html,x', 'https://github.com.evil.example/a/b/pull/1',
    ['https://user', 'github.com/a/b/pull/1'].join('@'), 'https://github.com:444/a/b/pull/1',
    'https://github.com/a/b/pull/1?x=1', 'https://github.com/a/b/pull/1#x',
    'https://github.com/a/b/pull/1?', 'https://github.com/a/b/pull/1#',
    'https://github.com/a%2fb/pull/1', 'https://github.com/a/b/pull/0', 'https://github.com/a/b/pull/1\n',
  ]) assert.equal(model.githubUrl(url), null, url);
  assert.equal(model.githubUrl('https://github.com/a_b/re.po/issues/12'), 'https://github.com/a_b/re.po/issues/12');

  const task = model.parse({ schema: 'task-completion.v1', tasks: [{
    id: 'one', target: 'repository', revision: 'head',
    gates: { deployment: { status: 'pass', evidence: [{ ref: '<b>text</b>', observed_at: '2026-10-20T00:00:00Z', revision: 'other', environment: 'local' }] } },
  }], coverage: { expected: ['a', 'b', 'b'], observed: ['a', 'extra'], unobserved: ['b'] } }, new Date('2026-10-19T00:00:00Z')).tasks[0];
  assert.match(task.gates.deployment.warnings.join(' '), /未来/);
  assert.match(task.gates.deployment.warnings.join(' '), /revision/);
  assert.match(task.gates.deployment.warnings.join(' '), /対象環境/);
  const coverage = model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'coverage' }], coverage: { expected: ['a', 'b', 'b'], observed: ['a', 'extra'], unobserved: ['b'] } }).coverage;
  assert.deepEqual(Array.from(coverage.missing), ['b']);
  assert.deepEqual(Array.from(coverage.extra), ['extra']);
  assert.match(coverage.duplicates.join(' '), /expected: b/);
});

test('review の入力例は正典の文書coverageとreview statusを使う', () => {
  const model = loadModel();
  const example = JSON.parse(fs.readFileSync('config/review.example.json', 'utf8'));
  const result = model.parse(example);
  assert.equal(result.tasks[0].review.acceptedLabel, '受入の申告（未照合）');
  assert.equal(result.coverage.declared, true);
});

test('未知のgateとreviewのstatusを成功や受入へ昇格しない', () => {
  const model = loadModel();
  for (const status of ['approved_elsewhere', 'constructor', 'toString', '__proto__']) {
    const task = model.parse({ schema: 'task-completion.v1', tasks: [{
      id: 'status', gates: { verification: { status } }, review: { status },
    }] }).tasks[0];
    assert.match(task.gates.verification.statusLabel, /未対応/, status);
    assert.match(task.review.acceptedLabel, /未対応/, status);
  }
});

test('値だけでなくキーも文字列上限を超えれば拒否する', () => {
  const model = loadModel();
  const key = 'x'.repeat(8193);
  assert.throws(() => model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'one', [key]: 'ignored' }] }));
});

test('正規化が必要なGitHub URLはリンクにせず、canonicalな文字列だけを許可する', () => {
  const model = loadModel();
  for (const url of [
    ' https://github.com/a/b/pull/1', 'https://github.com/a/b/pull/1 ', 'HTTPS://github.com/a/b/pull/1',
    'https://GITHUB.com/a/b/pull/1', 'https://github.com/a/b/../b/pull/1', 'https://github.com/a\\b/pull/1',
  ]) assert.equal(model.githubUrl(url), null, url);
  assert.equal(model.githubUrl('https://github.com/a/b/pull/1'), 'https://github.com/a/b/pull/1');
});

test('Python由来の1〜9桁小数秒を受け、空白idとsentinel文字列を区別する', () => {
  const model = loadModel();
  for (let digits = 1; digits <= 9; digits++) {
    const observedAt = `2026-10-19T00:00:00.${'1'.repeat(digits)}Z`;
    const gate = model.parse({
      schema: 'task-completion.v1',
      tasks: [{ id: 'fraction', gates: { verification: { evidence: [{ observed_at: observedAt }] } } }],
    }, new Date('2026-10-20T00:00:00Z')).tasks[0].gates.verification;
    assert.doesNotMatch(gate.warnings.join(' '), /日時/, observedAt);
  }
  assert.throws(() => model.parse({ schema: 'task-completion.v1', tasks: [{ id: '   ' }] }));
  assert.equal(model.parse({ schema: 'task-completion.v1', tasks: [{ id: '未記載' }] }).tasks[0].id, '未記載');
});

test('passとacceptedの不足・受入矛盾は警告し、completion判定には使わない', () => {
  const model = loadModel();
  const task = model.parse({ schema: 'task-completion.v1', tasks: [{
    id: 'claims', gates: { implementation: { status: 'pass', evidence: [{}] }, acceptance: { status: 'blocked' } }, review: { status: 'accepted' },
  }] }).tasks[0];
  assert.match(task.gates.implementation.warnings.join(' '), /ref/);
  assert.match(task.gates.implementation.warnings.join(' '), /observed_at/);
  assert.match(task.review.warnings.join(' '), /review ref/);
  assert.match(task.review.warnings.join(' '), /acceptance gate/);
  assert.equal(model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'pending', review: { status: 'pending' } }] }).tasks[0].review.acceptedLabel, '受入待ちの申告');
});

test('pass申告の証跡resultがpass以外なら警告し、passなら警告しない', () => {
  const model = loadModel();
  const base = { ref: 'https://github.com/a/b/pull/1', observed_at: '2026-10-19T00:00:00Z', revision: 'head', environment: 'repository' };
  const warningsFor = (result) => model.parse({ schema: 'task-completion.v1', tasks: [{
    id: 'result', target: 'repository', revision: 'head',
    gates: { verification: { status: 'pass', evidence: [{ ...base, result }] } },
  }] }, new Date('2026-10-20T00:00:00Z')).tasks[0].gates.verification.warnings.join(' ');
  assert.match(warningsFor('fail'), /result が pass ではありません: fail/);
  assert.doesNotMatch(warningsFor('pass'), /result が pass ではありません/);
});

test('前後空白を除いたtask idで一意性を判定し、表示にも除いた値を使う', () => {
  const model = loadModel();
  assert.throws(() => model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'same' }, { id: ' same ' }] }));
  assert.equal(model.parse({ schema: 'task-completion.v1', tasks: [{ id: ' trimmed ' }] }).tasks[0].id, 'trimmed');
  assert.throws(() => model.parse({ schema: 'task-completion.v1', tasks: [{ id: ' \t ' }] }));
});

test('coverageの重なりと想定外unobservedを警告して全値を保持する', () => {
  const model = loadModel();
  const coverage = model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'coverage' }], coverage: {
    expected: ['a', 'b'], observed: ['a'], unobserved: ['a', 'outside'],
  } }).coverage;
  assert.deepEqual(Array.from(coverage.overlap), ['a']);
  assert.deepEqual(Array.from(coverage.unobservedOutsideExpected), ['outside']);
});

test('文字列の未記載は値として警告し、欠測はnullで表す', () => {
  const model = loadModel();
  const task = model.parse({ schema: 'task-completion.v1', tasks: [{
    id: 'literal-missing', target: 'target-a', revision: 'revision-a',
    gates: { deployment: { status: '未記載', evidence: [{ ref: '未記載', observed_at: '未記載', revision: '未記載', environment: '未記載' }] } },
    review: { status: '未記載', ref: '未記載', observed_at: '未記載', revision: '未記載', target: '未記載' },
  }] }).tasks[0];
  const evidence = task.gates.deployment.evidence[0];
  assert.equal(model.missing, null);
  assert.equal(task.goal, null);
  assert.match(task.gates.deployment.statusLabel, /未対応/);
  assert.match(evidence.warnings.join(' '), /日時が不正/);
  assert.match(evidence.warnings.join(' '), /revision/);
  assert.match(evidence.warnings.join(' '), /対象環境/);
  assert.match(task.review.acceptedLabel, /未対応/);
  assert.match(task.review.warnings.join(' '), /レビュー日時が不正/);
  assert.match(task.review.warnings.join(' '), /review revision/);
  assert.match(task.review.warnings.join(' '), /review target/);
});

test('coverageの集合比較は重複を増幅せず、合計10000件を超えれば拒否する', () => {
  const model = loadModel();
  const coverage = model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'sets' }], coverage: {
    expected: ['a', 'a', 'b'], observed: ['a', 'a', 'extra'], unobserved: ['a', 'a', 'outside'],
  } }).coverage;
  assert.deepEqual(Array.from(coverage.missing), ['b']);
  assert.deepEqual(Array.from(coverage.extra), ['extra']);
  assert.deepEqual(Array.from(coverage.overlap), ['a']);
  assert.deepEqual(Array.from(coverage.unobservedOutsideExpected), ['outside']);
  assert.throws(() => model.parse({ schema: 'task-completion.v1', tasks: [{ id: 'large-coverage' }], coverage: {
    expected: Array.from({ length: 10001 }, (_, index) => `target-${index}`),
  } }), /coverage は合計10000件まで/);
});
