// 各機体のログを前回の続きから取り込む。読み取り専用。
// 取り込みは (node, source, uid) で一意なので、同じ範囲を読み直しても重複しない（失敗したら次回やり直せばよい）。
'use strict';
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { run, lastJsonLine, SSH_OPTS, localShell } = require('./collect');

const PROBES = path.join(__dirname, '..', 'probes');
const MAX_MESSAGE = 1000;

// 秘密らしいものを伏せる。ログは機体の外へ出る前提で扱う
const REDACT = [
  [/\b(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})/g, '<github-token>'],
  [/\bsk-[A-Za-z0-9_-]{16,}/g, '<api-key>'],
  [/\b(AKIA|ASIA)[0-9A-Z]{16}\b/g, '<aws-key>'],
  [/\bops_[A-Za-z0-9_-]{20,}/g, '<op-token>'],
  [/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/g, '<jwt>'],
  [/(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{12,}/gi, '$1 <redacted>'],
  [/\b(password|passwd|pwd|token|secret|api[_-]?key|access[_-]?key)(\s*[=:]\s*)("[^"]*"|'[^']*'|\S+)/gi, '$1$2<redacted>'],
];

function redact(s) {
  let out = String(s ?? '');
  for (const [re, rep] of REDACT) out = out.replace(re, rep);
  return out.length > MAX_MESSAGE ? out.slice(0, MAX_MESSAGE) + '…' : out;
}

// 同種のログをまとめる鍵。GUID・16進・数字・パス・引用符の中身を伏せてからハッシュする
function fingerprint(source, provider, eventId, message) {
  const norm = String(message)
    .toLowerCase()
    .replace(/\{?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\}?/g, '<guid>')
    .replace(/0x[0-9a-f]+/g, '<hex>')
    .replace(/[a-z]:\\[^\s"']+|\/(?:users|home|private|var|tmp|applications|library|system)\/[^\s"']*/g, '<path>')
    .replace(/"[^"]*"|'[^']*'/g, '<q>')
    .replace(/\d+(\.\d+)?/g, '<n>')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 400);
  return crypto.createHash('sha1').update(`${source}|${provider ?? ''}|${eventId ?? ''}|${norm}`).digest('hex').slice(0, 16);
}

function normalize(source, rows) {
  return (rows || []).filter((r) => r && r.uid != null && r.ts).map((r) => {
    const message = redact(r.message || '');
    return {
      uid: String(r.uid).slice(0, 200), ts: Number(r.ts), level: ['critical', 'error', 'warn', 'info'].includes(r.level) ? r.level : 'info',
      provider: r.provider ? String(r.provider).slice(0, 120) : null, event_id: r.event_id != null ? String(r.event_id).slice(0, 60) : null,
      message, fingerprint: fingerprint(source, r.provider, r.event_id, message),
    };
  });
}

const SOURCES = { macos: ['mac_diag', 'mac_kernel'], windows: ['win_system', 'win_application', 'neonmonitor'] };

function cursorArg(value) {
  const parsed = Number.parseInt(value ?? '0', 10);
  return Number.isSafeInteger(parsed) ? String(parsed) : '0';
}

function windowsRemoteTransport(cursors, script) {
  const c = (source) => cursorArg(cursors[source]);
  const params = `-SysCursor ${c('win_system')} -AppCursor ${c('win_application')} -NeonCursor ${c('neonmonitor')}`;
  const payload = script.subarray(script.subarray(0, 3).equals(Buffer.from([0xef, 0xbb, 0xbf])) ? 3 : 0).toString('base64');
  const bootstrap = `[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);$s=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String([Console]::In.ReadToEnd()));& ([ScriptBlock]::Create($s)) ${params}`;
  const command = `printf %s ${payload} | powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(bootstrap, 'utf16le').toString('base64')}`;
  if (Buffer.byteLength(command) >= 8191) throw new Error('Windows remote log command exceeds 8191 bytes');
  return {
    command,
    bootstrap,
  };
}

async function fetchLogs(node, cursors) {
  const c = (s) => cursorArg(cursors[s]);
  if (node.os === 'macos') {
    const script = fs.readFileSync(path.join(PROBES, 'mac_logs.py'), 'utf8');
    const args = [c('mac_diag'), c('mac_kernel')];
    return node.local
      ? run('/usr/bin/env', ['python3', '-', ...args], { input: script, timeoutMs: 60000 })
      : run('ssh', [...SSH_OPTS, node.alias, `command -v python3 >/dev/null && exec python3 - ${args.join(' ')} || exec /usr/bin/python3 - ${args.join(' ')}`], { input: script, timeoutMs: 60000 });
  }
  const script = fs.readFileSync(path.join(PROBES, 'win_logs.ps1'));
  const params = `-SysCursor ${c('win_system')} -AppCursor ${c('win_application')} -NeonCursor ${c('neonmonitor')}`;
  if (node.local) return localShell.powershellFile(script, params, 90000);
  const transport = windowsRemoteTransport(cursors, script);
  return run('ssh', [...SSH_OPTS, node.alias, transport.command], { timeoutMs: 90000 });
}

// 1台分を取り込む。戻り値は source ごとの { inserted, fetched, dropped, error }
async function syncNode(db, node) {
  const sources = SOURCES[node.os] || [];
  const cursors = Object.fromEntries(sources.map((s) => [s, db.cursor(node.id, s)?.cursor ?? null]));
  const res = await fetchLogs(node, cursors);
  const data = lastJsonLine(res.out);
  const summary = { node_id: node.id, sources: {}, meta: {} };
  if (!data?.sources) {
    const err = (res.err || res.out || `exit ${res.code}`).trim().slice(-400);
    for (const s of sources) db.cursorError(node.id, s, err);
    summary.error = err;
    return summary;
  }
  for (const s of sources) {
    const src = data.sources[s];
    if (!src || src.error) { db.cursorError(node.id, s, src?.error || 'no data'); summary.sources[s] = { error: src?.error || 'no data' }; continue; }
    const rows = normalize(s, Array.isArray(src.rows) ? src.rows : src.rows ? [src.rows] : []);
    const inserted = db.insertLogs(node.id, s, rows);
    db.cursorOk(node.id, s, src.cursor ?? null, rows.length, src.dropped || 0);
    summary.sources[s] = { fetched: rows.length, inserted, dropped: src.dropped || 0, note: src.note };
    if (src.meta) summary.meta[s] = src.meta;
  }
  return summary;
}

async function syncAll(db, nodes, onResult) {
  return Promise.all(nodes.map(async (n) => {
    const r = await syncNode(db, n).catch((e) => ({ node_id: n.id, error: String(e) }));
    onResult?.(r);
    return r;
  }));
}

// ログから所見を作る（直近7日）。rules.analyze の結果に足して使う
function logFindings(db, nodeId, now = Date.now()) {
  const rows = db.logCounts(nodeId, now - 7 * 86400e3);
  const sum = (pred) => rows.filter(pred).reduce((s, r) => s + r.n, 0);
  const out = [];
  const whea = sum((r) => r.provider === 'Microsoft-Windows-WHEA-Logger');
  if (whea) out.push({ id: 'log-whea', severity: whea >= 5 ? 'critical' : 'warn', category: 'stability', title: `ハードウェアエラー（WHEA）が7日で ${whea} 件`, detail: 'System ログの Microsoft-Windows-WHEA-Logger', advice: 'CPU・メモリ・PCIe の訂正／訂正不能エラー。メモリの XMP を切る、枚数を減らす、MemTest86 で切り分ける。', log_query: { node_id: nodeId, q: 'WHEA' } });
  const tdr = sum((r) => r.provider === 'Display' && r.event_id === '4101') + sum((r) => r.provider === 'nvlddmkm');
  if (tdr) out.push({ id: 'log-gpu-reset', severity: 'warn', category: 'stability', title: `GPU ドライバのリセット・エラーが7日で ${tdr} 件`, detail: 'Display 4101 / nvlddmkm', advice: 'GPU ドライバのタイムアウト。ドライバの入れ直し（DDU）、オーバークロックの解除、電源容量を確認する。', log_query: { node_id: nodeId, q: 'nvlddmkm OR Display' } });
  const lowmem = sum((r) => r.provider === 'Microsoft-Windows-Resource-Exhaustion-Detector');
  if (lowmem) out.push({ id: 'log-lowmem', severity: 'warn', category: 'memory', title: `メモリ枯渇の警告が7日で ${lowmem} 件`, detail: 'Resource-Exhaustion-Detector 2004', advice: 'コミットが上限に達しかけた。どのプロセスが使っていたかはメッセージに出ている。ページファイルの拡大か大口の整理。', log_query: { node_id: nodeId, q: 'Resource' } });
  const disk = sum((r) => /^(disk|Ntfs|stornvme|storahci|volmgr)$/.test(r.provider || '') && r.level !== 'info');
  if (disk) out.push({ id: 'log-disk', severity: 'warn', category: 'disk', title: `ディスク・ファイルシステムのエラーが7日で ${disk} 件`, detail: 'disk / Ntfs / stornvme', advice: 'SMART を確認し、バックアップを先に取る。ケーブルや M.2 の接触も疑う。', log_query: { node_id: nodeId, q: 'disk OR Ntfs OR stornvme' } });
  // 1000: Application Error、1002: Application Hang、1026: .NET Runtime の未処理例外
  const crashes = sum((r) => (r.source === 'win_application' && ['1000', '1002', '1026'].includes(r.event_id)) || (r.source === 'mac_diag' && ['crash', 'hang', 'spin'].includes(r.event_id)));
  if (crashes >= 3) out.push({ id: 'log-crashes', severity: crashes >= 15 ? 'warn' : 'info', category: 'stability', title: `アプリのクラッシュ・ハングが7日で ${crashes} 件`, detail: 'Application Error 1000 / Hang 1002、DiagnosticReports', advice: 'ログ画面の「同種ログ」で、どのアプリが繰り返しているかを見る。', log_query: { node_id: nodeId, level: 'error' } });
  const panics = sum((r) => r.event_id === 'kernel panic');
  if (panics) out.push({ id: 'log-panic', severity: 'critical', category: 'stability', title: `カーネルパニックが7日で ${panics} 件`, detail: 'DiagnosticReports', advice: '直前に入れた拡張・周辺機器・OS 更新を疑う。パニックログの panicString を確認する。', log_query: { node_id: nodeId, q: 'panic' } });
  const jetsam = sum((r) => r.event_id === 'jetsam (memory)');
  if (jetsam) out.push({ id: 'log-jetsam', severity: 'warn', category: 'memory', title: `メモリ不足でアプリが落とされた記録が7日で ${jetsam} 件`, detail: 'jetsam', advice: 'メモリの大口を減らす。', log_query: { node_id: nodeId, q: 'jetsam' } });
  const neonKill = sum((r) => r.source === 'neonmonitor' && r.level !== 'info');
  if (neonKill) out.push({ id: 'log-neon', severity: 'warn', category: 'memory', title: `NeonMonitor の自動保護が7日で ${neonKill} 回動いた`, detail: 'guard.log の警告・強制終了・失敗', advice: '逼迫が繰り返している。強制終了で落ちたアプリが無いか確認し、根本の大口を減らす。', log_query: { node_id: nodeId, source: 'neonmonitor' } });
  // サービスの起動失敗・異常終了（7000/7009/7023/7031/7034）
  const svc = sum((r) => r.provider === 'Service Control Manager' && ['7000', '7009', '7023', '7031', '7034'].includes(r.event_id));
  if (svc >= 3) out.push({ id: 'log-service', severity: 'warn', category: 'stability', title: `サービスの起動失敗・異常終了が7日で ${svc} 件`, detail: 'Service Control Manager 7000/7009/7023/7031/7034', advice: 'どのサービスかはログに出ている。セキュリティ製品（Defender など）なら、保護が止まっている可能性があるので先に直す。', log_query: { node_id: nodeId, q: '"Service Control Manager"' } });
  // 同じエラーの洪水（24時間で200件以上）。クラッシュと再起動の繰り返しや、ログの出し過ぎを見つける
  for (const t of db.topSignatures(nodeId, now - 86400e3, 2).filter((x) => x.n >= 200)) {
    out.push({ id: `log-flood-${t.fingerprint}`, severity: 'warn', category: 'background', title: `同じエラーが24時間で ${t.n} 件繰り返している（${t.provider || t.source}）`, detail: t.sample.replace(/\s+/g, ' ').slice(0, 160), advice: '起動と失敗を繰り返しているか、ログを出し過ぎている。どちらも CPU とディスクを使い続ける。出している側の設定を直すか止める。', log_query: { node_id: nodeId, q: t.provider ? `"${t.provider.replace(/"/g, '')}"` : '' } });
  }
  const dropped = db.droppedTotal(nodeId);
  if (dropped >= 1000) out.push({ id: 'log-dropped', severity: 'info', category: 'background', title: `ログが多すぎて ${dropped} 件を取り込めなかった`, detail: '1回の取り込みは1か所あたり300件まで', advice: '上の「同じエラーの繰り返し」を直すと収まる。', log_query: { node_id: nodeId } });
  return out;
}

module.exports = { syncNode, syncAll, logFindings, redact, fingerprint, normalize, SOURCES, windowsRemoteTransport };
