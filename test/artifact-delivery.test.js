'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const { sourceProof, validateProof, inspectPE, inspectPayload, inventory, safePath, checkArchive, packageDelivery, writeNew } = require('../scripts/artifact-delivery');
const hash = value => crypto.createHash('sha256').update(value).digest('hex');
const clone = value => JSON.parse(JSON.stringify(value));

function temporary(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'artifact-test-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true })); return root;
}
function fixture(t) {
  const root = temporary(t);
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  git('init'); git('config', 'user.name', 'Fixture'); git('config', 'user.email', 'fixture@example.com');
  git('config', 'core.autocrlf', 'false'); git('config', 'commit.gpgsign', 'false');
  fs.writeFileSync(path.join(root, 'Cargo.lock'), '# fixture\nversion = 4\n');
  fs.writeFileSync(path.join(root, '.gitignore'), 'target/\nsrc-tauri/binaries/\nsidecars.json\n');
  fs.mkdirSync(path.join(root, 'renderer')); fs.writeFileSync(path.join(root, 'renderer/index.html'), '<p>fixture</p>\n');
  fs.mkdirSync(path.join(root, 'src-tauri')); fs.writeFileSync(path.join(root, 'src-tauri/tauri.conf.json'), '{"version":"0.6.4"}\n');
  git('add', '.'); git('commit', '-m', 'fixture');
  const sha = git('rev-parse', 'HEAD'); git('update-ref', 'refs/remotes/origin/main', sha);
  fs.mkdirSync(path.join(root, 'src-tauri/binaries'), { recursive: true });
  fs.mkdirSync(path.join(root, 'target/x86_64-pc-windows-msvc/release'), { recursive: true });
  for (const bin of ['tune', 'tune-agent']) {
    const bytes = pe(false);
    fs.writeFileSync(path.join(root, `target/x86_64-pc-windows-msvc/release/${bin}.exe`), bytes);
    fs.writeFileSync(path.join(root, `src-tauri/binaries/${bin}-x86_64-pc-windows-msvc.exe`), bytes);
  }
  fs.writeFileSync(path.join(root, 'sidecars.json'), JSON.stringify({ bundle: { externalBin: ['binaries/tune', 'binaries/tune-agent'] } }));
  const env = { SOURCE_SHA: sha, GITHUB_SHA: sha, GITHUB_WORKFLOW_SHA: sha, GITHUB_EVENT_NAME: 'workflow_dispatch',
    GITHUB_REF: 'refs/heads/main', GITHUB_REPOSITORY: 'katalalab/katala-tune',
    GITHUB_WORKFLOW_REF: 'katalalab/katala-tune/.github/workflows/artifact-delivery.yml@refs/heads/main',
    GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '2', RUNNER_OS: 'Windows', RUNNER_ARCH: 'X64', ImageOS: 'win25', ImageVersion: '20261020.1' };
  const version = (cmd, args) => cmd === 'rustc' ? 'rustc 1.99.0 (fixture)\nhost: x86_64-pc-windows-msvc'
    : args[0] === 'tauri' ? 'tauri-cli 2.12.1' : 'cargo 1.99.0 (fixture)';
  const proof = () => sourceProof(root, 'windows-x64', env, version);
  return { root, git, env, proof, version };
}

// A real PE resource tree (RT_MANIFEST/name/language), not a string-search mock.
const GUI_MANIFEST = '<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0"><dependency><dependentAssembly><assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/></dependentAssembly></dependency></assembly>';
function pe(gui, manifest = GUI_MANIFEST) {
  const b = Buffer.alloc(2048); b.write('MZ'); b.writeUInt32LE(128, 60);
  b.write('PE\0\0', 128); b.writeUInt16LE(0x8664, 132); b.writeUInt16LE(1, 134);
  b.writeUInt16LE(240, 148); b.writeUInt16LE(2, 150);
  const opt = 152; b.writeUInt16LE(0x20b, opt); b.writeUInt16LE(gui ? 2 : 3, opt + 68);
  b.writeUInt32LE(16, opt + 108); b.writeUInt32LE(4096, opt + 128); b.writeUInt32LE(1536, opt + 132);
  const section = opt + 240; b.write('.rsrc', section); b.writeUInt32LE(4096, section + 12);
  b.writeUInt32LE(1536, section + 16); b.writeUInt32LE(512, section + 20);
  for (const [offset, id, next] of [[0, 24, 24], [24, 1, 48], [48, 1033, 72]]) {
    b.writeUInt16LE(1, 512 + offset + 14); b.writeUInt32LE(id, 512 + offset + 16);
    b.writeUInt32LE(offset === 48 ? next : (next + 0x80000000), 512 + offset + 20);
  }
  const xml = Buffer.isBuffer(manifest) ? manifest : Buffer.from(manifest);
  b.writeUInt32LE(4096 + 88, 512 + 72); b.writeUInt32LE(xml.length, 512 + 76); xml.copy(b, 512 + 88);
  return b;
}

