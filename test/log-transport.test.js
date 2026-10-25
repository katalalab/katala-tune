'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { windowsRemoteTransport, windowsLogonsTransport, syncNode, mergeLogons } = require('../lib/logs');

const sourceNames = ['win_system', 'win_application', 'neonmonitor'];
const node = { id: 'example-node', alias: 'example-node', os: 'windows', network: false };
const data = { sources: Object.fromEntries(sourceNames.map(s => [s, { cursor: '10', rows: [] }])) };
const ok = { code: 0, out: JSON.stringify(data), err: '' };

test('Windows log body travels on SSH stdin while bootstrap command stays fixed', () => {
  const script = Buffer.from('# synthetic\n'.repeat(2048));
  const small = windowsRemoteTransport({}, Buffer.from('Write-Output 1\n'));
  const large = windowsRemoteTransport({ win_system: '-9007199254740991' }, script);
  assert.equal(large.command, small.command);
  assert.ok(Buffer.byteLength(large.command) < 8191);
  assert.doesNotMatch(large.command, /printf|\|/);
  const frame = large.input.split('\n');
  assert.deepEqual(frame.slice(0, 6), ['KTLOG1', 'events', '-9007199254740991', '0', '0', String(script.length)]);
  assert.equal(Buffer.from(frame[7], 'base64').compare(script), 0);
  assert.equal(frame[6], require('node:crypto').createHash('sha256').update(script).digest('hex'));
  assert.equal(windowsLogonsTransport({ win_security: '9223372036854775807:9223372036854775807' }, script).command, large.command);
});

test('Windows log transport rejects empty body and normalizes BOM/CRLF', () => {
  assert.throws(() => windowsRemoteTransport({}, Buffer.alloc(0)), /Invalid Windows log transport request/);
  const lf = windowsRemoteTransport({}, Buffer.from('param()\nWrite-Output 1\n'));
  assert.deepEqual(windowsRemoteTransport({}, Buffer.from('\uFEFFparam()\r\nWrite-Output 1\r\n')), lf);
});

test('nonzero/timeout with valid JSON cannot advance log cursors', async () => {
  for (const result of [{ ...ok, code: 255 }, { ...ok, code: null, err: 'timeout 90000ms' }]) {
    const calls = [];
    const db = { cursor: () => ({ cursor: '5' }), cursorError: (...a) => calls.push(['error', ...a]),
      cursorOk: (...a) => calls.push(['ok', ...a]), insertLogs: () => { throw Error('unverified rows'); } };
    let runs = 0;
    const fake = async () => { runs++; return result; };
    const module = { exports: {} };
    require('node:vm').runInNewContext(require('node:fs').readFileSync(require.resolve('../lib/logs'), 'utf8'), {
      module, exports: module.exports, __dirname: require('node:path').dirname(require.resolve('../lib/logs')), Buffer,
      require: name => name === './collect' ? { ...require('../lib/collect'), run: fake } : name === './windows-log-transport' ? { ...require('../lib/windows-log-transport'), runWindowsLogs: async () => { throw Error('Real SSH forbidden'); } } : require(require.resolve(name, { paths: [require('node:path').dirname(require.resolve('../lib/logs'))] })),
    });
    const summary = await module.exports.syncNode(db, node, { runWindows: fake });
    assert.equal(runs, 1);
    assert.ok(summary.error);
    assert.equal(calls.filter(c => c[0] === 'error').length, 3);
    assert.equal(calls.filter(c => c[0] === 'ok').length, 0);
  }
});

test('failed Security transport preserves verified event sources without accepting Security JSON', () => {
  const sec = { code: 255, out: JSON.stringify({ sources: { win_security: { cursor: '9', rows: [] } } }), err: 'synthetic disconnect' };
  const merged = JSON.parse(mergeLogons(ok, sec).out);
  assert.deepEqual(merged.sources.win_system, data.sources.win_system);
  assert.ok(merged.sources.win_security.error);
  assert.equal(merged.sources.win_security.cursor, undefined);
});

const { runWindowsLogs } = require('../lib/windows-log-transport');
const payload = Buffer.from('x'.repeat(20000));
test('finite log runner writes complete stdin and reads both EOFs', async () => {
  const script = 'process.stdin.resume();let n=0;process.stdin.on("data",b=>n+=b.length);process.stdin.on("end",()=>{process.stdout.write(JSON.stringify({bytes:n,text:"日本語"}));process.stderr.write("warning");});';
  const result = await runWindowsLogs(process.execPath, ['-e', script], { input: payload, timeoutMs: 5000 });
  assert.equal(result.code, 0);
  assert.deepEqual(JSON.parse(result.out), { bytes: payload.length, text: '日本語' });
  assert.equal(result.err, 'warning');
});

test('finite log runner rejects valid JSON followed by nonzero and timeout', async () => {
  for (const [script, timeoutMs] of [
    ['process.stdin.resume();process.stdin.on("end",()=>{process.stdout.write("{}");process.exitCode=7;});', 5000],
    ['process.stdin.resume();process.stdout.write("{}");setInterval(()=>{},1000);', 100],
  ]) {
    const result = await runWindowsLogs(process.execPath, ['-e', script], { input: 'fixture', timeoutMs });
    assert.deepEqual(result, { code: null, out: '', err: 'Windows log transport failed' });
  }
});

