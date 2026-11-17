'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.join(__dirname, '..');

function isolatedTemp(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-temp-cleanup-test-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const tmp = path.join(dir, 'tmp');
  fs.mkdirSync(tmp);
  fs.writeFileSync(path.join(tmp, 'unrelated.txt'), 'keep');
  const env = { ...process.env, TMPDIR: tmp, TMP: tmp, TEMP: tmp };
  delete env.NODE_TEST_CONTEXT;
  return { dir, tmp, env };
}

for (const script of ['eval.js', 'eval_netsec.js']) {
  for (const malformed of [false, true]) {
    test(`${script} removes only its temporary database after ${malformed ? 'an error' : 'success'}`, (t) => {
      const { dir, tmp, env } = isolatedTemp(t);
      const cases = path.join(dir, 'cases.json');
      fs.writeFileSync(cases, JSON.stringify({
        logFindings: [{ node_id: 'test', logs: malformed ? null : [], dropped: [], now: 0 }],
      }));
      const result = spawnSync(process.execPath, [path.join(root, 'crates/tune-core/tests/js', script), cases], {
        cwd: root, env, encoding: 'utf8', timeout: 30000,
      });
      assert.equal(result.status, 0, result.stderr);
      const output = JSON.parse(result.stdout);
      assert.equal(output.logFindings.length, 1);
      assert.equal(typeof output.logFindings[0]?.__throw === 'string', malformed);
      const prefix = script === 'eval.js' ? 'kt-parity-' : 'kt-parity-ns-';
      assert.deepEqual(fs.readdirSync(tmp).filter((name) => name.startsWith(prefix)), []);
      assert.equal(fs.readFileSync(path.join(tmp, 'unrelated.txt'), 'utf8'), 'keep');
    });
  }
}

test('parity evaluation preserves a caller-provided database directory', (t) => {
  const { dir, tmp, env } = isolatedTemp(t);
  const userData = path.join(dir, 'existing-data');
  fs.mkdirSync(userData);
  fs.writeFileSync(path.join(userData, 'keep.txt'), 'keep');
  const cases = path.join(dir, 'cases.json');
  fs.writeFileSync(cases, JSON.stringify({
    logFindings: [{ node_id: 'test', logs: [], dropped: [], now: 0 }],
    dbWrite: {
      dir: userData,
      write: { snapshots: [], actions: [], logs: [], cursors: [], checks: [], meta: {} },
      spec: { nodes: [], queries: [], signatures: [], meta: [] },
    },
  }));
  const result = spawnSync(process.execPath, [path.join(root, 'crates/tune-core/tests/js/eval.js'), cases], {
    cwd: root, env, encoding: 'utf8', timeout: 30000,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.ok(fs.statSync(path.join(userData, 'katala-tune.db')).size > 0);
  assert.equal(fs.readFileSync(path.join(userData, 'keep.txt'), 'utf8'), 'keep');
  assert.deepEqual(fs.readdirSync(tmp).filter((name) => name.startsWith('kt-parity-')), []);
  assert.equal(fs.readFileSync(path.join(tmp, 'unrelated.txt'), 'utf8'), 'keep');
});

test('log resilience tests remove their temporary inputs and databases', (t) => {
  const { tmp, env } = isolatedTemp(t);
  const result = spawnSync(process.execPath, ['--test', '--test-reporter=tap', path.join(root, 'test/log-resilience.test.js')], {
    cwd: root, env, encoding: 'utf8', timeout: 60000,
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /^# tests [1-9]\d*/m);
  assert.deepEqual(fs.readdirSync(tmp).filter((name) => name.startsWith('kt-resil-')), []);
  assert.equal(fs.readFileSync(path.join(tmp, 'unrelated.txt'), 'utf8'), 'keep');
});
