#!/usr/bin/env node
'use strict';
// Private/manual delivery only. No installation, application launch, or release API.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync, spawnSync } = require('node:child_process');
const { isDeepStrictEqual } = require('node:util');

const PLATFORMS = {
  'macos-arm64': { target: 'aarch64-apple-darwin', os: 'macOS', arch: 'ARM64', runner: 'macos-15', bins: ['tune'] },
  'windows-x64': { target: 'x86_64-pc-windows-msvc', os: 'Windows', arch: 'X64', runner: 'windows-2025', bins: ['tune', 'tune-agent'] },
};
const APP = 'Katala Tune.app';
const PROOF = 'katala-source.json';
const WORKFLOW = '.github/workflows/artifact-delivery.yml';
const sha256 = b => crypto.createHash('sha256').update(b).digest('hex');
function need(ok, reason) { if (!ok) throw new Error(reason); }
function keys(value, expected) {
  need(value && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).sort().join() === [...expected].sort().join(), 'unknown schema/fields');
}
function platform(name) { need(Object.hasOwn(PLATFORMS, name), 'unknown platform'); return PLATFORMS[name]; }
function run(command, args, cwd) { return execFileSync(command, args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim(); }
function git(root, ...args) { return run('git', args, root); }
function writeNew(file, data) { fs.writeFileSync(file, data, { flag: 'wx' }); }
function json(value) { return JSON.stringify(value, null, 2) + '\n'; }
function safePath(value) {
  need(typeof value === 'string' && value.length > 0 && !/[\\\x00-\x1f:]/.test(value)
    && !value.startsWith('/') && value.split('/').every(p => p && p !== '.' && p !== '..'), 'unsafe path');
  return value;
}

function sourceProof(root, name, env = process.env, command = run) {
  const p = platform(name);
  const expected = env.SOURCE_SHA;
  need(/^[0-9a-f]{40}$/.test(expected || ''), 'source_sha must be 40 lowercase hex');
  need(env.GITHUB_EVENT_NAME === 'workflow_dispatch' && env.GITHUB_REF === 'refs/heads/main', 'dispatch main only');
  need(env.GITHUB_REPOSITORY === 'katalalab/katala-tune', 'unexpected repository');
  need(env.GITHUB_SHA === expected && env.GITHUB_WORKFLOW_SHA === expected, 'dispatch/workflow SHA mismatch');
  need(env.GITHUB_WORKFLOW_REF === `${env.GITHUB_REPOSITORY}/${WORKFLOW}@refs/heads/main`, 'workflow ref mismatch');
  need(env.RUNNER_OS === p.os && env.RUNNER_ARCH === p.arch, 'runner architecture mismatch');
  for (const key of ['GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT']) need(/^[1-9][0-9]*$/.test(env[key] || ''), `invalid ${key}`);
  for (const key of ['ImageOS', 'ImageVersion']) need(/^[a-zA-Z0-9._-]+$/.test(env[key] || ''), `missing/invalid ${key}`);
  for (const key of ['RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS']) need(!env[key], `unexpected ${key}`);
  const head = git(root, 'rev-parse', 'HEAD');
  need(head === expected, 'checkout HEAD mismatch');
  git(root, 'merge-base', '--is-ancestor', head, 'refs/remotes/origin/main');
  // diff compares normalized contents; a first status can report a stat-only CRLF change.
  try { git(root, 'diff', '--exit-code', 'HEAD', '--'); } catch { throw new Error('tracked source changed'); }
  const canonical = execFileSync('git', ['show', `${head}:Cargo.lock`], { cwd: root });
  const working = fs.readFileSync(path.join(root, 'Cargo.lock'));
  need(working.toString().replace(/\r\n/g, '\n') === canonical.toString(), 'Cargo.lock content mismatch');
  const overlay = { bundle: { externalBin: p.bins.map(b => `binaries/${b}`) } };
  need(isDeepStrictEqual(JSON.parse(fs.readFileSync(path.join(root, 'sidecars.json'))), overlay), 'sidecars overlay mismatch');
  const rustc = command('rustc', ['--version', '--verbose'], root);
  const cargo = command('cargo', ['--version'], root);
  const tauri = command('cargo', ['tauri', '--version'], root);
  need(rustc.startsWith('rustc 1.99.0 ') && cargo.startsWith('cargo 1.99.0 ') && tauri === 'tauri-cli 2.12.1', 'tool version mismatch');
  const paths = git(root, 'ls-files', '-z', '--', 'renderer', 'probes', 'config/nodes.example.json', 'data', 'src-tauri', 'crates', 'Cargo.toml', 'rust-toolchain.toml', 'scripts/artifact-delivery.js', WORKFLOW).split('\0').filter(Boolean);
  const sources = paths.map(file => ({ path: safePath(file), sha256: sha256(fs.readFileSync(path.join(root, file))) }));
  return {
    schema: 'katala-artifact-source/v1',
    source: { repository: env.GITHUB_REPOSITORY, requestedSha: expected, checkoutSha: head, tree: git(root, 'rev-parse', 'HEAD^{tree}'),
      cargoLock: { gitBlob: git(root, 'rev-parse', 'HEAD:Cargo.lock'), canonicalSha256: sha256(canonical), workingSha256: sha256(working) }, files: sources },
    ci: { event: env.GITHUB_EVENT_NAME, ref: env.GITHUB_REF, workflowRef: env.GITHUB_WORKFLOW_REF, workflowSha: env.GITHUB_WORKFLOW_SHA,
      runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT },
    build: { platform: name, target: p.target, profile: 'release', appVersion: JSON.parse(fs.readFileSync(path.join(root, 'src-tauri/tauri.conf.json'))).version,
      features: ['tauri/custom-protocol'], rustc, cargo, tauri, node: process.version,
      runner: { label: p.runner, os: env.RUNNER_OS, arch: env.RUNNER_ARCH, imageOS: env.ImageOS, imageVersion: env.ImageVersion },
      overlaySha256: sha256(fs.readFileSync(path.join(root, 'sidecars.json'))),
      sidecars: p.bins.map(bin => {
        const file = bin + (p.os === 'Windows' ? '.exe' : '');
        const bytes = fs.readFileSync(path.join(root, 'target', p.target, 'release', file));
        need(bytes.equals(fs.readFileSync(path.join(root, 'src-tauri/binaries', `${bin}-${p.target}${p.os === 'Windows' ? '.exe' : ''}`))), 'sidecar input mismatch');
        return { path: file, bytes: bytes.length, sha256: sha256(bytes) };
      }),
      commands: [
        ['cargo', 'build', '--release', '--locked', ...p.bins.flatMap(b => ['-p', b === 'tune' ? 'tune-cli' : b]), '--target', p.target],
        ['cargo', 'tauri', 'build', '--ci', '--target', p.target, ...(p.os === 'macOS' ? ['--bundles', 'app'] : ['--no-bundle']), '--config', 'sidecars.json', '--', '--locked'],
      ] },
  };
}

function validateProof(proof, name) {
  const p = platform(name);
  keys(proof, ['schema', 'source', 'ci', 'build']);
  need(proof.schema === 'katala-artifact-source/v1', 'unknown source schema');
  keys(proof.source, ['repository', 'requestedSha', 'checkoutSha', 'tree', 'cargoLock', 'files']);
  need(proof.source.repository === 'katalalab/katala-tune' && /^[a-f0-9]{40}$/.test(proof.source.checkoutSha)
    && proof.source.requestedSha === proof.source.checkoutSha && /^[a-f0-9]{40}$/.test(proof.source.tree), 'invalid source identity');
  keys(proof.source.cargoLock, ['gitBlob', 'canonicalSha256', 'workingSha256']);
  need(/^[a-f0-9]{40}$/.test(proof.source.cargoLock.gitBlob), 'invalid lock blob');
  for (const field of ['canonicalSha256', 'workingSha256']) need(/^[a-f0-9]{64}$/.test(proof.source.cargoLock[field]), 'invalid lock hash');
  need(Array.isArray(proof.source.files) && proof.source.files.length > 0, 'missing source files');
  const seen = new Set();
  for (const file of proof.source.files) {
    keys(file, ['path', 'sha256']); safePath(file.path);
    need(!seen.has(file.path) && /^[a-f0-9]{64}$/.test(file.sha256), 'invalid source file'); seen.add(file.path);
  }
  keys(proof.ci, ['event', 'ref', 'workflowRef', 'workflowSha', 'runId', 'runAttempt']);
  need(proof.ci.event === 'workflow_dispatch' && proof.ci.ref === 'refs/heads/main'
    && proof.ci.workflowRef === `katalalab/katala-tune/${WORKFLOW}@refs/heads/main`
    && proof.ci.workflowSha === proof.source.checkoutSha, 'invalid workflow identity');
  for (const f of ['runId', 'runAttempt']) need(/^[1-9][0-9]*$/.test(proof.ci[f]), 'invalid run identity');
  keys(proof.build, ['platform', 'target', 'profile', 'appVersion', 'features', 'rustc', 'cargo', 'tauri', 'node', 'runner', 'overlaySha256', 'sidecars', 'commands']);
  need(proof.build.platform === name && proof.build.target === p.target && proof.build.profile === 'release'
    && isDeepStrictEqual(proof.build.features, ['tauri/custom-protocol']) && proof.build.tauri === 'tauri-cli 2.12.1', 'invalid build identity');
  keys(proof.build.runner, ['label', 'os', 'arch', 'imageOS', 'imageVersion']);
  need(proof.build.runner.label === p.runner && proof.build.runner.os === p.os && proof.build.runner.arch === p.arch, 'invalid runner');
  need(typeof proof.build.rustc === 'string' && proof.build.rustc.startsWith('rustc 1.99.0 ')
    && typeof proof.build.cargo === 'string' && proof.build.cargo.startsWith('cargo 1.99.0 ')
    && /^v24\./.test(proof.build.node) && /^[a-f0-9]{64}$/.test(proof.build.overlaySha256)
    && /^\d+\.\d+\.\d+(?:[-+][a-zA-Z0-9.+-]+)?$/.test(proof.build.appVersion), 'invalid tools/version');
  for (const f of ['imageOS', 'imageVersion']) need(/^[a-zA-Z0-9._-]+$/.test(proof.build.runner[f]), 'invalid runner image');
  need(Array.isArray(proof.build.sidecars) && proof.build.sidecars.length === p.bins.length, 'invalid sidecars');
  for (const [i, file] of proof.build.sidecars.entries()) {
    keys(file, ['path', 'bytes', 'sha256']);
    need(file.path === p.bins[i] + (p.os === 'Windows' ? '.exe' : '') && Number.isSafeInteger(file.bytes) && file.bytes > 0
      && /^[a-f0-9]{64}$/.test(file.sha256), 'invalid sidecar proof');
  }
  const commands = [
    ['cargo', 'build', '--release', '--locked', ...p.bins.flatMap(b => ['-p', b === 'tune' ? 'tune-cli' : b]), '--target', p.target],
    ['cargo', 'tauri', 'build', '--ci', '--target', p.target, ...(p.os === 'macOS' ? ['--bundles', 'app'] : ['--no-bundle']), '--config', 'sidecars.json', '--', '--locked'],
  ];
  need(isDeepStrictEqual(proof.build.commands, commands), 'unexpected build commands');
}

// Walk without following links. This fixed bundle has no frameworks/symlinks.
function inventory(root) {
  need(fs.lstatSync(root).isDirectory() && !fs.lstatSync(root).isSymbolicLink(), 'invalid payload root');
  const files = [];
  function walk(relative) {
    const entries = fs.readdirSync(path.join(root, relative)).sort();
    need(entries.length > 0, 'empty payload directory');
    for (const entry of entries) {
      const file = relative ? `${relative}/${entry}` : entry; safePath(file);
      const stat = fs.lstatSync(path.join(root, file));
      need(!stat.isSymbolicLink(), 'symlink in payload');
      if (stat.isDirectory()) walk(file);
      else { need(stat.isFile() && stat.nlink === 1, 'non-regular/hardlinked payload'); files.push({ path: file, bytes: stat.size, sha256: sha256(fs.readFileSync(path.join(root, file))), executable: Boolean(stat.mode & 0o111) }); }
    }
  }
  walk(''); return files;
}

function inspectPE(buffer, gui) {
  function range(offset, bytes) { need(Number.isSafeInteger(offset) && offset >= 0 && offset + bytes <= buffer.length, 'truncated PE'); return offset; }
  need(buffer.length >= 64 && buffer.toString('ascii', 0, 2) === 'MZ', 'invalid DOS header');
  const pe = buffer.readUInt32LE(60); range(pe, 24);
  need(buffer.readUInt32LE(pe) === 0x4550 && buffer.readUInt16LE(pe + 4) === 0x8664, 'PE must be x64');
  const count = buffer.readUInt16LE(pe + 6), size = buffer.readUInt16LE(pe + 20), flags = buffer.readUInt16LE(pe + 22), opt = pe + 24;
  range(opt, size); need(size >= 136 && buffer.readUInt16LE(opt) === 0x20b && (flags & 2) && !(flags & 0x2000), 'not PE32+ executable');
  need(buffer.readUInt16LE(opt + 68) === (gui ? 2 : 3), 'PE subsystem mismatch');
  const sections = opt + size; range(sections, count * 40);
  function rva(address, length) {
    for (let i = 0; i < count; i++) {
      const s = sections + i * 40, start = buffer.readUInt32LE(s + 12), bytes = buffer.readUInt32LE(s + 16);
      if (address >= start && address - start + length <= bytes) return range(buffer.readUInt32LE(s + 20) + address - start, length);
    }
    throw new Error('unmapped PE RVA');
  }
  if (!gui) return { machine: 'x86_64', subsystem: 'console' };
  need(buffer.readUInt32LE(opt + 108) >= 3, 'missing PE resources');
  const base = rva(buffer.readUInt32LE(opt + 128), 16), resourceSize = buffer.readUInt32LE(opt + 132);
  function at(offset, length) { need(offset >= 0 && offset + length <= resourceSize, 'invalid resource offset'); return range(base + offset, length); }
  let leaves = [];
  function visit(offset, depth) {
    need(depth <= 2, 'invalid resource depth');
    const dir = at(offset, 16), entries = buffer.readUInt16LE(dir + 12) + buffer.readUInt16LE(dir + 14); at(offset + 16, entries * 8);
    for (let i = 0; i < entries; i++) {
      const e = dir + 16 + i * 8, id = buffer.readUInt32LE(e), next = buffer.readUInt32LE(e + 4);
      if (depth === 0 && id !== 24) continue;
      if (depth === 1) need(id === 1, 'RT_MANIFEST resource name must be ID 1');
      if (next & 0x80000000) visit(next & 0x7fffffff, depth + 1);
      else { need(depth === 2, 'invalid manifest leaf'); const d = at(next, 16); leaves.push(buffer.subarray(rva(buffer.readUInt32LE(d), buffer.readUInt32LE(d + 4)), rva(buffer.readUInt32LE(d), buffer.readUInt32LE(d + 4)) + buffer.readUInt32LE(d + 4))); }
    }
  }
  visit(0, 0); need(leaves.length === 1, 'missing/ambiguous embedded manifest');
  const manifest = leaves[0]; const xml = manifest.toString(manifest.includes(0) ? 'utf16le' : 'utf8');
  const identities = xml.match(/<assemblyIdentity\b[^>]*>/g) || [];
  need(/<assembly\b[^>]*\bmanifestVersion\s*=\s*["']1\.0["']/.test(xml)
    && identities.some(tag => /\bname\s*=\s*["']Microsoft\.Windows\.Common-Controls["']/.test(tag)
      && /\bversion\s*=\s*["']6\.0\.0\.0["']/.test(tag)), 'Common Controls v6 manifest missing');
  return { machine: 'x86_64', subsystem: 'windows', embeddedManifestSha256: sha256(manifest) };
}

function inspectPayload(root, name, proof) {
  validateProof(proof, name);
  const files = inventory(root);
  if (name === 'windows-x64') {
    need(files.map(f => f.path).join() === 'katala-tune.exe,tune-agent.exe,tune.exe', 'unexpected Windows payload files');
    for (const expected of proof.build.sidecars) {
      const actual = files.find(f => f.path === expected.path);
      need(actual.bytes === expected.bytes && actual.sha256 === expected.sha256, 'packaged sidecar mismatch');
    }
    return { files, binaries: files.map(f => ({ path: f.path, ...inspectPE(fs.readFileSync(path.join(root, f.path)), f.path === 'katala-tune.exe') })) };
  }
  const required = ['Contents/Info.plist', 'Contents/MacOS/katala-tune', 'Contents/MacOS/tune', `Contents/Resources/${PROOF}`, 'Contents/_CodeSignature/CodeResources'];
  const allowed = new Set([...required, 'Contents/PkgInfo', 'Contents/Resources/icon.icns']);
  for (const f of files) need(f.path.startsWith(`${APP}/`) && allowed.has(f.path.slice(APP.length + 1)), 'unexpected macOS payload file');
  for (const f of required) need(files.some(entry => entry.path === `${APP}/${f}`), `missing ${f}`);
  const embedded = fs.readFileSync(path.join(root, APP, 'Contents/Resources', PROOF));
  need(embedded.equals(Buffer.from(json(proof))), 'embedded source proof mismatch');
  const app = path.join(root, APP);
  const plist = key => run('/usr/libexec/PlistBuddy', ['-c', `Print :${key}`, path.join(app, 'Contents/Info.plist')]);
  need(plist('CFBundleExecutable') === 'katala-tune' && plist('CFBundleIdentifier') === 'org.katala.tune.tauri', 'Info.plist identity mismatch');
  need(plist('CFBundleShortVersionString') === proof.build.appVersion, 'Info.plist version mismatch');
  const binaries = ['katala-tune', 'tune'].map(bin => {
    const file = `${APP}/Contents/MacOS/${bin}`;
    need(files.find(f => f.path === file).executable, 'Mac binary not executable');
    need(run('lipo', ['-archs', path.join(root, file)]) === 'arm64', 'Mach-O must be arm64 only');
    return { path: file, machine: 'arm64' };
  });
  run('codesign', ['--verify', '--deep', '--strict', app]);
  const signature = spawnSync('codesign', ['--display', '--verbose=4', app], { encoding: 'utf8' });
  need(signature.status === 0 && /^Signature=adhoc$/m.test(signature.stderr), 'expected ad-hoc signature');
  return { files, binaries, bundle: { executable: 'katala-tune', identifier: plist('CFBundleIdentifier'), version: plist('CFBundleShortVersionString'), signature: 'ad-hoc; codesign --verify --deep --strict' } };
}

function checkArchive(archive) {
  const names = run('tar', ['-tzf', archive]).split('\n');
  need(names.length > 1, 'empty archive');
  for (const name of names) { safePath(name.replace(/\/$/, '')); need(name === `${APP}/` || name.startsWith(`${APP}/`), 'archive path outside bundle'); }
  const entries = run('tar', ['-tvzf', archive]).split('\n');
  need(entries.every(line => /^[d-]/.test(line)), 'archive contains link/special entry');
}

function packageDelivery(root, name, proofFile, payload, output, env = process.env, command = run) {
  const proof = JSON.parse(fs.readFileSync(proofFile));
  need(isDeepStrictEqual(proof, sourceProof(root, name, env, command)), 'source/build changed since proof');
  const inspected = inspectPayload(payload, name, proof);
  // Refuse an existing output directory, even if it is empty.
  fs.mkdirSync(output);
  const manifest = { schema: 'katala-artifact-delivery/v1', proof, ...inspected };
  if (name === 'macos-arm64') {
    const archive = path.join(output, 'KatalaTune_macos-arm64.app.tar.gz');
    run('tar', ['-czf', archive, '-C', payload, APP]); checkArchive(archive);
    const extracted = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'katala-artifact-'));
    try {
      run('tar', ['-xzf', archive, '-C', extracted]);
      need(isDeepStrictEqual(inspectPayload(extracted, name, proof), inspected), 'archive round-trip mismatch');
    } finally { fs.rmSync(extracted, { recursive: true, force: true }); }
    manifest.archive = { path: path.basename(archive), bytes: fs.statSync(archive).size, sha256: sha256(fs.readFileSync(archive)) };
  } else {
    for (const f of inspected.files) fs.copyFileSync(path.join(payload, f.path), path.join(output, f.path), fs.constants.COPYFILE_EXCL);
    need(isDeepStrictEqual(inspectPayload(output, name, proof), inspected), 'copy mismatch');
  }
  writeNew(path.join(output, 'manifest.json'), json(manifest));
  return manifest;
}

module.exports = { sourceProof, validateProof, inspectPE, inventory, safePath, inspectPayload, checkArchive, packageDelivery, writeNew };
if (require.main === module) {
  try {
    const [command, name, proofFile, payload, output, ...extra] = process.argv.slice(2);
    need(!extra.length, 'unexpected arguments'); platform(name);
    if (command === 'source') {
      need(proofFile && !payload && !output, 'usage: source PLATFORM PROOF');
      writeNew(proofFile, json(sourceProof(process.cwd(), name)));
    } else if (command === 'package') {
      need(proofFile && payload && output, 'usage: package PLATFORM PROOF PAYLOAD NEW_OUTPUT');
      packageDelivery(process.cwd(), name, proofFile, payload, output);
    } else throw new Error('unknown command');
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
