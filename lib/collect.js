// 各機体で調査スクリプトを走らせ、snapshot を返す。WSL 内観測は台帳の "wsl": true でのみ有効（停止との競合で起動する可能性あり）。
// macOS: この機体ならローカル実行、それ以外は ssh で python3 の標準入力へ渡す。
// Windows: ssh（既定シェルは Git Bash）で ~/.katala-tune/ に置いて PowerShell 5.1 で実行。この機体が Windows ならローカルで実行。
'use strict';
const { spawn } = require('node:child_process');
const { performance } = require('node:perf_hooks');
const ledger = require('./ledger');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { enabled: netsecEnabled, peersEnabled, dropPeers } = require('./netsec');

const PROBES = path.join(__dirname, '..', 'probes');
const SSH_OPTS = ['-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-o', 'ServerAliveInterval=10', '-o', 'ControlMaster=no', '-o', 'ControlPath=none'];
const BENCH_PY = "import time,json,statistics as s\nr=[]\nfor _ in range(5):\n t=time.perf_counter();sum(i*i for i in range(3000000));r.append(round((time.perf_counter()-t)*1000,1))\nprint(json.dumps({'runs_ms':r,'median_ms':s.median(r)}))";
const BENCH_MARK = '@@KATALA_TUNE_BENCH@@';

// 日本語版 Windows のコマンド（powercfg など）は CP932 で出力する。UTF-8 として読めなければ Shift_JIS で読む
const UTF8 = new TextDecoder('utf-8', { fatal: true });
const SJIS = new TextDecoder('shift_jis');
function decode(buf) {
  try { return UTF8.decode(buf); } catch { return SJIS.decode(buf); }
}

function run(cmd, args, { input, timeoutMs = 90000 } = {}) {
  return new Promise((resolve) => {
    const child = spawn(cmd, args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    const outB = [], errB = [];
    let extra = '', done = false, timer, closeTimer;
    const finish = (code) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      clearTimeout(closeTimer);
      child.stdin.destroy();
      child.stdout.destroy();
      child.stderr.destroy();
      resolve({ code, out: decode(Buffer.concat(outB)), err: decode(Buffer.concat(errB)) + extra });
    };
    const closeFallback = (code) => {
      if (!done && !closeTimer) {
        clearTimeout(timer);
        closeTimer = setTimeout(() => finish(code), 500);
      }
    };
    timer = setTimeout(() => {
      if (!done) {
        extra += `\ntimeout ${timeoutMs}ms`;
        child.kill('SIGKILL');
        closeFallback(null);
      }
    }, timeoutMs);
    child.stdout.on('data', (d) => { outB.push(d); });
    child.stderr.on('data', (d) => { errB.push(d); });
    child.on('error', (e) => { extra += String(e); finish(null); });
    child.on('exit', closeFallback);
    child.on('close', finish);
    child.stdin.on('error', () => {});
    child.stdin.end(input ?? '');
  });
}

function lastJsonLine(text) {
  const lines = text.split(/\r?\n/).map((l) => l.trim()).filter((l) => l.startsWith('{'));
  for (let i = lines.length - 1; i >= 0; i--) {
    try { return JSON.parse(lines[i]); } catch { /* 次へ */ }
  }
  return null;
}

// この機体（アプリを動かしている機体）でのコマンド実行
const localShell = {
  async powershellFile(scriptBuf, params = '', timeoutMs = 120000) {
    const dir = path.join(os.tmpdir(), 'katala-tune');
    fs.mkdirSync(dir, { recursive: true });
    // 分析とログ取り込みは並行して動くので、重ならない名前で新しく作る（既にあれば失敗する 'wx'）
    const f = path.join(dir, `p-${process.pid}-${crypto.randomUUID()}.ps1`);
    fs.writeFileSync(f, scriptBuf, { flag: 'wx' });
    try {
      return await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', f, ...params.split(' ').filter(Boolean)], { timeoutMs });
    } finally {
      fs.rmSync(f, { force: true });
    }
  },
  python(code, timeoutMs = 60000) {
    return run(process.platform === 'win32' ? 'python' : '/usr/bin/env', process.platform === 'win32' ? ['-c', code] : ['python3', '-c', code], { timeoutMs });
  },
  // ローカルの短いスクリプト（actions 用）。macOS は sh、Windows は PowerShell
  script(text, timeoutMs = 30000) {
    return process.platform === 'win32'
      ? run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', text], { timeoutMs })
      : run('/bin/sh', ['-c', text], { timeoutMs });
  },
};

