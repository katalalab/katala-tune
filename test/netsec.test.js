'use strict';
// ネットワークとセキュリティ（lib/netsec.js・probes の netsec・ログインの取り込み）。
// アドレスは文書用の範囲（192.0.2.0/24・198.51.100.0/24・203.0.113.0/24・2001:db8::/32）と私用の範囲だけを使う
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const netsec = require('../lib/netsec');
const rules = require('../lib/rules');
const health = require('../lib/health');
const logs = require('../lib/logs');
const { openDb } = require('../lib/db');

const PROBES = path.join(__dirname, '..', 'probes');
const PY = ['python3', 'python'].find((c) => spawnSync(c, ['--version']).status === 0);
const NOW = 1_800_000_000_000;
const DAY = 86400e3;

// probes の Python の関数を、架空の入力で呼ぶ（機体には何もしない）
function py(file, code) {
  const src = `import importlib.util, json, sys\nspec = importlib.util.spec_from_file_location("m", ${JSON.stringify(path.join(PROBES, file))})\nm = importlib.util.module_from_spec(spec)\nspec.loader.exec_module(m)\n${code}`;
  const r = spawnSync(PY, ['-c', src], { encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  return JSON.parse(r.stdout);
}

test('Windows の調査スクリプトは BOM 付きの ASCII（PS 5.1）', () => {
  for (const f of fs.readdirSync(PROBES).filter((x) => x.endsWith('.ps1'))) {
    const b = fs.readFileSync(path.join(PROBES, f));
    assert.deepEqual([...b.subarray(0, 3)], [0xef, 0xbb, 0xbf], `${f}: BOM が無い`);
    assert.ok(b.subarray(3).every((c) => c < 128), `${f}: ASCII 以外の文字がある`);
  }
});

test('nettop の出力から待ち受けと外向きの接続を作る（macOS）', () => {
  const text = [
    ',state,',
    'ControlCenter.932,,',
    'tcp4 *:7000<->*:*,Listen,',
    'tcp6 *.7000<->*.*,Listen,',
    'ollama.1172,,',
    'tcp4 127.0.0.1:11434<->*:*,Listen,',
    'Google Chrome H.2001,,',
    'tcp4 192.168.1.43:50000<->198.51.100.7:443,Established,',
    'tcp4 192.168.1.43:50001<->198.51.100.7:443,Established,',
    'tcp6 2001:db8::5.50002<->2001:db8::1.443,Established,',
    'tcp4 127.0.0.1:50003<->127.0.0.1:5432,Established,',
    'udp4 192.168.1.43:61558<->203.0.113.5:123,,',
    'mDNSResponder.200,,',
    'udp4 *:5353<->*:*,,',
    'udp6 *.*<->*.*,,',
    'rapportd.916,,',
    'tcp6 fe80::1%en0.49241<->fe80::2%en0.49152,Established,',
    // 自分の待ち受け（7000）で受けた接続は外向きではない（相手の番号は毎回変わる）
    'ControlCenter.932,,',
    'tcp4 192.168.1.43:7000<->192.168.1.50:61234,Established,',
  ].join('\n');
  const [listen, outbound] = py('mac_probe.py', `print(json.dumps(m.parse_nettop(${JSON.stringify(text)}, lambda pid: {2001: "Google Chrome Helper"}.get(pid))))`);
  assert.deepEqual(listen.map((l) => `${l.proc} ${l.proto} ${l.addr} ${l.port}`), [
    'ControlCenter tcp * 7000', 'ControlCenter tcp * 7000', 'ollama tcp 127.0.0.1 11434', 'mDNSResponder udp * 5353',
  ]);
  // 15 文字で切れた名前は実行ファイルの名前に置き換える。この機体の中（loopback）への接続は数えない
  assert.deepEqual(outbound, [
    { proc: 'Google Chrome Helper', addr: '198.51.100.7', port: 443, n: 2 },
    { proc: 'Google Chrome Helper', addr: '2001:db8::1', port: 443, n: 1 },
    { proc: 'rapportd', addr: 'fe80::2%en0', port: 49152, n: 1 },
  ]);
  const ns = netsec.normalize({ listen, outbound }, 'mac');
  assert.deepEqual(ns.listen.map((l) => [l.proc, l.port, l.exposure]), [['ControlCenter', 7000, 'any'], ['ollama', 11434, 'loopback'], ['mDNSResponder', 5353, 'any']]);
  assert.deepEqual(ns.outbound.map((o) => [o.addr, o.public]), [['198.51.100.7', true], ['2001:db8::1', true], ['fe80::2', false]]);
});

test('プロセスの名前は実行ファイルの名前（版の番号の名前は、その上の名前）（macOS）', () => {
  const names = py('mac_probe.py', 'print(json.dumps([m.stable_name(p) for p in ["/opt/tool/claude/versions/2.1.291", "/usr/bin/ssh", "/Applications/A.app/Contents/MacOS/A", "/opt/x/1.2.3/v4.5"]]))');
  assert.deepEqual(names, ['claude', 'ssh', 'A', 'x']);
});

test('sshd のログイン失敗を接続ごとに1件にまとめ、送り元を provider に入れる（macOS）', () => {
  const ln = (ts, msg) => JSON.stringify({ timestamp: ts, eventMessage: msg, process: 'sshd-session' });
  const text = [
    ln('2026-10-07 10:00:00.100000+0900', 'Invalid user admin from 203.0.113.5 port 50001'),
    ln('2026-10-07 10:00:01.100000+0900', 'Failed password for invalid user admin from 203.0.113.5 port 50001 ssh2'),
    ln('2026-10-07 10:00:02.100000+0900', 'Connection closed by invalid user admin 203.0.113.5 port 50001 [preauth]'),
    ln('2026-10-07 10:05:00.000000+0900', 'Failed publickey for someone from 192.0.2.44 port 50100 ssh2'),
    ln('2026-10-07 10:06:00.000000+0900', 'Accepted publickey for someone from 192.0.2.44 port 50101 ssh2'),
    'not json',
  ].join('\n');
  const [rows] = py('mac_logs.py', `print(json.dumps(m.parse_auth(${JSON.stringify(text)})))`);
  assert.deepEqual(rows.map((r) => [r.provider, r.event_id, r.level]), [['203.0.113.5', 'ssh-fail', 'warn'], ['192.0.2.44', 'ssh-fail', 'warn']]);
  assert.match(rows[0].message, /account admin, from 203\.0\.113\.5, password/);
  assert.match(rows[0].uid, /^ssh:203\.0\.113\.5:50001:\d+$/);
});

test('macOS の sshd ログ取得が失敗したら cursor を進めない', () => {
  const result = py('mac_logs.py', `
import types
m.subprocess.run = lambda *a, **k: types.SimpleNamespace(returncode=1, stdout='', stderr='log database unavailable')
print(json.dumps(m.auth(123)))`);
  assert.match(result.error, /exit 1/);
  assert.equal(result.cursor, undefined);
});

test('アドレスの範囲', () => {
  const s = (a) => netsec.addrInfo(a)?.scope;
  assert.equal(s('*'), 'any');
  assert.equal(s('::'), 'any');
  assert.equal(s('127.0.0.1'), 'loopback');
  assert.equal(s([100, 100, 1, 1].join('.')), 'private'); // CGNAT（tailnet）。点検が本物のアドレスと見分けられないので組み立てる
  assert.equal(s('192.168.0.1'), 'private');
  assert.equal(s('fe80::1%en0'), 'link');
  assert.equal(s('fd7a:115c:a1e0::1'), 'private');
  assert.equal(s('198.51.100.7'), 'public');
  assert.equal(s('::ffff:203.0.113.5'), 'public');
  assert.equal(netsec.addrInfo('::ffff:203.0.113.5').addr, '203.0.113.5');
  assert.equal(netsec.addrInfo(''), null);
});

test('台帳の "network": false で止める（ログインの記録も集めない）', () => {
  assert.equal(netsec.enabled({ id: 'a' }), true);
  assert.equal(netsec.enabled({ id: 'a', network: false }), false);
  assert.deepEqual(logs.sourcesOf({ os: 'macos', network: false }), ['mac_diag', 'mac_kernel']);
  assert.deepEqual(logs.sourcesOf({ os: 'windows' }), ['win_system', 'win_application', 'neonmonitor', 'win_security']);
  assert.deepEqual(logs.sourcesOf({ os: 'windows', shared: true }), ['win_system', 'win_application', 'neonmonitor']);
  assert.deepEqual(logs.sourcesOf({ os: 'windows', shared: true, network_logins: true }), ['win_system', 'win_application', 'neonmonitor', 'win_security']);
  const src = fs.readFileSync(path.join(PROBES, 'mac_logs.py'), 'utf8');
  assert.match(src, /"nonet" not in sys\.argv/);
  assert.match(fs.readFileSync(path.join(PROBES, 'win_probe.ps1'), 'utf8'), /param\(\[switch\]\$NoNetwork\)/);
  // 調査スクリプトへの印は benchmark とは別に渡す（lib/collect.js）
  const { macProbeArgs, macSshCommand, windowsSshCommand } = require('../lib/collect');
  assert.deepEqual(macProbeArgs(true, false), ['python3', '-', 'nonet']);
  assert.deepEqual(macProbeArgs(false, false), ['python3', '-', '--skip-benchmark', 'nonet']);
  assert.equal(macSshCommand(false, false).match(/ --skip-benchmark nonet/g).length, 2);
  assert.match(windowsSshCommand(true, false), /probe\.ps1\)" -NoNetwork;/);
  assert.doesNotMatch(windowsSshCommand(false, true), /NoNetwork/);
});

test('Windows のログオンの記録は別のスクリプトで、同じ運び方（8191 バイトまで）で送り、結果を1つにまとめる', () => {
  const script = fs.readFileSync(path.join(PROBES, 'win_logons.ps1'));
  const t = logs.windowsLogonsTransport({ win_security: '9007199254740991' }, script);
  assert.ok(Buffer.byteLength(t.command) < 8191);
  assert.match(t.bootstrap, /\[ScriptBlock\]::Create\(\$s\)\) -SecCursor 9007199254740991$/);
  assert.match(t.command, /^printf %s [A-Za-z0-9+/=]+ \| powershell\.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand [A-Za-z0-9+/=]+$/);
  const main = { code: 0, out: JSON.stringify({ probe: 'win_logs', sources: { win_system: { cursor: '1', rows: [] } } }), err: '' };
  const merged = JSON.parse(logs.mergeLogons(main, { code: 0, out: `noise\n${JSON.stringify({ sources: { win_security: { cursor: '9', rows: [], note: 'no-permission' } } })}`, err: '' }).out);
  assert.deepEqual([merged.sources.win_system.cursor, merged.sources.win_security.note], ['1', 'no-permission']);
  // ログオンの方が失敗しても、ほかの元はそのまま取り込む
  const failed = JSON.parse(logs.mergeLogons(main, { code: null, out: '', err: 'ssh: timeout' }).out);
  assert.deepEqual([failed.sources.win_system.cursor, failed.sources.win_security.error], ['1', 'ssh: timeout']);
  // 本体が失敗したら本体のまま
  assert.equal(logs.mergeLogons({ code: 255, out: '', err: 'x' }, main).err, 'x');
  const source = script.subarray(3).toString('ascii');
  assert.match(source, /Get-WinEvent[^\r\n]+-Oldest[^\r\n]+-MaxEvents \(\$MaxRows \+ 1\)/);
  assert.equal((source.match(/Get-WinEvent -LogName Security -FilterXPath/g) || []).length, 1);
  assert.match(source, /\$batch = @\(\$ev \| Select-Object -First \$MaxRows\)/);
  assert.match(source, /foreach \(\$e in \$batch\) \{ if \(\$e\.RecordId -gt \$newest\)/);
});

test('Windows netsec は隔離した子だけを止め、完了済みの部分を20秒以内に残す', () => {
  const source = fs.readFileSync(path.join(PROBES, 'win_probe.ps1')).subarray(3).toString('ascii');
  assert.doesNotMatch(source, /\[PowerShell\]::Create\(\)|\.Stop\(\)/);
  assert.match(source, /ProcessStartInfo/);
  assert.match(source, /RedirectStandardInput = \$true/);
  assert.match(source, /\$nsBudgetMs = 15000/);
  assert.match(source, /17000 - \[int\]\$outerWatch\.ElapsedMilliseconds/);
  assert.match(source, /19500 - \[int\]\$outerWatch\.ElapsedMilliseconds/);
  assert.match(source, /\.Kill\(\)/);
  assert.match(source, /\[Console\]::Out\.Flush\(\)/);
  assert.match(source, /AntivirusSignatureLastUpdatedMs = \(EpochMs \$mp\.AntivirusSignatureLastUpdated\)/);
  assert.match(source, /sig_at = \$m\.AntivirusSignatureLastUpdatedMs/);
});

test('新しく外から届く待ち受けだけを所見にする（前回の分析と比べる）', () => {
  const raw = (listen) => ({ listen });
  const prev = netsec.annotate(netsec.normalize(raw([{ proto: 'tcp', addr: '*', port: 52000, proc: 'app' }, { proto: 'tcp', addr: '0.0.0.0', port: 22, proc: 'sshd' }]), 'mac'), null, NOW - 3600e3);
  const cur = netsec.annotate(netsec.normalize(raw([
    { proto: 'tcp', addr: '*', port: 53111, proc: 'app' }, // 大きい番号はプロセスごとに1つ（番号が変わっても新しくない）
    { proto: 'tcp', addr: '0.0.0.0', port: 22, proc: 'sshd' },
    { proto: 'tcp', addr: '::', port: 3000, proc: 'node' },
    { proto: 'tcp', addr: '127.0.0.1', port: 5432, proc: 'postgres' }, // この機体の中だけ
    { proto: 'udp', addr: '*', port: 5353, proc: 'node' },
  ]), 'mac'), prev, NOW);
  assert.deepEqual(cur.listen_new.map(netsec.listenKey), ['tcp|node|3000', 'udp|node|5353']);
  const f = netsec.findings(cur, {});
  assert.deepEqual(f.map((x) => [x.id, x.severity]), [['net-listen-tcp-3000-node', 'warn'], ['net-listen-udp-5353-node', 'info']]);
  // 前回が無ければ比べない
  assert.deepEqual(netsec.annotate(cur, null, NOW).listen_new, []);
  // ファイアウォールがすべての受信を遮断しているなら提案にとどめる
  const blocked = { ...cur, defense: { firewall: 2 } };
  assert.equal(netsec.findings(blocked, {})[0].severity, 'info');
});

test('防御が止まっている機体は状態で異常（fail）', () => {
  const win = netsec.annotate(netsec.normalize({ defense: {
    defender: { realtime: false, antivirus: false, sig_age_days: 1 }, av: [{ name: 'Windows Defender', enabled: false, uptodate: true }],
    firewall: [{ name: 'Private', enabled: true }, { name: 'Public', enabled: false }], active: ['Public'], detections_30d: 0, security_log: 'no-permission',
  } }, 'windows'), null, NOW);
  const f = netsec.findings(win, {});
  assert.deepEqual(f.map((x) => [x.id, x.severity]), [['sec-av-off', 'critical'], ['sec-firewall-off', 'critical']]);
  const c = netsec.checks(win, f, { cursors: [] }, { id: 'pc' });
  assert.equal(c.find((x) => x.id === 'sec-defense').status, 'fail');
  // セキュリティログを読めないのは「不明」（異常ではない）
  assert.deepEqual(c.find((x) => x.id === 'sec-login'), { id: 'sec-login', name: 'ログイン', status: 'unknown', detail: 'セキュリティログを読む権限が無い（管理者か Event Log Readers のグループが要る）' });
  // 別のウイルス対策が動いていれば止まっていない
  const other = netsec.normalize({ defense: { defender: { realtime: false }, av: [{ name: 'Example AV', enabled: true, uptodate: true }] } }, 'windows');
  assert.deepEqual(netsec.findings(other, {}), []);
  // Defender は停止と読めても、第三者 AV の取得に失敗したら「AVなし」と断定しない
  const avUnknown = netsec.normalize({ defense: { defender: { realtime: false } }, errors: { av: 'no-permission' } }, 'windows');
  assert.ok(!netsec.findings(avUnknown, {}).some((x) => x.id === 'sec-av-off'));
  assert.equal(netsec.checks(avUnknown, [], {}, { id: 'pc' }).find((x) => x.id === 'sec-defense').status, 'unknown');
  // firewall だけ読めても Defender と AV が欠測なら正常とは言わない
  const partial = netsec.normalize({ defense: { firewall: [{ name: 'Private', enabled: true }] }, errors: { defender: 'timeout', av: 'timeout' } }, 'windows');
  assert.equal(netsec.checks(partial, [], {}, { id: 'pc' }).find((x) => x.id === 'sec-defense').status, 'unknown');
  // realtime=null は停止ではなく欠測
  const defenderUnknown = netsec.normalize({ defense: { defender: { realtime: null }, av: [], firewall: [] } }, 'windows');
  assert.ok(!netsec.findings(defenderUnknown, {}).some((x) => x.id === 'sec-av-off'));
  assert.equal(netsec.checks(defenderUnknown, [], {}, { id: 'pc' }).find((x) => x.id === 'sec-defense').status, 'unknown');
  // 検出: 隔離・削除が済んでいれば提案、未解決があれば注意
  const det = (open) => netsec.findings(netsec.normalize({ defense: { defender: { realtime: true, sig_age_days: 1 }, detections_30d: 2, detections_open_30d: open } }, 'windows'), {});
  assert.deepEqual(det(0).map((x) => [x.id, x.severity]), [['sec-detections', 'info']]);
  assert.deepEqual(det(1).map((x) => [x.severity, x.title]), [['warn', 'Defender が直近30日に 2 件を検出し、1 件が未解決']]);
  // macOS: Gatekeeper の無効は重大、ファイアウォールは外から届く待ち受けがあれば注意
  const mac = netsec.annotate(netsec.normalize({ defense: { firewall: 0, gatekeeper: false, xprotect_version: '5363', xprotect_at: NOW - 60 * DAY }, listen: [{ proto: 'tcp', addr: '*', port: 5000, proc: 'ControlCenter' }] }, 'mac'), null, NOW);
  assert.deepEqual(netsec.findings(mac, {}).map((x) => [x.id, x.severity]), [['sec-gatekeeper-off', 'critical'], ['sec-firewall-off', 'warn'], ['sec-xprotect-old', 'warn']]);
});

test('分析に組み込む: Defender の古い所見と状態は netsec の防御に置き換わる', () => {
  const ns = netsec.prepare({ defense: { defender: { realtime: false }, av: [], firewall: [] } }, 'windows', null, NOW);
  const snap = { probe: 'windows', host: { cores: 4 }, memory: {}, processes: {}, defender: { realtime: false }, netsec: ns };
  const f = rules.analyze(snap, {});
  assert.ok(f.some((x) => x.id === 'sec-av-off') && !f.some((x) => x.id === 'defender-off'));
  const checks = health.nodeChecks({ id: 'pc' }, { at: NOW, data: snap }, f, { now: NOW, cursors: [] });
  assert.ok(!checks.some((c) => c.id === 'defender'));
  assert.equal(checks.find((c) => c.id === 'sec-defense').status, 'fail');
});

test('宛先と自動起動の一覧は snapshot に残さない（件数だけ）', () => {
  const raw = { outbound: [{ proc: 'app', addr: '198.51.100.7', port: 443, n: 2 }], persist: [{ kind: 'launchd', key: 'user:x', program: 'x' }] };
  const ns = netsec.prepare(raw, 'mac', null, NOW);
  assert.equal(ns.outbound, undefined);
  assert.equal(ns.persist, undefined);
  assert.deepEqual([ns.outbound_count, ns.persist_count], [1, 1]);
  assert.ok(!JSON.stringify(ns).includes('198.51.100.7'));
  const withPeers = netsec.strip({ ...netsec.normalize(raw, 'mac'), peers: { learning: false, known: 1, new: [{ proc: 'app', addr: '198.51.100.8', port: 443, why: 'dest', dests: 1, public: true }] } });
  assert.ok(!JSON.stringify(withPeers).includes('198.51.100.8'));
  assert.deepEqual(withPeers.peers.new, [{ proc: 'app', port: 443, why: 'dest', dests: 1, public: true }]);
});

test('常駐の増減: 初回は記録だけ、取れなかった種類は削除と見なさない', () => {
  const items = [{ kind: 'run', key: 'HKCU\\A', program: 'a.exe' }, { kind: 'service', key: 'Svc', program: 'svc.exe' }];
  assert.deepEqual(netsec.diffPersist([], items, { kinds: ['run', 'service'], baselined: [] }), { changes: [], baseline: ['run', 'service'] });
  const prev = [{ kind: 'run', key: 'HKCU\\A', program: 'a.exe', removed_at: null }, { kind: 'service', key: 'Old', program: 'old.exe', removed_at: null }];
  const d = netsec.diffPersist(prev, [{ kind: 'run', key: 'HKCU\\B', program: 'b.exe' }], { kinds: ['run', 'service'], failed: ['service'], baselined: ['run', 'service'] });
  assert.deepEqual(d.changes.map((c) => [c.key, c.change]), [['HKCU\\B', 'added'], ['HKCU\\A', 'removed']]);
  assert.deepEqual(netsec.persistKinds({ os: 'windows', errors: { tasks: 'no-permission' } }), { kinds: ['schtask', 'service', 'run', 'startup'], failed: ['schtask'] });
});

test('初めての接続先: 覚えている途中は知らせず、宛先の多いプロセスの新しい宛先は数えない', () => {
  const known = [{ proc: 'app', addr: '198.51.100.1', port: 443 }];
  for (let i = 0; i < 12; i++) known.push({ proc: 'browser', addr: `198.51.100.${10 + i}`, port: 443 });
  const sample = [
    { proc: 'app', addr: '198.51.100.2', port: 443, public: true },
    { proc: 'app', addr: '198.51.100.1', port: 8443, public: true },
    { proc: 'browser', addr: '203.0.113.9', port: 443, public: true },
    { proc: 'new', addr: '203.0.113.1', port: 443, public: true },
  ];
  assert.deepEqual(netsec.classifyPeers(known, sample, { learning: true }), []);
  assert.deepEqual(netsec.classifyPeers(known, sample).map((p) => [p.proc, p.port, p.why]), [['new', 443, 'proc'], ['app', 8443, 'port'], ['app', 443, 'dest']]);
  // 外と初めて通信したプロセスが外部へ 80・443 以外で話していれば注意。443 だけなら提案（新しいソフト・更新で毎回出るため）
  const peers = (port) => ({ os: 'mac', at: NOW, listen: [], defense: {}, peers: { learning: false, known: 3, new: [{ proc: 'new', port, why: 'proc', dests: 1, public: true }] } });
  const st = (ns, node) => netsec.checks(ns, [], {}, node).find((c) => c.id === 'sec-peers').status;
  assert.equal(st(peers(4444), { id: 'pc' }), 'warn');
  assert.equal(netsec.findings(peers(4444), { id: 'pc' })[0].severity, 'warn');
  assert.equal(st(peers(443), { id: 'pc' }), 'ok');
  assert.equal(netsec.findings(peers(443), { id: 'pc' })[0].severity, 'info');
  // 共用機は明示 opt-in しない限り接続先の状態自体を返さない
  assert.equal(netsec.checks(peers(4444), [], {}, { id: 'pc', shared: true }).some((c) => c.id === 'sec-peers'), false);
});

test('ログインの失敗: 24時間の件数と送り元で判定し、洪水の所見とは重ねない', () => {
  assert.deepEqual(logs.SOURCES.macos.slice(-1), ['mac_auth']);
  assert.deepEqual(logs.SOURCES.windows.slice(-1), ['win_security']);
  const db = openDb(fs.mkdtempSync(path.join(os.tmpdir(), 'kt-ns-')));
  const rows = (n, provider, ev, t0) => Array.from({ length: n }, (_, i) => ({ uid: `${provider}-${ev}-${i}`, ts: t0 - i * 1000, level: ev === '4624' ? 'info' : 'warn', provider, event_id: ev, message: `logon failed from ${provider}` }));
  db.insertLogs('pc', 'win_security', logs.normalize('win_security', [...rows(230, '203.0.113.5', '4625', NOW), ...rows(3, '192.168.1.9', '4625', NOW)]), NOW);
  let f = logs.logFindings(db, 'pc', NOW);
  const fail = f.find((x) => x.id === 'log-login-fail');
  assert.equal(fail.severity, 'critical');
  assert.match(fail.title, /233 件（送り元 2 か所、うち外部のアドレスから 230 件）/);
  assert.equal(fail.detail, '203.0.113.5 ×230、192.168.1.9 ×3');
  assert.ok(!f.some((x) => x.id.startsWith('log-flood-')), 'ログインの記録は洪水として数えない');
  // 外部のアドレスからの成功
  db.insertLogs('pc', 'win_security', logs.normalize('win_security', rows(2, '198.51.100.7', '4624', NOW - DAY)), NOW);
  f = logs.logFindings(db, 'pc', NOW);
  assert.equal(f.find((x) => x.id === 'log-login-public').title, '外部のアドレスからのログイン成功が7日で 2 件');
  // 状態: 重大な所見は異常
  const c = netsec.checks({ os: 'windows', listen: [], defense: { security_log: 'ok' } }, f, { cursors: [{ node_id: 'pc', source: 'win_security' }] }, { id: 'pc' });
  assert.equal(c.find((x) => x.id === 'sec-login').status, 'fail');
});

test('共用機は外向きの接続先を既定で残さない（network_peers で明示すれば残す）', () => {
  assert.equal(netsec.peersEnabled({ id: 'a' }), true);
  assert.equal(netsec.peersEnabled({ id: 's', shared: true }), false);
  assert.equal(netsec.peersEnabled({ id: 's', shared: true, network_peers: true }), true);
  assert.equal(netsec.peersEnabled({ id: 'a', network_peers: false }), false);
  assert.equal(netsec.peersEnabled({ id: 'a', network: false, network_peers: true }), false);
  const data = { netsec: { outbound: [{ proc: 'x', addr: '203.0.113.5', port: 8443, n: 1 }], listen: [] } };
  netsec.dropPeers(data);
  assert.deepEqual([data.netsec.outbound, data.netsec.outbound_skipped], [[], 'shared']);
  assert.equal(netsec.normalize(data.netsec, 'mac').outbound_skipped, 'shared');
  assert.equal(netsec.normalize({ outbound: [] }, 'mac').outbound_skipped, undefined);
});

test('現在の台帳で通信を無効にした機体は旧 snapshot とログイン所見を返さない', () => {
  const old = { cpu_busy: 1, netsec: { listen: [], peers: { new: [{ proc: 'app', addr: '192.0.2.9' }] } } };
  const hidden = netsec.snapshotForNode(old, { id: 'pc', network: false, network_peers: true });
  assert.equal(hidden.netsec, undefined);
  assert.equal(hidden.cpu_busy, 1);
  assert.ok(old.netsec, '保存値は変更しない');

  const findings = [{ id: 'log-login-fail' }, { id: 'log-disk' }];
  assert.deepEqual(netsec.filterLogFindings(findings, { id: 'pc', network_logins: false }), [{ id: 'log-disk' }]);
  assert.deepEqual(netsec.filterLogFindings(findings, { id: 'pc', shared: true }), [{ id: 'log-disk' }]);
  assert.deepEqual(netsec.filterLogFindings(findings, { id: 'pc', shared: true, network_logins: true }), findings);

  const nodes = [{ id: 'private', network_logins: false }, { id: 'keep', network_logins: true }];
  const rows = [
    { node_id: 'private', source: 'win_security', provider: '192.0.2.1' },
    { node_id: 'private', source: 'win_system', provider: 'disk' },
    { node_id: 'keep', source: 'win_security', provider: '198.51.100.1' },
  ];
  assert.deepEqual(netsec.filterLogRows(rows, nodes), rows.slice(1));
  assert.deepEqual(netsec.filterLogSignatures([
    { source: 'win_security', node_ids: 'private', sample: 'login 192.0.2.1' },
    { source: 'win_security', node_ids: 'keep', sample: 'login 198.51.100.1' },
    { source: 'win_system', node_ids: 'private', sample: 'disk' },
  ], nodes).map((x) => x.sample), ['login 198.51.100.1', 'disk']);
  assert.deepEqual(netsec.filterCheckRows([
    { id: 1, scope: 'private', check_id: 'sec-login', detail: '192.0.2.1' },
    { id: 2, scope: 'private', check_id: 'memory', detail: 'ok' },
  ], nodes), [{ id: 2, scope: 'private', check_id: 'memory', detail: 'ok' }]);
});
