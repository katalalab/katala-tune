#!/usr/bin/env node
// 端末から道具を棚卸しする: node scripts/inventory.js [--dogu] [node_id ...]
// 保存はしない（アプリの DB には書かない）。--dogu で Do-gu の共通マスターを取り、照合の結果も出す
'use strict';
const { loadConfig } = require('../lib/nodes');
const { inventoryNode, matrix } = require('../lib/inventory');
const dogu = require('../lib/dogu');

(async () => {
  const args = process.argv.slice(2);
  const want = args.filter((a) => !a.startsWith('--'));
  const { nodes } = loadConfig();
  const targets = want.length ? nodes.filter((n) => want.includes(n.id)) : nodes;
  const results = await Promise.all(targets.map((n) => inventoryNode(n)));
  const rows = [];
  for (const r of results) {
    if (!r.ok) { process.stderr.write(`${r.node_id}: NG ${r.error.split('\n').pop()}\n`); continue; }
    const by = {};
    for (const it of r.items) { by[it.source] = (by[it.source] || 0) + 1; rows.push({ ...it, node_id: r.node_id }); }
    process.stderr.write(`${r.node_id}: ${r.items.length} 件 ${r.wall_s.toFixed(1)}s ${JSON.stringify(by)}${r.errors.length ? ' errors=' + JSON.stringify(r.errors) : ''}\n`);
  }
  let matchSlug = null, tools = null;
  if (args.includes('--dogu')) {
    const mem = new Map();
    tools = (await dogu.tools({ getMeta: (k) => mem.get(k), setMeta: (k, v) => mem.set(k, v) })).tools;
    matchSlug = dogu.makeMatcher(tools);
  }
  const m = matrix(rows, { matchSlug });
  const out = { tools: m.length, drift: m.filter((g) => g.drift).map((g) => ({ name: g.name, nodes: g.nodes })) };
  if (tools) out.dogu = { master: tools.length, matched: m.filter((g) => g.slug).length, draft: dogu.deckDraft(m, tools).map((d) => d.slug) };
  process.stdout.write(JSON.stringify(out) + '\n');
})();
