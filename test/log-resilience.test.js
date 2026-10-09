'use strict';
// ログ収集の耐性: 同じ形の繰り返しの集約（probe 側）、実際の件数が署名・洪水判定に届くこと、一時的な不達で状態が振れないこと、
// 取れても 0 件の取り込み元（対象外・ログ無し）の区別。入力はすべて架空（実機の機体名・ホスト名・ログの本文は使わない）
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { DatabaseSync } = require('node:sqlite');
const { normalize, fingerprint, logFindings, sourceNote, occurrencesOf } = require('../lib/logs');
const { nodeChecks, LOGS_FAIL_STREAK, LOGS_FAIL_AFTER_MIN } = require('../lib/health');
const { openDb } = require('../lib/db');

const PROBES = path.join(__dirname, '..', 'probes');
const PY = ['python3', 'python'].find((c) => spawnSync(c, ['--version']).status === 0);
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'kt-resil-'));

// probes/mac_logs.py の関数を架空の入力で呼ぶ（機体には何もしない）
// 大きな入力は引数に載らないので、ファイルに書いて DATA_PATH で渡す
function py(code, data) {
  const dataPath = path.join(tmp(), 'input.txt');
  fs.writeFileSync(dataPath, data ?? '');
  const src = `DATA_PATH = ${JSON.stringify(dataPath)}\n` + `import importlib.util, json, sys\nspec = importlib.util.spec_from_file_location("m", ${JSON.stringify(path.join(PROBES, 'mac_logs.py'))})\nm = importlib.util.module_from_spec(spec)\nspec.loader.exec_module(m)\n${code}`;
  const r = spawnSync(PY, ['-c', src], { encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 });
  assert.equal(r.status, 0, r.stderr);
  return JSON.parse(r.stdout);
}

// log show --style ndjson に似た架空の1行
const T0 = Date.UTC(2026, 9, 7, 1, 0, 0);
const stamp = (i) => {
  const d = new Date(T0 + i * 100);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  // time.mktime は現地時刻で読むので、現地時刻の表記で作る
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}000+0000`;
};
const line = (i, msg, over = {}) => JSON.stringify({ timestamp: stamp(i), machTimestamp: 1000 + i, threadID: 7, eventMessage: msg, subsystem: 'com.example.kernel', senderImagePath: '/System/Library/Extensions/ExampleGfx.kext/ExampleGfx', ...over });

test('probe: 同じ形の繰り返しを代表1行と実際の件数にまとめ、集約のあとに上限を掛ける（macOS カーネル）', () => {
  const FLOOD = 20000;
  const lines = [];
  // 洪水: 数字・16進・パスだけが違う同じエラー。途中に本物の異常（形が違う）が20種類混ざる
  for (let i = 0; i < FLOOD; i++) {
    lines.push(line(i, `ExampleGfx: surface ${i} mapping failed (error 0x${(i % 255).toString(16)}) at /private/var/tmp/job-${i}/buf`));
    if (i % 1000 === 500) lines.push(line(i, `ExampleStorage: unexpected media state ${['alpha', 'beta', 'gamma', 'delta', 'epsilon', 'zeta', 'eta', 'theta', 'iota', 'kappa', 'lambda', 'mu', 'nu', 'xi', 'omicron', 'pi', 'rho', 'sigma', 'tau', 'upsilon'][i / 1000 | 0]}`, { senderImagePath: '/System/Library/Extensions/ExampleStorage.kext/ExampleStorage' }));
  }
  const res = py(`
