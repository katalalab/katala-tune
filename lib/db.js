// ローカルの保存先。Node 内蔵の node:sqlite（依存ゼロ・ネイティブビルド不要）。
// 表の名前と列は katala-fleet の中央 DB 設計（docs/data-platform）に合わせ、後でそのまま送れる形にする。
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { DatabaseSync } = require('node:sqlite');

const SCHEMA = `
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS tune_snapshots (
  node_id TEXT NOT NULL, at INTEGER NOT NULL, wall_s REAL, score INTEGER,
  bench_ms REAL, cpu REAL, mem_avail REAL, swap_gb REAL, critical INTEGER, warn INTEGER,
  findings TEXT, data TEXT NOT NULL,
  PRIMARY KEY (node_id, at)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS tune_actions (
  id TEXT PRIMARY KEY, at INTEGER NOT NULL, node_id TEXT NOT NULL, type TEXT NOT NULL,
  params TEXT, label TEXT, ok INTEGER, output TEXT, undo TEXT, undo_of TEXT
);
CREATE INDEX IF NOT EXISTS tune_actions_at ON tune_actions (at DESC);
-- ログ: (node_id, source, uid) で一意。同じものを何度取り込んでも1行
CREATE TABLE IF NOT EXISTS logs (
  id INTEGER PRIMARY KEY, node_id TEXT NOT NULL, source TEXT NOT NULL, uid TEXT NOT NULL,
  ts INTEGER NOT NULL, level TEXT NOT NULL, provider TEXT, event_id TEXT, message TEXT NOT NULL,
  fingerprint TEXT NOT NULL, ingested_at INTEGER NOT NULL,
  UNIQUE (node_id, source, uid)
);
CREATE INDEX IF NOT EXISTS logs_ts ON logs (ts DESC);
CREATE INDEX IF NOT EXISTS logs_node_ts ON logs (node_id, ts DESC);
CREATE INDEX IF NOT EXISTS logs_fp ON logs (fingerprint, ts DESC);
CREATE VIRTUAL TABLE IF NOT EXISTS logs_fts USING fts5 (message, provider, content='logs', content_rowid='id');
-- 同種のログ（数字・ID・パスを伏せた形が同じもの）をまとめた件数
CREATE TABLE IF NOT EXISTS log_signatures (
  fingerprint TEXT PRIMARY KEY, source TEXT, provider TEXT, level TEXT, sample TEXT,
  first_seen INTEGER, last_seen INTEGER, total INTEGER NOT NULL DEFAULT 0
);
-- 機能チェックの現在値（scope = '_app' はアプリ自身）と、状態が変わったときだけの記録
CREATE TABLE IF NOT EXISTS checks (
  scope TEXT NOT NULL, id TEXT NOT NULL, name TEXT, status TEXT NOT NULL, detail TEXT, since INTEGER NOT NULL, checked_at INTEGER NOT NULL,
  PRIMARY KEY (scope, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS check_events (
  id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, scope TEXT NOT NULL, check_id TEXT NOT NULL, name TEXT, from_status TEXT, to_status TEXT NOT NULL, detail TEXT
);
CREATE INDEX IF NOT EXISTS check_events_ts ON check_events (ts DESC);
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT, updated_at INTEGER);
-- 道具の棚卸し: 機体ごとの今の一覧（消えたものは removed_at を入れて残す）と、増減・版の変化の記録
CREATE TABLE IF NOT EXISTS inventory (
  node_id TEXT NOT NULL, source TEXT NOT NULL, name TEXT NOT NULL, version TEXT, explicit INTEGER NOT NULL DEFAULT 1, extra TEXT,
  first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, removed_at INTEGER,
  PRIMARY KEY (node_id, source, name)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS inventory_events (
  id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, node_id TEXT NOT NULL, source TEXT NOT NULL, name TEXT NOT NULL,
  kind TEXT NOT NULL, from_version TEXT, to_version TEXT
);
CREATE INDEX IF NOT EXISTS inventory_events_ts ON inventory_events (ts DESC);
-- 機体ごとの棚卸しの記録（初回を済ませたか。0 件の機体でも初回を1回で終える）
CREATE TABLE IF NOT EXISTS inventory_nodes (node_id TEXT PRIMARY KEY, baseline_at INTEGER NOT NULL, last_ok_at INTEGER NOT NULL) WITHOUT ROWID;
-- 取り込みの続きの位置と健全性（どこまで読んだか、最後に成功・失敗したのはいつか）
CREATE TABLE IF NOT EXISTS log_cursors (
  node_id TEXT NOT NULL, source TEXT NOT NULL, cursor TEXT, updated_at INTEGER,
  last_ok_at INTEGER, last_error TEXT, last_count INTEGER, dropped_total INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (node_id, source)
) WITHOUT ROWID;
`;

