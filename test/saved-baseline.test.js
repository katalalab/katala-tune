'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { DatabaseSync, constants } = require('node:sqlite');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const retention = require('../lib/metrics-retention');
const { readBaseline } = require('../lib/saved-baseline');
const H = 3600000, D = 24 * H, M = 60000;
const END = Date.UTC(2026, 9, 26, 12), NOW = END + H / 2;
function fixture(file = ':memory:') {
  const db = new DatabaseSync(file);
  db.exec("PRAGMA journal_mode=WAL; CREATE TABLE metrics_minute(node_id TEXT,minute INTEGER,n INTEGER,v TEXT,src TEXT,PRIMARY KEY(node_id,minute)) WITHOUT ROWID;");
  retention.initialize(db);
  return db;
}
function state(db, seal, last = NOW) { db.prepare('UPDATE metrics_retention_state SET sealed_before=?,last_now=?').run(seal, last); }
function hour(db, at, sum, count, max = sum / count, node = 'fixture', resolution = 'hour') {
  db.prepare('INSERT INTO metrics_history VALUES (?,?,?,?)').run(node, resolution, at, JSON.stringify({mem:[sum,count,max]}));
}
function minute(db, at, value, n = 60, node = 'fixture') {
  db.prepare('INSERT OR REPLACE INTO metrics_minute VALUES (?,?,?,?,NULL)').run(node, at, n, JSON.stringify({mem:[value,value]}));
}
function profile(db) {
  const seal = END - 14 * D - 4 * H + 30 * M;
  state(db, seal);
  const values = [0,10,20,22,26,24,28,30,40,50,60,70];
  let i = 0;
  for (const weeks of [3,2,1]) for (let slot = 0; slot < 4; slot++) {
    const at = END - weeks * 7 * D - 4 * H + slot * H;
    const value = values[i++];
    if (at < seal - 30 * M) hour(db, at, value * 30, 30);
    else if (at === seal - 30 * M) {
      hour(db, at, 300, 30, 10);
      for (let m = 30; m < 50; m++) minute(db, at + m * M, 50, m % 2 ? 1 : 9999);
      minute(db, at, 100); // Late replacement is frozen below seal.
    } else for (let m = 0; m < 30; m++) minute(db, at + m * M, value);
  }
  return seal;
}
function target(report) { return report.reference.profiles.find(p => p.weekday === 1 && p.slot === 2); }

test('saved partial hours combine sums and minute counts, excluding frozen late raw', () => {
  const db = fixture();
  const seal = profile(db);
  const r = readBaseline(db, 'fixture', 'mem', NOW);
  assert.equal(target(r)?.median, 27); // 300+20*50 over30+20 =>26, not30 or sample-weighted.
  assert.equal(target(r).hours, 12);
  assert.equal(r.source.sealed_before, seal);
  assert.equal(r.source.raw_rows, 230);
  assert.equal(r.coverage.accepted_hours, 12);
  assert.deepEqual(r.observations, []);
  db.close();
});

test('a saved empty database exposes insufficient evidence without writes', () => {
  const db = fixture();
  db.exec('PRAGMA query_only=ON');
  const before = db.prepare('SELECT total_changes() AS n').get().n;
  const r = readBaseline(db, 'fixture', 'mem', NOW);
  assert.equal(r.state, 'insufficient');
  assert.equal(r.source.state, 'ready');
  assert.equal(r.source.raw_rows, 0);
  assert.equal(db.prepare('SELECT total_changes() AS n').get().n, before);
  db.close();
});

test('invalid raw values stay missing while real zero and other-node filtering remain valid', () => {
  const db = fixture();
  for (let m = 0; m < 30; m++) minute(db, END - H + m * M, 0);
  minute(db, END - H + 30 * M, null);
  minute(db, END - H + 31 * M, false);
  minute(db, END - H + 32 * M, '0');
  minute(db, END - H + 33 * M, 9, 0);
  minute(db, END - H + 34 * M + 1, 9);
  db.prepare('INSERT INTO metrics_minute VALUES (?,?,?,?,NULL)').run('fixture', END - H + 35 * M, 9007199254740992n, '{"mem":[9,9]}');
  db.prepare('INSERT INTO metrics_minute VALUES (?,?,?,?,NULL)').run('fixture', END - H + 36 * M, 60, Buffer.from('{"mem":[9,9]}'));
  minute(db, END - H, 90, 60, 'other');
  minute(db, END, 100); // incomplete current hour is excluded.
  const r = readBaseline(db, 'fixture', 'mem', NOW);
  assert.equal(r.coverage.accepted_hours, 1);
  assert.equal(r.source.raw_rows, 37);
  assert.equal(r.source.omitted_raw_rows, 7);
  const missing = readBaseline(db, 'fixture', 'cpu', NOW);
  assert.equal(missing.coverage.accepted_hours, 0);
  assert.deepEqual(missing.observations, []);
  assert.ok(!JSON.stringify(r).includes('other'));
  db.close();
});

