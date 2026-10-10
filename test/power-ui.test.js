'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

test('power view labels missing/stale values and escapes target telemetry', async () => {
  let html='';
  const esc = (v) => String(v ?? '').replace(/[&<>"']/g,(c) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const node={id:'node<img>',os:'windows',shared:false};
  const r={node_id:node.id,at:1,fresh:false,settings:{},data:{cpu_clock:{},power:{},gpus:[{name:'<script>bad()</script>',source:'<img src=x>',power_w:null}]},summary:{kind:'missing',total_w:null,missing:['cpu_w'],gpu_w:null}};
  const context={window:{tune:{powerReport:async()=>({nodes:[r],fleet:{expected:1,available:0,total_w:null,available_w:null}})}},state:{nodes:[node],view:'power'},UI:{esc,head:(v)=>esc(v.title),empty:esc,toggle:(v)=>v.summary+v.body,chip:esc,props:(v)=>v.map(([k,x])=>esc(k)+x).join(''),table:(v)=>v.rows.map(v.row).join('')},Live:{toggle:()=>'',panel:()=>'',mount:()=>{}},ticket:()=>1,stale:()=>false,setCrumbs:()=>{},page:(v)=>{html=v;},stat:(k,v)=>k+v,ago:()=>'',fmtTime:()=>'',document:{querySelectorAll:()=>[]},console};
  vm.runInNewContext(fs.readFileSync(require.resolve('../renderer/power.js'),'utf8'),context);
  await context.window.KT_VIEWS[0].render();
  assert.match(html,/未取得/);
  assert.match(html,/合計対象外/);
  assert.match(html,/CPU電力/);
  assert.match(html,/&lt;script&gt;/);
  assert.doesNotMatch(html,/<script>|<img src=x>/);
  assert.match(html,/disabled/);
});
