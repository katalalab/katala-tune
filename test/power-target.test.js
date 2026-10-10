// 実際の対象側PowerShellを実行する。OS/GPUコマンドだけを偽物にし、設定やドライバーには触れない。
'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync, spawn } = require('node:child_process');
const { plan, execute } = require('../lib/actions');
const ps = process.platform === 'win32' ? 'powershell.exe' : 'pwsh';
const present = spawnSync(ps,['-NoProfile','-Command','exit 0']).status === 0;
const node = {id:'test-node',os:'windows',local:true};
const uuid='GPU-01234567-89ab-cdef-0123-456789abcdef';
const prev='01234567-89ab-cdef-0123-456789abcdef',desired='abcdef01-2345-6789-abcd-ef0123456789',third='98765432-10ab-cdef-0123-456789abcdef';
function run(kind, mode, busy=false) {
  const action = kind==='gpu' ? {type:'set-gpu-power-limit',params:{uuid,watts:250,min:100,max:300,prev_w:200}} : {type:'set-power-plan',params:{guid:desired,prev_guid:prev}};
  const p=plan(node,action,{protect:[]});
  // busy はWaitOneの取得拒否を差し込み、書込み口に到達しないことを確かめる。
  const body=busy ? p.script.replace('$mutex.WaitOne(0)','$false') : p.script;
  const setup=`$script:state = ${kind==='gpu' ? '200' : '"'+prev+'"'}; $script:writes = @(); $script:reads = 0;
function nvidia-smi.exe {
 if ($args -contains "-pl") { $v=[double]$args[([array]::IndexOf($args,"-pl")+1)]; $script:writes += $v; $script:state=$v;
  if ("${mode}" -eq "third") {$script:state=225}; if ("${mode}" -eq "nochange") {$script:state=200}; $global:LASTEXITCODE=0;
  if ("${mode}" -eq "applyfail" -and $script:writes.Count -eq 1) {$global:LASTEXITCODE=1}; return }
 $script:reads++; $global:LASTEXITCODE=0;
 if (("${mode}" -eq "readfail" -and $script:writes.Count -gt 0) -or ("${mode}" -eq "retry" -and $script:reads -eq 2)) {$global:LASTEXITCODE=1;return};
 Write-Output "100, 300, $script:state"
}
function powercfg.exe {
 if ($args -contains "-setactive") { $script:state=$args[1]; $script:writes += $script:state;
  if ("${mode}" -eq "third") {$script:state="${third}"}; if ("${mode}" -eq "nochange") {$script:state="${prev}"}; $global:LASTEXITCODE=0;
  if ("${mode}" -eq "applyfail" -and $script:writes.Count -eq 1) {$global:LASTEXITCODE=1};return }
 $script:reads++; $global:LASTEXITCODE=0;
 if (("${mode}" -eq "readfail" -and $script:writes.Count -gt 0) -or ("${mode}" -eq "retry" -and $script:reads -eq 2)) {$global:LASTEXITCODE=1;return};
 Write-Output "Power Scheme GUID: $script:state (Test)"
}
try { ${body} } finally { Write-Output ("TRACE:" + (@{state=$script:state;writes=@($script:writes);reads=$script:reads}|ConvertTo-Json -Compress)) }`;
  const r=spawnSync(ps,['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(setup,'utf16le').toString('base64')],{encoding:'utf8',timeout:15000});
  const trace=r.stdout?.split(/\r?\n/).find(s=>s.startsWith('TRACE:'));
  assert.ok(trace,`${r.stderr}\n${r.stdout}`);
  return {code:r.status,trace:JSON.parse(trace.slice(6)),action};
}
for (const kind of ['gpu','plan']) {
 test(`${kind}: target script validates success, retries and restores changed+failed apply`,{skip:!present},()=>{
  const target=kind==='gpu'?250:desired,old=kind==='gpu'?200:prev;
  const a=run(kind,'success');assert.equal(a.code,0);assert.equal(a.trace.state,target);assert.equal(a.trace.writes.length,1);
  const b=run(kind,'retry');assert.equal(b.code,0);assert.equal(b.trace.state,target);assert.ok(b.trace.reads>=3);
  const n=run(kind,'nochange');assert.equal(n.code,6);assert.equal(n.trace.state,old);assert.equal(n.trace.writes.length,1);
  const c=run(kind,'applyfail');assert.equal(c.code,6);assert.equal(c.trace.state,old);assert.deepEqual(c.trace.writes,[target,old]);
 });
 test(`${kind}: third-party changes and missing readback are retained, busy has no undo`,{skip:!present},async()=>{
  const a=run(kind,'third');assert.equal(a.code,7);assert.equal(a.trace.state,kind==='gpu'?225:third);assert.equal(a.trace.writes.length,1);
  const b=run(kind,'readfail');assert.equal(b.code,7);assert.equal(b.trace.writes.length,1);
  const c=run(kind,'success',true);assert.equal(c.code,8);assert.equal(c.trace.writes.length,0);
  const r=await execute(node,c.action,{protect:[]},async()=>({code:8,out:'busy',err:''}));assert.equal(r.undo,null);
 });
}

test('two native clients contend for the real target mutex; losing client performs no write',{skip:!present},async()=>{
 const code='$m=New-Object System.Threading.Mutex($false,"Global\\KatalaTunePowerControl");$held=$m.WaitOne(0);if(-not $held){exit 8};try{[Console]::WriteLine("READY");[Console]::ReadLine()|Out-Null}finally{$m.ReleaseMutex();$m.Dispose()}';
 const holder=spawn(ps,['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(code,'utf16le').toString('base64')],{windowsHide:true,stdio:['pipe','pipe','pipe']});
 try {
  await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('mutex holder failed to become ready')),8000);holder.stdout.on('data',d=>{if(String(d).includes('READY')){clearTimeout(timer);resolve();}});holder.once('error',reject);holder.once('exit',c=>{if(c)reject(Error('holder exit '+c));});});
  const r=run('gpu','success');assert.equal(r.code,8);assert.equal(r.trace.writes.length,0);
 }finally { holder.stdin.end('\n');await new Promise(resolve=>{holder.once('exit',resolve);setTimeout(()=>{holder.kill();resolve();},1500).unref();}); }
 const next=run('gpu','success');assert.equal(next.code,0);
});