test('source binds requested SHA, actual Git tree, workflow SHA and run attempt separately', t => {
  const f = fixture(t), p = f.proof(); validateProof(p, 'windows-x64');
  assert.equal(p.source.checkoutSha, f.env.SOURCE_SHA);
  assert.equal(p.source.tree, f.git('rev-parse', 'HEAD^{tree}'));
  assert.equal(p.ci.runAttempt, '2'); assert.equal(p.ci.runId, '123');
  assert.equal(p.ci.workflowSha, f.env.SOURCE_SHA);
  assert.equal(p.source.cargoLock.canonicalSha256, hash(fs.readFileSync(path.join(f.root, 'Cargo.lock'))));
  assert.equal(p.source.files.find(v => v.path === 'renderer/index.html').sha256, hash('<p>fixture</p>\n'));
});

test('CRLF working lock bytes are distinct from canonical Git lock bytes', t => {
  const f = fixture(t); f.git('config', 'core.autocrlf', 'true');
  fs.writeFileSync(path.join(f.root, 'Cargo.lock'), '# fixture\r\nversion = 4\r\n');
  const p = f.proof();
  assert.notEqual(p.source.cargoLock.workingSha256, p.source.cargoLock.canonicalSha256);
  assert.equal(p.source.cargoLock.canonicalSha256, hash('# fixture\nversion = 4\n'));
});

test('reject malformed/mismatched SHA, non-main dispatch, missing CI identity and wrong tools', t => {
  const f = fixture(t);
  for (const patch of [
    { SOURCE_SHA: '../main' }, { SOURCE_SHA: 'a'.repeat(40) }, { GITHUB_SHA: 'a'.repeat(40) },
    { GITHUB_WORKFLOW_SHA: 'a'.repeat(40) }, { GITHUB_EVENT_NAME: 'pull_request' },
    { GITHUB_REF: 'refs/heads/other' }, { GITHUB_REPOSITORY: 'other/repo' }, { GITHUB_WORKFLOW_REF: 'other.yml' },
    { GITHUB_RUN_ATTEMPT: '0' }, { GITHUB_RUN_ID: '' }, { ImageVersion: '' }, { RUNNER_ARCH: 'ARM64' },
    { RUSTFLAGS: '-C target-cpu=native' },
  ]) assert.throws(() => sourceProof(f.root, 'windows-x64', { ...f.env, ...patch }, f.version));
  assert.throws(() => sourceProof(f.root, 'windows-x64', f.env, () => 'wrong'), /version mismatch/);
  assert.throws(() => sourceProof(f.root, '__proto__', f.env, f.version), /unknown platform/);
});

test('reject source outside main ancestry, modified source and substituted helper bytes', t => {
  const f = fixture(t);
  f.git('update-ref', '-d', 'refs/remotes/origin/main'); assert.throws(f.proof);
  f.git('update-ref', 'refs/remotes/origin/main', f.env.SOURCE_SHA);
  fs.writeFileSync(path.join(f.root, 'renderer/index.html'), 'changed'); assert.throws(f.proof, /source changed/);
  fs.writeFileSync(path.join(f.root, 'renderer/index.html'), '<p>fixture</p>\n');
  fs.writeFileSync(path.join(f.root, 'src-tauri/binaries/tune-x86_64-pc-windows-msvc.exe'), 'substituted');
  assert.throws(f.proof, /sidecar input mismatch/);
});