// 調査スクリプトへの印。benchmark: false でベンチマークを省き、network: false（台帳の "network": false）で
// ネットワークとセキュリティ（netsec）を集めない。どちらも機体ごとの台帳の値
function macProbeFlags(benchmark, network = true) {
  return [...(benchmark ? [] : ['--skip-benchmark']), ...(network ? [] : ['nonet'])];
}

function macProbeArgs(benchmark, network = true) {
  return ['python3', '-', ...macProbeFlags(benchmark, network)];
}

function macSshCommand(benchmark, network = true) {
  const suffix = macProbeFlags(benchmark, network).map((a) => ` ${a}`).join('');
  return `command -v python3 >/dev/null && exec python3 -${suffix} || exec /usr/bin/python3 -${suffix}`;
}

function actionHelper(node) {
  const helper = node.action_helper;
  if (!helper || typeof helper !== 'object' || Array.isArray(helper)
    || Object.keys(helper).length !== 6
    || helper.schema !== 'katala-tune/action-helper/v2'
    || !lowerHex(helper.source_sha, 40) || !lowerHex(helper.tune_sha256, 64)
    || !lowerHex(helper.manifest_sha256, 64)
    || !Number.isSafeInteger(helper.run_id) || helper.run_id < 1
    || !Number.isSafeInteger(helper.attempt) || helper.attempt < 1) throw new Error('approved action helper record が無いため実行しない');
  return helper.tune_sha256;
}

function windowsProbeCommand(helperSha256, benchmark = true, network = true, observeWsl = false) {
  return `exec "$HOME/.katala-tune/bin/tune.exe" windows-probe-v1 --helper-sha256 ${helperSha256} --benchmark ${Number(benchmark)} --network ${Number(network)} --observe-wsl ${Number(observeWsl)} --budget-ms 120000`;
}

function windowsSshCommand(benchmark = true, network = true, observeWsl = false) {
  return windowsProbeCommand('a'.repeat(64), benchmark, network, observeWsl);
}

function fixedLocalProbePath() {
  const home = os.homedir();
  const helper = path.join(home, '.katala-tune', 'bin', 'tune.exe');
  if (!path.isAbsolute(home) || !path.isAbsolute(helper) || home.split(path.sep).some((part) => part === '.' || part === '..')) throw new Error('Windows probe helper path is invalid; collection refused');
  return helper;
}

function probeExchange(node, helperSha256, benchmark, network, observeWsl, { spawnChild = spawn } = {}) {
  if (!lowerHex(helperSha256, 64)) return Promise.resolve({ code: 9, out: '', err: 'Windows probe request is invalid; collection refused' });
  let cmd = 'ssh';
  let args = [...SSH_OPTS, node.alias, windowsProbeCommand(helperSha256, benchmark, network, observeWsl)];
  if (node.local) {
    try { cmd = fixedLocalProbePath(); } catch (error) { return Promise.resolve({ code: 9, out: '', err: String(error.message || error) }); }
    args = ['windows-probe-v1', '--helper-sha256', helperSha256, '--benchmark', String(Number(benchmark)), '--network', String(Number(network)), '--observe-wsl', String(Number(observeWsl)), '--budget-ms', '120000'];
  }
  return new Promise((resolve) => {
    let child;
    try { child = spawnChild(cmd, args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true }); } catch (error) { resolve({ code: 9, out: '', err: `Windows probe spawn failed: ${String(error.message || error)}` }); return; }
    const out = [], err = [];
    let outSize = 0, errSize = 0, failed = false, done = false;
    const finish = (value) => { if (!done) { done = true; clearTimeout(timer); resolve(value); } };
    const fail = (message) => { failed = true; child.kill('SIGKILL'); child.stdin.destroy(); finish({ code: 9, out: '', err: message }); };
    const timer = setTimeout(() => fail('Windows probe transport deadline exceeded'), 135000);
    child.stdin.on('error', () => {});
    child.stdout.on('data', (chunk) => { outSize += chunk.length; if (outSize > 1048576) fail('Windows probe stdout exceeded limit'); else out.push(chunk); });
    child.stderr.on('data', (chunk) => { errSize += chunk.length; if (errSize > 65536) fail('Windows probe stderr exceeded limit'); else err.push(chunk); });
    child.on('error', () => fail('Windows probe spawn failed'));
    child.on('close', (code) => {
      if (failed) return;
      if (code !== 0) return finish({ code: 9, out: '', err: 'Windows probe transport outcome unverified' });
      finish({ code: 0, out: decode(Buffer.concat(out)), err: decode(Buffer.concat(err)) });
    });
    child.stdin.end();
  });
}

