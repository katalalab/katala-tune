#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { execFileSync } = require('node:child_process');

const MAX_BINARY = 128 * 1024 * 1024;
const HOST = 'x86_64-pc-windows-msvc';
function command(exe, args, root) {
  try {
    return execFileSync(exe, args, { cwd: root, encoding: 'utf8', timeout: 10000, maxBuffer: exe === 'git' && args.join(' ') === 'show HEAD:Cargo.lock' ? 2 * 1024 * 1024 : 65536, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  } catch { throw new Error('artifact provenance command failed'); }
}
function field(value, pattern, label) {
  if (typeof value !== 'string' || value.length > 512 || !pattern.test(value)) throw new Error(`invalid ${label}`);
  return value;
}
function regular(root, relative, max, label) {
  let file = root;
  for (const part of relative.split('/')) {
    file = path.join(file, part);
    let stat;
    try { stat = fs.lstatSync(file); } catch { throw new Error(`${label} missing`); }
    if (stat.isSymbolicLink()) throw new Error(`${label} symlink rejected`);
  }
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.size < 1 || stat.size > max) throw new Error(`${label} size/type rejected`);
  return { file, stat };
}
function digest(file) { return createHash('sha256').update(fs.readFileSync(file)).digest('hex'); }
function copyBinary(input, destination) {
  const source = fs.openSync(input.file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
  try {
    const before = fs.fstatSync(source);
    if (!before.isFile() || before.ino !== input.stat.ino || before.dev !== input.stat.dev || before.size !== input.stat.size) throw new Error('binary changed before copy');
    const output = fs.openSync(destination, 'wx');
    let bytes = 0;
    const hash = createHash('sha256');
    try {
      const buffer = Buffer.alloc(1024 * 1024);
      for (;;) {
        const count = fs.readSync(source, buffer, 0, buffer.length, null);
        if (count === 0) break;
        bytes += count;
        if (bytes > MAX_BINARY || bytes > before.size) throw new Error('binary changed during copy');
        let written = 0;
        while (written < count) written += fs.writeSync(output, buffer, written, count - written);
        hash.update(buffer.subarray(0, count));
      }
      const after = fs.fstatSync(source);
      if (bytes !== before.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs) throw new Error('binary changed during copy');
    } finally { fs.closeSync(output); }
    return { file: 'tune.exe', bytes, sha256: hash.digest('hex') };
  } finally { fs.closeSync(source); }
}
function createArtifact({ root = process.cwd(), env = process.env, platform = process.platform, arch = process.arch, run = command } = {}) {
  if (platform !== 'win32' || arch !== 'x64') throw new Error('native Windows x64 required');
  for (const key of ['CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_TARGET']) if (env[key]) throw new Error('nondefault Cargo target rejected');
  const gitSha = (value, label) => field(value, /^[a-f0-9]{40}$/, label);
  const githubSha = gitSha(env.GITHUB_SHA, 'GitHub SHA');
  const repository = field(env.GITHUB_REPOSITORY, /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/, 'repository');
  const workflowRef = field(env.GITHUB_WORKFLOW_REF, /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/\.github\/workflows\/ci\.yml@refs\/(?:heads\/main|pull\/[1-9][0-9]*\/merge)$/, 'workflow ref');
  if (!workflowRef.startsWith(`${repository}/`)) throw new Error('workflow repository mismatch');
  const workflowSha = gitSha(env.GITHUB_WORKFLOW_SHA, 'workflow SHA');
  const eventName = field(env.GITHUB_EVENT_NAME, /^(?:pull_request|push)$/, 'event');
  if ((eventName === 'pull_request') !== workflowRef.includes('@refs/pull/')) throw new Error('workflow event mismatch');
  const runId = field(env.GITHUB_RUN_ID, /^[1-9][0-9]{0,31}$/, 'run ID');
  const runAttempt = field(env.GITHUB_RUN_ATTEMPT, /^[1-9][0-9]{0,31}$/, 'run attempt');
  const head = gitSha(run('git', ['rev-parse', 'HEAD'], root).trim(), 'HEAD');
  if (head !== githubSha) throw new Error('checked-out HEAD differs from GitHub SHA');
  const tree = gitSha(run('git', ['rev-parse', 'HEAD^{tree}'], root).trim(), 'HEAD tree');
  try { run('git', ['diff', '--quiet', 'HEAD', '--'], root); } catch { throw new Error('tracked source changes rejected'); }
  const rust = run('rustc', ['-vV'], root);
  const release = /^release: (\S+)\r?$/m.exec(rust)?.[1];
  const host = /^host: (\S+)\r?$/m.exec(rust)?.[1];
  if (release !== '1.99.0' || host !== HOST) throw new Error('toolchain release/host rejected');
  const compilerField = (label, pattern) => field(new RegExp(`^${label}: (.+)\\r?$`, 'm').exec(rust)?.[1]?.trim(), pattern, 'toolchain metadata');
  const versionLine = field(rust.split(/\r?\n/, 1)[0], /^rustc 1\.99\.0 \([a-f0-9]{9,40} [0-9]{4}-[0-9]{2}-[0-9]{2}\)$/, 'toolchain version');
  const rustc = {
    versionLine,
    binary: compilerField('binary', /^rustc$/),
    commitHash: compilerField('commit-hash', /^[a-f0-9]{40}$/),
    commitDate: compilerField('commit-date', /^[0-9]{4}-[0-9]{2}-[0-9]{2}$/),
    release, host,
    llvmVersion: compilerField('LLVM version', /^[0-9]{1,3}(?:\.[0-9]{1,3}){1,2}$/),
  };
  if (!versionLine.includes(rustc.commitDate) || !rustc.commitHash.startsWith(versionLine.split('(')[1].split(' ')[0])) throw new Error('toolchain metadata inconsistent');
  const binary = regular(root, 'target/debug/tune.exe', MAX_BINARY, 'binary');
  const lock = regular(root, 'Cargo.lock', 2 * 1024 * 1024, 'lock');
  // Checkout bytes may be CRLF; Git's canonical blob retains its original final LF.
  const gitLock = run('git', ['show', 'HEAD:Cargo.lock'], root);
  if (typeof gitLock !== 'string' || gitLock.length < 1 || Buffer.byteLength(gitLock) > 2 * 1024 * 1024) throw new Error('canonical lock blob rejected');
  const source = { githubSha, head, tree, cargoLockSha256: digest(lock.file), cargoLockGitSha256: createHash('sha256').update(gitLock, 'utf8').digest('hex') };
  if (eventName === 'pull_request') {
    let event;
    try {
      const info = fs.statSync(env.GITHUB_EVENT_PATH);
      if (!info.isFile() || info.size > 2 * 1024 * 1024) throw new Error();
      event = JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, 'utf8'));
    } catch { throw new Error('PR event unavailable'); }
    source.pullRequest = { headSha: gitSha(event.pull_request?.head?.sha, 'PR head SHA'), baseSha: gitSha(event.pull_request?.base?.sha, 'PR base SHA') };
  }
  const parent = path.join(root, 'dist');
  if (fs.existsSync(parent)) {
    const stat = fs.lstatSync(parent);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error('artifact parent rejected');
  } else fs.mkdirSync(parent);
  const dist = path.join(parent, 'ci-cli');
  // mkdir is exclusive; existing directories/symlinks fail instead of being reused.
  fs.mkdirSync(dist);
  const artifact = copyBinary(binary, path.join(dist, 'tune.exe'));
  const manifest = { schemaVersion: 1, artifact, build: { package: 'tune-cli', profile: 'debug', target: HOST, platform, arch, rustc }, source, ci: { repository, workflowRef, workflowSha, eventName, runId, runAttempt } };
  fs.writeFileSync(path.join(dist, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, { flag: 'wx' });
  return manifest;
}
if (require.main === module) {
  try {
    if (Number(process.versions.node.split('.')[0]) !== 24) throw new Error('Node 24 required');
    createArtifact();
    console.log('CI CLI artifact prepared');
  } catch { console.error('CI CLI artifact validation failed'); process.exitCode = 1; }
}
module.exports = { createArtifact };
