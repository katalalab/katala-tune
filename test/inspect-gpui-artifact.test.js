'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawnSync } = require('node:child_process');
const { createArtifact } = require('../scripts/ci-gpui-artifact');
const CLI = path.resolve(__dirname, '../scripts/inspect-gpui-artifact.js');
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const inspect = options => require('../scripts/inspect-gpui-artifact').inspectArtifact(options);

function fixture(t, platform = 'darwin') {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'gpui-inspection-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const release = path.join(root, 'target', 'release');
  fs.mkdirSync(release, { recursive: true });
  const suffix = platform === 'win32' ? '.exe' : '';
  for (const [name, bytes] of [['katala-tune-gpui', 'gui'], ['tune', 'cli'], ['tune-agent', 'sampler']]) fs.writeFileSync(path.join(release, name + suffix), bytes);
  fs.writeFileSync(path.join(root, 'package.json'), '{"version":"0.7.0-rc.1"}');
  const env = {
    GITHUB_SHA: 'a'.repeat(40), GITHUB_REPOSITORY: 'example/project',
    GITHUB_WORKFLOW_REF: 'example/project/.github/workflows/gpui-preview.yml@refs/pull/7/merge',
    GITHUB_WORKFLOW_SHA: 'c'.repeat(40), GITHUB_EVENT_NAME: 'pull_request',
    GITHUB_RUN_ID: '12345678901234567890123456789012', GITHUB_RUN_ATTEMPT: '2',
  };
  const architecture = platform === 'win32' ? 'x64' : 'arm64';
  const manifest = createArtifact({ root, env, platform, arch: architecture, run: (_exe, args) => {
    if (args.join(' ') === 'rev-parse HEAD') return env.GITHUB_SHA;
    if (args.join(' ') === 'rev-parse HEAD^{tree}') return 'b'.repeat(40);
    if (args.join(' ') === 'diff --quiet HEAD --') return '';
    throw new Error('unexpected fixture command');
  } });
  const source = path.join(root, 'dist', 'gpui-preview');
  const options = { source, platform, architecture };
  const manifestFile = path.join(source, 'manifest.json');
  const sampler = path.join(source, 'tune-agent' + suffix);
  const save = () => fs.writeFileSync(manifestFile, JSON.stringify(manifest, null, 2) + '\n');
  const args = ['--source', source, '--platform', platform, '--architecture', architecture];
  const cli = (argv = args) => spawnSync(process.execPath, [CLI, ...argv], { encoding: 'utf8', timeout: 5000 });
  return { root, source, options, manifest, manifestFile, sampler, suffix, save, args, cli };
}

test('explicit CLI checks producer files and raw manifest integrity without granting approval', t => {
  const f = fixture(t);
  fs.appendFileSync(f.manifestFile, ' \n\t');
  const result = f.cli();
  assert.equal(result.status, 0, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.status, 'integrity-checked');
  assert.match(output.text, /files.*manifest.*integrity checked/i);
  assert.match(output.text, /provenance.*approval.*install.*unverified/i);
  assert.equal(output.manifestSha256, sha(fs.readFileSync(f.manifestFile)));
  assert.equal(output.source, fs.realpathSync(f.source));
  assert.deepEqual(output.files, f.manifest.files);
  assert.deepEqual(output.nativeSampler, { file: 'tune-agent', role: 'native-sampler', bytes: 7, sha256: sha('sampler') });
  assert.equal(output.manifest.ci.runId, '12345678901234567890123456789012');
  for (const field of ['approved', 'installed', 'ciSuccess']) assert.equal(Object.hasOwn(output, field), false);
});

for (const platform of ['darwin', 'win32']) test(`${platform} declared tuple works on the inspection host and returns complete validated metadata`, t => {
  const f = fixture(t, platform);
  // Extra entries are neither opened nor promoted into output.
  fs.mkdirSync(path.join(f.source, 'unrelated'));
  fs.writeFileSync(path.join(f.source, 'unrelated', 'keep'), 'unchanged');
  const output = inspect(f.options);
  const { files, ...metadata } = f.manifest;
  assert.deepEqual(output.manifest, metadata);
  assert.deepEqual(output.files, files);
  assert.deepEqual(output.nativeSampler, files[2]);
  assert.equal(output.manifest.build.target, platform === 'darwin' ? 'aarch64-apple-darwin' : 'x86_64-pc-windows-msvc');
});

