#!/usr/bin/env node
// JS 版（lib/*.js）を仕様として、Rust 側のテスト（tests/parity.rs）から渡された入力を評価して返す。
// 使い方: node eval.js <cases.json>  → 標準出力に結果の JSON
// 例外は { __throw: メッセージ } として返す（同じ入力で Rust 側も失敗するかを比べる）
'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const LIB = path.join(__dirname, '..', '..', '..', '..', 'lib');
const rules = require(path.join(LIB, 'rules'));
const health = require(path.join(LIB, 'health'));
const logs = require(path.join(LIB, 'logs'));
const actions = require(path.join(LIB, 'actions'));
const { openDb } = require(path.join(LIB, 'db'));

const cases = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const safe = (fn) => { try { return fn(); } catch (e) { return { __throw: String(e && e.message || e) }; } };
// undefined を JSON に載せない（Rust 側の「キーが無い」と揃える）
const plain = (v) => JSON.parse(JSON.stringify(v === undefined ? null : v));

// DB の中身を読み出す（どちらのアプリが書いた DB でも同じ形で比べるため）
function dump(db, spec) {
  const out = {};
  for (const id of spec.nodes) {
    out[`last:${id}`] = db.lastSnapshots(id, 3);
    out[`history:${id}`] = db.history(id, 60);
    out[`dropped:${id}`] = db.droppedTotal(id);
  }
  out.actions = db.actions(100);
  for (const [i, f] of spec.queries.entries()) out[`query:${i}`] = safe(() => db.queryLogs(f));
  for (const [i, f] of spec.signatures.entries()) out[`sig:${i}`] = db.signatures(f);
  out.cursors = db.cursors();
  out.checks = db.checks();
  out.events = db.checkEvents(200).map(({ id, ...r }) => r);
  for (const k of spec.meta) out[`meta:${k}`] = db.getMeta(k);
  return out;
}

// DB に書く（Rust 側の write と同じ手順）
function write(db, w) {
  for (const s of w.snapshots) db.addSnapshot(s.node_id, s.entry);
  for (const a of w.actions) db.addAction(a);
  for (const l of w.logs) db.insertLogs(l.node_id, l.source, logs.normalize(l.source, l.rows), l.now);
  for (const c of w.cursors) {
    if (c.error) db.cursorError(c.node_id, c.source, c.error);
    else db.cursorOk(c.node_id, c.source, c.cursor, c.count, c.dropped);
  }
  for (const c of w.checks) db.saveChecks(c.scope, c.checks, c.now);
  for (const [k, v] of Object.entries(w.meta)) db.setMeta(k, v);
}

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'kt-parity-'));
const out = {};
out.analyze = (cases.analyze || []).map((c) => safe(() => plain(rules.analyze(c.snap, c.node))));
out.score = (cases.analyze || []).map((c) => safe(() => rules.score(rules.analyze(c.snap, c.node))));
out.compare = (cases.compare || []).map((c) => safe(() => plain(rules.compare(c.prev, c.cur))));
out.nodeChecks = (cases.nodeChecks || []).map((c) => safe(() => plain(health.nodeChecks(c.node, c.snap, c.findings, c.ctx))));
out.appChecks = (cases.appChecks || []).map((c) => safe(() => plain(health.appChecks(c))));
out.failingJobs = (cases.failingJobs || []).map((jobs) => safe(() => plain(health.failingJobs(jobs))));
out.redact = (cases.redact || []).map((s) => safe(() => logs.redact(s)));
out.fingerprint = (cases.fingerprint || []).map((c) => safe(() => logs.fingerprint(c.source, c.provider, c.event_id, c.message)));
out.normalize = (cases.normalize || []).map((c) => safe(() => plain(logs.normalize(c.source, c.rows))));
out.plan = (cases.plan || []).map((c) => safe(() => {
  const p = actions.plan(c.node, c.action, c.protect === undefined ? {} : { protect: c.protect });
  const w = safe(() => actions.wrap(c.node, p));
  return plain({ describe: p.describe, script: p.script, shell: p.shell, undo: p.undo ?? null, exits: !!p.exits, wrap: w });
}));
out.logFindings = (cases.logFindings || []).map((c) => safe(() => {
  const db = openDb(tmp());
  for (const l of c.logs) db.insertLogs(c.node_id, l.source, logs.normalize(l.source, l.rows), c.now);
  for (const d of c.dropped) db.cursorOk(c.node_id, d.source, '1', 0, d.n);
  return plain(logs.logFindings(db, c.node_id, c.now));
}));
// DB の相互運用: Rust が書いた DB を JS で読む／JS が書いた DB を Rust で読むための書き出しと読み出し
if (cases.dbRead) out.dbRead = plain(dump(openDb(cases.dbRead.dir), cases.dbRead.spec));
if (cases.dbWrite) {
  const db = openDb(cases.dbWrite.dir);
  write(db, cases.dbWrite.write);
  out.dbWrite = plain(dump(db, cases.dbWrite.spec));
}
// 途中で切れたサロゲート対は U+FFFD にする（UTF-8 にしたとき＝SQLite・ハッシュ・画面と同じ。Rust の String と比べるため）
process.stdout.write(JSON.stringify(out, (_k, v) => (typeof v === "string" ? v.toWellFormed() : v)));