// Deterministic stream faults: no external program or SSH may be spawned here.
function pipeFixture(mode) {
  const { EventEmitter } = require('node:events');
  const child = new EventEmitter();
  child.kill = () => true;
  for (const stream of ['stdin', 'stdout', 'stderr']) child[stream] = Object.assign(new EventEmitter(), { destroy() {} });
  child.stdin.end = (_input, callback) => queueMicrotask(() => {
    if (mode === 'write-error') { callback(Error('fixture')); return; }
    callback();
    child.stdout.emit('data', Buffer.from('{}'));
    if (mode === 'stdout-limit') child.stdout.emit('data', Buffer.alloc(4 * 1024 * 1024 + 1));
    if (mode === 'stderr-limit') child.stderr.emit('data', Buffer.alloc(64 * 1024 + 1));
    if (mode === 'invalid-utf8') child.stdout.emit('data', Buffer.from([255]));
    if (mode.endsWith('-error')) child[mode.split('-')[0]].emit('error', Error('fixture'));
    child.stdout.emit('end');
    if (mode !== 'stderr-no-eof') child.stderr.emit('end');
    child.emit('exit', 0);
    if (mode !== 'no-close') child.emit('close', 0);
  });
  return child;
}
test('finite log runner refuses pipe failures, output overflow and missing EOF', async () => {
  for (const mode of ['write-error', 'stdout-error', 'stderr-error', 'stdout-limit', 'stderr-limit', 'invalid-utf8', 'stderr-no-eof', 'no-close']) {
    const result = await runWindowsLogs('fixture', [], { input: 'fixture', timeoutMs: 20 }, () => pipeFixture(mode));
    assert.deepEqual(result, { code: null, out: '', err: 'Windows log transport failed' }, mode);
  }
});

test('verified logs keep DB idempotency; failed transport preserves stored cursor', async t => {
  const { openDb } = require('../lib/db');
  const fs = require('node:fs');
  const dir = fs.mkdtempSync(require('node:path').join(require('node:os').tmpdir(), 'kt-log-'));
  const db = openDb(dir);
  t.after(() => { db.db.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  const row = { uid: '10', ts: 1800000000000, message: 'synthetic', level: 'info' };
  const result = { ...ok, out: JSON.stringify({ sources: { ...data.sources, win_system: { cursor: '10', rows: [row] } } }) };
  assert.equal((await syncNode(db, node, { runWindows: async () => result })).sources.win_system.inserted, 1);
  assert.equal((await syncNode(db, node, { runWindows: async () => result })).sources.win_system.inserted, 0);
  assert.ok((await syncNode(db, node, { runWindows: async () => ({ ...result, code: 255 }) })).error);
  assert.equal(db.cursor(node.id, 'win_system').cursor, '10');
  assert.ok(db.cursor(node.id, 'win_system').last_error);
});

test('Windows Git Bash executes >8191-byte bodies and rejects corrupt or truncated stdin', { skip: process.platform !== 'win32' && 'Windows Git Bash + PS5.1 acceptance fixture' }, async t => {
  const fs = require('node:fs');
  const path = require('node:path');
  const bash = path.join(process.env.ProgramFiles || 'C:\\Program Files', 'Git', 'bin', 'bash.exe');
  assert.ok(fs.existsSync(bash), 'Windows CI must provide Git Bash');
  const script = Buffer.from('param([long]$SysCursor,[long]$AppCursor,[long]$NeonCursor)\n' + '# padding\n'.repeat(2000) + '[Console]::WriteLine("{`"text`":`"日本語`",`"cursor`":"+$SysCursor+"}")\n');
  const transport = windowsRemoteTransport({ win_system: '42' }, script);
  const execute = input => runWindowsLogs(bash, ['-c', transport.command], { input, timeoutMs: 30000 });
  const result = await execute(transport.input);
  assert.equal(result.code, 0, result.err);
  assert.deepEqual(JSON.parse(result.out), { text: '日本語', cursor: 42 });
  const fields = transport.input.split('\n');
  const changed = (i, v) => { const a = [...fields]; a[i] = v; return a.join('\n'); };
  for (const [name, input] of [
    ['empty', ''], ['missing-final-quantum', transport.input.slice(0, -4)], ['partial-quantum', transport.input.slice(0, -1)],
    ['extra-byte', transport.input + 'A'], ['extra-newline', transport.input + '\n'], ['empty-body', changed(7, '')], ['bad-hash', changed(6, '0'.repeat(64))], ['bad-cursor', changed(2, '42;Write-Output bad')],
    ['bad-length', changed(5, '0')], ['bad-kind', changed(1, 'other')], ['invalid-base64', changed(7, '!')],
  ]) await t.test(name, async () => assert.equal((await execute(input)).code, null));
  for (const body of [Buffer.from('param(\n'), Buffer.from([255]), Buffer.from('param()\n[Console]::WriteLine("{}")\nexit 7')]) {
    const t = windowsRemoteTransport({}, body);
    assert.equal((await runWindowsLogs(bash, ['-c', t.command], { input: t.input, timeoutMs: 30000 })).code, null);
  }
  const logons = windowsLogonsTransport({ win_security: '9223372036854775807:9223372036854775807' }, Buffer.from('param([long]$SecCursor,[long]$SecTime)\n[Console]::WriteLine("$SecCursor`n$SecTime")'));
  const security = await runWindowsLogs(bash, ['-c', logons.command], { input: logons.input, timeoutMs: 30000 });
  assert.equal(security.code, 0);
  assert.equal(security.out.trim().replace(/\r/g, ''), '9223372036854775807\n9223372036854775807');
});
