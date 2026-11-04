'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { fleetLaunch } = require('../lib/fleet-launch');

test('other platforms keep the direct executable and literal arguments', () => {
  const launch = fleetLaunch('literal & value', { platform: 'linux' });
  assert.equal(launch.program, 'op-agent');
  assert.equal(launch.args[1], '--env-file=literal & value');
});

test('Windows fleet launcher passes arguments literally to a hidden Git Bash process', { skip: process.platform !== 'win32' }, () => {
  const path = require('node:path');
  const root = path.join(__dirname, 'fixtures', 'fleet launcher');
  const launch = fleetLaunch('value & "quoted"', { platform: 'win32', home: root, programFiles: process.env.ProgramFiles });
  assert.equal(launch.options.windowsHide, true);
  const child = spawnSync(launch.program, launch.args, launch.options);
  assert.equal(child.status, 7, child.error?.message);
  assert.match(child.stdout.toString(), /<--env-file=value & "quoted">/);
  assert.match(child.stdout.toString(), /<scripts\/fleet-status\.py>/);
  assert.match(child.stdout.toString(), /console=hidden/);
  assert.equal(child.stderr.toString(), 'fixture stderr\n');
});
