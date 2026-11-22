#!/usr/bin/env node
'use strict';
// Informational integrity inspection only. Never install or execute these files.
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { SEMVER } = require('./version');

const MAX_BINARY = 256 * 1024 * 1024;
const MAX_MANIFEST = 64 * 1024;
const ENTRIES = [['katala-tune-gpui', 'gui'], ['tune', 'diagnostic-cli'], ['tune-agent', 'native-sampler']];
const failure = (role, stage) => new Error(`GPUI artifact ${role}/${stage} failed`);
function keys(value, required) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).length === required.length && required.every(key => Object.hasOwn(value, key));
}
function field(value, pattern) {
  return typeof value === 'string' && value.length <= 512 && !/[\r\n]/.test(value) && pattern.test(value);
}
function tuple(platform, architecture) {
  if (platform === 'darwin' && architecture === 'arm64') return 'aarch64-apple-darwin';
  if (platform === 'win32' && architecture === 'x64') return 'x86_64-pc-windows-msvc';
  throw failure('input', 'arguments');
}
function validateManifest(m, platform, architecture, target) {
  const invalid = () => { throw failure('manifest', 'schema'); };
  if (!keys(m, ['schema', 'version', 'revision', 'sourceTree', 'platform', 'architecture', 'build', 'ci', 'files'])
      || m.schema !== 'katala_gpui_preview.v2' || !field(m.version, SEMVER)
      || !field(m.revision, /^[a-f0-9]{40}$/) || !field(m.sourceTree, /^[a-f0-9]{40}$/)
      || m.platform !== platform || m.architecture !== architecture
      || !keys(m.build, ['profile', 'target']) || m.build.profile !== 'release' || m.build.target !== target
      || !keys(m.ci, ['repository', 'workflowRef', 'workflowSha', 'eventName', 'runId', 'runAttempt'])) invalid();
  const ci = m.ci;
  if (!field(ci.repository, /^[A-Za-z0-9_-][A-Za-z0-9_.-]*\/[A-Za-z0-9_-][A-Za-z0-9_.-]*$/)
      || !field(ci.workflowRef, /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/\.github\/workflows\/gpui-preview\.yml@refs\/(?:heads\/[A-Za-z0-9][A-Za-z0-9._/-]*|pull\/[1-9][0-9]*\/merge)$/)
      || !ci.workflowRef.startsWith(`${ci.repository}/`) || !field(ci.workflowSha, /^[a-f0-9]{40}$/)
      || !field(ci.eventName, /^(?:pull_request|workflow_dispatch)$/)
      || ((ci.eventName === 'pull_request') !== ci.workflowRef.includes('@refs/pull/'))
      || !field(ci.runId, /^[1-9][0-9]{0,31}$/) || !field(ci.runAttempt, /^[1-9][0-9]{0,31}$/)) invalid();
  if (!Array.isArray(m.files) || m.files.length !== 3) invalid();
  const suffix = platform === 'win32' ? '.exe' : '';
  const seen = new Set();
  for (const entry of m.files) {
    if (!keys(entry, ['file', 'role', 'bytes', 'sha256'])
        || !ENTRIES.some(([name, role]) => entry.file === name + suffix && entry.role === role)
        || seen.has(entry.role) || !Number.isSafeInteger(entry.bytes) || entry.bytes < 1 || entry.bytes > MAX_BINARY
        || !field(entry.sha256, /^[a-f0-9]{64}$/)) invalid();
    seen.add(entry.role);
  }
  return m;
}

