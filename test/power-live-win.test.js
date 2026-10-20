'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const {spawnSync} = require('node:child_process');
const vramFixtures = require('./fixtures/vram-slots.json');

test('PS GPU parser shares zero, N/A and driver-reserved VRAM fixtures with core and renderer', {skip:process.platform !== 'win32'}, () => {
  const source=fs.readFileSync('probes/live_win.ps1','utf8');
  const helpers=source.slice(source.indexOf('function ParseGpuRows'),source.indexOf('# ---- Top processes'));
  const jsonHelpers=source.slice(source.indexOf('function N('),source.indexOf('function OpenCat'));
  const cases=vramFixtures.filter(f => ['zero-reserved','reserved','n-a'].includes(f.name));
  const rows=cases.map(f => {
    const g=f.gpus[0], cell=v => v == null ? 'N/A' : v;
    return [g.adapter_index,'Fixture GPU',0,cell(g.mem_used_mb),cell(g.mem_total_mb),cell(g.mem_free_mb),'N/A','N/A','N/A','N/A',g.uuid,'P8','N/A','N/A','N/A','N/A',1,0].join(', ');
  });
  const script=`$ErrorActionPreference="Stop"
$inv=[Globalization.CultureInfo]::InvariantCulture
${jsonHelpers}
${helpers}
@(${rows.map(row => `ParseGpuRows ${JSON.stringify(row)} 123`).join('\n')})|ConvertTo-Json -Compress
`;
  const r=spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(script,'utf16le').toString('base64')],{encoding:'utf8',timeout:15000,windowsHide:true});
  assert.equal(r.status,0,r.stderr);
  const result=JSON.parse(r.stdout.trim());
  for(let i=0;i<cases.length;i++) {
    const actual=JSON.parse(result[i])[0], expected=cases[i].gpus[0];
    for(const k of ['adapter_index','mem_used_mb','mem_total_mb','mem_free_mb','mem_used_pct']) assert.equal(actual[k],expected[k],`${cases[i].name}: ${k}`);
  }
});

test('PS5.1 bounded GPU query keeps adapter identity, N/A null and expires cached values', {skip:process.platform !== 'win32'}, () => {
  const source=fs.readFileSync('probes/live_win.ps1','utf8');
  const helpers=source.slice(source.indexOf('function ParseGpuRows'),source.indexOf('# ---- Top processes'));
  const jsonHelpers=source.slice(source.indexOf('function N('),source.indexOf('function OpenCat'));
  const script=`$ErrorActionPreference="Stop"
$inv=[Globalization.CultureInfo]::InvariantCulture
${jsonHelpers}
${helpers}
$gpuRows=ParseGpuRows "0, Test GPU A, 10, 50, 100, 49, 40, 25, 500, 800, GPU-test-a, P8, 100, 80, 120, 500, 1, 0\n1, Test GPU B, N/A, N/A, 200, N/A, N/A, N/A, N/A, N/A, GPU-test-b, P8, N/A, N/A, N/A, N/A, 1, 0" 123
$gpuMaxAgeMs=9000;$gpuReceived=1000;$gpuReason="query-timeout"
$gpuSampleClock=[pscustomobject]@{Elapsed=[timespan]::FromMilliseconds(1000)}
$fresh=GpuJson
$gpuSampleClock.Elapsed=[timespan]::FromMilliseconds(10000);$boundary=GpuJson
$gpuSampleClock.Elapsed=[timespan]::FromMilliseconds(10001);$expired=GpuJson
[ordered]@{fresh=$fresh;boundary=$boundary;expired=$expired}|ConvertTo-Json -Compress
`;
  const r=spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(script,'utf16le').toString('base64')],{encoding:'utf8',timeout:15000,windowsHide:true});
  assert.equal(r.status,0,r.stderr);
  const d=JSON.parse(r.stdout.trim());const g=JSON.parse(d.fresh);
  assert.equal(g[0].adapter_index,0);assert.equal(g[0].mem_free_mb,49);assert.equal(g[0].mem_used_pct,50);assert.equal(g[0].sampled_at,123);
  assert.equal(g[1].mem_used_mb,null);assert.equal(g[1].memory_status,'partial');
  assert.equal(JSON.parse(d.boundary)[0].mem_used_mb,50);
  assert.equal(JSON.parse(d.expired)[0].available,false);
  assert.equal(JSON.parse(d.expired)[0].memory_reason,'query-timeout');
});

test('PS GPU parser nulls invalid VRAM capacity values before serializing partial rows', {skip:process.platform !== 'win32'}, () => {
  const source=fs.readFileSync('probes/live_win.ps1','utf8');
  const helpers=source.slice(source.indexOf('function ParseGpuRows'),source.indexOf('# ---- Top processes'));
  const jsonHelpers=source.slice(source.indexOf('function N('),source.indexOf('function OpenCat'));
  const rows=[
    '0, Invalid Total, 0, 50, 0, 0, N/A, N/A, N/A, N/A, GPU-invalid-total, P8, N/A, N/A, N/A, N/A, 1, 0',
    '1, Invalid Used, 0, 101, 100, 0, N/A, N/A, N/A, N/A, GPU-invalid-used, P8, N/A, N/A, N/A, N/A, 1, 0',
    '2, Invalid Free, 0, 50, 100, -1, N/A, N/A, N/A, N/A, GPU-invalid-free, P8, N/A, N/A, N/A, N/A, 1, 0',
  ];
  const script=`$ErrorActionPreference="Stop"
$inv=[Globalization.CultureInfo]::InvariantCulture
${jsonHelpers}
${helpers}
@(${rows.map(row => `ParseGpuRows ${JSON.stringify(row)} 123`).join('\n')})|ConvertTo-Json -Compress
`;
  const r=spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(script,'utf16le').toString('base64')],{encoding:'utf8',timeout:15000,windowsHide:true});
  assert.equal(r.status,0,r.stderr);
  const actual=JSON.parse(r.stdout.trim()).map(row=>JSON.parse(row)[0]);
  for(const gpu of actual) {
    assert.equal(gpu.memory_status,'partial');
    assert.equal(gpu.memory_reason,'driver returned unavailable or invalid memory fields');
  }
  assert.deepEqual([actual[0].mem_used_mb,actual[0].mem_total_mb,actual[0].mem_free_mb],[null,null,null]);
  assert.deepEqual([actual[1].mem_used_mb,actual[1].mem_total_mb,actual[1].mem_free_mb],[null,100,0]);
  assert.deepEqual([actual[2].mem_used_mb,actual[2].mem_total_mb,actual[2].mem_free_mb],[50,100,null]);
});