test('unknown schemas/fields and fabricated build recipes fail closed', t => {
  const f = fixture(t), valid = f.proof();
  for (const mutate of [
    p => { p.schema = 'katala-artifact-source/v2'; }, p => { p.extra = true; },
    p => { p.source.extra = true; }, p => { p.source.files[0].path = '../outside'; },
    p => { p.build.extra = true; }, p => { p.build.commands[1].push('--debug'); },
    p => { p.build.runner.extra = true; }, p => { p.build.sidecars[0].extra = true; },
    p => { p.source.cargoLock.extra = true; }, p => { p.ci.extra = true; },
  ]) { const p = clone(valid); mutate(p); assert.throws(() => validateProof(p, 'windows-x64')); }
});

test('PE parser requires executable x64 PE32+, correct subsystem and embedded GUI manifest', () => {
  assert.deepEqual(inspectPE(pe(false), false), { machine: 'x86_64', subsystem: 'console' });
  assert.equal(inspectPE(pe(true), true).embeddedManifestSha256.length, 64);
  for (const mutate of [
    b => b.writeUInt16LE(0x14c, 132), b => b.writeUInt16LE(0x10b, 152), b => b.writeUInt16LE(0x2002, 150),
    b => b.writeUInt16LE(3, 220), b => b.writeUInt32LE(0, 280), b => b.writeUInt32LE(0xffffffff, 584),
    b => b.write('Common-Wrong-Controls', b.indexOf('Common-Controls')),
  ]) { const b = pe(true); mutate(b); assert.throws(() => inspectPE(b, true)); }
  for (const bytes of [0, 63, 140, 400, 550, 620]) assert.throws(() => inspectPE(pe(true).subarray(0, bytes), true));
  assert.throws(() => inspectPE(Buffer.from('MZ fake text Microsoft.Windows.Common-Controls version="6.0.0.0"'), true));
});

test('GUI manifest must use active executable resource ID 1', () => {
  const otherId = pe(true); otherId.writeUInt32LE(2, 512 + 24 + 16);
  assert.throws(() => inspectPE(otherId, true), /manifest/i);
  const namedPartition = pe(true); namedPartition.writeUInt16LE(1, 512 + 24 + 12); namedPartition.writeUInt16LE(0, 512 + 24 + 14);
  assert.throws(() => inspectPE(namedPartition, true), /manifest resource name partition/);
});

test('GUI manifest cannot satisfy its dependency with a commented identity', () => {
  const identity = GUI_MANIFEST.match(/<assemblyIdentity[^>]*\/>/)[0];
  const commented = GUI_MANIFEST.replace(identity, `<!--${identity}-->`);
  assert.throws(() => inspectPE(pe(true, commented), true), /manifest/i);
});

test('GUI manifest reader rejects inactive text, unsupported XML, namespace tricks and malformed structure', () => {
  const identity = GUI_MANIFEST.match(/<assemblyIdentity[^>]*\/>/)[0];
  const invalid = [
    GUI_MANIFEST.replace('<dependency>', '<!--<dependency>').replace('</dependency>', '</dependency>-->'),
    GUI_MANIFEST.replace(identity, `<![CDATA[${identity}]]>`),
    GUI_MANIFEST.replace(identity, identity.replaceAll('<', '&lt;').replaceAll('>', '&gt;')),
    GUI_MANIFEST.replace(identity, `<?fake ${identity}?>`),
    '<!DOCTYPE assembly [<!ENTITY controls SYSTEM "https://example.com/never-fetched">]>' + GUI_MANIFEST,
    GUI_MANIFEST.replace('Microsoft.Windows.Common-Controls', '&controls;'),
    GUI_MANIFEST.replace('Microsoft.Windows.Common-Controls', '&#0;'),
    GUI_MANIFEST.replace('urn:schemas-microsoft-com:asm.v1', 'urn:wrong'),
    GUI_MANIFEST.replace('<assemblyIdentity ', '<assemblyIdentity xmlns="urn:wrong" '),
    GUI_MANIFEST.replace('<assemblyIdentity ', '<unbound:assemblyIdentity '),
    GUI_MANIFEST.replace('<dependentAssembly>', '').replace('</dependentAssembly>', ''),
    GUI_MANIFEST.replace('</dependency>', '</different>'),
    GUI_MANIFEST.replace('<dependency>', '<dependency><!-- invalid--comment -->'),
    GUI_MANIFEST.replace('type="win32"', 'type="win32" type="win32"'),
    GUI_MANIFEST.replace('<assemblyIdentity ', '<assemblyIdentity xmlns:p="urn:same" xmlns:q="urn:same" p:a="1" q:a="2" '),
    GUI_MANIFEST.replace('<dependency>', '<dependency>unexpected text'),
    GUI_MANIFEST + GUI_MANIFEST,
    GUI_MANIFEST.replace('</assembly>', ''),
    '<?xml version="1.0" standalone="yes" encoding="UTF-8"?>' + GUI_MANIFEST,
    '<?xml version="1.0" encoding=""?>' + GUI_MANIFEST,
    '<?xml version="&#49;.0"?>' + GUI_MANIFEST,
    '&#32;' + GUI_MANIFEST,
    GUI_MANIFEST.replace('</dependentAssembly>', '<invalid/></dependentAssembly>'),
  ];
  for (const xml of invalid) assert.throws(() => inspectPE(pe(true, xml), true), undefined, xml);
});

