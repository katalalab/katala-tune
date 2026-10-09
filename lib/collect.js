// 各機体で調査スクリプトを走らせ、snapshot を返す。読み取り専用。
// macOS: この機体ならローカル実行、それ以外は ssh で python3 の標準入力へ渡す。
// Windows: ssh（既定シェルは Git Bash）で ~/.katala-tune/ に置いて PowerShell 5.1 で実行。この機体が Windows ならローカルで実行。
'use strict';
const { spawn } = require('node:child_process');
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

function windowsSshCommand(benchmark = true, network = true) {
  const bench = BENCH_PY.replace(/'/g, '"');
  const parts = [
    'mkdir -p ~/.katala-tune && cat > ~/.katala-tune/probe.ps1 &&',
    `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(cygpath -w ~/.katala-tune/probe.ps1)"${network ? '' : ' -NoNetwork'};`,
  ];
  if (benchmark) parts.push(
    `echo ${BENCH_MARK};`,
    `PY=$(command -v python3 || command -v python); [ -n "$PY" ] && "$PY" -c '${bench}'`,
  );
  return parts.join(' ');
}

// 失敗の理由を、画面と判定が使える種類に分ける（crates/tune-core/src/collect.rs の classify_failure と同じ）。
// timeout: 接続はできたが、こちらの打ち切り（run が足す `timeout <ms>ms`）まで終わらなかった。
// unreachable: ssh が相手に届かない（名前が引けない・拒否・経路なし・接続の時間切れ）。auth: 鍵・ホスト鍵で入れない。error: それ以外
const AUTH_FAIL = /Permission denied|Host key verification failed|Authentication failed|Too many authentication failures/i;
const UNREACHABLE = /Could not resolve hostname|Name or service not known|Connection refused|No route to host|Network is unreachable|Connection timed out|Operation timed out|Host is down|Connection reset|Connection closed by|kex_exchange_identification|banner exchange/i;
const REASON_TEXT = { timeout: '時間切れ（接続後に応答が返らなかった）', unreachable: '接続できない（電源・ネットワーク・Host 名）', auth: '認証できない（鍵・ホスト鍵）', error: '調査の失敗' };
function classifyFailure(res) {
  const text = `${res.err || ''}\n${res.out || ''}`;
  if (res.code == null && /timeout \d+ms\s*$/.test(res.err || '')) return 'timeout';
  if (AUTH_FAIL.test(text)) return 'auth';
  if (UNREACHABLE.test(text)) return 'unreachable';
  return 'error';
}

// deps.run: 子プロセスの実行口（テストが偽の ssh に差し替える）
async function probeNode(node, deps = {}) {
  const runCmd = deps.run || run;
  const started = Date.now();
  const network = netsecEnabled(node);
  let res;
  const benchmark = node.benchmark !== false;
  if (node.os === 'macos') {
    const script = fs.readFileSync(path.join(PROBES, 'mac_probe.py'), 'utf8');
    res = node.local
      ? await runCmd('/usr/bin/env', macProbeArgs(benchmark, network), { input: script })
      : await runCmd('ssh', [...SSH_OPTS, node.alias, macSshCommand(benchmark, network)], { input: script });
  } else if (node.local) {
    const p = await localShell.powershellFile(fs.readFileSync(path.join(PROBES, 'win_probe.ps1')), network ? '' : '-NoNetwork');
    if (benchmark) {
      const b = await localShell.python(BENCH_PY);
      res = { ...p, out: `${p.out}\n${BENCH_MARK}\n${b.out}` };
    } else res = p;
  } else {
    const script = fs.readFileSync(path.join(PROBES, 'win_probe.ps1'));
    res = await runCmd('ssh', [...SSH_OPTS, node.alias, windowsSshCommand(benchmark, network)], { input: script, timeoutMs: 120000 });
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
  return Promise.all(nodes.map(async (n) => {
    const r = await probeNode(n, deps).catch((e) => ({ node_id: n.id, ok: false, error: String(e), at: Date.now() }));
    onResult?.(r);
    return r;
  }));
}

// 機体でコマンドを実行する（actions 用）。ローカルならその OS のシェル、リモートは ssh
function execOn(node, script, timeoutMs = 30000) {
  return node.local ? localShell.script(script, timeoutMs) : run('ssh', [...SSH_OPTS, node.alias, script], { timeoutMs });
}

module.exports = { run, probeNode, classifyFailure, REASON_TEXT, probeAll, lastJsonLine, execOn, localShell, SSH_OPTS, windowsSshCommand, macProbeArgs, macSshCommand };
