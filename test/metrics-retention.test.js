'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { aggregateMinutes, mergeStats } = require('../lib/metrics-retention');
const HOUR = 3600000;
const minute = (t, v, node_id = 'fixture') => ({ node_id, minute: t, n: 60, v });

test('hourly rollup counts valid minutes per metric and preserves real zero', () => {
  // Zero-fill or dividing by all rows would change mem from 50 to 25.
  const got = aggregateMinutes([
    minute(0, { cpu: [0, 0], mem: [50, 70], top_cpu: ['private-name', 100] }),
    minute(60000, { cpu: [4, 8] }),
    minute(HOUR, { cpu: [6, 9], rx: [0, 0] }),
  ]);
  assert.deepEqual(got, { rows: [
    { node_id: 'fixture', bucket: 0, v: { cpu: [4, 2, 8], mem: [50, 1, 70] } },
    { node_id: 'fixture', bucket: HOUR, v: { cpu: [6, 1, 9], rx: [0, 1, 0] } },
  ], omittedRows: 0 });
});

test('daily merge weights each metric by valid minutes rather than hours', () => {
  // Means 2 and 6 with 3 and 1 minutes must give 3, not 4.
  assert.deepEqual(mergeStats([{ cpu: [6, 3, 4] }, { cpu: [6, 1, 6], mem: [20, 1, 20] }]), {
    cpu: [12, 4, 6], mem: [20, 1, 20],
  });
});

test('invalid values and oversized identities never enter numeric history', () => {
  const got = aggregateMinutes([
    minute(0, { cpu: [null, 5], mem: [5, 4], swap: [-1, 0], tx: [NaN, 2], rx: [0, 0] }),
    minute(60000, { cpu: [Infinity, Infinity] }),
    minute(-1, { cpu: [1, 1] }),
    minute(120000, { cpu: [1, 1] }, 'x'.repeat(257)),
    minute(180000, { cpu: [1, 1] }, 'あ'.repeat(86)),
  ]);
  assert.deepEqual(got, { rows: [{ node_id: 'fixture', bucket: 0, v: { rx: [0, 1, 0] } }], omittedRows: 4 });
});

const { DatabaseSync } = require('node:sqlite');
const retention = require('../lib/metrics-retention');
const DAY = 24 * HOUR;
const dbFixture = (file = ':memory:') => {
  const db = new DatabaseSync(file);
  db.exec(`CREATE TABLE metrics_minute (node_id TEXT, minute INTEGER, n INTEGER, v TEXT, src TEXT, PRIMARY KEY(node_id,minute)) WITHOUT ROWID;
    CREATE INDEX metrics_minute_by_time ON metrics_minute(minute);`);
  return db;
};
const put = (db, t, cpu, node = 'fixture', extra = {}) => db.prepare('INSERT OR REPLACE INTO metrics_minute VALUES (?, ?, 60, ?, NULL)').run(node, t, JSON.stringify({ cpu: [cpu, cpu], ...extra }));
const init = (db) => retention.initialize(db);
const rows = (db, resolution = 'hour') => retention.query(db, 'fixture', 0, 700 * DAY, resolution);

test('SQLite archives corrected minutes once and discloses frozen replays', () => {
  const db = dbFixture();
  init(db);
  put(db, 10 * DAY, 0);
  put(db, 10 * DAY + 60000, 2);
  put(db, 10 * DAY + 60000, 4);
  const out = retention.compact(db, 16 * DAY, 30 * DAY);
  assert.equal(out.deleted, 2);
  assert.deepEqual(rows(db), [{ node_id: 'fixture', resolution: 'hour', bucket: 10 * DAY, v: { cpu: { mean: 2, max: 4, minutes: 2 } } }]);
  const first = JSON.stringify(rows(db));
  retention.compact(db, 16 * DAY, 30 * DAY);
  assert.equal(JSON.stringify(rows(db)), first);
  put(db, 10 * DAY, 999);
  retention.compact(db, 16 * DAY, 30 * DAY);
  assert.equal(JSON.stringify(rows(db)), first);
  assert.equal(retention.status(db).late_rows, 1);
  assert.equal(retention.status(db).sealed_before, 16 * DAY);
  retention.compact(db, 15 * DAY, 30 * DAY);
  assert.equal(retention.status(db).sealed_before, 16 * DAY);
  db.close();
});

