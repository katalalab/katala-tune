#!/usr/bin/env node
// 道具の棚卸し・Do-gu の JS 版（lib/inventory.js・lib/dogu.js・lib/db.js の saveInventory など）を仕様として、
// Rust 側のテスト（tests/parity_inventory.rs）から渡された入力を評価して返す。
// 使い方: node eval_inventory.js <cases.json>  → 標準出力に結果の JSON。例外は { __throw: メッセージ }
'use strict';
// localeCompare は OS の言語で並びが変わる（ja だと漢字や長音記号の位置が違う）。
// Rust 側は ICU の root（= en-US）の並びに合わせているので、ここでも en-US に固定して比べる
const COLL = new Intl.Collator('en-US');
// eslint-disable-next-line no-extend-native
String.prototype.localeCompare = function localeCompare(other) { return COLL.compare(String(this), String(other)); };

const fs = require('node:fs');
const path = require('node:path');

const LIB = path.join(__dirname, '..', '..', '..', '..', 'lib');
const inventory = require(path.join(LIB, 'inventory'));
const dogu = require(path.join(LIB, 'dogu'));
const { openDb } = require(path.join(LIB, 'db'));

const cases = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const safe = (fn) => { try { return fn(); } catch (e) { return { __throw: String((e && e.message) || e) }; } };
const plain = (v) => JSON.parse(JSON.stringify(v === undefined ? null : v));

const out = {};
out.normalize = (cases.normalize || []).map((items) => safe(() => plain(inventory.normalizeItems(items))));
out.normName = (cases.normName || []).map((s) => safe(() => inventory.normName(s)));
out.sort = (cases.sort || []).map((xs) => safe(() => xs.slice().sort((a, b) => a.localeCompare(b))));
out.matrix = (cases.matrix || []).map((c) => safe(() => {
  const matchSlug = c.tools ? dogu.makeMatcher(c.tools) : undefined;
  const m = inventory.matrix(c.rows, { matchSlug, explicitOnly: c.explicitOnly });
  const draft = c.tools ? safe(() => dogu.deckDraft(m, c.tools, c.exclude)) : null;
  return plain({ groups: m, draft });
}));
out.index = (cases.index || []).map((tools) => safe(() => [...dogu.buildIndex(tools)].sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))));
out.match = (cases.match || []).map((c) => safe(() => { const m = dogu.makeMatcher(c.tools); return c.items.map((it) => m(it)); }));
out.plan = (cases.plan || []).map((c) => safe(() => {
  const a = dogu.planPublish(c.draft, c.slugs);
  const b = dogu.planPublish(c.draft2, c.slugs);
  return plain({ a, b, same: dogu.samePlan(a, b), payload: a.pick ? dogu.deckPayload([...a.pick, ...a.pick]) : null });
}));
out.apiKey = (cases.apiKey || []).map((c) => safe(() => dogu.apiKey(c.env, c.home, c.platform)));

// DB: 同じ手順で書き、読み出す（Rust が書いた DB を JS で読む／JS が書いた DB を Rust で読む）
function dump(db, spec) {
  const o = {
    all: db.inventory(),
    removed: db.inventory({ includeRemoved: true }),
    events: db.inventoryEvents(1000),
    nodes: db.db.prepare('SELECT * FROM inventory_nodes ORDER BY node_id').all(),
  };
  for (const id of spec.nodes) o[`node:${id}`] = db.inventory({ node_id: id });
  return plain(o);
}
if (cases.dbRead) out.dbRead = dump(openDb(cases.dbRead.dir), cases.dbRead.spec);
if (cases.dbWrite) {
  const db = openDb(cases.dbWrite.dir);
  const results = cases.dbWrite.ops.map((op) => safe(() => plain(db.saveInventory(op.node_id, inventory.normalizeItems(op.items), op.now, { skipSources: op.skip }))));
  out.dbWrite = { results, dump: dump(db, cases.dbWrite.spec) };
}
process.stdout.write(JSON.stringify(out, (_k, v) => (typeof v === 'string' ? v.toWellFormed() : v)));