const LOG_RETENTION_DAYS = 30;
const SNAPSHOT_KEEP = 200;

function openDb(dir) {
  fs.mkdirSync(dir, { recursive: true });
  const db = new DatabaseSync(path.join(dir, 'katala-tune.db'));
  db.exec(SCHEMA);
  const q = (sql) => db.prepare(sql);
  const tx = (fn) => { db.exec('BEGIN'); try { const r = fn(); db.exec('COMMIT'); return r; } catch (e) { db.exec('ROLLBACK'); throw e; } };

  const st = {
    addSnap: q(`INSERT OR REPLACE INTO tune_snapshots (node_id, at, wall_s, score, bench_ms, cpu, mem_avail, swap_gb, critical, warn, findings, data)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`),
    trimSnap: q(`DELETE FROM tune_snapshots WHERE node_id = ? AND at NOT IN (SELECT at FROM tune_snapshots WHERE node_id = ? ORDER BY at DESC LIMIT ${SNAPSHOT_KEEP})`),
    lastSnaps: q('SELECT node_id, at, wall_s, data FROM tune_snapshots WHERE node_id = ? ORDER BY at DESC LIMIT ?'),
    history: q('SELECT at, score, bench_ms, cpu, mem_avail, swap_gb, critical, warn FROM tune_snapshots WHERE node_id = ? ORDER BY at DESC LIMIT ?'),
    addAction: q('INSERT INTO tune_actions (id, at, node_id, type, params, label, ok, output, undo, undo_of) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)'),
    actions: q('SELECT * FROM tune_actions ORDER BY at DESC LIMIT ?'),
    insLog: q(`INSERT OR IGNORE INTO logs (node_id, source, uid, ts, level, provider, event_id, message, fingerprint, ingested_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`),
    insFts: q('INSERT INTO logs_fts (rowid, message, provider) VALUES (?, ?, ?)'),
    upSig: q(`INSERT INTO log_signatures (fingerprint, source, provider, level, sample, first_seen, last_seen, total) VALUES (?, ?, ?, ?, ?, ?, ?, 1)
              ON CONFLICT (fingerprint) DO UPDATE SET total = total + 1, last_seen = max(last_seen, excluded.last_seen), first_seen = min(first_seen, excluded.first_seen)`),
    getCursor: q('SELECT * FROM log_cursors WHERE node_id = ? AND source = ?'),
    cursors: q('SELECT * FROM log_cursors ORDER BY node_id, source'),
    okCursor: q(`INSERT INTO log_cursors (node_id, source, cursor, updated_at, last_ok_at, last_error, last_count, dropped_total) VALUES (?, ?, ?, ?, ?, NULL, ?, ?)
                 ON CONFLICT (node_id, source) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at, last_ok_at = excluded.last_ok_at,
                 last_error = NULL, last_count = excluded.last_count, dropped_total = log_cursors.dropped_total + excluded.dropped_total`),
    errCursor: q(`INSERT INTO log_cursors (node_id, source, updated_at, last_error) VALUES (?, ?, ?, ?)
                  ON CONFLICT (node_id, source) DO UPDATE SET updated_at = excluded.updated_at, last_error = excluded.last_error`),
  };

  // 移行: v0.2 までの Windows の取り込みは時刻が UTC との時差ぶんずれていた（PS 5.1 の [DateTime]'...Z' が現地時刻になる）。
  // uid は同じなので INSERT OR IGNORE では直らない。Windows 由来の行と位置を消して、次の取り込みで入れ直す
  if (!q("SELECT 1 FROM meta WHERE k = 'migr_win_ts_v1'").get()) {
    const WIN = "('win_system', 'win_application', 'neonmonitor')";
    tx(() => {
      db.exec(`INSERT INTO logs_fts (logs_fts, rowid, message, provider) SELECT 'delete', id, message, coalesce(provider, '') FROM logs WHERE source IN ${WIN}`);
      db.exec(`DELETE FROM logs WHERE source IN ${WIN}`);
      db.exec(`DELETE FROM log_signatures WHERE source IN ${WIN}`);
      db.exec(`DELETE FROM log_cursors WHERE source IN ${WIN}`);
      q("INSERT OR REPLACE INTO meta (k, v, updated_at) VALUES ('migr_win_ts_v1', 'true', ?)").run(Date.now());
    });
  }

  return {
    db,
    addSnapshot(node_id, e) {
      const s = e.summary;
      tx(() => {
        st.addSnap.run(node_id, e.at, e.wall_s ?? null, e.score ?? null, s.bench_ms, s.cpu, s.mem_avail, s.swap_gb, s.critical, s.warn, JSON.stringify(e.findings || []), JSON.stringify(e.data));
        st.trimSnap.run(node_id, node_id);
      });
    },
    lastSnapshots(node_id, n = 2) {
      return st.lastSnaps.all(node_id, n).map((r) => ({ node_id: r.node_id, at: r.at, wall_s: r.wall_s, data: JSON.parse(r.data) }));
    },
    history(node_id, n = 60) { return st.history.all(node_id, n).reverse(); },
    addAction(a) {
      st.addAction.run(a.id, a.at, a.node_id, a.type, JSON.stringify(a.params ?? null), a.label ?? null, a.ok ? 1 : 0, a.output ?? null, a.undo ? JSON.stringify(a.undo) : null, a.undo_of ?? null);
    },
    actions(n = 200) {
      return st.actions.all(n).map((r) => ({ ...r, ok: !!r.ok, params: JSON.parse(r.params), undo: r.undo ? JSON.parse(r.undo) : null }));
    },
    // 棚卸しの結果で機体の一覧を置き換える。初回は増減を記録しない（全部が「追加」になるため）。
    // skipSources（取り方が失敗した種類）は前回の一覧を残し、削除と見なさない
    saveInventory(node_id, items, now = Date.now(), { skipSources = [] } = {}) {
      const prev = new Map(q('SELECT * FROM inventory WHERE node_id = ?').all(node_id).map((r) => [`${r.source}\u0000${r.name}`, r]));
      const marker = q('SELECT * FROM inventory_nodes WHERE node_id = ?').get(node_id);
      // 印の無い古い DB で行だけあるときは、初回は済んでいるとみなす
      const baseline = !marker && prev.size === 0;
      const skip = new Set(skipSources);
      const ev = q('INSERT INTO inventory_events (ts, node_id, source, name, kind, from_version, to_version) VALUES (?, ?, ?, ?, ?, ?, ?)');
      const up = q(`INSERT INTO inventory (node_id, source, name, version, explicit, extra, first_seen, last_seen, removed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)
                    ON CONFLICT (node_id, source, name) DO UPDATE SET version = excluded.version, explicit = excluded.explicit, extra = excluded.extra,
                    last_seen = excluded.last_seen,
                    first_seen = CASE WHEN inventory.removed_at IS NULL THEN inventory.first_seen ELSE excluded.first_seen END, removed_at = NULL`);
      const c = { added: 0, removed: 0, updated: 0, total: items.length, baseline };
      tx(() => {
        for (const it of items) {
          const key = `${it.source}\u0000${it.name}`;
          const old = prev.get(key);
          prev.delete(key);
          up.run(node_id, it.source, it.name, it.version, it.explicit ? 1 : 0, it.extra ? JSON.stringify(it.extra) : null, old && !old.removed_at ? old.first_seen : now, now);
          if (baseline) continue;
          if (!old || old.removed_at) { ev.run(now, node_id, it.source, it.name, 'added', null, it.version); c.added++; }
          else if ((old.version || null) !== (it.version || null)) { ev.run(now, node_id, it.source, it.name, 'updated', old.version, it.version); c.updated++; }
        }
        for (const old of prev.values()) {
          if (old.removed_at || skip.has(old.source)) continue;
          q('UPDATE inventory SET removed_at = ? WHERE node_id = ? AND source = ? AND name = ?').run(now, node_id, old.source, old.name);
          ev.run(now, node_id, old.source, old.name, 'removed', old.version, null);
          c.removed++;
        }
        q(`INSERT INTO inventory_nodes (node_id, baseline_at, last_ok_at) VALUES (?, ?, ?)
           ON CONFLICT (node_id) DO UPDATE SET last_ok_at = excluded.last_ok_at`).run(node_id, now, now);
      });
      return { ...c, skipped: [...skip] };
    },
    inventory({ node_id, includeRemoved = false } = {}) {
      const where = [node_id ? 'node_id = ?' : null, includeRemoved ? null : 'removed_at IS NULL'].filter(Boolean);
      return q(`SELECT * FROM inventory ${where.length ? 'WHERE ' + where.join(' AND ') : ''} ORDER BY node_id, source, name`).all(...(node_id ? [node_id] : []))
        .map((r) => ({ ...r, explicit: !!r.explicit, extra: r.extra ? JSON.parse(r.extra) : null }));
    },
    inventoryEvents(limit = 200) { return q('SELECT * FROM inventory_events ORDER BY ts DESC, id DESC LIMIT ?').all(limit); },
    // 正規化済みのログを入れる。新しく入った件数を返す（再送分は数えない）
    insertLogs(node_id, source, rows, now = Date.now()) {
      let inserted = 0;
      tx(() => {
        for (const r of rows) {
          const res = st.insLog.run(node_id, source, r.uid, r.ts, r.level, r.provider ?? null, r.event_id ?? null, r.message, r.fingerprint, now);
          if (res.changes) {
            inserted++;
            st.insFts.run(res.lastInsertRowid, r.message, r.provider ?? '');
            st.upSig.run(r.fingerprint, source, r.provider ?? null, r.level, r.message.slice(0, 300), r.ts, r.ts);
          }
        }
      });
      return inserted;
    },
    cursor(node_id, source) { return st.getCursor.get(node_id, source) || null; },
    cursors() { return st.cursors.all(); },
    cursorOk(node_id, source, cursor, count, dropped = 0) { st.okCursor.run(node_id, source, cursor, Date.now(), Date.now(), count, dropped); },
    cursorError(node_id, source, err) { st.errCursor.run(node_id, source, Date.now(), String(err).slice(0, 500)); },
    queryLogs({ node_id, level, source, q, since, limit = 300 } = {}) {
      const where = [], args = [];
      if (q) { where.push('l.id IN (SELECT rowid FROM logs_fts WHERE logs_fts MATCH ?)'); args.push(q); }
      if (node_id) { where.push('l.node_id = ?'); args.push(node_id); }
      if (level) { where.push('l.level = ?'); args.push(level); }
      if (source) { where.push('l.source = ?'); args.push(source); }
      if (since) { where.push('l.ts >= ?'); args.push(since); }
      const sql = `SELECT l.id, l.node_id, l.source, l.ts, l.level, l.provider, l.event_id, l.message, l.fingerprint FROM logs l
                   ${where.length ? 'WHERE ' + where.join(' AND ') : ''} ORDER BY l.ts DESC LIMIT ?`;
      return db.prepare(sql).all(...args, Math.min(limit, 2000));
    },
    // 同種ログの上位。期間内の件数と、何台で出ているか
    signatures({ since, node_id, limit = 50 } = {}) {
      const args = [since ?? 0];
      let nodeCond = '';
      if (node_id) { nodeCond = 'AND l.node_id = ?'; args.push(node_id); }
      return db.prepare(`SELECT l.fingerprint, s.level, s.provider, s.source, s.sample, count(*) AS n, count(DISTINCT l.node_id) AS nodes,
                           group_concat(DISTINCT l.node_id) AS node_ids, max(l.ts) AS last_ts, s.total
                         FROM logs l JOIN log_signatures s USING (fingerprint)
                         WHERE l.ts >= ? ${nodeCond} GROUP BY l.fingerprint ORDER BY n DESC LIMIT ?`).all(...args, limit);
    },
    // 機体ごとの集計（所見づくり用）。provider×event_id の件数
    logCounts(node_id, since) {
      return db.prepare(`SELECT source, provider, event_id, level, count(*) AS n, max(ts) AS last_ts FROM logs
                         WHERE node_id = ? AND ts >= ? GROUP BY source, provider, event_id, level`).all(node_id, since);
    },
    // 期間内で最も多い同種ログ（洪水の検出用）。ログインの記録は lib/logs.js の loginFindings で別に見る
    topSignatures(node_id, since, limit = 3) {
      return db.prepare(`SELECT l.fingerprint, l.source, l.provider, l.level, count(*) AS n, max(l.message) AS sample FROM logs l
                         WHERE l.node_id = ? AND l.ts >= ? AND l.level != 'info' AND l.source NOT IN ('mac_auth', 'win_security')
                         GROUP BY l.fingerprint ORDER BY n DESC LIMIT ?`).all(node_id, since, limit);
    },
    // 取り込みで捨てた件数（洪水で上限を超えた量）
    droppedTotal(node_id) {
      return db.prepare('SELECT coalesce(sum(dropped_total), 0) AS n FROM log_cursors WHERE node_id = ?').get(node_id).n;
    },
    // 機能チェックを保存し、状態が変わったものを返す（通知と履歴に使う）。消えたチェックは削除する
    saveChecks(scope, checks, now = Date.now()) {
      const prev = new Map(db.prepare('SELECT * FROM checks WHERE scope = ?').all(scope).map((r) => [r.id, r]));
      const changed = [];
      tx(() => {
        for (const c of checks) {
          const p = prev.get(c.id);
          prev.delete(c.id);
          const since = p && p.status === c.status ? p.since : now;
          db.prepare('INSERT OR REPLACE INTO checks (scope, id, name, status, detail, since, checked_at) VALUES (?, ?, ?, ?, ?, ?, ?)').run(scope, c.id, c.name, c.status, c.detail ?? null, since, now);
          if (!p || p.status !== c.status) {
            db.prepare('INSERT INTO check_events (ts, scope, check_id, name, from_status, to_status, detail) VALUES (?, ?, ?, ?, ?, ?, ?)').run(now, scope, c.id, c.name, p?.status ?? null, c.status, c.detail ?? null);
            changed.push({ scope, id: c.id, name: c.name, from: p?.status ?? null, to: c.status, detail: c.detail });
          }
        }
        for (const id of prev.keys()) db.prepare('DELETE FROM checks WHERE scope = ? AND id = ?').run(scope, id);
      });
      return changed;
    },
    checks() { return db.prepare('SELECT * FROM checks ORDER BY scope, id').all(); },
    checkEvents(limit = 200) { return db.prepare('SELECT * FROM check_events ORDER BY ts DESC, id DESC LIMIT ?').all(limit); },
    getMeta(k) { const r = db.prepare('SELECT v FROM meta WHERE k = ?').get(k); return r ? JSON.parse(r.v) : null; },
    setMeta(k, v) { db.prepare('INSERT OR REPLACE INTO meta (k, v, updated_at) VALUES (?, ?, ?)').run(k, JSON.stringify(v), Date.now()); },
    integrity() {
      const r = db.prepare('PRAGMA quick_check').get();
      const size = db.prepare('SELECT page_count * page_size AS b FROM pragma_page_count(), pragma_page_size()').get();
      return { check: Object.values(r)[0], bytes: size.b };
    },
    prune(now = Date.now()) {
      db.prepare('DELETE FROM check_events WHERE ts < ?').run(now - 180 * 86400e3);
      const cut = now - LOG_RETENTION_DAYS * 86400e3;
      tx(() => {
        db.prepare("INSERT INTO logs_fts (logs_fts, rowid, message, provider) SELECT 'delete', id, message, coalesce(provider, '') FROM logs WHERE ts < ?").run(cut);
        db.prepare('DELETE FROM logs WHERE ts < ?').run(cut);
      });
    },
    // 以前の JSON Lines（history/*.jsonl, actions.jsonl）を一度だけ取り込む
    importLegacy(dir, summarize) {
      const hist = path.join(dir, 'history');
      let n = 0;
      if (fs.existsSync(hist)) {
        for (const f of fs.readdirSync(hist).filter((x) => x.endsWith('.jsonl'))) {
          const id = f.replace(/\.jsonl$/, '');
          for (const line of fs.readFileSync(path.join(hist, f), 'utf8').split('\n').filter(Boolean)) {
            try { const e = JSON.parse(line); this.addSnapshot(id, { ...e, summary: summarize(e) }); n++; } catch { /* 壊れた行は飛ばす */ }
          }
        }
        fs.renameSync(hist, hist + '.imported');
      }
      const af = path.join(dir, 'actions.jsonl');
      if (fs.existsSync(af)) {
        for (const line of fs.readFileSync(af, 'utf8').split('\n').filter(Boolean)) {
          try { this.addAction(JSON.parse(line)); n++; } catch { /* 重複・壊れた行は飛ばす */ }
        }
        fs.renameSync(af, af + '.imported');
      }
      return n;
    },
  };
}

module.exports = { openDb, LOG_RETENTION_DAYS };
