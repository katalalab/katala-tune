#!/usr/bin/env node
'use strict';
// 手動実行だけ。設定・サービスを変えず、IP・SSID・DNS名・通信本文を出力しない。
const { spawnSync } = require('node:child_process');
function execute(command, args) {
  return spawnSync(command, args, { encoding: 'utf8', timeout: 5000, maxBuffer: 128 * 1024, windowsHide: true });
}
function inspect(platform = process.platform, run = execute) {
  const value = { platform, default_route_present: null, ip_address_present: null, dns_configured: null };
  if (platform === 'darwin') {
    let route = run('route', ['-n', 'get', 'default']);
    if (route.status !== 0) {
      const ipv6 = run('route', ['-n', 'get', '-inet6', 'default']);
      if (ipv6.status === 0) route = ipv6;
      else if (route.status !== 1 || ipv6.status !== 1) route = { status: null };
    }
    if (route.status === 0) {
      const iface = /interface:\s*([a-zA-Z0-9]+)\b/.exec(route.stdout || '')?.[1];
      if (iface) value.default_route_present = true;
      if (iface) {
        const link = run('ifconfig', [iface]);
        if (link.status === 0) {
          const text = link.stdout || '';
          if (/^\w+: flags=/m.test(text)) {
            value.interface_up = /<[^>]*\bUP\b[^>]*>/.test(text);
            value.link_active = /status:\s*active\b/.test(text) ? true : /status:\s*inactive\b/.test(text) ? false : null;
            value.ip_address_present = /\binet\s/.test(text) || /\binet6\s+(?!fe80:)/i.test(text);
          }
        } else value.link_active = null;
      }
    } else if (route.status === 1) value.default_route_present = false;
    const dns = run('scutil', ['--dns']);
    if (dns.status === 0) {
      const text = dns.stdout || '';
      if (/nameserver\[\d+\]/.test(text)) value.dns_configured = true;
      else if (/^No DNS configuration available\s*$/.test(text.trim())) value.dns_configured = false;
    }
  } else if (platform === 'win32') {
    // .NETのNIC状態を読む。CIM棚卸しを避け、仮想NICを含む接続中インターフェースだけを数える。
    const source = '[Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false);$ErrorActionPreference="Stop";$a=@([Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()|Where-Object {$_.OperationalStatus -eq "Up" -and $_.NetworkInterfaceType -ne "Loopback"});$p=@($a|ForEach-Object {$_.GetIPProperties()});$ip=@($p|ForEach-Object {$_.UnicastAddresses}|Where-Object {$_.Address.ToString() -notmatch "^(fe80:|169[.]254[.]|127[.]|::1$)"});$g=@($p|ForEach-Object {$_.GatewayAddresses}|Where-Object {$_.Address.ToString() -notin @("0.0.0.0","::")});$d=@($p|ForEach-Object {$_.DnsAddresses});@{interfaces_up=$a.Count;ip_address_present=($ip.Count -gt 0);default_route_present=($g.Count -gt 0);dns_configured=($d.Count -gt 0)}|ConvertTo-Json -Compress';
    const res = run('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(source, 'utf16le').toString('base64')]);
    if (res.status === 0) {
      try {
        const data = JSON.parse((res.stdout || '').trim().replace(/^\uFEFF/, ''));
        for (const key of ['ip_address_present', 'default_route_present', 'dns_configured']) {
          if (typeof data[key] === 'boolean') value[key] = data[key];
        }
        if (Number.isSafeInteger(data.interfaces_up) && data.interfaces_up >= 0) value.interfaces_up = data.interfaces_up;
      } catch { /* 未観測として null を保つ */ }
    }
  } else value.unsupported_platform = true;
  return value;
}
function probeHttps(run = execute) {
  const errors = { 6: 'dns_failed', 7: 'connect_failed', 28: 'timed_out', 35: 'tls_failed', 60: 'tls_certificate_failed' };
  const one = (url) => {
    const res = run(process.platform === 'win32' ? 'curl.exe' : 'curl', ['--proto', '=https', '--tlsv1.2', '--connect-timeout', '2', '--max-time', '4', '--silent', '--output', process.platform === 'win32' ? 'NUL' : '/dev/null', '--write-out', '%{http_code}', url]);
    const http = Number((res.stdout || '').trim());
    return res.status === 0 && Number.isInteger(http) && http >= 100 && http <= 599
      ? { state: 'reachable', http_status: http }
      : { state: errors[res.status] || (res.error?.code === 'ETIMEDOUT' ? 'timed_out' : res.error?.code === 'ENOENT' ? 'tool_missing' : 'unavailable') };
  };
  return { named: one('https://www.apple.com/'), fixed_ip: one('https://1.1.1.1/cdn-cgi/trace') };
}
if (require.main === module) {
  const args = process.argv.slice(2);
  if (args.some(arg => arg !== '--probe')) {
    console.error('使い方: npm run network -- [--probe]');
    process.exitCode = 2;
  } else {
    const value = { schema: 'katala_network_check.v1', observed_at: new Date().toISOString(), ...inspect() };
    if (args.includes('--probe')) value.https = probeHttps();
    else value.active_probes = false;
    console.log(JSON.stringify(value));
  }
}
module.exports = { inspect, probeHttps };