test('daily compaction preserves unequal minute coverage and midnight cutoff contributions', () => {
  const db = dbFixture();
  init(db);
  for (const [t, cpu] of [[0, 0], [60000, 2], [120000, 4], [HOUR, 6]]) put(db, 10 * DAY + t, cpu);
  retention.compact(db, 16 * DAY, 30 * DAY);
  retention.compact(db, 100 * DAY, 130 * DAY);
  assert.deepEqual(rows(db, 'day'), [{ node_id: 'fixture', resolution: 'day', bucket: 10 * DAY, v: { cpu: { mean: 3, max: 6, minutes: 4 } } }]);
  assert.deepEqual(rows(db), []);
  db.close();

  const edge = dbFixture();
  init(edge);
  put(edge, 110 * DAY + 11 * HOUR, 2);
  put(edge, 110 * DAY + 12 * HOUR, 4);
  retention.compact(edge, 180 * DAY, 200 * DAY + 12 * HOUR + 34 * 60000);
  assert.equal(rows(edge, 'day')[0].v.cpu.mean, 2);
  assert.equal(rows(edge)[0].v.cpu.mean, 4);
  // The remaining hour must subsequently add to, rather than replace, that day.
  retention.compact(edge, 181 * DAY, 201 * DAY + 12 * HOUR);
  assert.deepEqual(rows(edge, 'day')[0].v.cpu, { mean: 3, max: 4, minutes: 2 });
  edge.close();
});

test('archive failure rolls back raw deletion, history, and watermark', () => {
  const db = dbFixture();
  init(db);
  put(db, 10 * DAY, 3);
  db.exec("CREATE TRIGGER fail_archive BEFORE INSERT ON metrics_history BEGIN SELECT RAISE(ABORT, 'fixture archive failure'); END");
  assert.throws(() => retention.compact(db, 16 * DAY, 30 * DAY), /fixture archive failure/);
  assert.equal(db.prepare('SELECT count(*) AS n FROM metrics_minute').get().n, 1);
  assert.equal(retention.status(db).sealed_before, 0);
  assert.deepEqual(rows(db), []);
  db.exec('DROP TRIGGER fail_archive');
  assert.equal(retention.compact(db, 16 * DAY, 30 * DAY).deleted, 1);
  db.close();
});

test('history enforces both row and UTF-8 byte budgets and reports capacity loss', () => {
  for (const budget of [{ maxRows: 2, maxBytes: 512 }, { maxRows: 20, maxBytes: 200 }]) {
    const db = dbFixture();
    init(db);
    for (let h = 0; h < 3; h++) put(db, 10 * DAY + h * HOUR, h + 1);
    retention.compact(db, 16 * DAY, 30 * DAY, budget);
    const s = retention.status(db);
    assert.ok(s.rows <= budget.maxRows);
    assert.ok(s.logical_bytes <= budget.maxBytes);
    assert.equal(s.capacity_rows, budget.maxBytes === 200 ? 2 : 1);
    assert.equal(rows(db).at(-1).v.cpu.mean, 3);
    db.close();
  }
});

test('TTL keeps boundary buckets, handles missing metrics, and protects recent raw minutes', () => {
  const db = dbFixture();
  init(db);
  put(db, 99 * DAY, 9);
  put(db, 100 * DAY, 3, 'fixture', { mem: [20, 30] });
  put(db, 490 * DAY, 5);
  retention.compact(db, 486 * DAY, 500 * DAY + 12 * HOUR);
  assert.deepEqual(rows(db, 'day').map(r => r.bucket), [100 * DAY]);
  assert.equal(retention.status(db).expired_rows, 1);
  assert.equal(db.prepare('SELECT count(*) AS n FROM metrics_minute').get().n, 1);
  assert.throws(() => retention.compact(db, 500 * DAY, 500 * DAY), /14/);
  assert.equal(db.prepare('SELECT count(*) AS n FROM metrics_minute').get().n, 1);
  assert.throws(() => retention.query(db, 'fixture', 0, 1, 'other'), /resolution/);
  db.close();
});

test('fractional monitor readings survive long aggregation without false corruption', () => {
  const input = Array.from({ length: 60 }, (_, i) => minute(i * 60000, { cpu: [0.1, 0.1] }));
  const hour = aggregateMinutes(input).rows[0].v;
  assert.equal(hour.cpu[1], 60);
  assert.ok(Math.abs(hour.cpu[0] / 60 - 0.1) < 1e-12);
  assert.equal(mergeStats([hour, hour]).cpu[1], 120);
});

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
test('old minute-only DB survives initialization and archive survives reopen', (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-retention-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const file = path.join(dir, 'fixture.sqlite');
  let db = new DatabaseSync(file);
  db.exec('CREATE TABLE metrics_minute(node_id TEXT, minute INTEGER, n INTEGER, v TEXT, src TEXT, PRIMARY KEY(node_id,minute)) WITHOUT ROWID');
  put(db, 10 * DAY, 7);
  db.close();
  db = new DatabaseSync(file);
  init(db);
  init(db);
  assert.equal(db.prepare('SELECT count(*) AS n FROM metrics_minute').get().n, 1);
  retention.compact(db, 16 * DAY, 30 * DAY);
  db.close();
  db = new DatabaseSync(file);
  init(db);
  assert.equal(rows(db)[0].v.cpu.mean, 7);
  assert.equal(retention.status(db).sealed_before, 16 * DAY);
  const changes = db.prepare('SELECT total_changes() AS n').get().n;
  rows(db);
  retention.status(db);
  assert.equal(db.prepare('SELECT total_changes() AS n').get().n, changes, 'history reads never run maintenance');
  db.close();
});

