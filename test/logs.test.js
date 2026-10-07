'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { redact, fingerprint, normalize, logFindings, windowsRemoteTransport } = require('../lib/logs');
const { openDb } = require('../lib/db');

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'kt-test-'));

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