test('corrupt archive, impossible seal coverage and rollback withhold the entire assessment', () => {
  for (const v of [[300,0,10],[300,61,10],[300,30,5],[300,30,true],[300,31,10]]) {
    const db = fixture();
    state(db, END - H + 30 * M);
    db.prepare('INSERT INTO metrics_history VALUES (?,?,?,?)').run('fixture','hour',END-H,JSON.stringify({mem:v}));
    const r = readBaseline(db, 'fixture', 'mem', NOW);
    assert.deepEqual(r.reasons, ['source_invalid']);
    assert.deepEqual(r.observations, []);
    assert.equal(r.coverage.history_hours, null);
    db.close();
  }
  const db = fixture();
  state(db, 0, NOW + 1);
  assert.deepEqual(readBaseline(db, 'fixture', 'mem', NOW).reasons, ['clock_rollback']);
  state(db, 0);
  db.exec('DELETE FROM metrics_retention_state');
  assert.deepEqual(readBaseline(db, 'fixture', 'mem', NOW).reasons, ['storage_unavailable']);
  db.close();
});

test('source limits withhold results rather than silently trimming bad timestamps', () => {
  for (const type of ['raw','hour']) {
    const db = fixture();
    db.exec('BEGIN');
    if (type === 'raw') {
      const put = db.prepare('INSERT INTO metrics_minute VALUES (?,?,60,?,NULL)');
      for (let i=0; i<40321; i++) put.run('fixture', END - D + i, '{"mem":[0,0]}');
    } else {
      state(db, END - D);
      const put = db.prepare('INSERT INTO metrics_history VALUES (?,\'hour\',?,?)');
      for (let i=0; i<673; i++) put.run('fixture', END - 2*D+i, '{"mem":[0,30,0]}');
    }
    db.exec('COMMIT');
    const r = readBaseline(db, 'fixture', 'mem', NOW);
    assert.deepEqual(r.reasons, ['source_limit']);
    assert.deepEqual(r.observations, []);
    assert.equal(r.coverage.recent_hours, null);
    db.close();
  }
});

test('a WAL compaction between SELECTs cannot mix old seal with new history/raw', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'baseline-wal-'));
  const file = path.join(dir, 'fixture.db');
  const db = fixture(file);
  const writer = new DatabaseSync(file);
  try {
    const at = END - 15 * D;
    for (let m=0; m<30; m++) minute(db, at + m*M, 20);
    let compacted = false;
    db.setAuthorizer((action, table) => {
      if (action === constants.SQLITE_READ && table === 'metrics_history' && !compacted) {
        compacted = true;
        retention.compact(writer, END - 14 * D, NOW);
      }
      return constants.SQLITE_OK;
    });
    const old = readBaseline(db, 'fixture', 'mem', NOW);
    assert.equal(compacted, true);
    assert.equal(old.source.sealed_before, 0);
    assert.equal(old.source.history_rows, 0);
    assert.equal(old.source.raw_rows, 30);
    assert.equal(old.coverage.accepted_hours, 1);
    db.setAuthorizer(null);
    const next = readBaseline(db, 'fixture', 'mem', NOW);
    assert.equal(next.source.sealed_before, END - 14*D);
    assert.equal(next.source.history_rows, 1);
    assert.equal(next.source.raw_rows, 0);
    assert.equal(next.coverage.accepted_hours, 1);
  } finally { db.close(); writer.close(); fs.rmSync(dir, {recursive:true}); }
});

test('old read-only and nested transaction connections stay unchanged; SQL errors propagate', () => {
  const old = new DatabaseSync(':memory:');
  old.exec('PRAGMA query_only=ON');
  assert.deepEqual(readBaseline(old,'fixture','mem',NOW).reasons,['storage_unavailable']);
  assert.equal(old.prepare('SELECT count(*) AS n FROM sqlite_schema').get().n,0);
  old.close();
  const db = fixture();
  db.exec('BEGIN');
  minute(db, END-H, 20);
  readBaseline(db,'fixture','mem',NOW);
  db.exec('ROLLBACK');
  assert.equal(db.prepare('SELECT count(*) AS n FROM metrics_minute').get().n,0);
  db.exec('DROP TABLE metrics_minute; CREATE TABLE metrics_minute(unrelated TEXT)');
  assert.throws(()=>readBaseline(db,'fixture','mem',NOW));
  db.close();
});

test('registered saved Store entry reads a reopened synthetic database without mutating it', () => {
  const {openDb} = require('../lib/db');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(),'baseline-store-'));
  const file = path.join(dir,'katala-tune.db');
  const seed = fixture(file);
  profile(seed);
  seed.close();
  const store = openDb(dir); // Synthetic only: normal openDb has schema/migration writes.
  store.db.close();
  const ro = new DatabaseSync(file,{readOnly:true});
  try {
    assert.equal(target(readBaseline(ro,'fixture','mem',NOW)).median,27);
  } finally {ro.close();}
  const reopened = openDb(dir);
  try {
    const before = reopened.db.prepare('SELECT total_changes() AS n').get().n;
    assert.equal(target(reopened.metricsBaseline('fixture','mem',NOW)).median,27);
    assert.equal(reopened.db.prepare('SELECT total_changes() AS n').get().n,before);
    for (const bad of ['', 'x'.repeat(257), '\ud800']) assert.throws(()=>reopened.metricsBaseline(bad,'mem',NOW));
  } finally {reopened.db.close();fs.rmSync(dir,{recursive:true});}
});