test('500 synthetic days stay bounded and retain documented periods without raw metadata', (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-retention-long-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const file = path.join(dir, 'fixture.sqlite');
  const db = dbFixture(file);
  db.exec('PRAGMA journal_mode=WAL');
  init(db);
  const ins = db.prepare('INSERT OR REPLACE INTO metrics_minute VALUES (?, ?, 60, ?, NULL)');
  for (let start = 100; start < 600; start += 30) {
    const end = Math.min(start + 30, 600);
    db.exec('BEGIN');
    for (let day = start; day < end; day++) {
      for (let hour = 0; hour < 24; hour++) {
        for (let node = 0; node < 6; node++) {
          ins.run(`fixture-${node}`, day * DAY + hour * HOUR, JSON.stringify({ cpu: [hour, hour + 1], mem: hour % 2 ? [50, 60] : null, top_cpu: ['private-raw-name', 1000] }));
        }
      }
    }
    db.exec('COMMIT');
    const now = end * DAY;
    retention.compact(db, now - 14 * DAY, now);
    const s = retention.status(db);
    assert.ok(s.rows <= 50000);
    assert.ok(s.logical_bytes <= 16777216);
  }
  const hour = retention.query(db, 'fixture-0', 0, 700 * DAY, 'hour');
  const day = retention.query(db, 'fixture-0', 0, 700 * DAY, 'day');
  assert.equal(hour.length, 76 * 24);
  assert.equal(hour[0].bucket, 510 * DAY);
  assert.equal(hour.at(-1).bucket, 586 * DAY - HOUR);
  assert.equal(day.length, 310);
  assert.equal(day[0].bucket, 200 * DAY);
  assert.equal(day.at(-1).bucket, 509 * DAY);
  assert.deepEqual(day.at(-1).v, { cpu: { mean: 11.5, max: 24, minutes: 24 }, mem: { mean: 50, max: 60, minutes: 12 } });
  const s = retention.status(db);
  assert.equal(s.capacity_rows, 0);
  assert.equal(s.rows, 12804);
  assert.ok(s.expired_rows > 0);
  assert.ok(!JSON.stringify(day).includes('private-raw-name'));
  assert.equal(db.prepare('SELECT min(minute) AS t FROM metrics_minute').get().t, 586 * DAY);
  const pages = db.prepare('PRAGMA page_count').get().page_count;
  const pageSize = db.prepare('PRAGMA page_size').get().page_size;
  const free = db.prepare('PRAGMA freelist_count').get().freelist_count;
  t.diagnostic(JSON.stringify({ ...s, allocated_bytes: pages * pageSize, free_pages: free, db_file_bytes: fs.statSync(file).size, wal_bytes: fs.existsSync(file + '-wal') ? fs.statSync(file + '-wal').size : 0 }));
  db.close();
});

test('default row and payload budgets bound a fleet-sized synthetic archive', () => {
  const allowed = ['cpu', 'mem', 'swap', 'commit', 'dr', 'dw', 'rx', 'tx', 'power_cpu_w', 'power_soc_w', 'power_platform_w', 'power_gpu_w', 'gpu', 'gmem', 'gtemp'];
  for (const wide of [false, true]) {
    const db = dbFixture();
    init(db);
    const ins = db.prepare('INSERT OR REPLACE INTO metrics_minute VALUES (?, ?, 60, ?, NULL)');
    const v = JSON.stringify(Object.fromEntries((wide ? allowed : ['cpu']).map(k => [k, [100000000001.25, 100000000002.5]])));
    db.exec('BEGIN');
    for (let day = 100; day < 190; day++) for (let hour = 0; hour < 24; hour++) for (let n = 0; n < 30; n++) {
      const id = `fixture-${n}`.padEnd(wide ? 256 : 10, 'x');
      ins.run(id, day * DAY + hour * HOUR, v);
    }
    db.exec('COMMIT');
    const s = retention.compact(db, 186 * DAY, 200 * DAY);
    assert.ok(s.rows <= 50000);
    assert.ok(s.logical_bytes <= 16777216);
    assert.ok(s.capacity_rows > 0);
    if (wide) assert.ok(s.rows < 50000, 'payload budget must evict more than row budget');
    else assert.equal(s.rows, 50000);
    const node = 'fixture-29'.padEnd(wide ? 256 : 10, 'x');
    assert.equal(retention.query(db, node, 0, 201 * DAY, 'hour').at(-1).bucket, 186 * DAY - HOUR);
    db.close();
  }
});

test('unsafe SQLite sample counts are omitted without blocking valid history', () => {
  const db = dbFixture();
  init(db);
  put(db, 10 * DAY, 4);
  db.prepare('INSERT INTO metrics_minute VALUES (?, ?, ?, ?, NULL)').run('fixture', 10 * DAY + 60000, 9007199254740992n, '{"cpu":[9,9]}');
  const s = retention.compact(db, 16 * DAY, 30 * DAY);
  assert.equal(s.deleted, 2);
  assert.equal(s.omitted_rows, 1);
  assert.deepEqual(rows(db)[0].v.cpu, { mean: 4, max: 4, minutes: 1 });
  db.close();
});
