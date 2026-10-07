#!/usr/bin/env node
'use strict';
const { spawnSync } = require('node:child_process');
const steps = [
  [process.execPath, ['scripts/version.js']],
  [process.execPath, ['--test', ...require('node:fs').readdirSync('test').filter(f => f.endsWith('.test.js')).map(f => `test/${f}`)]],
  [process.execPath, ['scripts/oss-check.js']],
  ['cargo', ['fmt', '--all', '--check']],
  ['cargo', ['clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings']],
  ['cargo', ['test', '--workspace', '--locked', ...(process.platform === 'win32' ? ['--exclude', 'katala-tune'] : [])]],
];
for (const [command, args] of steps) {
  const result = spawnSync(command, args, { stdio: 'inherit' });
  if (result.error || result.status !== 0) process.exit(result.status || 1);
}
