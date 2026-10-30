'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { createArtifact } = require('../scripts/ci-cli-artifact');
const sha = (data) => createHash('sha256').update(data).digest('hex');
const MERGE = 'a'.repeat(40), TREE = 'b'.repeat(40), HEAD = 'c'.repeat(40), BASE = 'd'.repeat(40);
function fixture(t, patch = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ci-cli-artifact-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'target', 'debug'), { recursive: true });
  fs.writeFileSync(path.join(root, 'target', 'debug', 'tune.exe'), Buffer.from([77, 90, 0, 1, 2]));
  fs.writeFileSync(path.join(root, 'Cargo.lock'), 'version = 4\n');
  const eventPath = path.join(root, 'event.json');
  fs.writeFileSync(eventPath, JSON.stringify({ pull_request: { head: { sha: HEAD, label: 'discard' }, base: { sha: BASE } }, actor: 'discard', secret: 'DO_NOT_EXPORT' }));
  const env = { GITHUB_SHA: MERGE, GITHUB_REPOSITORY: 'example/project', GITHUB_WORKFLOW_REF: 'example/project/.github/workflows/ci.yml@refs/pull/7/merge', GITHUB_WORKFLOW_SHA: MERGE, GITHUB_EVENT_NAME: 'pull_request', GITHUB_RUN_ID: '12345678901234567890', GITHUB_RUN_ATTEMPT: '2', GITHUB_EVENT_PATH: eventPath, EXTRA_SECRET: 'DO_NOT_EXPORT', RUNNER_NAME: 'DO_NOT_EXPORT', ...patch };
  const calls = [];
  const run = (exe, args) => {
    calls.push([exe, args]);
    if (exe === 'git' && args.join(' ') === 'rev-parse HEAD') return MERGE + '\n';
    if (exe === 'git' && args.join(' ') === 'rev-parse HEAD^{tree}') return TREE + '\n';
    if (exe === 'git' && args.join(' ') === 'diff --quiet HEAD --') return '';
    if (exe === 'git' && args.join(' ') === 'show HEAD:Cargo.lock') return 'version = 4\n';
    if (exe === 'rustc' && args.join(' ') === '-vV') return 'rustc 1.99.0 (012345678 2026-10-01)\nbinary: rustc\ncommit-hash: 0123456789012345678901234567890123456789\ncommit-date: 2026-10-01\nrelease: 1.99.0\nhost: x86_64-pc-windows-msvc\nLLVM version: 23.1.0\n';
    throw new Error('unexpected command');
  };
  const options = { root, env, platform: 'win32', arch: 'x64', run };
  return { root, env, options, calls, dist: path.join(root, 'dist', 'ci-cli') };
}
test('PR merge artifact preserves actual merge/tree and separate PR head/base with exact two hashed files', t => {
  const f = fixture(t);
  const manifest = createArtifact(f.options);
  assert.deepEqual(manifest.source, { githubSha: MERGE, head: MERGE, tree: TREE, cargoLockSha256: sha('version = 4\n'), cargoLockGitSha256: sha('version = 4\n'), pullRequest: { headSha: HEAD, baseSha: BASE } });
  assert.deepEqual(manifest.artifact, { file: 'tune.exe', bytes: 5, sha256: sha(Buffer.from([77, 90, 0, 1, 2])) });
  assert.equal(manifest.build.profile, 'debug');
  assert.equal(manifest.build.target, 'x86_64-pc-windows-msvc');
  assert.deepEqual(manifest.build.rustc, { versionLine: 'rustc 1.99.0 (012345678 2026-10-01)', binary: 'rustc', commitHash: '0123456789012345678901234567890123456789', commitDate: '2026-10-01', release: '1.99.0', host: 'x86_64-pc-windows-msvc', llvmVersion: '23.1.0' });
  assert.equal(manifest.ci.runId, '12345678901234567890');
  assert.equal(manifest.ci.runAttempt, '2');
  assert.deepEqual(fs.readdirSync(f.dist).sort(), ['manifest.json', 'tune.exe']);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(f.dist, 'manifest.json'), 'utf8')), manifest);
  assert.deepEqual(fs.readFileSync(path.join(f.dist, 'tune.exe')), fs.readFileSync(path.join(f.root, 'target', 'debug', 'tune.exe')));
  assert.deepEqual(f.calls, [['git', ['rev-parse', 'HEAD']], ['git', ['rev-parse', 'HEAD^{tree}']], ['git', ['diff', '--quiet', 'HEAD', '--']], ['rustc', ['-vV']], ['git', ['show', 'HEAD:Cargo.lock']]]);
  const text = JSON.stringify(manifest);
  for (const forbidden of ['DO_NOT_EXPORT', 'actor', 'runnerName', f.root, 'event.json', 'label']) assert.equal(text.includes(forbidden), false, forbidden);
});
test('push keeps no PR fields and never requires an event payload', t => {
  const f = fixture(t, { GITHUB_EVENT_NAME: 'push', GITHUB_EVENT_PATH: 'missing', GITHUB_WORKFLOW_REF: 'example/project/.github/workflows/ci.yml@refs/heads/main' });
  const manifest = createArtifact(f.options);
  assert.equal(manifest.ci.eventName, 'push');
  assert.equal(Object.hasOwn(manifest.source, 'pullRequest'), false);
});
test('GitHub SHA must equal actual checked-out HEAD before any dist is created', t => {
  const f = fixture(t, { GITHUB_SHA: HEAD });
  assert.throws(() => createArtifact(f.options), /HEAD/);
  assert.equal(fs.existsSync(f.dist), false);
});
test('only native Windows x64 and pinned rustc host/release may produce an artifact', t => {
  for (const changed of [{ platform: 'darwin' }, { arch: 'arm64' }, { run: () => 'untrusted raw output' }]) {
    const f = fixture(t);
    assert.throws(() => createArtifact({ ...f.options, ...changed }));
    assert.equal(fs.existsSync(f.dist), false);
  }
  for (const rust of ['release: 1.98.0\nhost: x86_64-pc-windows-msvc', 'release: 1.99.0\nhost: aarch64-pc-windows-msvc']) {
    const f = fixture(t), real = f.options.run;
    assert.throws(() => createArtifact({ ...f.options, run: (exe, args) => exe === 'rustc' ? rust : real(exe, args) }), /toolchain/);
  }
});
test('missing, empty and oversized binaries are rejected', t => {
  for (const kind of ['missing', 'empty', 'oversized']) {
    const f = fixture(t), binary = path.join(f.root, 'target', 'debug', 'tune.exe');
    fs.unlinkSync(binary);
    if (kind === 'empty') fs.writeFileSync(binary, '');
    if (kind === 'oversized') { const fd = fs.openSync(binary, 'wx'); fs.ftruncateSync(fd, 128 * 1024 * 1024 + 1); fs.closeSync(fd); }
    assert.throws(() => createArtifact(f.options), /binary/);
    assert.equal(fs.existsSync(f.dist), false, kind);
  }
});
test('existing artifact directory and nondefault Cargo targets fail without overwriting', t => {
  const f = fixture(t);
  fs.mkdirSync(f.dist, { recursive: true }); fs.writeFileSync(path.join(f.dist, 'keep'), 'unchanged');
  assert.throws(() => createArtifact(f.options), /exist/);
  assert.equal(fs.readFileSync(path.join(f.dist, 'keep'), 'utf8'), 'unchanged');
  for (const key of ['CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET', 'CARGO_BUILD_TARGET_DIR']) {
    const g = fixture(t, { [key]: 'other-target' });
    assert.throws(() => createArtifact(g.options), /target/);
  }
});
test('only explicit CI metadata is accepted, with no arbitrary event or field fallback', t => {
  for (const patch of [{ GITHUB_EVENT_NAME: 'workflow_dispatch' }, { GITHUB_RUN_ATTEMPT: '0' }, { GITHUB_WORKFLOW_SHA: '' }, { GITHUB_REPOSITORY: 'invalid' }]) {
    const f = fixture(t, patch); assert.throws(() => createArtifact(f.options));
  }
  const f = fixture(t); fs.writeFileSync(f.env.GITHUB_EVENT_PATH, JSON.stringify({ pull_request: { head: { sha: 'bad' }, base: { sha: BASE } } }));
  assert.throws(() => createArtifact(f.options), /PR/);
});

