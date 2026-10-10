'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync, spawn } = require('node:child_process');
const ps = path.resolve('probes/win_network.ps1');
const py = path.resolve('probes/mac_network.py');
const python = process.platform === 'win32' ? 'python' : 'python3';
const pythonReady = spawnSync(python, ['--version'], { windowsHide: true }).status === 0;
function fixture(code) {
  const source = `import importlib.util,json\ns=importlib.util.spec_from_file_location('n',${JSON.stringify(py.replaceAll('\\','/'))});n=importlib.util.module_from_spec(s);s.loader.exec_module(n)\n${code}`;
  const r = spawnSync(python, ['-c', source], { encoding: 'utf8', timeout: 5000, windowsHide: true });
  assert.equal(r.status, 0, r.stderr); return JSON.parse(r.stdout);
}
test('native probes keep PS5.1 format and avoid broad inventories and mutable commands', () => {
  const b = fs.readFileSync(ps);
  assert.deepEqual([...b.subarray(0, 3)], [239,187,191]);
  assert.ok([...b.subarray(3)].every(n => n < 128));
  assert.ok(!b.subarray(3).toString().includes("'"));
  for (const source of [fs.readFileSync(ps,'utf8'), fs.readFileSync(py,'utf8')]) {
    assert.doesNotMatch(source, /Get-CimInstance|Get-WmiObject|Get-NetTCPConnection|Get-WinEvent|Set-Net|netsh|Win32_Service|schtasks|security find|lsof/);
    assert.match(source, /--max-time/);
  }
});
test('Mac uses an active IPv6 route and emits only state, with multiple IP families', {skip:!pythonReady}, () => {
  const r=fixture(`def fake(a):\n if a[0].endswith('route'): return (1,'') if '-inet6' not in a else (0,' interface: en0\\n gateway: fe80::123')\n if a[0].endswith('ifconfig'): return (0,'en0: flags=8863<UP,BROADCAST,RUNNING>\\n inet6 fe80::12\\n inet 192.0.2.8\\n status: active')\n return (0,' nameserver[0] : 192.0.2.9')\nprint(json.dumps(n.inspect(fake)))`);
  assert.equal(r.default_route_present,true); assert.equal(r.interface_up,true); assert.equal(r.ip_address_present,true); assert.equal(r.dns_configured,true);
  assert.doesNotMatch(JSON.stringify(r), /192\.0\.2|fe80|en0|gateway:/);
});
test('Mac distinguishes command errors, absent routes and link-local-only configuration', {skip:!pythonReady}, () => {
  const unknown=fixture("print(json.dumps(n.inspect(lambda a:(None,''))))");
  assert.equal(unknown.default_route_present,null); assert.equal(unknown.dns_configured,null);
  const absent=fixture("print(json.dumps(n.inspect(lambda a:(1,'') if a[0].endswith('route') else (0,'No DNS configuration available'))))");
  assert.equal(absent.default_route_present,false); assert.equal(absent.dns_configured,false);
  const local=fixture("print(json.dumps(n.inspect(lambda a:(0,'interface: en0') if a[0].endswith('route') else (0,'en0: flags=1<UP>\\n inet6 fe80::12\\n inet 169.254.1.2') if a[0].endswith('ifconfig') else (None,''))))");
  assert.equal(local.ip_address_present,false);
});
test('Mac curl results preserve HTTP status and cumulative timings, discard raw errors', {skip:!pythonReady}, () => {
  const out=fixture("print(json.dumps([n.https('https://example.test/',lambda a:(0,'403 0.001 0.002 0.003 0.004 0.005')),n.https('https://example.test/',lambda a:(60,'private-error'))]))");
  assert.equal(out[0].http_status,403); assert.equal(out[0].timings.total_ms,5); assert.equal(out[0].timings.dns_ms,1);
  assert.equal(out[1].state,'tls_certificate_failed'); assert.doesNotMatch(JSON.stringify(out),/private-error/);
});
test('Windows native parser excludes persistent routes regardless of localized headings', {skip:process.platform!=='win32'}, () => {
  const source=`. ${JSON.stringify(ps)}; $t="IPv4 ルート テーブル\n===========\nアクティブ ルート:\n  192.0.2.0  255.255.255.0  On-link  192.0.2.8  10\n===========\n固定ルート:\n  0.0.0.0  0.0.0.0  192.0.2.1  192.0.2.8  10\n"; [ordered]@{active=(Active-Table @{status=0;out=$t} 4);unknown=(Active-Table @{status=$null;out=$t} 4)}|ConvertTo-Json -Compress`;
  const r=spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(source,'utf16le').toString('base64')],{encoding:'utf8',timeout:10000,windowsHide:true});
  assert.equal(r.status,0,r.stderr); const d=JSON.parse(r.stdout.trim()); assert.match(d.active,/192\.0\.2\.0/); assert.doesNotMatch(d.active,/0\.0\.0\.0/); assert.equal(d.unknown,'');
});
test('Windows recognizes absent default routes only when both active family formats are known', {skip:process.platform!=='win32'}, () => {
  const source=`. ${JSON.stringify(ps)}; $v4="  127.0.0.0  255.0.0.0  On-link  127.0.0.1  10"; $v6="  1  331  ::1/128  On-link"; @{absent=(Route-State $v4 $v6);unknown=(Route-State $v4 "");vpn=(Route-State $v4 "  8  25  ::/0  On-link")}|ConvertTo-Json -Compress`;
  const r=spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(source,'utf16le').toString('base64')],{encoding:'utf8',timeout:10000,windowsHide:true});
  assert.equal(r.status,0,r.stderr); const d=JSON.parse(r.stdout.trim()); assert.equal(d.absent,false); assert.equal(d.unknown,null); assert.equal(d.vpn,true);
});
test('Windows stdin loader receives the complete embedded probe with a short command line', {skip:process.platform!=='win32'}, () => {
  const core=fs.readFileSync('crates/tune-core/src/network.rs','utf8');
  const loader=/let loader = "([^"]+)";/.exec(core)?.[1]; assert.ok(loader);
  const encoded=Buffer.from(loader,'utf16le').toString('base64');
  assert.ok(encoded.length<2000);
  const source=`& { ${fs.readFileSync(ps,'utf8').replace(/^\uFEFF/,'')}\n }; [Console]::WriteLine("stdin_complete")`;
  assert.ok(Buffer.from(source,'utf16le').toString('base64').length>8191);
  const r=spawnSync('powershell.exe',['-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand',encoded],{input:Buffer.from(source,'utf8').toString('base64')+'\n',encoding:'utf8',timeout:20000,windowsHide:true});
  assert.equal(r.status,0,r.stderr); const lines=r.stdout.trim().split(/\r?\n/);
  assert.equal(lines.pop(),'stdin_complete'); const data=JSON.parse(lines.pop());
  assert.equal(data.schema,'katala_network_check.v1'); assert.equal(data.platform,'win32'); assert.equal(data.active_probes,false);
});