test('CLI requires exact explicit options and never echoes rejected values', t => {
  const f = fixture(t), secret = 'UNTRUSTED_VALUE';
  for (const argv of [[], f.args.slice(0, 4), [...f.args, '--source', f.source], [...f.args, '--other', secret], [...f.args, secret], ['--source=' + secret, ...f.args.slice(2)], ['--source', '--platform', 'darwin', '--architecture', 'arm64'], ['--source', 'relative', ...f.args.slice(2)]]) {
    const result = f.cli(argv);
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.match(result.stderr, /^GPUI artifact input\/arguments failed\n$/);
    assert.equal(result.stderr.includes(secret), false);
  }
  for (const options of [{}, { ...f.options, source: 'relative' }, { ...f.options, source: f.source + '\0' }, { ...f.options, unexpected: true }, { ...f.options, platform: 'linux' }, { ...f.options, architecture: 'x64' }]) assert.throws(() => inspect(options), /input\/arguments/);
});

test('manifest decoding and bounded regular-file requirements reject malformed, empty and oversized input', t => {
  for (const data of ['{not json', '', ' '.repeat(65537), 'null', '[]']) {
    const f = fixture(t);
    fs.writeFileSync(f.manifestFile, data);
    assert.throws(() => inspect(f.options), /manifest\/(metadata|decode|schema)/);
  }
});

test('closed v2 metadata rejects omitted, extra, wrong-type and mismatched producer fields', t => {
  const changes = [
    m => { m.schema = 'katala_gpui_preview.v1'; },
    m => { delete m.sourceTree; }, m => { m.extra = 'UNTRUSTED_VALUE'; },
    m => { m.build.extra = true; }, m => { m.ci.extra = true; },
    m => { m.version = '1.0.0-01'; }, m => { m.version += '\n'; },
    m => { m.revision = 'A'.repeat(40); }, m => { m.sourceTree = 1; },
    m => { m.ci.workflowSha = 'a'.repeat(39); },
    m => { m.ci.runId = 123; }, m => { m.ci.runAttempt = '01'; },
    m => { m.ci.runId = '1'.repeat(33); }, m => { m.ci.runAttempt = '2\n'; },
    m => { m.platform = 'win32'; }, m => { m.architecture = 'x64'; },
    m => { m.build.profile = 'debug'; }, m => { m.build.target = 'x86_64-pc-windows-msvc'; },
    m => { m.ci.repository = '../project'; },
    m => { m.ci.repository = 'other/project'; },
    m => { m.ci.eventName = 'push'; },
    m => { m.ci.eventName = 'workflow_dispatch'; },
    m => { m.ci.workflowRef = 'example/project/.github/workflows/ci.yml@refs/pull/7/merge'; },
    m => { m.ci = []; }, m => { m.build = null; },
  ];
  for (const change of changes) {
    const f = fixture(t);
    change(f.manifest); f.save();
    const result = f.cli();
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.equal(result.stderr, 'GPUI artifact manifest/schema failed\n');
  }
});

test('manual-dispatch branch relationship is accepted without assuming CI success', t => {
  const f = fixture(t);
  f.manifest.ci.eventName = 'workflow_dispatch';
  f.manifest.ci.workflowRef = 'example/project/.github/workflows/gpui-preview.yml@refs/heads/preview';
  f.save();
  assert.equal(inspect(f.options).manifest.ci.eventName, 'workflow_dispatch');
});

test('exact three fixed names and roles reject aliasing, traversal and untrusted file fields', t => {
  for (const change of [
    m => { m.files.pop(); }, m => { m.files.push({ ...m.files[2] }); },
    m => { m.files[2].role = 'gui'; },
    m => { [m.files[1].role, m.files[2].role] = [m.files[2].role, m.files[1].role]; },
    m => { m.files[2].file = '../tune-agent'; }, m => { m.files[2].file = 'sub/tune-agent'; },
    m => { m.files[2].file = 'tune-agent.exe'; }, m => { m.files[2].file = 'tune'; },
    m => { m.files[2].role = 'UNTRUSTED_VALUE'; }, m => { m.files[2].extra = true; },
    m => { delete m.files[2].bytes; }, m => { m.files[2].bytes = '7'; },
    m => { m.files[2].bytes = 0; }, m => { m.files[2].bytes = 268435457; },
    m => { m.files[2].bytes = 7.5; }, m => { m.files[2].sha256 = 'A'.repeat(64); },
    m => { m.files = {}; }, m => { m.files[2] = null; },
  ]) {
    const f = fixture(t); change(f.manifest); f.save();
    assert.throws(() => inspect(f.options), /manifest\/schema/);
  }
});