// BigInt timestamps and identity describe the actual open descriptor, not an
// unchecked second read by pathname. This detects changes, not hostile same-user
// namespace races. Node does not expose arbitrary Windows reparse tags.
function sameFile(a, b) {
  return ['dev', 'ino', 'mode', 'nlink', 'size', 'mtimeNs', 'ctimeNs'].every(key => a[key] === b[key]);
}
function regular(stat, limit) {
  return stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1n && stat.size >= 1n && stat.size <= BigInt(limit);
}
function readChecked(source, basename, role, limit, collect = false) {
  const input = path.join(source, basename);
  let fd, stage = 'metadata', outcome;
  try {
    const before = fs.lstatSync(input, { bigint: true });
    if (!regular(before, limit) || path.relative(source, fs.realpathSync(input)) !== basename) throw failure(role, stage);
    stage = 'open';
    fd = fs.openSync(input, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0) | (fs.constants.O_NONBLOCK || 0));
    stage = 'metadata';
    const opened = fs.fstatSync(fd, { bigint: true });
    if (!regular(opened, limit) || !sameFile(before, opened)) throw failure(role, stage);
    stage = 'read';
    const hash = createHash('sha256'), chunks = [];
    const buffer = Buffer.alloc(Math.min(limit, 1024 * 1024));
    let bytes = 0;
    for (;;) {
      const count = fs.readSync(fd, buffer, 0, buffer.length, null);
      if (!count) break;
      bytes += count;
      if (bytes > Number(opened.size) || bytes > limit) throw failure(role, stage);
      hash.update(buffer.subarray(0, count));
      if (collect) chunks.push(Buffer.from(buffer.subarray(0, count)));
    }
    const after = fs.fstatSync(fd, { bigint: true });
    const named = fs.lstatSync(input, { bigint: true });
    if (bytes !== Number(opened.size) || !sameFile(opened, after) || !sameFile(after, named)
        || path.relative(source, fs.realpathSync(input)) !== basename) throw failure(role, stage);
    outcome = { bytes, sha256: hash.digest('hex'), raw: collect ? Buffer.concat(chunks) : undefined };
  } catch {
    throw failure(role, stage);
  } finally {
    // A failed close also invalidates the result. Never hide it behind success.
    if (fd !== undefined) {
      try { fs.closeSync(fd); } catch { throw failure(role, 'close'); }
    }
  }
  return outcome;
}

function inspectArtifact(options) {
  if (!keys(options, ['source', 'platform', 'architecture'])) throw failure('input', 'arguments');
  const { source, platform, architecture } = options;
  if (typeof source !== 'string' || source.includes('\0') || !path.isAbsolute(source)) throw failure('input', 'arguments');
  const target = tuple(platform, architecture);
  let resolved;
  try {
    resolved = fs.realpathSync(source);
    if (!fs.statSync(resolved).isDirectory()) throw failure('source', 'resolve');
  } catch { throw failure('source', 'resolve'); }
  const raw = readChecked(resolved, 'manifest.json', 'manifest', MAX_MANIFEST, true);
  let parsed;
  try { parsed = JSON.parse(raw.raw.toString('utf8')); } catch { throw failure('manifest', 'decode'); }
  const manifest = validateManifest(parsed, platform, architecture, target);
  const files = manifest.files.map(entry => {
    const actual = readChecked(resolved, entry.file, entry.role, MAX_BINARY);
    if (actual.bytes !== entry.bytes || actual.sha256 !== entry.sha256) throw failure(entry.role, 'integrity');
    return { ...entry };
  });
  const { files: _entries, ...metadata } = manifest;
  return {
    status: 'integrity-checked',
    text: 'Files and manifest integrity checked; provenance, approval and install unverified.',
    source: resolved, manifestSha256: raw.sha256, manifest: metadata, files,
    nativeSampler: files.find(entry => entry.role === 'native-sampler'),
  };
}

function argumentsFor(argv) {
  const options = {};
  for (let i = 0; i < argv.length; i += 2) {
    const key = { '--source': 'source', '--platform': 'platform', '--architecture': 'architecture' }[argv[i]];
    const value = argv[i + 1];
    if (!key || Object.hasOwn(options, key) || typeof value !== 'string' || !value || value.startsWith('--')) throw failure('input', 'arguments');
    options[key] = value;
  }
  return options;
}
if (require.main === module) {
  try {
    if (Number(process.versions.node.split('.')[0]) !== 24) throw failure('input', 'arguments');
    console.log(JSON.stringify(inspectArtifact(argumentsFor(process.argv.slice(2)))));
  } catch (error) {
    console.error(/^GPUI artifact (?:input|source|manifest|gui|diagnostic-cli|native-sampler)\/[a-z]+ failed$/.test(error.message) ? error.message : 'GPUI artifact input/arguments failed');
    process.exitCode = 1;
  }
}
module.exports = { inspectArtifact };
