'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const source = fs.readFileSync(path.join(__dirname, '..', 'probes', 'win_probe.ps1'), 'utf8');
test('nested Windows collectors have a bounded shared output reader', () => {
  assert.ok(source.includes('function Read-KtProcessOutput('));
  assert.doesNotMatch(source, /\.ReadToEndAsync\(\)/);
});
test('PS5.1 exercises nested collector limits, exit and EOF using owned children', { skip: process.platform !== 'win32' }, () => {
  const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-File', path.join(__dirname, 'win-worker-pipes-regression.ps1')], { encoding: 'utf8', timeout: 30000, windowsHide: true });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /worker-pipes: 10 passed/);
});