// 失敗の理由を、画面と判定が使える種類に分ける（crates/tune-core/src/collect.rs の classify_failure と同じ）。
// timeout: 接続はできたが、こちらの打ち切り（run が足す `timeout <ms>ms`）まで終わらなかった。
// unreachable: ssh が相手に届かない（名前が引けない・拒否・経路なし・接続の時間切れ）。auth: 鍵・ホスト鍵で入れない。error: それ以外
const AUTH_FAIL = /Permission denied|Host key verification failed|Authentication failed|Too many authentication failures/i;
const UNREACHABLE = /Could not resolve hostname|Name or service not known|Connection refused|No route to host|Network is unreachable|Connection timed out|Operation timed out|Host is down|Connection reset|Connection closed by|kex_exchange_identification|banner exchange/i;
const REASON_TEXT = { timeout: '時間切れ（接続後に応答が返らなかった）', unreachable: '接続できない（電源・ネットワーク・Host 名）', auth: '認証できない（鍵・ホスト鍵）', error: '調査の失敗' };
function classifyFailure(res) {
  const text = `${res.err || ''}\n${res.out || ''}`;
  if (/timeout \d+ms\s*$/.test(res.err || '')) return 'timeout';
  if (AUTH_FAIL.test(text)) return 'auth';
  if (UNREACHABLE.test(text)) return 'unreachable';
  return 'error';
}

// deps.run: 子プロセスの実行口、deps.localShell: この機体でのローカル実行の口（テストが偽物に差し替える。既定は本物）
async function probeNode(node, deps = {}) {
  // 運用対象でない機体（retired など）には ssh も probe も実行しない。失敗ではなく「対象外」として返す
  const refused = ledger.refusal(node);
  if (refused) return { node_id: node.id, ok: false, skipped: true, reason: 'excluded', reason_text: refused, error: refused, at: Date.now() };
  const runCmd = deps.run || run;
  const local = deps.localShell || localShell;
  const started = Date.now();
  const network = netsecEnabled(node);
  let res;
  const benchmark = node.benchmark !== false;
  const observeWsl = node.os === 'windows' && node.wsl === true;
  if (node.os === 'macos') {
    const script = fs.readFileSync(path.join(PROBES, 'mac_probe.py'), 'utf8');
    res = node.local
      ? await runCmd('/usr/bin/env', macProbeArgs(benchmark, network), { input: script })
      : await runCmd('ssh', [...SSH_OPTS, node.alias, macSshCommand(benchmark, network)], { input: script });
  } else {
    try {
      const helper = actionHelper(node);
      res = await (deps.probeExchange || probeExchange)(node, helper, benchmark, network, observeWsl);
    } catch (error) { res = { code: 9, out: '', err: String(error.message || error) }; }
  }
  const [main, benchPart] = res.out.split(BENCH_MARK);
  const data = lastJsonLine(main);
  if (!data) {
    const reason = classifyFailure(res);
    return { node_id: node.id, ok: false, reason, reason_text: REASON_TEXT[reason], error: (res.err || res.out || `exit ${res.code}`).trim().slice(-800), wall_s: (Date.now() - started) / 1000, at: Date.now() };
  }
  if (!benchmark) { data.bench = null; data.benchmark_skipped = true; }
  if (benchPart && !data.bench) data.bench = lastJsonLine(benchPart);
  if (!peersEnabled(node)) dropPeers(data);
  return { node_id: node.id, ok: true, data, wall_s: (Date.now() - started) / 1000, at: Date.now() };
}

