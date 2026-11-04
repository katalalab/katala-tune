'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

function fleetLaunch(envFile, { platform = process.platform, home = os.homedir(), programFiles = process.env.ProgramFiles } = {}) {
  const args = ['run', `--env-file=${envFile}`, '--', 'python3', 'scripts/fleet-status.py', '--json'];
  const script = path.join(home, 'bin', 'op-agent');
  const bash = programFiles && path.join(programFiles, 'Git', 'bin', 'bash.exe');
  if (platform === 'win32' && bash && fs.existsSync(bash) && fs.existsSync(script)) {
    return { program: bash, args: ['--noprofile', '--norc', script, ...args], options: { windowsHide: true } };
  }
  return { program: 'op-agent', args, options: { windowsHide: true } };
}
module.exports = { fleetLaunch };
