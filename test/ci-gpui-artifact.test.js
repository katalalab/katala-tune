'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { createArtifact: build } = require('../scripts/ci-gpui-artifact');
const REVISION = 'a'.repeat(40), TREE = 'b'.repeat(40);
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
function fixture(t, platform = 'darwin', arch = 'arm64') {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ci-gpui-artifact-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const release = path.join(root, 'target', 'release');
  fs.mkdirSync(release, { recursive: true });
  const suffix = platform === 'win32' ? '.exe' : '';
  for (const [name, bytes] of [['katala-tune-gpui', 'gui'], ['tune', 'cli'], ['tune-agent', 'sampler']]) {
    fs.writeFileSync(path.join(release, name + suffix), bytes);
  }
  fs.writeFileSync(path.join(root, 'package.json'), JSON.stringify({ version: '0.7.0-rc.1' }));
  const env = { GITHUB_SHA: REVISION, GITHUB_REPOSITORY: 'example/project', GITHUB_WORKFLOW_REF: 'example/project/.github/workflows/gpui-preview.yml@refs/pull/7/merge', GITHUB_WORKFLOW_SHA: REVISION, GITHUB_EVENT_NAME: 'pull_request', GITHUB_RUN_ID: '12345678901234567890', GITHUB_RUN_ATTEMPT: '2', EXTRA_SECRET: 'DO_NOT_EXPORT' };
  const run = (exe, args) => {
    assert.equal(exe, 'git');
    if (args.join(' ') === 'rev-parse HEAD') return REVISION + '\n';
    if (args.join(' ') === 'rev-parse HEAD^{tree}') return TREE + '\n';
    if (args.join(' ') === 'diff --quiet HEAD --') return '';
    throw new Error('unexpected command');
  };
  return { root, release, suffix, dist: path.join(root, 'dist', 'gpui-preview'), options: { root, env, platform, arch, run } };
}
for (const [platform, arch, target, suffix] of [['darwin', 'arm64', 'aarch64-apple-darwin', ''], ['win32', 'x64', 'x86_64-pc-windows-msvc', '.exe']]) {
  test(`${platform} artifact contains exactly three roles and the staged bytes' hashes`, t => {
    const f = fixture(t, platform, arch);
    // Cargo release has other files: only the three fixed platform names may ship.
    fs.writeFileSync(path.join(f.release, 'unrelated-binary'), 'excluded');
    const manifest = build(f.options);
    assert.deepEqual(manifest, {
      schema: 'katala_gpui_preview.v2', version: '0.7.0-rc.1', revision: REVISION, sourceTree: TREE, platform, architecture: arch,
      build: { profile: 'release', target },
      ci: { repository: 'example/project', workflowRef: f.options.env.GITHUB_WORKFLOW_REF, workflowSha: REVISION, eventName: 'pull_request', runId: '12345678901234567890', runAttempt: '2' },
      files: [
        { file: 'katala-tune-gpui' + suffix, role: 'gui', bytes: 3, sha256: sha('gui') },
        { file: 'tune' + suffix, role: 'diagnostic-cli', bytes: 3, sha256: sha('cli') },
        { file: 'tune-agent' + suffix, role: 'native-sampler', bytes: 7, sha256: sha('sampler') },
      ],
    });
    assert.deepEqual(fs.readdirSync(f.dist).sort(), ['katala-tune-gpui' + suffix, 'manifest.json', 'tune' + suffix, 'tune-agent' + suffix].sort());
    for (const file of manifest.files) {
      const bytes = fs.readFileSync(path.join(f.dist, file.file));
      assert.equal(bytes.length, file.bytes);
      assert.equal(sha(bytes), file.sha256);
      assert.deepEqual(bytes, fs.readFileSync(path.join(f.release, file.file)));
      if (process.platform !== 'win32') assert.equal(fs.statSync(path.join(f.dist, file.file)).mode & 0o777, 0o755);
    }
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(f.dist, 'manifest.json'), 'utf8')), manifest);
    for (const forbidden of ['DO_NOT_EXPORT', f.root, 'approved', 'success']) assert.equal(JSON.stringify(manifest).includes(forbidden), false);
  });
}
test('manual dispatch records its branch without inventing approval', t => {
  const f = fixture(t);
  f.options.env.GITHUB_EVENT_NAME = 'workflow_dispatch';
  f.options.env.GITHUB_WORKFLOW_REF = 'example/project/.github/workflows/gpui-preview.yml@refs/heads/preview';
  const manifest = build(f.options);
  assert.equal(manifest.ci.eventName, 'workflow_dispatch');
});
test('missing sampler, empty CLI, wrong-platform names and nonregular inputs fail before output', t => {
  for (const kind of ['missing sampler', 'empty CLI', 'wrong-platform name', 'directory']) {
    const f = fixture(t);
    const sampler = path.join(f.release, 'tune-agent');
    if (kind === 'empty CLI') fs.writeFileSync(path.join(f.release, 'tune'), '');
    else {
      fs.unlinkSync(sampler);
      if (kind === 'wrong-platform name') fs.writeFileSync(sampler + '.exe', 'sampler');
      if (kind === 'directory') fs.mkdirSync(sampler);
    }
    assert.throws(() => build(f.options), /binary/, kind);
    assert.equal(fs.existsSync(f.dist), false, kind);
  }
});
test('stale output is refused without altering the original directory or file', t => {
  for (const kind of ['directory', 'file']) {
    const f = fixture(t);
    fs.mkdirSync(path.dirname(f.dist));
    if (kind === 'directory') { fs.mkdirSync(f.dist); fs.writeFileSync(path.join(f.dist, 'keep'), 'original'); }
    else fs.writeFileSync(f.dist, 'original');
    assert.throws(() => build(f.options), /exist/);
    assert.equal(fs.readFileSync(kind === 'file' ? f.dist : path.join(f.dist, 'keep'), 'utf8'), 'original');
  }
});
test('unsupported platform/architecture and inherited Cargo target overrides fail closed', t => {
  for (const patch of [{ platform: 'linux' }, { platform: 'darwin', arch: 'x64' }, { platform: 'win32', arch: 'arm64' }, ...['CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET', 'CARGO_BUILD_TARGET_DIR'].map(key => ({ envKey: key }))]) {
    const f = fixture(t);
    if (patch.envKey) f.options.env[patch.envKey] = 'stale';
    else Object.assign(f.options, patch);
    assert.throws(() => build(f.options), /platform|architecture|target/);
    assert.equal(fs.existsSync(f.dist), false);
  }
});
test('invalid revision, version, run and workflow metadata never leave a success manifest', t => {
  for (const patch of [{ GITHUB_SHA: '../bad' }, { GITHUB_SHA: 'c'.repeat(40) }, { GITHUB_WORKFLOW_SHA: '' }, { GITHUB_RUN_ID: '0' }, { GITHUB_RUN_ATTEMPT: '1\n' }, { GITHUB_REPOSITORY: '../project' }, { GITHUB_WORKFLOW_REF: 'other/project/.github/workflows/gpui-preview.yml@refs/pull/7/merge' }, { GITHUB_WORKFLOW_REF: 'example/project/.github/workflows/ci.yml@refs/pull/7/merge' }, { GITHUB_EVENT_NAME: 'push' }]) {
    const f = fixture(t);
    Object.assign(f.options.env, patch);
    assert.throws(() => build(f.options), /invalid|mismatch|differs/);
    assert.equal(fs.existsSync(f.dist), false);
  }
  const f = fixture(t);
  fs.writeFileSync(path.join(f.root, 'package.json'), JSON.stringify({ version: '../../bad' }));
  assert.throws(() => build(f.options), /version/);
  assert.equal(fs.existsSync(f.dist), false);
});
test('invalid source tree or modified tracked source fails before staging', t => {
  for (const kind of ['tree', 'dirty']) {
    const f = fixture(t), run = f.options.run;
    f.options.run = (exe, args) => {
      if (kind === 'tree' && args[1] === 'HEAD^{tree}') return 'bad';
      if (kind === 'dirty' && args[0] === 'diff') throw new Error('dirty source');
      return run(exe, args);
    };
    assert.throws(() => build(f.options), /tree|source/);
    assert.equal(fs.existsSync(f.dist), false);
  }
});
test('symlink binary, target directory and dist parent are rejected without writing through them', t => {
  for (const kind of ['binary', 'target', 'dist']) {
    const f = fixture(t), actual = path.join(f.root, 'actual');
    const link = kind === 'binary' ? path.join(f.release, 'tune-agent') : path.join(f.root, kind);
    if (kind === 'binary') { fs.writeFileSync(actual, 'sampler'); fs.unlinkSync(link); }
    else { fs.mkdirSync(actual); if (kind === 'target') fs.renameSync(link, path.join(f.root, 'saved-target')); }
    try { fs.symlinkSync(actual, link, kind === 'binary' ? 'file' : 'junction'); }
    catch (error) { if (error.code === 'EPERM') { t.skip('fixture symlinks unavailable'); return; } throw error; }
    assert.throws(() => build(f.options), /symlink|parent/);
    assert.equal(fs.existsSync(f.dist), false);
    if (kind !== 'binary') assert.deepEqual(fs.readdirSync(actual), []);
  }
});
test('a copy failure leaves no manifest and the partial directory cannot be reused', t => {
  const f = fixture(t), original = fs.openSync;
  // Inject a storage failure on this owned fixture only, after GUI/CLI are copied.
  fs.openSync = (file, ...args) => {
    if (file === path.join(f.dist, 'tune-agent')) throw new Error('fixture disk failure');
    return original(file, ...args);
  };
  try { assert.throws(() => build(f.options), /fixture disk failure/); }
  finally { fs.openSync = original; }
  assert.deepEqual(fs.readdirSync(f.dist).sort(), ['katala-tune-gpui', 'tune']);
  assert.throws(() => build(f.options), /exist/);
  assert.equal(fs.existsSync(path.join(f.dist, 'manifest.json')), false);
});
for (const failure of ['partial write', 'close']) {
  test(`manifest ${failure} failure leaves no published manifest and refuses directory reuse`, t => {
    const f = fixture(t), original = fs.writeFileSync;
    fs.writeFileSync = (file, data, ...args) => {
      if (path.dirname(file) !== f.dist || !path.basename(file).includes('manifest')) return original(file, data, ...args);
      // Real fixture bytes reach disk. A close failure is reported by the writer
      // after all bytes; neither writer error may publish the final filename.
      original(file, failure === 'partial write' ? data.slice(0, 24) : data, ...args);
      const error = new Error(`fixture manifest ${failure} failure`);
      error.code = 'EIO';
      throw error;
    };
    try { assert.throws(() => build(f.options), { code: 'EIO' }); }
    finally { fs.writeFileSync = original; }
    assert.equal(fs.existsSync(path.join(f.dist, 'manifest.json')), false);
    assert.throws(() => build(f.options), /exist/);
    assert.equal(fs.existsSync(path.join(f.dist, 'manifest.json')), false);
  });
}
test('oversized required binary is rejected before copying any bytes', t => {
  const f = fixture(t), file = path.join(f.release, 'tune-agent');
  fs.truncateSync(file, 256 * 1024 * 1024 + 1);
  assert.throws(() => build(f.options), /binary/);
  assert.equal(fs.existsSync(f.dist), false);
});
test('artifact version obeys the existing package SemVer contract', t => {
  for (const version of ['01.0.0', '1.0.0-alpha..1', '1.0.0-01']) {
    const f = fixture(t);
    fs.writeFileSync(path.join(f.root, 'package.json'), JSON.stringify({ version }));
    assert.throws(() => build(f.options), /version/);
    assert.equal(fs.existsSync(f.dist), false);
  }
});
