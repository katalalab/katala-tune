'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { inspect, probeHttps } = require('../scripts/network-check');
const result = (stdout = '', status = 0) => ({ stdout, status });

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
  const value = inspect('win32', () => result('{"interfaces_up":1,"ip_address_present":true,"default_route_present":true,"dns_configured":false}'));
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
  assert.ok(commands.every(args => !args.includes('-k') && args.includes('--max-time') && args.includes('--output')));
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
