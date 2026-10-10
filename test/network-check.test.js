'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { inspect, probeHttps } = require('../scripts/network-check');
const result = (stdout = '', status = 0) => ({ stdout, status });
const routeTable = (family, active = '', persistent = '', localized = false) => `IPv${family} ${localized ? 'ルート テーブル' : 'Route Table'}\n===========\n${localized ? 'アクティブ ルート' : 'Active Routes'}:\n${active}\n===========\n${localized ? '固定ルート' : 'Persistent Routes'}:\n${persistent}\n`;

test('Node互換CLIも全IPv6リンクローカル・未指定・loopbackを利用可能にしない', () => {
  for(const address of ['fe80::1','fe90::1','febf:ffff::1','fe80::1%en0','::','::1','0:0:0:0:0:0:0:1','0.0.0.0','127.0.0.2','169.254.1.2','::ffff:127.0.0.1','::ffff:169.254.1.2','ff02::1','invalid','2001:db8::1','fec0::1','192.0.2.8']) {
    const value=inspect('darwin',cmd=>cmd==='route' ? result('interface: en0') : cmd==='ifconfig' ? result(`en0: flags=1<UP>\n inet6 ${address}`) : result(''));
    assert.equal(value.ip_address_present,['2001:db8::1','fec0::1','192.0.2.8'].includes(address),address);
  }
});

test('接続断・経路欠落でもDNS状態を取り、IPやSSIDを出力しない', () => {
  const calls = [];
  const execute = (cmd) => {
    calls.push(cmd);
    if (cmd === 'route') return result('', 1);
    if (cmd === 'scutil') return result('resolver #1\n  nameserver[0] : 192.0.2.9\n');
    return result('');
  };
  const value = inspect('darwin', execute);
  assert.equal(value.default_route_present, false);
  assert.equal(value.dns_configured, true);
  assert.ok(calls.includes('scutil'));
  assert.doesNotMatch(JSON.stringify(value), /192\.0\.2|SSID|nameserver\[/);
});

test('コマンド失敗を正常ゼロと混同しない', () => {
  const value = inspect('darwin', () => result('', null));
  assert.equal(value.default_route_present, null);
  assert.equal(value.dns_configured, null);
});

test('Windowsのactive interface・gateway・DNSだけを返す', () => {
  const value = inspect('win32', cmd => result(cmd === 'route.exe' ? routeTable(4, '  0.0.0.0  0.0.0.0  192.0.2.1  192.0.2.2  10') : '{"interfaces_up":1,"ip_address_present":true,"gateway_configured":true,"dns_configured":false}'));
  assert.equal(value.interfaces_up, 1);
  assert.equal(value.default_route_present, true);
  assert.equal(value.dns_configured, false);
});

test('HTTPS診断は証明書検証を維持してDNS失敗とTLS失敗を分ける', () => {
  const commands = [];
  let count = 0;
  const value = probeHttps((cmd, args) => {
    commands.push(args);
    return result('', ++count === 1 ? 6 : 60);
  });
  assert.equal(value.named.state, 'dns_failed');
  assert.equal(value.fixed_ip.state, 'tls_certificate_failed');
  assert.ok(commands.every(args => args[0] === '-q' && !args.includes('-k') && args.includes('--max-time') && args.includes('--output')));
});


test('成功終了でも解析できない経路・DNS出力は未観測にする', () => {
  const value = inspect('darwin', () => result('unexpected output'));
  assert.equal(value.default_route_present, null);
  assert.equal(value.dns_configured, null);
});

test('DNSが無いというOSの明示出力だけをfalseにする', () => {
  const value = inspect('darwin', cmd => result(cmd === 'scutil' ? 'No DNS configuration available' : '', 0));
  assert.equal(value.dns_configured, false);
});


test('Windowsのgateway設定と未知の経路表を経路ありと混同しない', () => {
  const value = inspect('win32', cmd => result(cmd === 'route.exe' ? 'unexpected output' : '{"gateway_configured":true,"dns_configured":true}'));
  assert.equal(value.gateway_configured, true);
  assert.equal(value.default_route_present, null);
});

test('WindowsのIPv6 VPN既定経路もgateway設定なしで検出する', () => {
  const value = inspect('win32', (cmd, args) => result(cmd === 'route.exe' && args.includes('-6') ? routeTable(6, '  12  25  ::/0  On-link') : '{}'));
  assert.equal(value.default_route_present, true);
});


test('WindowsのNIC取得失敗も全項目をnullとして残す', () => {
  const value = inspect('win32', () => result('', null));
  for (const key of ['gateway_configured', 'interfaces_up', 'ip_address_present', 'default_route_present', 'dns_configured']) assert.equal(value[key], null);
});

test('Windowsの固定ルートだけでは現在の既定経路ありと判定しない', () => {
  const value = inspect('win32', (cmd, args) => result(cmd === 'route.exe' && args.includes('-4') ? '固定ルート:\n  0.0.0.0  0.0.0.0  192.0.2.1  10\n' : '{}'));
  assert.equal(value.default_route_present, null);
});

test('MacのIPはリンクローカル・ループバックだけなら利用可能と判定しない', () => {
  for (const addresses of ['inet 169.254.1.2\n inet6 fe80::1', 'inet 127.0.0.1\n inet6 ::1']) {
    const value = inspect('darwin', cmd => result(cmd === 'route' ? 'interface: en0' : cmd === 'ifconfig' ? `en0: flags=8863<UP,RUNNING>\n ${addresses}\n status: active` : ''));
    assert.equal(value.ip_address_present, false);
  }
  const unknown = inspect('darwin', cmd => result(cmd === 'route' ? 'interface: en0' : '', cmd === 'ifconfig' ? null : 0));
  assert.equal(unknown.interface_up, null);
  assert.equal(unknown.link_active, null);
});


test('WindowsのIPv6固定ルートを言語によらず現在の経路と混同しない', () => {
  for (const localized of [false, true]) {
    const value = inspect('win32', (cmd, args) => result(cmd === 'route.exe' && args.includes('-6') ? routeTable(6, '  1  331  ::1/128  On-link', '  12  25  ::/0  On-link', localized) : '{}'));
    assert.equal(value.default_route_present, null);
    const active = inspect('win32', (cmd, args) => result(cmd === 'route.exe' && args.includes('-6') ? routeTable(6, '  12  25  ::/0  On-link', '', localized) : '{}'));
    assert.equal(active.default_route_present, true);
  }
});

test('区切りのないWindows経路出力と未知のMac ifconfig形式は未観測', () => {
  const windows = inspect('win32', cmd => result(cmd === 'route.exe' ? ' 12 25 ::/0 On-link' : '{}'));
  assert.equal(windows.default_route_present, null);
  const mac = inspect('darwin', cmd => result(cmd === 'route' ? 'interface: en0' : 'unexpected format'));
  assert.equal(mac.interface_up, null);
  assert.equal(mac.link_active, null);
  assert.equal(mac.ip_address_present, null);
});