test('Common Controls binding checks all required identity values and rejects duplicate active resources', () => {
  for (const attribute of ['type', 'name', 'version', 'processorArchitecture', 'publicKeyToken']) {
    const xml = GUI_MANIFEST.replace(new RegExp(` ${attribute}="[^"]*"`), '');
    assert.throws(() => inspectPE(pe(true, xml), true), /manifest/i, attribute);
  }
  for (const [before, after] of [['win32', 'win64'], ['6.0.0.0', '6.1.0.0'], ['6595b64144ccf1df', '0000000000000000'], ['processorArchitecture="*"', 'processorArchitecture="x86"'], ['<dependency>', '<dependency optional="yes">']]) {
    assert.throws(() => inspectPE(pe(true, GUI_MANIFEST.replace(before, after)), true), /manifest/i, after);
  }
  const duplicate = pe(true); duplicate.fill(0, 512);
  for (const [offset, id, next] of [[0, 24, 64], [64, 1, 88], [88, 1033, 112]]) {
    duplicate.writeUInt16LE(1, 512 + offset + 14); duplicate.writeUInt32LE(id, 512 + offset + 16);
    duplicate.writeUInt32LE(offset === 88 ? next : next + 0x80000000, 512 + offset + 20);
  }
  duplicate.writeUInt16LE(2, 512 + 14); duplicate.writeUInt32LE(24, 512 + 24); duplicate.writeUInt32LE(64 + 0x80000000, 512 + 28);
  duplicate.writeUInt32LE(4096 + 128, 512 + 112); duplicate.writeUInt32LE(Buffer.byteLength(GUI_MANIFEST), 512 + 116);
  duplicate.write(GUI_MANIFEST, 512 + 128);
  assert.throws(() => inspectPE(duplicate, true), /duplicate manifest resource ID|ambiguous embedded manifest/);
});

test('valid Tauri-style manifests retain comments, namespace binding, declarations and UTF encodings', () => {
  for (const xml of [
    GUI_MANIFEST,
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\r\n<!-- harmless -->' + GUI_MANIFEST,
    GUI_MANIFEST.replace('processorArchitecture="*"', 'processorArchitecture="amd64"'),
    GUI_MANIFEST.replace('processorArchitecture="*"', 'processorArchitecture="&#x2a;"'),
    GUI_MANIFEST.replace('xmlns=', 'xmlns:a=').replace(/<(\/?)(assembly|dependency|dependentAssembly|assemblyIdentity)(?=[ >])/g, '<$1a:$2'),
  ]) assert.equal(inspectPE(pe(true, xml), true).subsystem, 'windows');
  const utf16 = Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from('<?xml version="1.0" encoding="UTF-16"?>' + GUI_MANIFEST, 'utf16le')]);
  assert.equal(inspectPE(pe(true, utf16), true).subsystem, 'windows');
  assert.equal(inspectPE(pe(true, Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from(GUI_MANIFEST)])), true).subsystem, 'windows');
});