// 全機体を並列に調べる。onResult は機体ごとに終わった順で呼ぶ
async function probeAll(nodes, onResult, deps) {
  return Promise.all(ledger.partition(nodes).targets.map(async (n) => {
    const r = await probeNode(n, deps).catch((e) => ({ node_id: n.id, ok: false, reason: 'error', reason_text: REASON_TEXT.error, error: String(e), at: Date.now() }));
    onResult?.(r);
    return r;
  }));
}

// 機体でコマンドを実行する（actions 用）。ローカルならその OS のシェル、リモートは ssh
function execOn(node, script, timeoutMs = 30000, input = null) {
  const refused = ledger.refusal(node);
  if (refused) return Promise.resolve({ code: null, out: '', err: refused });
  if (node.local && input != null) return Promise.resolve({ code: null, out: '', err: 'ローカル action に stdin を渡す経路は未対応のため実行しない' });
  return node.local ? localShell.script(script, timeoutMs) : run('ssh', [...SSH_OPTS, node.alias, script], { timeoutMs, input });
}

function lowerHex(value, size) {
  return typeof value === 'string' && value.length === size && /^[0-9a-f]+$/.test(value);
}

function canonicalSize(value) {
  if (!/^(?:0|[1-9][0-9]*)$/.test(value)) return null;
  const number = Number(value);
  return Number.isSafeInteger(number) && String(number) === value ? number : null;
}

const monotonicNow = () => performance.now();

function deadline(operation, endsAt, now = monotonicNow) {
  const left = endsAt - now();
  if (left <= 0) return Promise.reject(new Error('deadline'));
  let promise;
  try { promise = operation(); } catch (error) { return Promise.reject(error); }
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('deadline')), left); }),
  ]).finally(() => clearTimeout(timer));
}

function byteReader(stream) {
  const input = stream[Symbol.asyncIterator]();
  let buffered = Buffer.alloc(0);
  let done = false;
  async function fill(maxBuffered) {
    if (done) return false;
    const next = await input.next();
    done = !!next.done;
    if (!done && next.value.length) {
      if (buffered.length + next.value.length > maxBuffered) throw new Error('overflow');
      buffered = Buffer.concat([buffered, next.value]);
    }
    return !done;
  }
  return {
    async line(max, remainderLimit = 0) {
      for (;;) {
        const end = buffered.indexOf(10);
        if (end >= 0) {
          if (end > max || buffered.subarray(0, end).includes(13)) throw new Error('header');
          const raw = buffered.subarray(0, end);
          if (raw.some((byte) => byte < 0x20 || byte > 0x7e)) throw new Error('header');
          const value = raw.toString('ascii');
          buffered = buffered.subarray(end + 1);
          return value;
        }
        if (buffered.length >= max || done) throw new Error('header');
        const next = await input.next();
        done = !!next.done;
        if (done || !next.value.length) continue;
        const newline = next.value.indexOf(10);
        if (newline < 0) {
          if (buffered.length + next.value.length > max) throw new Error('header');
          buffered = Buffer.concat([buffered, next.value]);
          continue;
        }
        if (buffered.length + newline > max || next.value.length - newline - 1 > remainderLimit) throw new Error('header');
        const raw = Buffer.concat([buffered, next.value.subarray(0, newline)]);
        if (raw.includes(13) || raw.some((byte) => byte < 0x20 || byte > 0x7e)) throw new Error('header');
        buffered = next.value.subarray(newline + 1);
        return raw.toString('ascii');
      }
    },
    async bytes(length) {
      while (buffered.length < length) {
        if (!await fill(length + 1)) throw new Error('truncated');
      }
      const value = buffered.subarray(0, length);
      buffered = buffered.subarray(length);
      return value;
    },
    async eof() {
      if (buffered.length) throw new Error('surplus');
      while (!done) {
        const next = await input.next();
        done = !!next.done;
        if (!done && next.value.length) throw new Error('surplus');
      }
    },
    pending: () => buffered.length,
  };
}

