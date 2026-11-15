#!/usr/bin/env node
'use strict';
// Stage CI outputs only; this does not install or approve either helper.
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { execFileSync } = require('node:child_process');
const { SEMVER } = require('./version');

const MAX_BINARY = 256 * 1024 * 1024;
function command(exe, args, root) {
  return execFileSync(exe, args, { cwd: root, encoding: 'utf8', timeout: 10000, maxBuffer: 65536, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
}
function field(value, pattern, label) {
  // RegExp's $ accepts a trailing newline; metadata must be a single exact value.
  if (typeof value !== 'string' || value.length > 512 || /[\r\n]/.test(value) || !pattern.test(value)) throw new Error(`invalid ${label}`);
  return value;
}
function binary(root, file) {
  let input = root;
  for (const part of ['target', 'release', file]) {
    input = path.join(input, part);
    let stat;
    try { stat = fs.lstatSync(input); } catch { throw new Error('binary missing'); }
    if (stat.isSymbolicLink()) throw new Error('binary symlink rejected');
  }
  const stat = fs.lstatSync(input);
  if (!stat.isFile() || stat.size < 1 || stat.size > MAX_BINARY) throw new Error('binary size/type rejected');
  return { input, stat };
}
function copyBinary({ input, stat, file, role }, dist) {
  const source = fs.openSync(input, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0));
  try {
    const before = fs.fstatSync(source);
    if (!before.isFile() || before.ino !== stat.ino || before.dev !== stat.dev || before.size !== stat.size) throw new Error('binary changed before copy');
    const output = fs.openSync(path.join(dist, file), 'wx', 0o755);
    let bytes = 0;
    const hash = createHash('sha256');
    try {
      const buffer = Buffer.alloc(1024 * 1024);
      for (;;) {
        const count = fs.readSync(source, buffer, 0, buffer.length, null);
        if (!count) break;
        bytes += count;
        if (bytes > before.size) throw new Error('binary changed during copy');
        let written = 0;
        while (written < count) written += fs.writeSync(output, buffer, written, count - written);
        hash.update(buffer.subarray(0, count));
      }
      const after = fs.fstatSync(source);
      if (bytes !== before.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs) throw new Error('binary changed during copy');
    } finally { fs.closeSync(output); }
    return { file, role, bytes, sha256: hash.digest('hex') };
  } finally { fs.closeSync(source); }
}
function createArtifact({ root = process.cwd(), env = process.env, platform = process.platform, arch = process.arch, run = command } = {}) {
  let target;
  if (platform === 'darwin' && arch === 'arm64') target = 'aarch64-apple-darwin';
  else if (platform === 'win32' && arch === 'x64') target = 'x86_64-pc-windows-msvc';
  else throw new Error('unsupported platform/architecture');
  for (const key of ['CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_TARGET']) if (env[key]) throw new Error('nondefault Cargo target rejected');
  const gitSha = (value, label) => field(value, /^[a-f0-9]{40}$/, label);
  const revision = gitSha(env.GITHUB_SHA, 'GitHub SHA');
  const repository = field(env.GITHUB_REPOSITORY, /^[A-Za-z0-9_-][A-Za-z0-9_.-]*\/[A-Za-z0-9_-][A-Za-z0-9_.-]*$/, 'repository');
  const workflowRef = field(env.GITHUB_WORKFLOW_REF, /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/\.github\/workflows\/gpui-preview\.yml@refs\/(?:heads\/[A-Za-z0-9][A-Za-z0-9._/-]*|pull\/[1-9][0-9]*\/merge)$/, 'workflow ref');
  if (!workflowRef.startsWith(`${repository}/`)) throw new Error('workflow repository mismatch');
  const workflowSha = gitSha(env.GITHUB_WORKFLOW_SHA, 'workflow SHA');
  const eventName = field(env.GITHUB_EVENT_NAME, /^(?:pull_request|workflow_dispatch)$/, 'event');
  if ((eventName === 'pull_request') !== workflowRef.includes('@refs/pull/')) throw new Error('workflow event mismatch');
  const runId = field(env.GITHUB_RUN_ID, /^[1-9][0-9]{0,31}$/, 'run ID');
  const runAttempt = field(env.GITHUB_RUN_ATTEMPT, /^[1-9][0-9]{0,31}$/, 'run attempt');
  const head = gitSha(run('git', ['rev-parse', 'HEAD'], root).trim(), 'HEAD');
  if (head !== revision) throw new Error('checked-out HEAD differs from GitHub SHA');
  const sourceTree = gitSha(run('git', ['rev-parse', 'HEAD^{tree}'], root).trim(), 'source tree');
  try { run('git', ['diff', '--quiet', 'HEAD', '--'], root); } catch { throw new Error('tracked source changes rejected'); }
  const version = field(JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8')).version, SEMVER, 'version');
  const suffix = platform === 'win32' ? '.exe' : '';
  // Fixed source and destination names; unrelated Cargo outputs are never included.
  const inputs = [['katala-tune-gpui', 'gui'], ['tune', 'diagnostic-cli'], ['tune-agent', 'native-sampler']].map(([name, role]) => {
    const file = name + suffix;
    return { ...binary(root, file), file, role };
  });
  const parent = path.join(root, 'dist');
  if (fs.existsSync(parent)) {
    const stat = fs.lstatSync(parent);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error('artifact parent rejected');
  } else fs.mkdirSync(parent);
  const dist = path.join(parent, 'gpui-preview');
  // Never reuse or delete stale output. A failed copy leaves no success manifest.
  fs.mkdirSync(dist);
  const files = inputs.map(input => copyBinary(input, dist));
  const manifest = { schema: 'katala_gpui_preview.v2', version, revision, sourceTree, platform, architecture: arch, build: { profile: 'release', target }, ci: { repository, workflowRef, workflowSha, eventName, runId, runAttempt }, files };
  fs.writeFileSync(path.join(dist, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, { flag: 'wx' });
  return manifest;
}
if (require.main === module) {
  try {
    if (Number(process.versions.node.split('.')[0]) !== 24) throw new Error('Node 24 required');
    createArtifact();
    console.log('GPUI CI artifact prepared');
  } catch { console.error('GPUI CI artifact validation failed'); process.exitCode = 1; }
}
module.exports = { createArtifact };