test('tracked source modifications reject provenance before packaging', t => {
  const f = fixture(t), run = f.options.run;
  assert.throws(() => createArtifact({ ...f.options, run: (exe, args) => {
    if (exe === 'git' && args[0] === 'diff') throw new Error('dirty tracked source');
    return run(exe, args);
  } }), /tracked|source/);
  assert.equal(fs.existsSync(f.dist), false);
});

test('a symlink input binary is rejected explicitly', t => {
  const f = fixture(t), binary = path.join(f.root, 'target', 'debug', 'tune.exe');
  fs.unlinkSync(binary);
  const real = path.join(f.root, 'actual.exe'); fs.writeFileSync(real, 'MZ');
  try { fs.symlinkSync(real, binary); } catch (e) {
    if (e.code === 'EPERM') { t.skip('OS does not permit fixture symlinks; rejection requires a symlink-capable host'); return; }
    throw e;
  }
  assert.throws(() => createArtifact(f.options), /binary.*symlink/);
  assert.equal(fs.existsSync(f.dist), false);
});
test('Windows artifact steps append to existing checks and upload exactly the verified pair', () => {
  const workflowSource = fs.readFileSync(path.join(__dirname, '..', '.github', 'workflows', 'ci.yml'), 'utf8');
  for (const eol of ['\n', '\r\n']) {
    const raw = workflowSource.replace(/\r?\n/g, eol);
    const workflow = raw.replace(/\r\n/g, '\n');
    const build = workflow.indexOf('      - name: Windows CLI artifact build');
    assert.ok(build > workflow.indexOf('run: cargo build --package katala-tune --locked'));
    const tail = workflow.slice(build);
    assert.match(tail, /run: cargo build --locked -p tune-cli/);
    assert.match(tail, /timeout-minutes: 20/);
    assert.equal((tail.match(/if: runner.os == 'Windows'/g) || []).length, 3);
    assert.match(tail, /run: node scripts\/ci-cli-artifact.js/);
    assert.match(tail, /uses: actions\/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9/);
    assert.match(tail, /name: tune-cli-windows-x64-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}/);
    assert.match(tail, /path: \|\n            dist\/ci-cli\/tune.exe\n            dist\/ci-cli\/manifest.json\n/);
    assert.match(tail, /if-no-files-found: error/);
    assert.match(tail, /retention-days: 7/);
    assert.match(tail, /overwrite: false/);
    assert.match(tail, /include-hidden-files: false/);
  }
});

test('Cargo lock retains distinct checkout-byte and canonical Git hashes under CRLF checkout', t => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.root, 'Cargo.lock'), 'version = 4\r\n');
  const manifest = createArtifact(f.options);
  assert.equal(manifest.source.cargoLockSha256, sha('version = 4\r\n'));
  assert.equal(manifest.source.cargoLockGitSha256, sha('version = 4\n'));
  assert.notEqual(manifest.source.cargoLockSha256, manifest.source.cargoLockGitSha256);
});