test('Windows payload is exactly three release executables bound to helper input hashes', t => {
  const f = fixture(t), p = f.proof(), payload = path.join(f.root, 'payload'); fs.mkdirSync(payload);
  for (const bin of ['katala-tune', 'tune', 'tune-agent']) fs.writeFileSync(path.join(payload, `${bin}.exe`), pe(bin === 'katala-tune'));
  const result = inspectPayload(payload, 'windows-x64', p);
  assert.equal(result.files.length, 3); assert.equal(result.binaries[0].subsystem, 'windows');
  fs.writeFileSync(path.join(payload, 'extra.txt'), 'extra'); assert.throws(() => inspectPayload(payload, 'windows-x64', p), /unexpected Windows/);
  fs.unlinkSync(path.join(payload, 'extra.txt')); fs.writeFileSync(path.join(payload, 'tune.exe'), Buffer.concat([pe(false), Buffer.from('changed')]));
  assert.throws(() => inspectPayload(payload, 'windows-x64', p), /sidecar mismatch/);
});

test('packaging rechecks source, writes exact file hashes and refuses overwrite', t => {
  const f = fixture(t), p = f.proof(), payload = path.join(f.root, 'payload'), output = path.join(f.root, 'delivery');
  fs.mkdirSync(payload);
  for (const bin of ['katala-tune', 'tune', 'tune-agent']) fs.writeFileSync(path.join(payload, `${bin}.exe`), pe(bin === 'katala-tune'));
  const proofFile = path.join(f.root, 'proof.json'); fs.writeFileSync(proofFile, JSON.stringify(p));
  const result = packageDelivery(f.root, 'windows-x64', proofFile, payload, output, f.env, f.version);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(output, 'manifest.json'))), result);
  assert.deepEqual(fs.readdirSync(output).sort(), ['katala-tune.exe', 'manifest.json', 'tune-agent.exe', 'tune.exe']);
  assert.equal(result.files[0].sha256, hash(fs.readFileSync(path.join(output, 'katala-tune.exe'))));
  assert.throws(() => packageDelivery(f.root, 'windows-x64', proofFile, payload, output, f.env, f.version), /EEXIST/);
  const altered = clone(p); altered.schema = 'unknown'; fs.writeFileSync(proofFile, JSON.stringify(altered));
  assert.throws(() => packageDelivery(f.root, 'windows-x64', proofFile, payload, path.join(f.root, 'other'), f.env, f.version), /changed since proof/);
  assert.equal(fs.existsSync(path.join(f.root, 'other')), false);
});

test('path traversal, links, special entries and overwriting existing proof are refused', t => {
  for (const value of ['../file', '/root', 'C:/file', 'a\\b', 'a/./b', 'a//b', 'a\nb']) assert.throws(() => safePath(value));
  const root = temporary(t), file = path.join(root, 'proof.json');
  writeNew(file, 'first'); assert.throws(() => writeNew(file, 'second'), /EEXIST/); assert.equal(fs.readFileSync(file, 'utf8'), 'first');
  if (process.platform !== 'win32') {
    fs.symlinkSync(file, path.join(root, 'link')); assert.throws(() => inventory(root), /symlink/);
    fs.unlinkSync(path.join(root, 'link'));
  }
  fs.linkSync(file, path.join(root, 'hardlink')); assert.throws(() => inventory(root), /hardlinked/);
});

test('archive path guard rejects traversal/root escape and links before extraction', t => {
  if (process.platform === 'win32') return; // macOS archive tool runs only in the macOS job.
  const root = temporary(t); fs.mkdirSync(path.join(root, 'outside')); fs.writeFileSync(path.join(root, 'outside/file'), 'x');
  const archive = path.join(root, 'bad.tar.gz');
  execFileSync('tar', ['-czf', archive, '-C', root, 'outside']); assert.throws(() => checkArchive(archive), /outside bundle/);
  fs.mkdirSync(path.join(root, 'Katala Tune.app')); fs.symlinkSync('../outside/file', path.join(root, 'Katala Tune.app/link'));
  execFileSync('tar', ['-czf', archive, '-C', root, 'Katala Tune.app']); assert.throws(() => checkArchive(archive), /link\/special/);
});