rows, newest = m.parse_kernel(open(DATA_PATH).read(), 0)
kept, dropped, events = m.aggregate(rows)
print(json.dumps({"parsed": len(rows), "kept": kept, "dropped": dropped, "events": events}))`, lines.join('\n'));
  assert.equal(res.parsed, FLOOD + 20);
  // 旧実装なら最新 300 行（ほぼ洪水）で、20 種の異常は洪水の背後に残るだけ。集約すれば 21 行ですべて残る
  assert.equal(res.kept.length, 21);
  assert.equal(res.dropped, 0);
  assert.equal(res.events, FLOOD + 20);
  const flood = res.kept.find((r) => r.provider === 'ExampleGfx');
  assert.equal(flood.count, FLOOD);
  assert.equal(res.kept.filter((r) => r.provider === 'ExampleStorage').length, 20);
  assert.ok(res.kept.filter((r) => r.provider === 'ExampleStorage').every((r) => r.count === 1));
  // 件数の総和 = 見た件数（取りこぼしなし）
  assert.equal(res.kept.reduce((s, r) => s + r.count, 0) + res.dropped, res.events);
  // 代表行は最初の行の uid・本文。時刻は最後に出たもの
  assert.equal(flood.uid, '1000-7');
  assert.match(flood.message, /surface 0 mapping failed/);
  assert.ok(flood.ts >= T0 + (FLOOD - 1) * 100 - 1000);
});

test('probe: 形の違うものが上限を超えたら新しい 300 行を残し、切った分は実際の件数で dropped に数える', () => {
  const lines = [];
  for (let i = 0; i < 500; i++) lines.push(line(i, `ExampleBus: unique fault ${'abcdefghij'[i % 10]}${'klmnopqrst'[(i / 10 | 0) % 10]}${'uvwxyzabcd'[(i / 100 | 0) % 10]} word${String.fromCharCode(97 + (i % 26))}`.replace(/\d/g, '')));
  // 洪水（数字だけ違う）を一番古い側に 1000 件
  for (let i = 0; i < 1000; i++) lines.unshift(line(0, `ExampleGfx: retry ${i}`));
  const res = py(`
rows, newest = m.parse_kernel(open(DATA_PATH).read(), 0)
kept, dropped, events = m.aggregate(rows)
print(json.dumps({"kept": len(kept), "dropped": dropped, "events": events, "sum": sum(r["count"] for r in kept)}))`, lines.join('\n'));
  assert.equal(res.events, 1500);
  assert.equal(res.kept, 300);
  assert.equal(res.sum + res.dropped, res.events);
  assert.ok(res.dropped >= 200);
  assert.equal(res.dropped, 1000 + 200, '一番古い 1000 件の洪水と、古い形 200 種類が切られる（501 グループのうち古い 201 を切る）');
});

test('probe: 集約の鍵は lib/logs.js の fingerprint と同じ分け方（GUID・16進・パス・引用符・数字を伏せる）', () => {
  const msgs = [
    'device 12 failed at 0x1F3 in /Users/a/x.txt',
    'device 99 failed at 0xAB in /Users/b/y.txt',
    'port {1C3A4C45-3712-47C5-BAD3-BFD302864317} refused after 3.5 s',
    'port {00000000-0000-0000-0000-000000000000} refused after 12 s',
    'name "first value" rejected', "name 'second value' rejected", 'name "third" rejected',
    'device failed', 'Device   FAILED',
    'unrelated message',
  ];
  const keys = py(`
