#!/usr/bin/env node
// 公開前の点検: リポジトリに個人・環境の情報が混ざっていないかを調べる。
//   node scripts/oss-check.js            … HEAD のファイル
//   node scripts/oss-check.js --history  … HEAD から辿れる全コミットのファイルとコミットメッセージ
//   node scripts/oss-check.js --all-refs … 上を全部の枝・タグで（Dependabot などのマージしていない枝も含む）
// 機体台帳（~/.config/katala-tune/nodes.json）の id・alias・hostname と、この機体のユーザー名・ホスト名も探す。
// 探す語は台帳から実行時に読むので、リポジトリに個人の語を書かずに済む。見つかれば終了コード 1。
'use strict';
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

// 一般的な目印。例示用の名前（me・example・1文字など）は除く
const PATTERNS = [
  ['tailscale-ip', /\b100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d{1,3}\.\d{1,3}\b/g],
  ['tailnet-host', /\b[a-z0-9-]+\.[a-z0-9-]+\.ts\.net\b/gi],
  ['op-ref', /\bop:\/\/[^\s'"`)]+/g],
  ['home-path', /(?:\/Users\/|\/home\/|[A-Z]:\\\\?Users\\\\?)(?!(?:me|you|user|username|example|runner|Public|Default|[a-z])\b)[A-Za-z0-9._-]{2,}/g],
  // GitHub のボットのアドレス（noreply@github.com・Dependabot の Signed-off-by の support@github.com・*[bot]@users.noreply.github.com）は除く
  ['email', /\b(?!noreply@|support@github\.com\b)[A-Za-z0-9._%+-]+@(?!example\.(?:com|org|net)\b|users\.noreply\.github\.com\b)[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b/g],
];

// 一般的すぎて誤検出になる語は台帳由来でも探さない
const GENERIC = new Set(['mac', 'macos', 'windows', 'linux', 'local', 'localhost', 'example', 'server', 'desktop', 'laptop', 'admin', 'user', 'root', 'runner']);

function ledgerTerms(cfg, extra = []) {
  const raw = [...extra];
  for (const n of cfg?.nodes || []) raw.push(n.id, n.alias, n.local_hostname);
  if (cfg?.fleet) raw.push(cfg.fleet.repo, cfg.fleet.env_file);
  const terms = new Set();
  for (const v of raw) {
    if (typeof v !== 'string') continue;
    const t = v.trim().replace(/\.local$/i, '');
    if (t.length >= 4 && !GENERIC.has(t.toLowerCase())) terms.add(t);
  }
  return [...terms];
}

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

// skip: その本文では探さない規則（package-lock.json の作者メールは第三者の公開情報なので email を外す）
function scanText(text, terms = [], skip = []) {
  const found = [];
  const termRe = terms.length ? new RegExp(`(?<![A-Za-z0-9_-])(?:${terms.map(escapeRe).join('|')})(?![A-Za-z0-9_])`, 'gi') : null;
  text.split('\n').forEach((line, i) => {
    for (const [rule, re] of PATTERNS) if (!skip.includes(rule)) for (const m of line.matchAll(re)) found.push({ rule, line: i + 1, match: m[0] });
    if (termRe) for (const m of line.matchAll(termRe)) found.push({ rule: 'ledger', line: i + 1, match: m[0] });
  });
  return found;
}

const git = (args, opts = {}) => execFileSync('git', args, { encoding: 'utf8', maxBuffer: 256 << 20, ...opts });

// rev ごとのファイル一覧から、中身が同じものは1回だけ読む
function blobsOf(revs) {
  const blobs = new Map();
  for (const rev of revs) {
    for (const row of git(['ls-tree', '-r', rev]).split('\n').filter(Boolean)) {
      const [meta, file] = row.split('\t');
      const [, type, oid] = meta.split(' ');
      if (type === 'blob' && !blobs.has(oid)) blobs.set(oid, { file, rev });
    }
  }
  return blobs;
}

function readBlob(oid) {
  const buf = execFileSync('git', ['cat-file', 'blob', oid], { maxBuffer: 256 << 20 });
  return buf.includes(0) ? null : buf.toString('utf8');
}

function loadLedger() {
  const file = process.env.KATALA_TUNE_NODES || path.join(os.homedir(), '.config', 'katala-tune', 'nodes.json');
  try { return { file, cfg: JSON.parse(fs.readFileSync(file, 'utf8')) }; } catch { return { file, cfg: null }; }
}

// 履歴を見る範囲。--history は HEAD から辿れるコミット（いま出そうとしているもの）、--all-refs は全部の枝・タグ。どちらも無ければ null（HEAD のファイルだけ）
function revListArgs(argv) {
  if (argv.includes('--all-refs')) return ['rev-list', '--all'];
  if (argv.includes('--history')) return ['rev-list', 'HEAD'];
  return null;
}

function main(argv) {
  const range = revListArgs(argv);
  const history = !!range;
  const { file, cfg } = loadLedger();
  const terms = ledgerTerms(cfg, [os.userInfo().username, os.hostname()]);
  const revs = history ? git(range).split('\n').filter(Boolean) : ['HEAD'];
  const short = (rev) => (history ? `@${rev.slice(0, 7)}` : '');
  const findings = [];

  for (const [oid, { file: f, rev }] of blobsOf(revs)) {
    const text = readBlob(oid);
    const skip = path.basename(f) === 'package-lock.json' ? ['email'] : [];
    if (text) for (const x of scanText(text, terms, skip)) findings.push({ ...x, where: `${f}${short(rev)}:${x.line}` });
  }
  if (history) {
    for (const rev of revs) {
      for (const x of scanText(git(['log', '-1', '--format=%B', rev]), terms)) findings.push({ ...x, where: `commit ${rev.slice(0, 7)} message:${x.line}` });
    }
  }

  const warn = [];
  if (!cfg) warn.push(`台帳を読めないので台帳の語は探していない: ${file}`);
  const files = git(['ls-tree', '-r', '--name-only', 'HEAD']).split('\n');
  if (!files.some((f) => /^LICEN[CS]E/i.test(f))) warn.push('LICENSE が無い');
  const pkg = JSON.parse(git(['show', 'HEAD:package.json']));
  if (!pkg.license) warn.push('package.json に license が無い');

  const scope = !history ? 'HEAD' : `${range.includes('--all') ? '全部の枝の' : 'HEAD から辿れる'} ${revs.length} コミット`;
  console.log(`対象: ${scope}、台帳の語 ${terms.length} 個`);
  for (const w of warn) console.log(`注意: ${w}`);
  for (const x of findings) console.log(`NG ${x.rule.padEnd(12)} ${x.where}  ${x.match}`);
  console.log(findings.length ? `NG ${findings.length} 件` : 'OK 個人・環境の情報は見つからない');
  return findings.length ? 1 : 0;
}

if (require.main === module) process.exitCode = main(process.argv.slice(2));
module.exports = { ledgerTerms, scanText, revListArgs };
