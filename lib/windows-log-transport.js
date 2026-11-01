// Windows log transport only. Keep controller/probe runner contracts separate.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawn } = require('node:child_process');
const { performance } = require('node:perf_hooks');
const MAX_SCRIPT = 1024 * 1024;
const MAX_STDOUT = 4 * 1024 * 1024;
const MAX_STDERR = 64 * 1024;
const FAILURE = 'Windows log transport failed';
const INVALID = 'Invalid Windows log transport request';

function windowsScriptTransport(script, kind, cursors) {
  const body = script.subarray(script.subarray(0, 3).equals(Buffer.from([0xef, 0xbb, 0xbf])) ? 3 : 0);
  const normalized = Buffer.from(body.toString('latin1').replace(/\r\n/g, '\n'), 'latin1');
  if (!normalized.length || normalized.length > MAX_SCRIPT) throw Error(INVALID);
  if (!['events', 'logons'].includes(kind) || cursors.length !== 3) throw Error(INVALID);
  for (const cursor of cursors) {
    if (!/^(0|-?[1-9][0-9]{0,18})$/.test(cursor)) throw Error(INVALID);
    const value = BigInt(cursor);
    if (kind === 'events' ? value < -9007199254740991n || value > 9007199254740991n : value < 0n || value > 9223372036854775807n) throw Error(INVALID);
  }
  if (kind === 'logons' && cursors[2] !== '0') throw Error(INVALID);
  const bootstrap = fs.readFileSync(path.join(__dirname, '..', 'probes', 'win_logs_bootstrap.ps1'), 'utf8').replace(/^\uFEFF/, '').replace(/\r\n/g, '\n').trim();
  const command = `powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(bootstrap, 'utf16le').toString('base64')}`;
  if (Buffer.byteLength(command) >= 8191) throw Error('Windows log bootstrap exceeds 8191 bytes');
  const input = ['KTLOG1', kind, ...cursors, String(normalized.length), crypto.createHash('sha256').update(normalized).digest('hex'), normalized.toString('base64')].join('\n');
  return { command, bootstrap, input };
}

// Only complete successful exchanges may supply parser input. No partial output
// escapes on timeout, pipe failure, nonzero exit, or missing EOF after root exit.
function runWindowsLogs(cmd, args, { input, timeoutMs = 90000 } = {}, spawnChild = spawn) {
  return new Promise((resolve) => {
    const deadline = performance.now() + timeoutMs;
    let child;
    try { child = spawnChild(cmd, args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true }); }
    catch { resolve({ code: null, out: '', err: FAILURE }); return; }
    let done = false, written = false, stdoutEof = false, stderrEof = false;
    const out = [], err = [];
    let outBytes = 0, errBytes = 0;
    const finish = (success) => {
      if (done) return;
      success &&= performance.now() <= deadline;
      done = true;
      clearTimeout(timer);
      if (!success) child.kill('SIGKILL');
      child.stdin.destroy(); child.stdout.destroy(); child.stderr.destroy();
      try {
        const decode = new TextDecoder('utf-8', { fatal: true });
        resolve(success ? { code: 0, out: decode.decode(Buffer.concat(out)), err: decode.decode(Buffer.concat(err)) } : { code: null, out: '', err: FAILURE });
      } catch { resolve({ code: null, out: '', err: FAILURE }); }
    };
    const timer = setTimeout(() => finish(false), timeoutMs);
    const fail = () => finish(false);
    child.on('error', fail);
    child.stdin.on('error', fail);
    child.stdout.on('error', fail); child.stderr.on('error', fail);
    child.stdout.on('data', b => { outBytes += b.length; if (outBytes > MAX_STDOUT) fail(); else out.push(b); });
    child.stderr.on('data', b => { errBytes += b.length; if (errBytes > MAX_STDERR) fail(); else err.push(b); });
    child.stdout.on('end', () => { stdoutEof = true; });
    child.stderr.on('end', () => { stderrEof = true; });
    child.on('close', code => finish(code === 0 && written && stdoutEof && stderrEof));
    child.stdin.end(input, error => { if (error) fail(); else written = true; });
  });
}
module.exports = { windowsScriptTransport, runWindowsLogs, FAILURE };