test('missing source and missing manifest fail without discovering or creating a source', t => {
  const f = fixture(t), missing = path.join(f.root, 'missing');
  assert.throws(() => inspect({ ...f.options, source: missing }), /source\/resolve/);
  assert.equal(fs.existsSync(missing), false);
  assert.throws(() => inspect({ ...f.options, source: f.manifestFile }), /source\/resolve/);
  fs.unlinkSync(f.manifestFile);
  assert.throws(() => inspect(f.options), /manifest\/metadata/);
});

test('every required binary must exist, be bounded and match its declared byte count and hash', t => {
  for (const role of [0, 1, 2]) for (const kind of ['missing', 'empty', 'oversized', 'size', 'hash']) {
    const f = fixture(t), entry = f.manifest.files[role], file = path.join(f.source, entry.file);
    if (kind === 'missing') fs.unlinkSync(file);
    if (kind === 'empty') fs.truncateSync(file, 0);
    if (kind === 'oversized') fs.truncateSync(file, 268435457);
    if (kind === 'size') fs.appendFileSync(file, '!');
    if (kind === 'hash') fs.writeFileSync(file, Buffer.alloc(entry.bytes, 0x78));
    assert.throws(() => inspect(f.options), new RegExp(`${entry.role}/(metadata|integrity)`), `${entry.role}: ${kind}`);
  }
});

test('manifest and binaries reject hardlinks and nonregular directories without modifying aliases', t => {
  for (const name of ['manifest.json', 'katala-tune-gpui', 'tune', 'tune-agent']) for (const kind of ['hardlink', 'directory']) {
    const f = fixture(t), file = path.join(f.source, name), alias = path.join(f.root, 'alias');
    const original = fs.readFileSync(file);
    if (kind === 'hardlink') fs.linkSync(file, alias);
    else { fs.unlinkSync(file); fs.mkdirSync(file); }
    assert.throws(() => inspect(f.options), /(?:manifest|gui|diagnostic-cli|native-sampler)\/metadata/);
    if (kind === 'hardlink') assert.deepEqual(fs.readFileSync(alias), original);
    else assert.deepEqual(fs.readdirSync(file), []);
  }
});

test('child symlinks are rejected and explicitly selected source symlink is canonicalized', t => {
  const f = fixture(t), sourceLink = path.join(f.root, 'selected-source');
  try { fs.symlinkSync(f.source, sourceLink, 'junction'); }
  catch (error) { if (error.code === 'EPERM') { t.skip('source symlink privilege unavailable'); return; } throw error; }
  assert.equal(inspect({ ...f.options, source: sourceLink }).source, fs.realpathSync(f.source));
  for (const name of ['manifest.json', 'katala-tune-gpui', 'tune', 'tune-agent']) {
    const g = fixture(t), file = path.join(g.source, name), outside = path.join(g.root, 'outside');
    fs.renameSync(file, outside);
    try { fs.symlinkSync(outside, file, 'file'); }
    catch (error) { if (error.code === 'EPERM') { t.skip('file symlink privilege unavailable'); return; } throw error; }
    const before = fs.readFileSync(outside);
    assert.throws(() => inspect(g.options), /\/metadata/);
    assert.deepEqual(fs.readFileSync(outside), before);
  }
});

