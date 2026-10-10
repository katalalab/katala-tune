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

test('power actions refresh attempted outcomes and hide old settings until a fresh snapshot is saved', async () => {
  for(const scenario of ['success','failed','uncertain','cancel','refused','record-failure','action-error','probe-error','probe-failed','probe-busy']) {
    let html='',at=100,watts=150,recover=false;const calls=[],toasts=[];
    const button={dataset:{planAction:'test'},parentElement:{querySelector:()=>({value:'next'})}};
    const refreshButton={dataset:{powerRefresh:'test'}};
    const report=()=>({nodes:[{node_id:'test',at,fresh:true,settings:{},data:{power:{plan_guid:'old',plans:[{guid:'old',name:'old plan'}]},gpus:[{name:'GPU',power_limit_w:watts}]},summary:{kind:'estimated',total_w:watts}}],fleet:{expected:1,available:1,total_w:watts,available_w:watts}});
    const result={success:{ok:true,entry:{}},failed:{ok:false,code:3,entry:{}},uncertain:{ok:false,code:null,entry:{}},cancel:{cancelled:true},refused:{refused:'before execution'},'record-failure':{ok:false,refused:'record failure',refresh_required:true}};
    const context={Map,Date:{now:()=>150},window:{tune:{powerReport:async()=>{calls.push('report');return report();},action:async()=>{calls.push('action');if(scenario==='action-error')throw Error('lost response');return result[scenario] || {ok:false,entry:{}};},probe:async()=>{calls.push('probe');if(recover){at=400;watts=350;return {done:1,results:[{node_id:'test',ok:true,at}]};}if(scenario==='probe-error')throw Error('offline');if(scenario==='probe-busy'){at=200;watts=250;return {busy:true};}if(scenario!=='probe-failed'){at=200;watts=250;}return {done:1,results:[{node_id:'test',ok:scenario!=='probe-failed',at}]};}}},state:{nodes:[{id:'test',os:'windows'}],view:'power'},UI:{esc:String,head:()=>'',empty:String,toggle:v=>v.summary+v.body,chip:String,props:v=>v.map(([k,x])=>k+x).join(''),table:v=>v.rows.map(v.row).join('')},Live:{toggle:()=>'',panel:()=>'',mount:()=>{}},ticket:()=>1,stale:()=>false,setCrumbs:()=>{},page:v=>{html=v;},stat:(k,v)=>k+v,ago:()=>'',fmtTime:()=>'',toast:v=>toasts.push(v),document:{querySelectorAll:s=>s==='[data-plan-action]' && html.includes('data-plan-action') ? [button] : s==='[data-power-refresh]' && html.includes('data-power-refresh') ? [refreshButton] : []}};
    vm.runInNewContext(fs.readFileSync(require.resolve('../renderer/power.js'),'utf8'),context);
    const view=context.window.KT_VIEWS[0];await view.render();calls.length=0;
    // Existing handlers initiate the async action without returning its Promise.
    button.onclick();
    for(let i=0;i<20 && calls.at(-1)!=='report';i++)await new Promise(r=>setImmediate(r));
    const attempted=!['cancel','refused'].includes(scenario);
    const unknown=scenario.startsWith('probe-');
    assert.deepEqual(calls,attempted ? unknown ? ['action','probe','report'] : ['action','probe','report','report'] : ['action','report'],scenario);
    if(unknown){assert.match(html,/状態未確認/,scenario);assert.doesNotMatch(html,/old plan|150\.0 W|data-plan-action/,scenario);
      at=300;watts=350;await view.render();assert.match(html,/状態未確認/,scenario);
      recover=true;await refreshButton.onclick();assert.doesNotMatch(html,/状態未確認/,scenario);assert.match(html,/350\.0 W/,scenario);
    }else if(attempted){assert.match(html,/250\.0 W/,scenario);assert.doesNotMatch(html,/150\.0 W/,scenario);}
    assert.ok(toasts.length>0,scenario);
    assert.match(html,/Clockは読み取り専用/);
  }
});