// Dedicated controller path: no generic command writes stdin until the fixed KT2
// helper supplied a fresh nonce. `spawnChild` is test-only injection of a local process.
async function actionExchange(node, helperSha256, body, { spawnChild = spawn, timeoutMs = 40000, command } = {}) {
  const refused = ledger.refusal(node);
  if (refused) return { code: 9, out: '', err: refused };
  if (node.local || !Buffer.isBuffer(body) || !body.length || body.length > 65536 || !lowerHex(helperSha256, 64)) {
    return { code: 9, out: '', err: 'KT2 action request is invalid; operation refused' };
  }
  const remote = `exec "$HOME/.katala-tune/bin/tune.exe" windows-action-receiver-v2 --helper-sha256 ${helperSha256} --budget-ms 25000`;
  let child;
  try {
    child = command
      ? spawnChild(command.cmd, command.args, { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
      : spawnChild('ssh', [...SSH_OPTS, node.alias, remote], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  } catch (error) {
    return { code: 9, out: '', err: `KT2 spawn failed: ${String(error.message || error)}` };
  }
  const now = monotonicNow;
  const end = now() + Math.min(timeoutMs, 40000);
  const readyEnd = Math.min(end, now() + 15000);
  let stderr = Buffer.alloc(0);
  let stderrOverflow = false;
  let spawnError = null;
  child.stderr.on('data', (chunk) => {
    const room = 65536 - stderr.length;
    stderr = Buffer.concat([stderr, chunk.subarray(0, Math.max(0, room))]);
    if (chunk.length > room) stderrOverflow = true;
  });
  child.stdin.on('error', () => {});
  child.stdout.on('error', () => {});
  child.stderr.on('error', () => {});
  child.on('error', (error) => { spawnError = error; });
  const closed = new Promise((resolve) => {
    child.once('close', (code) => resolve(code));
    child.once('error', () => resolve(null));
  });
  let committed = false;
  const fail = async (message) => {
    child.kill('SIGKILL');
    child.stdin.destroy();
    child.stdout.destroy();
    child.stderr.destroy();
    try { await deadline(() => closed, end, now); } catch { /* process is discarded by the caller on close failure */ }
    return { code: committed ? 7 : 9, out: '', err: message };
  };
  try {
    const reader = byteReader(child.stdout);
    const ready = await deadline(() => reader.line(256), readyEnd, now);
    const nonce = ready.startsWith('KT2 READY ') ? ready.slice('KT2 READY '.length) : '';
    if (!lowerHex(nonce, 64) || reader.pending() || stderrOverflow || spawnError) return fail('KT2 READY invalid; operation refused');
    const hash = crypto.createHash('sha256').update(body).digest('hex');
    const request = Buffer.concat([Buffer.from(`KT2 REQUEST ${nonce} ${body.length} ${hash}\n`, 'ascii'), body]);
    await deadline(() => {
      committed = true;
      return new Promise((resolve, reject) => child.stdin.end(request, (error) => error ? reject(error) : resolve()));
    }, end, now);
    const fields = (await deadline(() => reader.line(256, 131073), end, now)).split(' ');
    const code = fields.length === 7 && fields[0] === 'KT2' && fields[1] === 'RESULT' && fields[2] === nonce && fields[3] === hash
      && /^(?:0|3|4|5|6|7|8|9)$/.test(fields[4]) ? Number(fields[4]) : null;
    const outLength = canonicalSize(fields[5] || '');
    const errLength = canonicalSize(fields[6] || '');
    if (code === null || outLength === null || errLength === null || outLength > 65536 || errLength > 65536) return fail('KT2 result invalid');
    const payload = await deadline(() => reader.bytes(outLength + errLength), end, now);
    await deadline(() => reader.eof(), end, now);
    const exitCode = await deadline(() => closed, end, now);
    if (exitCode !== 0 || stderrOverflow || spawnError) return { code: 7, out: '', err: 'KT2 ssh closure unverified' };
    return { code, out: decode(payload.subarray(0, outLength)), err: decode(payload.subarray(outLength)) };
  } catch {
    return fail(committed ? 'KT2 result outcome unverified' : 'KT2 READY unavailable; operation refused');
  }
}

module.exports = { run, probeNode, classifyFailure, REASON_TEXT, probeAll, lastJsonLine, execOn, actionExchange, actionHelper, probeExchange, localShell, SSH_OPTS, windowsSshCommand, macProbeArgs, macSshCommand };