test('FIFO inputs fail promptly in a bounded CLI child without opening or repairing them', t => {
  if (process.platform === 'win32') { t.skip('Unix FIFO fixture unavailable on Windows'); return; }
  for (const name of ['manifest.json', 'tune-agent']) {
    const f = fixture(t), file = path.join(f.source, name);
    fs.unlinkSync(file);
    // Fixture setup only. The inspector never launches a command.
    const made = spawnSync('mkfifo', [file], { encoding: 'utf8', timeout: 2000 });
    assert.equal(made.status, 0, made.stderr);
    assert.equal(fs.lstatSync(file).isFIFO(), true);
    const result = f.cli();
    assert.equal(result.error, undefined, 'inspection must not time out on a FIFO');
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.equal(result.stderr, `GPUI artifact ${name === 'manifest.json' ? 'manifest' : 'native-sampler'}/metadata failed\n`);
    assert.equal(fs.lstatSync(file).isFIFO(), true);
  }
  const f = fixture(t), fifo = path.join(f.root, 'pending-fifo'), saved = path.join(f.root, 'saved-sampler');
  assert.equal(spawnSync('mkfifo', [fifo], { timeout: 2000 }).status, 0);
  // Swap a FIFO into the pathname after lstat, just before the real open.
  // Run in a bounded child so losing O_NONBLOCK cannot hang the test runner.
  const result = spawnSync(process.execPath, ['-e', `
    const fs = require('node:fs');
    const p = JSON.parse(process.argv[1]);
    const { inspectArtifact } = require(p.script);
    const open = fs.openSync;
    fs.openSync = (file, ...args) => {
      if (file === p.sampler) { fs.renameSync(file, p.saved); fs.renameSync(p.fifo, file); }
      return open(file, ...args);
    };
    try { inspectArtifact(p.options); }
    catch (error) { console.error(error.message); process.exitCode = 1; }
  `, JSON.stringify({ script: CLI, sampler: f.sampler, options: f.options, fifo, saved })], { encoding: 'utf8', timeout: 5000 });
  assert.equal(result.error, undefined, 'raced FIFO must not block the open');
  assert.equal(result.status, 1);
  assert.equal(result.stderr, 'GPUI artifact native-sampler/metadata failed\n');
  assert.equal(fs.readFileSync(saved, 'utf8'), 'sampler');
});

// Failure injection stays at the owned fixture's real fs boundary, without a
// mutable inspector dependency or production test mode. All other IO stays real.
function interceptFile(file, hooks, run) {
  const original = Object.fromEntries(['openSync', 'readSync', 'closeSync', 'fstatSync', 'lstatSync', 'realpathSync'].map(key => [key, fs[key]]));
  const owned = new Set();
  fs.openSync = (name, ...args) => {
    if (name === file && hooks.open) hooks.open();
    const fd = original.openSync(name, ...args);
    if (name === file) owned.add(fd);
    return fd;
  };
  fs.readSync = (fd, ...args) => {
    const count = original.readSync(fd, ...args);
    if (owned.has(fd) && hooks.read) hooks.read(fd, count);
    return count;
  };
  fs.fstatSync = (fd, ...args) => {
    const stat = original.fstatSync(fd, ...args);
    return owned.has(fd) && hooks.fstat ? hooks.fstat(stat) : stat;
  };
  fs.lstatSync = (name, ...args) => {
    if (name === file && hooks.lstat) hooks.lstat();
    return original.lstatSync(name, ...args);
  };
  fs.realpathSync = (name, ...args) => {
    if (name === file && hooks.realpath) return hooks.realpath();
    return original.realpathSync(name, ...args);
  };
  fs.closeSync = fd => {
    const wasOwned = owned.delete(fd);
    original.closeSync(fd);
    if (wasOwned && hooks.close) hooks.close();
  };
  try { run(); }
  finally {
    Object.assign(fs, original);
    // A missing inspector close is a leak, even if test cleanup closes it.
    const leaked = owned.size;
    for (const fd of owned) original.closeSync(fd);
    assert.equal(leaked, 0, 'inspector leaked an owned descriptor');
  }
}

test('source permissions and escaped canonical child fail through the actual read boundary', t => {
  const f = fixture(t), realpath = fs.realpathSync;
  fs.realpathSync = name => { if (name === f.source) throw new Error('UNTRUSTED_VALUE'); return realpath(name); };
  try { assert.throws(() => inspect(f.options), { message: 'GPUI artifact source/resolve failed' }); }
  finally { fs.realpathSync = realpath; }
  interceptFile(f.sampler, { realpath: () => path.join(f.root, 'outside') }, () => assert.throws(() => inspect(f.options), /native-sampler\/metadata/));
});

test('open, read and close failures are sanitized, fail closed and close owned descriptors', t => {
  for (const name of ['manifest.json', 'tune-agent']) for (const stage of ['open', 'read', 'close']) {
    const f = fixture(t), file = path.join(f.source, name);
    interceptFile(file, { [stage]: () => { throw new Error('UNTRUSTED_VALUE'); } }, () => {
      assert.throws(() => inspect(f.options), { message: `GPUI artifact ${name === 'manifest.json' ? 'manifest' : 'native-sampler'}/${stage} failed` });
    });
  }
});

