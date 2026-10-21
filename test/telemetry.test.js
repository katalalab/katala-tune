'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');
const { spawnSync } = require('node:child_process');
const { openDb } = require('../lib/db');
const root = path.join(__dirname, '..');
function py(code) {
  const r = spawnSync('python3', ['-c', `import importlib.util,json\ns=importlib.util.spec_from_file_location('m',${JSON.stringify(path.join(root,'probes/mac_probe.py'))})\nm=importlib.util.module_from_spec(s);s.loader.exec_module(m)\n${code}`], { encoding:'utf8' });
  assert.equal(r.status, 0, r.stderr); return JSON.parse(r.stdout);
}
test('Apple GPU explicitly shares unified memory and never reports dedicated VRAM zero', () => {
  const gpu = py('print(json.dumps(m.gpu_memory({"name":"Apple GPU","util":None}, True, 123)))');
  assert.equal(gpu.memory_kind, 'unified');
  assert.equal(gpu.mem_total_mb, null); assert.equal(gpu.mem_used_mb, null); assert.equal(gpu.mem_free_mb, null);
  assert.equal(gpu.memory_status, 'not-applicable'); assert.equal(gpu.sampled_at, 123);
  assert.match(gpu.memory_reason, /shared/);
});
test('unsupported GPU keeps every unknown field null with provider and reason', () => {
  const gpu = py('print(json.dumps(m.gpu_memory({}, False, 123)))');
  assert.equal(gpu.memory_status, 'unsupported'); assert.equal(gpu.mem_used_mb, null);
  assert.ok(gpu.memory_reason); assert.ok(gpu.source);
});
test('snapshot history preserves per-adapter VRAM and process counts, including unknowns', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(),'kt-telemetry-')); const db = openDb(dir);
  try {
    const gpus = [{ adapter_index:0, mem_total_mb:24000, mem_used_mb:12000, mem_free_mb:12000, mem_used_pct:50 }, { adapter_index:1, mem_total_mb:null, mem_used_mb:null, memory_status:'unsupported' }];
    db.addSnapshot('dev',{at:1, data:{gpus,processes:{count:42,threads:84,handles:null}},summary:{bench_ms:null,cpu:2,mem_avail:50,swap_gb:null,critical:0,warn:0}});
    const h=db.history('dev')[0]; assert.deepEqual(h.gpus,gpus); assert.equal(h.process_count,42); assert.equal(h.thread_count,84); assert.equal(h.handle_count,null);
    db.saveProbeFailure('dev',2,'timeout'); assert.deepEqual(db.history('dev')[0].gpus,gpus);
  } finally { db.db.close(); fs.rmSync(dir,{recursive:true,force:true}); }
});
test('GPU rendering exposes free VRAM, percent, source, units and missing-data reason', () => {
  const src=fs.readFileSync(path.join(root,'renderer/app.js'),'utf8');
  const start=src.indexOf('function gpuHtml('), end=src.indexOf('\nfunction machineHtml(',start);
  assert.ok(start>=0); const ctx={esc:String,fmtTime:String,Charts:{meter:(v,o)=>`${v??'missing'}:${o.label}:${o.text}`}};
  vm.createContext(ctx);vm.runInContext(src.slice(start,end),ctx);
  const html=ctx.gpuHtml({name:'GPU',mem_total_mb:16000,mem_used_mb:8000,mem_free_mb:7900,mem_used_pct:50,memory_status:'available',memory_unit:'MiB',sampled_at:123,source:'nvidia-smi'});
  assert.match(html,/7900/);assert.match(html,/50/);assert.match(html,/MiB/);assert.match(html,/nvidia-smi/);assert.match(html,/123/);
  const missing=ctx.gpuHtml({name:'Apple GPU',memory_kind:'unified',memory_status:'not-applicable',memory_reason:'shared unified memory',sampled_at:123});
  assert.match(missing,/統合メモリ共有/);assert.doesNotMatch(missing,/null%|null°C|undefined/);
});
test('Windows GPU telemetry adds actual free MiB and explicit unavailable reasons', () => {
  const src=fs.readFileSync(path.join(root,'probes/win_probe.ps1'),'utf8');
  assert.match(src,/memory\.free/);assert.match(src,/mem_free_mb/);assert.match(src,/memory_status/);
  assert.match(src,/private_mb/);assert.match(src,/cpu_seconds/);assert.match(src,/handle_count/);assert.match(src,/thread_count/);
});

test('Windows GPU telemetry keeps an unacknowledged cleanup as failure and stops the later compute query', () => {
  const src=fs.readFileSync(path.join(root,'probes/win_probe.ps1'),'utf8');
  assert.match(src,/\$ack = \$process\.WaitForExit\(\[Math\]::Min\(1000, \$left\)\)/);
  assert.match(src,/return \(\$ack -and \$process\.HasExited\)/);
  assert.match(src,/reason = "cleanup-unverified"/);
  assert.match(src,/\$compute = if \(\$query\.reason\) \{ @\{ rows = @\(\); reason = \$query\.reason \} \} else \{ InvokeGpuComputeQuery/);
});

test('macOS accumulated CPU time parses minutes, hours, days and rejects invalid time', () => {
  const values=py('print(json.dumps([m.cpu_time_seconds(x) for x in ["02:03.45", "01:02:03.50", "2-03:04:05.25", "12345:06.7", "invalid", "-1:23", "NaN"]]))');
  assert.deepEqual(values,[123.45,3723.5,183845.25,740706.7,null,null,null]);
});