print(json.dumps([list(m.norm_key("p", "e", x)) for x in ${JSON.stringify(msgs)}]))`).map((k) => k.join('|'));
  const fps = msgs.map((x) => fingerprint('mac_kernel', 'p', 'e', x));
  for (let i = 0; i < msgs.length; i++) for (let j = 0; j < msgs.length; j++) assert.equal(keys[i] === keys[j], fps[i] === fps[j], `${msgs[i]} / ${msgs[j]}`);
});

test('probe: 代表行の本文は伏せ字を通り、繰り返しの本文は増えない', () => {
  const secret = ['ghp', '_', 'a'.repeat(36)].join('');
  const rows = [...Array(50)].map((_, i) => ({ uid: String(i), ts: 1000 + i, level: 'error', provider: 'p', event_id: 'e', message: `auth failed token=${secret}${i} n=${i}` }));
  const res = py(`print(json.dumps(m.aggregate(${JSON.stringify(rows)})[0]))`);
  assert.equal(res.length, 1);
  assert.equal(res[0].count, 50);
  const n = normalize('mac_kernel', res);
  assert.equal(n.length, 1);
  assert.ok(!n[0].message.includes(secret.slice(0, 12)), n[0].message);
  assert.equal(n[0].occurrences, 50);
});

test('取り込み: 件数は署名・洪水判定・集計に実際の量で届く（旧実装は取り込めた行しか数えなかった）', () => {
  const db = openDb(tmp());
  const now = Date.now();
  const rows = [
    { uid: 'a1', ts: now - 60e3, level: 'error', provider: 'ExampleGfx', event_id: 'com.example.kernel', message: 'ExampleGfx: surface 1 mapping failed', count: 2_280_000 },
    { uid: 'b1', ts: now - 30e3, level: 'error', provider: 'ExampleStorage', event_id: 'com.example.kernel', message: 'ExampleStorage: unexpected media state alpha' },
  ];
  assert.equal(db.insertLogs('node-a', 'mac_kernel', normalize('mac_kernel', rows)), 2);
  // 同じ範囲の読み直しでは増えない
  assert.equal(db.insertLogs('node-a', 'mac_kernel', normalize('mac_kernel', rows)), 0);
  const sigs = db.signatures({ since: 0 });
  const g = sigs.find((s) => s.provider === 'ExampleGfx');
  assert.equal(g.n, 2_280_000);
  assert.equal(g.total, 2_280_000);
  assert.equal(sigs.find((s) => s.provider === 'ExampleStorage').n, 1);
  const f = logFindings(db, 'node-a', now);
  const flood = f.find((x) => x.id.startsWith('log-flood-'));
  assert.ok(flood, JSON.stringify(f.map((x) => x.title)));
  assert.match(flood.title, /2280000 件/);
  // 画面の一覧は代表行そのもの（件数つき）
  assert.equal(db.queryLogs({ q: 'surface' })[0].occurrences, 2_280_000);
  // 件数が無い・壊れている行は 1
  assert.equal(occurrencesOf(undefined), 1);
  assert.equal(occurrencesOf('x'), 1);
  assert.equal(occurrencesOf(0), 1);
  assert.equal(occurrencesOf(-5), 1);
  assert.equal(occurrencesOf('7'), 7);
  assert.equal(occurrencesOf(2.9), 2);
  assert.equal(occurrencesOf(1e15), 1_000_000_000);
});

test('取り込み: 199 件の代表行は洪水ではなく、200 件の代表行は洪水（しきい値は件数で数える）', () => {
  const db = openDb(tmp());
  const now = Date.now();
  const mk = (uid, provider, count) => ({ uid, ts: now - 1000, level: 'error', provider, event_id: 'e', message: `${provider} failure`, count });
  db.insertLogs('n', 's', normalize('s', [mk('1', 'Below', 199)]));
  assert.equal(logFindings(db, 'n', now).filter((x) => x.id.startsWith('log-flood-')).length, 0);
  db.insertLogs('n', 's', normalize('s', [mk('2', 'Exact', 200)]));
  assert.equal(logFindings(db, 'n', now).filter((x) => x.id.startsWith('log-flood-')).length, 1);
});

test('移行: 件数の列が無い古い DB を開くと列が足され、既存の行は 1 件として数える', () => {
  const dir = tmp();
  const old = new DatabaseSync(path.join(dir, 'katala-tune.db'));
  old.exec(`CREATE TABLE logs (id INTEGER PRIMARY KEY, node_id TEXT NOT NULL, source TEXT NOT NULL, uid TEXT NOT NULL, ts INTEGER NOT NULL, level TEXT NOT NULL, provider TEXT, event_id TEXT, message TEXT NOT NULL, fingerprint TEXT NOT NULL, ingested_at INTEGER NOT NULL, UNIQUE (node_id, source, uid));
    CREATE TABLE log_cursors (node_id TEXT NOT NULL, source TEXT NOT NULL, cursor TEXT, updated_at INTEGER, last_ok_at INTEGER, last_error TEXT, last_count INTEGER, dropped_total INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (node_id, source)) WITHOUT ROWID;
    INSERT INTO logs (node_id, source, uid, ts, level, provider, event_id, message, fingerprint, ingested_at) VALUES ('n', 's', 'old', ${Date.now()}, 'error', 'p', 'e', 'legacy row', 'fp', 1);
    INSERT INTO log_cursors (node_id, source, cursor, last_error) VALUES ('n', 's', '5', 'ssh timeout');`);
  old.close();
  const db = openDb(dir);
  assert.equal(db.queryLogs({})[0].occurrences, 1);
  assert.equal(db.cursor('n', 's').fail_streak, 0);
  assert.equal(db.cursor('n', 's').note, null);
  db.insertLogs('n', 's', normalize('s', [{ uid: 'new', ts: Date.now(), level: 'error', provider: 'p', event_id: 'e', message: 'folded row', count: 40 }]));
  assert.equal(db.logCounts('n', 0).reduce((s, r) => s + r.n, 0), 41);
  // 開き直しても壊れない（列は一度だけ足す）
  assert.equal(openDb(dir).queryLogs({}).length, 2);
});

test('取り込み位置: 失敗は連続回数を数え、成功で 0 に戻して取り込み元の状態を記録する', () => {
  const db = openDb(tmp());
  db.cursorOk('n', 'neonmonitor', '0', 0, 0, 'not-installed');
  assert.equal(db.cursor('n', 'neonmonitor').note, 'not-installed');
  db.cursorError('n', 'neonmonitor', 'ssh timeout');
  db.cursorError('n', 'neonmonitor', 'ssh timeout');
  assert.equal(db.cursor('n', 'neonmonitor').fail_streak, 2);
  assert.equal(db.cursor('n', 'neonmonitor').note, 'not-installed');
  db.cursorOk('n', 'neonmonitor', '0', 0, 0, null);
  assert.equal(db.cursor('n', 'neonmonitor').fail_streak, 0);
  assert.equal(db.cursor('n', 'neonmonitor').note, null);
  // 初回から失敗なら 1
  db.cursorError('n', 'win_system', 'x');
  assert.equal(db.cursor('n', 'win_system').fail_streak, 1);
});

const NOW = 1_800_000_000_000;
const logsCheck = (cursors, schedule) => nodeChecks({ id: 'pc' }, null, [], { now: NOW, cursors: cursors.map((c) => ({ node_id: 'pc', ...c })), schedule }).find((c) => c.id === 'logs');

test('状態: 1回の不達（タイムアウト）では logs の状態を変えない', () => {
  const ok = { source: 'win_system', last_ok_at: NOW - 10 * 60000 };
  const one = logsCheck([ok, { source: 'win_application', last_ok_at: NOW - 10 * 60000, last_error: 'ssh timeout', fail_streak: 1 }]);
  assert.equal(one.status, 'ok');
  assert.match(one.detail, /win_application は一時的に届かない（連続 1 回、3 回か 60 分で失敗扱い）/);
  const two = logsCheck([ok, { source: 'win_application', last_ok_at: NOW - 25 * 60000, last_error: 'ssh timeout', fail_streak: 2 }]);
  assert.equal(two.status, 'ok');
});

test('状態: 連続失敗の回数か、最後の成功からの時間がしきい値に達したら fail にする', () => {
  assert.equal(LOGS_FAIL_STREAK, 3);
  assert.equal(LOGS_FAIL_AFTER_MIN, 60);
  const base = { source: 'win_system', last_error: 'ssh timeout' };
  const byStreak = logsCheck([{ ...base, last_ok_at: NOW - 20 * 60000, fail_streak: 3 }]);
  assert.equal(byStreak.status, 'fail');
  assert.match(byStreak.detail, /ssh timeout（連続 3 回）/);
  assert.equal(logsCheck([{ ...base, last_ok_at: NOW - 61 * 60000, fail_streak: 1 }]).status, 'fail');
  assert.equal(logsCheck([{ ...base, last_ok_at: NOW - 59 * 60000, fail_streak: 1 }]).status, 'ok');
  // 一度も成功していない取り込みは待たずに fail
  assert.equal(logsCheck([{ ...base, fail_streak: 1 }]).status, 'fail');
  // 取り込み間隔が長い設定では、間隔の3倍までは待つ（60 分より長いとき）
  assert.equal(logsCheck([{ ...base, last_ok_at: NOW - 100 * 60000, fail_streak: 1 }], { logs_minutes: 60 }).status, 'ok');
  assert.equal(logsCheck([{ ...base, last_ok_at: NOW - 181 * 60000, fail_streak: 1 }], { logs_minutes: 60 }).status, 'fail');
  // 古い DB（連続回数の列が無い）でも、成功が新しい間は一時的として扱う
  assert.equal(logsCheck([{ ...base, last_ok_at: NOW - 5 * 60000 }]).status, 'ok');
});

test('状態: 一時的な不達のあいだも、回復したあとも、状態は ok のまま', () => {
  const db = openDb(tmp());
  const t = (n) => NOW + n * 15 * 60000;
  const status = (at) => {
    const cur = db.cursors();
    return nodeChecks({ id: 'pc' }, null, [], { now: at, cursors: cur.map((c) => ({ ...c, last_ok_at: at - 15 * 60000 })) }).find((c) => c.id === 'logs').status;
  };
  db.cursorOk('pc', 'win_system', '1', 1, 0);
  const seen = [status(t(1))];
  db.cursorError('pc', 'win_system', 'ssh timeout');
  seen.push(status(t(2)));
  db.cursorOk('pc', 'win_system', '2', 1, 0);
  seen.push(status(t(3)));
  assert.deepEqual(seen, ['ok', 'ok', 'ok']);
});

test('取れても 0 件の取り込み元（NeonMonitor の未導入・ログ無し）は、状態の detail で静かな場合と区別する', () => {
  assert.equal(sourceNote({ note: 'not-installed' }), 'not-installed');
  assert.equal(sourceNote({ note: 'no-guard-log' }), 'no-guard-log');
  assert.equal(sourceNote({ note: 'no-permission' }), null);
  assert.equal(sourceNote({ note: 'anything else' }), null);
  assert.equal(sourceNote({}), null);
  const c = logsCheck([
    { source: 'win_system', last_ok_at: NOW - 60000 },
    { source: 'neonmonitor', last_ok_at: NOW - 60000, note: 'not-installed' },
  ]);
  assert.equal(c.status, 'ok');
  assert.match(c.detail, /neonmonitor: 対象外（未導入）（取れても 0 件）/);
  // note が無い（NeonMonitor があり、静か）なら何も足さない
  assert.doesNotMatch(logsCheck([{ source: 'neonmonitor', last_ok_at: NOW - 60000 }]).detail, /対象外|ログ無し/);
  assert.match(logsCheck([{ source: 'neonmonitor', last_ok_at: NOW - 60000, note: 'no-guard-log' }]).detail, /neonmonitor: ログ無し/);
});

test('Windows の調査スクリプト: 同じ形のイベントを件数にまとめ、NeonMonitor の状態を note で返す（文面の確認。実行は Windows 実機が必要）', () => {
  const ps = fs.readFileSync(path.join(PROBES, 'win_logs.ps1'), 'utf8');
  assert.match(ps, /function Norm/);
  assert.match(ps, /count = 1/);
  assert.match(ps, /\.count\+\+/);
  assert.match(ps, /'not-installed'/);
  assert.match(ps, /'no-guard-log'/);
  // 上限は集約の後（行を切る前に件数にまとめる）
  assert.ok(ps.indexOf('Select-Object -Last $MaxRows') > ps.indexOf('.count++'));
});