test('macOS real arm64 signed bundle survives archive/extraction and rejects changed provenance', { skip: process.platform !== 'darwin' || process.arch !== 'arm64' }, t => {
  const f = fixture(t), target = 'aarch64-apple-darwin';
  const release = path.join(f.root, 'target', target, 'release'); fs.mkdirSync(release, { recursive: true });
  const tune = path.join(release, 'tune');
  execFileSync('cc', ['-arch', 'arm64', '-x', 'c', '-', '-o', tune], { input: 'int main(void) { return 0; }\n' });
  fs.copyFileSync(tune, path.join(f.root, `src-tauri/binaries/tune-${target}`));
  fs.writeFileSync(path.join(f.root, 'sidecars.json'), JSON.stringify({ bundle: { externalBin: ['binaries/tune'] } }));
  const env = { ...f.env, RUNNER_OS: 'macOS', RUNNER_ARCH: 'ARM64', ImageOS: 'macos15' };
  const proof = sourceProof(f.root, 'macos-arm64', env, f.version);
  const payload = path.join(f.root, 'payload'), app = path.join(payload, 'Katala Tune.app');
  fs.mkdirSync(path.join(app, 'Contents/MacOS'), { recursive: true }); fs.mkdirSync(path.join(app, 'Contents/Resources'));
  for (const bin of ['tune', 'katala-tune']) { fs.copyFileSync(tune, path.join(app, 'Contents/MacOS', bin)); fs.chmodSync(path.join(app, 'Contents/MacOS', bin), 0o755); }
  fs.writeFileSync(path.join(app, 'Contents/Info.plist'), '<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>katala-tune</string><key>CFBundleIdentifier</key><string>org.katala.tune.tauri</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>0.6.4</string></dict></plist>');
  const proofFile = path.join(f.root, 'proof.json'), bytes = JSON.stringify(proof, null, 2) + '\n';
  fs.writeFileSync(proofFile, bytes); fs.writeFileSync(path.join(app, 'Contents/Resources/katala-source.json'), bytes);
  execFileSync('codesign', ['--force', '--deep', '--sign', '-', app], { stdio: 'pipe' });
  const manifest = packageDelivery(f.root, 'macos-arm64', proofFile, payload, path.join(f.root, 'delivery'), env, f.version);
  assert.equal(manifest.archive.sha256.length, 64); assert.equal(manifest.binaries.length, 2);
  assert.equal(manifest.bundle.signature, 'ad-hoc; codesign --verify --deep --strict');
  fs.writeFileSync(path.join(app, 'Contents/Resources/katala-source.json'), '{}');
  assert.throws(() => inspectPayload(payload, 'macos-arm64', proof), /proof mismatch/);
});

test('workflow is dispatch-only, permission-limited, SHA-pinned and uses real locked Tauri builds', () => {
  const source = fs.readFileSync(path.join(__dirname, '../.github/workflows/artifact-delivery.yml'), 'utf8');
  for (const eol of ['\n', '\r\n']) {
    const raw = source.replace(/\r?\n/g, eol);
    const workflow = raw.replace(/\r\n/g, '\n');
    assert.match(workflow, /workflow_dispatch:/); assert.doesNotMatch(workflow, /^  (push|pull_request|schedule|workflow_run):/m);
    assert.doesNotMatch(workflow, /\$\{\{\s*secrets\.|^\s+environment:|contents: write|gh release|tauri\.release\.conf/m);
    const uses = [...workflow.matchAll(/uses: ([^\s]+)/g)].map(m => m[1]);
    assert.equal(uses.length, 3); for (const use of uses) assert.match(use, /^actions\/[a-z-]+@[a-f0-9]{40}$/);
    assert.match(workflow, /os: macos-15\n/); assert.match(workflow, /os: windows-2025\n/); assert.doesNotMatch(workflow, /-latest/);
    assert.match(workflow, /--bundles app --config sidecars\.json -- --locked/);
    assert.match(workflow, /--no-bundle --config sidecars\.json -- --locked/);
    assert.match(workflow, /cmp .*tune/); assert.match(workflow, /codesign --verify --deep --strict/);
  }
});
