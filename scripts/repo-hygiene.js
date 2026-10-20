#!/usr/bin/env node
// 端末からリポジトリの衛生監査を行う（opt-in。アプリの分析・自動スキャンには入っていない）:
//   node scripts/repo-hygiene.js [--ghq-root <dir>] [--root <dir>]... [--max-repos N] [--budget 秒] [--repos] [node_id ...]
// 読み取り専用（git の読み取りだけ・fetch しない）で、保存もしない。標準出力は JSON（所見と機体×リポジトリの表）、
// --repos でリポジトリごとの行（パス・件数）も出す。ファイルの内容・差分・認証情報入りの URL は出さない。
// 台帳の "repo_hygiene": false の機体と、書いていない共用機（shared）は、機体を指定しても対象外。"repo_roots" で機体ごとに置き場を足せる。
'use strict';
const { loadConfig } = require('../lib/nodes');
const hygiene = require('../lib/hygiene');
const { partition } = require('../lib/ledger');

function parse(argv) {
  const o = { roots: [], ids: [], repos: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--ghq-root') o.ghq_root = argv[++i];
    else if (a === '--root') o.roots.push(argv[++i]);
    else if (a === '--max-repos') o.max_repos = Number(argv[++i]);
    else if (a === '--budget') o.budget_s = Number(argv[++i]);
    else if (a === '--repos') o.repos = true;
    else if (a.startsWith('--')) throw new Error(`知らない引数: ${a}`);
    else o.ids.push(a);
  }
  return o;
}

(async () => {
  const o = parse(process.argv.slice(2));
  const { nodes } = loadConfig();
  const selected = o.ids.length ? nodes.filter((n) => o.ids.includes(n.id)) : nodes;
  // 運用対象でない機体（retired など）には接続しない
  const { targets: want, excluded } = partition(selected);
  for (const x of excluded) process.stderr.write(`${x.node_id}: 対象外（${x.state}）${x.reason ? ` ${x.reason}` : ''}\n`);
  const targets = want.filter(hygiene.enabled);
  const skipped = [...excluded.map((x) => x.node_id), ...want.filter((n) => !targets.includes(n)).map((n) => n.id)];
  const results = await Promise.all(targets.map((n) => hygiene.auditNode(n, o)));
  const out = { nodes: [], skipped_nodes: skipped, matrix: [] };
  for (const r of results) {
    if (!r.ok) {
      process.stderr.write(`${r.node_id}: NG ${r.error}\n`);
      out.nodes.push({ node_id: r.node_id, ok: false, error: r.error });
      continue;
    }
    const findings = hygiene.analyze(r);
    const unobserved = r.repos.filter((x) => x.state !== 'ok').length + r.skipped;
    process.stderr.write(`${r.node_id}: 見つけた ${r.enumerated} 件・未観測 ${unobserved} 件・${r.wall_s.toFixed(1)}s・所見 ${findings.length}\n`);
    const { repos, ...rest } = r;
    out.nodes.push({ ...rest, observed: repos.length - repos.filter((x) => x.state !== 'ok').length, findings, ...(o.repos ? { repos } : {}) });
  }
  out.matrix = hygiene.matrix(results.filter((r) => r.ok));
  process.stdout.write(JSON.stringify(out) + '\n');
})().catch((e) => { process.stderr.write(`${e.message}\n`); process.exit(1); });