test('Windows loader finishes a complete probe while the sender keeps stdin open', {skip:process.platform!=='win32'}, async () => {
  const loader=/let loader = "([^"]+)";/.exec(fs.readFileSync('crates/tune-core/src/network.rs','utf8'))?.[1];assert.ok(loader);
  const source=`& { ${fs.readFileSync(ps,'utf8').replace(/^\uFEFF/,'')}\n }; [Console]::WriteLine("line_complete")`;
  const child=spawn('powershell.exe',['-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(loader,'utf16le').toString('base64')],{stdio:['pipe','pipe','pipe'],windowsHide:true});
  let out='',err='',timer;
  child.stdout.setEncoding('utf8');child.stdout.on('data',v=>{out+=v;});
  child.stderr.setEncoding('utf8');child.stderr.on('data',v=>{err+=v;});
  child.stdin.on('error',()=>{});
  try {
    const code=await new Promise((resolve,reject)=>{
      child.once('error',reject);child.once('close',resolve);
      timer=setTimeout(()=>{child.kill();reject(Error('Loader waited for stdin EOF'));},15000);
      child.stdin.write(Buffer.from(source,'utf8').toString('base64')+'\n');
    });
    assert.equal(code,0,err);assert.equal(child.stdin.writableEnded,false);
    const lines=out.trim().split(/\r?\n/);assert.equal(lines.pop(),'line_complete');
    assert.equal(JSON.parse(lines.pop()).schema,'katala_network_check.v1');
  }finally{clearTimeout(timer);child.stdin.destroy();}
});

test('Windows loader never accepts empty or malformed input as a network report', {skip:process.platform!=='win32'}, () => {
  const loader=/let loader = "([^"]+)";/.exec(fs.readFileSync('crates/tune-core/src/network.rs','utf8'))?.[1];assert.ok(loader);
  for(const input of ['\n','invalid!\n']){
    const r=spawnSync('powershell.exe',['-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(loader,'utf16le').toString('base64')],{input,encoding:'utf8',timeout:5000,windowsHide:true});
    assert.equal(r.error,undefined);assert.doesNotMatch(r.stdout,/katala_network_check\.v1/);
  }
});
