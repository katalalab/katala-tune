'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
test('native network command is registered in both Tauri handler and window capability', () => {
  assert.match(fs.readFileSync('src-tauri/build.rs','utf8'), /"network_check"/);
  assert.match(fs.readFileSync('src-tauri/src/lib.rs','utf8'), /commands::network_check/);
  const permissions=JSON.parse(fs.readFileSync('src-tauri/capabilities/default.json','utf8')).permissions;
  assert.ok(permissions.includes('allow-network-check'));
  assert.match(fs.readFileSync('src-tauri/bridge/tune.js','utf8'), /invoke\('network_check'/);
});
test('network view starts without probing, escapes labels and routes explicit selection through IPC', async () => {
  let html='', calls=[], rejectFirst=false;
  const buttons={};
  const esc=v=>String(v??'').replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('"','&quot;');
  const context={ Map, console, state:{view:'network',nodes:[{id:'<node>',shared:true}]}, fmtTime:()=> 'now', setCrumbs:()=>{}, page:v=>{html=v;}, toast:()=>{}, document:{querySelector:s=>buttons[s]??=( {}),querySelectorAll:()=>[]}, UI:{esc,chip:esc,head:({title,props})=>title+props,table:({rows,row})=>rows.map(row).join('')},window:{tune:{networkCheck:async(ids,active)=>{calls.push({ids,active});if(rejectFirst && ids[0]==='offline') throw new Error('offline');return{nodes:[{node_id:ids[0],at:1,ok:true,data:{default_route_present:true,ip_address_present:true,dns_configured:null,https:{named:{state:'reachable',http_status:403,timings:{total_ms:5}},fixed_ip:{state:'timed_out'}}}}]};}}}};
  vm.runInNewContext(fs.readFileSync('renderer/network.js','utf8'),context);
  const view=context.window.KT_VIEWS[0]; view.render();
  assert.equal(calls.length,0); assert.match(html,/未取得/); assert.match(html,/&lt;node>/); assert.doesNotMatch(html,/<node>/);
  await buttons['#netActive'].onclick();
  assert.equal(calls.length,1); assert.equal(calls[0].ids[0],'<node>'); assert.equal(calls[0].active,true);
  assert.match(html,/HTTP 403/); assert.match(html,/5.0 ms/); assert.match(html,/時間切れ/);
  rejectFirst=true; context.state.nodes=[{id:'offline'},{id:'online'}]; calls=[]; view.render();
  await buttons['#netActive'].onclick();
  assert.equal(calls.length,2); assert.equal(calls[1].ids[0],'online'); assert.match(html,/offline/); assert.match(html,/HTTP 403/);
});
