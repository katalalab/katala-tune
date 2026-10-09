'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { redact, fingerprint, normalize, logFindings, windowsRemoteTransport, sourceError } = require('../lib/logs');
const { openDb } = require('../lib/db');

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'kt-test-'));

test('同種ログの集計は期間・機体・繰り返し件数を保ち、全期間のtotalと区別する', (t) => {
  const dir = tmp();
  const db = openDb(dir);
  t.after(() => { db.db.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  const row = (uid, ts, occurrences, fingerprint = 'shared') => ({ uid, ts, occurrences, fingerprint, level: 'warn', provider: null, message: 'example' });
  db.insertLogs('node-a', 'example', [row('old', 99, 50), row('edge', 100, 3), row('new', 200, 4), row('other', 300, 1, 'other')]);
  db.insertLogs('node-b', 'example', [row('b', 150, 9)]);
  const all = db.signatures({ since: 100 });
  assert.equal(all.length, 2);
  assert.deepEqual({ n: all[0].n, nodes: all[0].nodes, ids: all[0].node_ids.split(',').sort(), last: all[0].last_ts, total: all[0].total },
    { n: 16, nodes: 2, ids: ['node-a', 'node-b'], last: 200, total: 66 });
  assert.equal(all[0].provider, null);
  const one = db.signatures({ since: 100, node_id: 'node-a', limit: 1 });
  assert.deepEqual({ n: one[0].n, nodes: one[0].nodes, ids: one[0].node_ids, total: one[0].total }, { n: 7, nodes: 1, ids: 'node-a', total: 66 });
  assert.deepEqual(db.signatures({ since: 301 }), []);
  assert.deepEqual(db.signatures({ node_id: 'missing' }), []);
  assert.deepEqual(db.signatures({ limit: 0 }), []);
  db.insertLogs('node-a', 'example', [row('z', 500, 2, 'z'), row('a', 400, 2, 'a')]);
  assert.equal(db.signatures({ since: 400, node_id: 'node-a', limit: 1 })[0].fingerprint, 'a');
  assert.equal(db.signatures({ since: 400, node_id: 'node-a', limit: 1, offset: 1 })[0].fingerprint, 'z');
  assert.deepEqual(db.signatures({ limit: -1 }), []);
  assert.deepEqual(db.signatures({ limit: 1, offset: -1 }), db.signatures({ limit: 1 }));
  assert.deepEqual(db.signatures({ offset: 1e20 }), []);
  assert.deepEqual(db.signatures({ offset: Number.MAX_VALUE }), []);
});

test('同種ログは取得上限500件を守り、次ページで残りを取得できる', (t) => {
  const dir = tmp();
  const db = openDb(dir);
  t.after(() => { db.db.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  db.insertLogs('node-a', 'example', Array.from({ length: 501 }, (_, i) => ({
    uid: String(i), ts: 100, occurrences: 1, fingerprint: String(i).padStart(3, '0'), level: 'warn', message: 'example',
  })));
  assert.equal(db.signatures({ limit: 1000 }).length, 500);
  assert.deepEqual(db.signatures({ limit: 1000, offset: 500 }).map(r => r.fingerprint), ['500']);
});

test('Windows リモートログは固定ファイルを使わず Base64 stdin から実行する', () => {
  const script = Buffer.from('\uFEFFparam([long]$SysCursor)\n[Console]::OutputEncoding = [Text.Encoding]::UTF8\n', 'utf8');
  const transport = windowsRemoteTransport({ win_system: '42; Write-Error bad', win_application: ' 7tail', neonmonitor: 'not-a-number' }, script);
  assert.doesNotMatch(transport.command, /katala-tune|logs\.ps1|-File|cygpath/);
  assert.match(transport.command, /^printf %s [A-Za-z0-9+/=]+ \| powershell\.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand [A-Za-z0-9+/=]+$/);
  assert.ok(Buffer.byteLength(transport.command) < 8191);
  assert.match(transport.bootstrap, /FromBase64String\(\[Console\]::In\.ReadToEnd\(\)\)/);
  assert.match(transport.bootstrap, /\[ScriptBlock\]::Create\(\$s\)/);
  assert.match(transport.bootstrap, /-SysCursor 42 -AppCursor 7 -NeonCursor 0$/);
  assert.equal(transport.input, undefined);
  const fullScript = fs.readFileSync(path.join(__dirname, '..', 'probes', 'win_logs.ps1'));
  const maxCursors = { win_system: '9007199254740991', win_application: '9007199254740991', neonmonitor: '9007199254740991' };
  assert.ok(Buffer.byteLength(windowsRemoteTransport(maxCursors, fullScript).command) < 8191);
  assert.throws(() => windowsRemoteTransport({}, Buffer.alloc(6000)), /8191/);
});

test('秘密らしい値を伏せる', () => {
  // 偽の値は実行時に組み立てる（ソースに秘密らしい文字列を置かない。gitleaks が正しく反応するため）
  const fakeAws = ['AKIA', 'ABCDEFGHIJKLMNOP'].join('');
  const s = redact('token=abc123 Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig12345678 ghp_' + 'a'.repeat(36) + ` ${fakeAws} sk-` + 'x'.repeat(30) + ' password: "p w"');
  assert.ok(!/abc123|ghp_a|AKIAABCD|sk-xxx|p w/.test(s), s);
  assert.match(s, /token=<redacted>/);
  assert.match(s, /<github-token>/);
  assert.match(s, /<aws-key>/);
});

test('長いメッセージは切り詰める', () => {
  assert.ok(redact('a'.repeat(5000)).length <= 1001);
});

test('数字・GUID・パスが違うだけのログは同じ指紋になる', () => {
  const a = fingerprint('win_system', 'disk', '7', 'The device \\Device\\Harddisk1\\DR1 has a bad block at 0x1F3 in C:\\Users\\a\\x.txt');
  const b = fingerprint('win_system', 'disk', '7', 'The device \\Device\\Harddisk2\\DR2 has a bad block at 0xAB in C:\\Users\\b\\y.txt');
  assert.equal(a, b);
  assert.notEqual(a, fingerprint('win_system', 'disk', '51', 'The device has a bad block'));
  assert.equal(fingerprint('x', 'p', '1', 'port {1C3A4C45-3712-47C5-BAD3-BFD302864317} failed'), fingerprint('x', 'p', '1', 'port {00000000-0000-0000-0000-000000000000} failed'));
});

test('同じログを2回取り込んでも1行（冪等）、件数と署名も増えない', () => {
  const db = openDb(tmp());
  const rows = normalize('win_application', [
    { uid: '10', ts: 1000, level: 'error', provider: '.NET Runtime', event_id: '1026', message: 'beszel_lhm.exe FileNotFoundException' },
    { uid: '11', ts: 2000, level: 'error', provider: '.NET Runtime', event_id: '1026', message: 'beszel_lhm.exe FileNotFoundException' },
  ]);
  assert.equal(db.insertLogs('n1', 'win_application', rows), 2);
  assert.equal(db.insertLogs('n1', 'win_application', rows), 0);
  const sig = db.signatures({ since: 0 });
  assert.equal(sig.length, 1);
  assert.equal(sig[0].n, 2);
  assert.equal(sig[0].total, 2);
  assert.equal(db.queryLogs({ q: 'beszel_lhm' }).length, 2);
  assert.equal(db.queryLogs({ q: 'nothing' }).length, 0);
});

test('取り込み位置: 成功で位置と捨てた数を記録し、失敗では位置を保ったまま理由を残す', () => {
  const db = openDb(tmp());
  db.cursorOk('n1', 'win_system', '100', 5, 3);
  db.cursorError('n1', 'win_system', 'ssh timeout');
  const c = db.cursor('n1', 'win_system');
  assert.equal(c.cursor, '100');
  assert.equal(c.dropped_total, 3);
  assert.equal(c.last_error, 'ssh timeout');
  db.cursorOk('n1', 'win_system', '120', 2, 4);
  const c2 = db.cursor('n1', 'win_system');
  assert.equal(c2.cursor, '120');
  assert.equal(c2.dropped_total, 7);
  assert.equal(c2.last_error, null);
});

test('ログオン権限欠測は成功扱いにせず cursor のエラーにする', () => {
  assert.equal(sourceError({ rows: [], note: 'no-permission' }), 'no-permission');
  assert.equal(sourceError({ rows: [], note: 'ok' }), null);
});

test('ログから所見: WHEA・GPU リセット・NeonMonitor の自動保護', () => {
  const db = openDb(tmp());
  const now = Date.now();
  const mk = (source, provider, event_id, level, i) => ({ uid: `${source}-${provider}-${i}`, ts: now - i * 1000, level, provider, event_id, message: `${provider} ${event_id} ${i}` });
  db.insertLogs('pc', 'win_system', normalize('win_system', [...Array(6)].map((_, i) => mk('win_system', 'Microsoft-Windows-WHEA-Logger', '17', 'warn', i))));
  db.insertLogs('pc', 'win_system', normalize('win_system', [mk('win_system', 'Display', '4101', 'warn', 1)]));
  db.insertLogs('pc', 'neonmonitor', normalize('neonmonitor', [mk('neonmonitor', 'NeonMonitor', null, 'warn', 1)]));
  const f = logFindings(db, 'pc', now);
  assert.equal(f.find((x) => x.id === 'log-whea').severity, 'critical');
  assert.ok(f.find((x) => x.id === 'log-gpu-reset'));
  assert.ok(f.find((x) => x.id === 'log-neon'));
  assert.deepEqual(logFindings(db, 'other', now), []);
});

test('保持期限より古いログは消え、全文検索からも消える', () => {
  const db = openDb(tmp());
  const now = Date.now();
  db.insertLogs('n', 's', normalize('s', [{ uid: 'old', ts: now - 40 * 86400e3, level: 'info', message: 'ancient' }, { uid: 'new', ts: now, level: 'info', message: 'fresh' }]));
  db.prune(now);
  assert.equal(db.queryLogs({}).length, 1);
  assert.equal(db.queryLogs({ q: 'ancient' }).length, 0);
});

test('同じエラーの洪水（24時間で200件以上）と .NET の未処理例外を所見にする', () => {
  const db = openDb(tmp());
  const now = Date.now();
  const rows = [...Array(250)].map((_, i) => ({ uid: String(i), ts: now - i * 60e3 / 4, level: 'error', provider: '.NET Runtime', event_id: '1026', message: `beszel_lhm.exe FileNotFoundException ${i}` }));
  db.insertLogs('pc', 'win_application', normalize('win_application', rows));
  db.cursorOk('pc', 'win_application', '250', 250, 1700);
  const f = logFindings(db, 'pc', now);
  assert.ok(f.find((x) => x.id.startsWith('log-flood-') && /250 件/.test(x.title)), JSON.stringify(f.map((x) => x.title)));
  assert.ok(f.find((x) => x.id === 'log-crashes'));
  assert.ok(f.find((x) => x.id === 'log-dropped'));
});

test('volmgr の 161・162（BSOD 後のクラッシュダンプ作成）はディスクのエラーに数えない', () => {
  const db = openDb(tmp());
  const now = Date.now();
  const mk = (event_id, i) => ({ uid: `v-${event_id}-${i}`, ts: now - i * 1000, level: 'error', provider: 'volmgr', event_id, message: `volmgr ${event_id} ${i}` });
  db.insertLogs('pc', 'win_system', normalize('win_system', [mk('161', 1), mk('162', 2), mk('161', 3), mk('162', 4)]));
  assert.equal(logFindings(db, 'pc', now).find((x) => x.id === 'log-disk'), undefined);
  // 同じ volmgr でも別の事象（例: 46）は数える
  db.insertLogs('pc', 'win_system', normalize('win_system', [mk('46', 5)]));
  assert.match(logFindings(db, 'pc', now).find((x) => x.id === 'log-disk').title, / 1 件$/);
});