test('descriptor identity and post-read namespace replacement are rejected', t => {
  for (const kind of ['fd identity', 'namespace']) {
    const f = fixture(t);
    const hooks = kind === 'fd identity' ? { fstat: stat => Object.assign(stat, { ino: stat.ino + 1n }) } : { read: (_fd, count) => {
      if (!count) return;
      fs.renameSync(f.sampler, path.join(f.root, 'old'));
      fs.writeFileSync(f.sampler, 'sampler');
    } };
    interceptFile(f.sampler, hooks, () => assert.throws(() => inspect(f.options), /native-sampler\/(metadata|read)/));
  }
});

test('mid-read growth, truncation and same-size mutation fail instead of returning partial entries', t => {
  for (const kind of ['grow', 'truncate', 'same-size']) {
    const f = fixture(t);
    interceptFile(f.sampler, { read: (_fd, count) => {
      if (!count) return;
      if (kind === 'grow') fs.appendFileSync(f.sampler, '!');
      if (kind === 'truncate') fs.truncateSync(f.sampler, 0);
      if (kind === 'same-size') { fs.writeFileSync(f.sampler, 'changed'); fs.utimesSync(f.sampler, new Date(0), new Date(0)); }
    } }, () => assert.throws(() => inspect(f.options), /native-sampler\/read/));
  }
});

test('binary hashing uses bounded reads across multiple chunks', t => {
  const f = fixture(t), bytes = Buffer.alloc(3 * 1024 * 1024 + 13, 0x78);
  fs.writeFileSync(f.sampler, bytes);
  f.manifest.files[2].bytes = bytes.length; f.manifest.files[2].sha256 = sha(bytes); f.save();
  let reads = 0;
  interceptFile(f.sampler, { read: (_fd, count) => { assert.ok(count <= 1024 * 1024); if (count) reads++; } }, () => {
    assert.equal(inspect(f.options).nativeSampler.sha256, sha(bytes));
  });
  assert.ok(reads >= 4);
});

test('success and rejected inspection preserve source and separate inactive helper/config/settings sentinels', t => {
  const f = fixture(t), inactive = path.join(f.root, 'inactive');
  fs.mkdirSync(inactive);
  // Raw current-schema meta rows, exported as inert bytes rather than opening a
  // DB. Whitespace, unknown fields and old updated_at values must all survive.
  const meta = [
    ['monitor', '  {"enabled":true,"hub":"example-hub"}\n', 1000],
    ['schedule', '  {"enabled":true,"probe_minutes":15,"ai_minutes":20,"fixture_extension":"keep"}\n', 1001],
    ['hubCursor', '  123456\n', 1002],
    ['hubPull', '  {"hub":"example-hub","ok":true,"at":1234,"next":123456}\n', 1003],
    ['helpers', '  {"version":"synthetic","tools":[{"name":"tune","changed":false}]}\n', 1004],
  ];
  const sentinels = {
    'tune-agent': 'inactive sampler bytes', 'tune.exe': 'inactive action helper bytes',
    'config.json': '{"nodes":[{"id":"example-hub","action_helper":{"schema":"katala-tune/action-helper/v2","run_id":1,"attempt":1}}],"schedule":{"enabled":false,"probe_minutes":30},"unknown":{"keep":true}}\n',
    'meta-rows.json': JSON.stringify(meta) + '\n',
  };
  for (const [name, bytes] of Object.entries(sentinels)) fs.writeFileSync(path.join(inactive, name), bytes);
  const before = Object.fromEntries(fs.readdirSync(f.source).map(name => [name, fs.readFileSync(path.join(f.source, name))]));
  inspect(f.options);
  for (const [name, bytes] of Object.entries(before)) assert.deepEqual(fs.readFileSync(path.join(f.source, name)), bytes);
  f.manifest.files[2].sha256 = '0'.repeat(64); f.save();
  const rejected = fs.readFileSync(f.manifestFile);
  const result = f.cli();
  assert.equal(result.status, 1); assert.equal(result.stdout, '');
  assert.match(result.stderr, /native-sampler\/integrity/);
  assert.deepEqual(fs.readFileSync(f.manifestFile), rejected);
  for (const [name, bytes] of Object.entries(before)) if (name !== 'manifest.json') assert.deepEqual(fs.readFileSync(path.join(f.source, name)), bytes);
  for (const [name, bytes] of Object.entries(sentinels)) assert.equal(fs.readFileSync(path.join(inactive, name), 'utf8'), bytes);
});
