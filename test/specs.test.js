'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const model = require('../renderer/specs.js');
const report = (fields, platform='windows') => ({platform, at:Date.now(), sections:{cpu:{status:'ok',source:'CIM',components:[{name:'CPU',fields,source:'CIM',official:{status:'unverified'}}]}}});

test('comparison aligns fields, preserves unknowns and filters real differences',()=>{
  const nodes = [{node_id:'a',report:report({cores:8,threads:16})},{node_id:'b',report:report({cores:8,threads:24})}];
  const rows = model.rows(nodes);
  assert.equal(rows.find(r=>r.key==='cpu.cores').different,false);
  assert.equal(rows.find(r=>r.key==='cpu.threads').different,true);
  assert.equal(model.filter(rows,'',true).some(r=>r.key==='cpu.cores'),false);
  assert.equal(model.filter(rows,'threads',false).length,1);
  assert.equal(model.rows([...nodes,{node_id:'c',report:null}]).find(r=>r.key==='cpu.cores').different,true);
});

test('comparison escapes device names and rejects unsafe official URLs',()=>{
  const r = report({model:'<img src=x onerror=alert(1)>'});
  r.sections.cpu.components[0].official = {status:'matched',url:'javascript:alert(1)',specifications:{cores:8}};
  const html = model.table([{node_id:'<script>',report:r}],model.rows([{report:r}]));
  assert(!html.includes('<img'));
  assert(!html.includes('<script>'));
  assert(!html.includes('href="javascript:'));
  assert(html.includes('&lt;img'));
});

test('memory labels do not equate unified memory and dedicated VRAM',()=>{
  assert.match(model.label('memory_kind'),/メモリ/);
  assert.match(model.label('dedicated_vram_bytes'),/専用/);
  assert.notEqual(model.format('memory_kind','unified'),model.format('memory_kind','dedicated'));
  assert.equal(model.format('capacity_bytes',17179869184),'16 GiB');
  assert.equal(model.format('cores',null),'未取得');
});

test('navigation during a pending fetch does not overwrite another view',async()=>{
  let resolve; let written = false;
  const ctx = {window:{tune:{specsView:()=>new Promise(r=>resolve=r)}},UI:require('../renderer/ui.js'),state:{view:'specs',nodes:[]},setCrumbs:()=>{},page:()=>{written=true;},document:{querySelector:()=>null,querySelectorAll:()=>[]},console};
  vm.runInNewContext(fs.readFileSync('renderer/specs.js','utf8'),ctx);
  const view = ctx.window.KT_VIEWS[0];
  const pending = view.render();
  written=false; ctx.state.view='overview'; resolve({nodes:[]}); await pending;
  assert.equal(written,false);
});

test('Tauri command permissions and bridge include both specification endpoints',()=>{
  const capability = JSON.parse(fs.readFileSync('src-tauri/capabilities/default.json'));
  for(const name of ['specs-view','specs-refresh']) assert(capability.permissions.includes(`allow-${name}`));
  const bridge = fs.readFileSync('src-tauri/bridge/tune.js','utf8');
  assert(bridge.includes("invoke('specs_view')"));
  assert(bridge.includes("invoke('specs_refresh'"));
});

test('differences retain per-device value associations',()=>{
  const r=(a,b)=>({sections:{gpu:{status:'ok',components:[{name:'GPU A',fields:{model:'GPU A',driver_version:a}},{name:'GPU B',fields:{model:'GPU B',driver_version:b}}]}}});
  const rows=model.rows([{report:r('1','2')},{report:r('2','1')}]);
  assert.equal(rows.find(r=>r.field==='driver_version').different,true);
});

test('a cached running backend never disables manual reload',async()=>{
  let html='';
  const element = {onclick:null,onchange:null,oninput:null};
  const ctx={window:{tune:{specsView:async()=>({nodes:[],running:true})}},UI:require('../renderer/ui.js'),state:{view:'specs',nodes:[]},setCrumbs:()=>{},page:s=>{html=s;},document:{querySelector:()=>element,querySelectorAll:()=>[]},console};
  vm.runInNewContext(fs.readFileSync('renderer/specs.js','utf8'),ctx);
  await ctx.window.KT_VIEWS[0].render();
  assert(!/id="specsReload"[^>]*disabled/.test(html));
});

test('search updates results without rebuilding the page or changing focus', async () => {
  let pageCount = 0, input, focusCount = 0;
  const controls = {};
  const ctx = {
    window: {tune: {specsView: async () => ({nodes: [{node_id:'a', report:report({cores:8, threads:16})}]})}},
    UI: require('../renderer/ui.js'), state: {view: 'specs', nodes: []},
    setCrumbs: () => {}, console,
    page: () => {
      pageCount++;
      input = {isConnected: true, value: '', focus() { focusCount++; }, setSelectionRange() {}};
      controls['#specsSearch'] = input;
      controls['#specsTable'] = {innerHTML:''};
      controls['#specsCount'] = {textContent:''};
    },
    document: {querySelector: selector => controls[selector] || (controls[selector] = {}), querySelectorAll: () => []},
  };
  vm.runInNewContext(fs.readFileSync('renderer/specs.js', 'utf8'), ctx);
  await ctx.window.KT_VIEWS[0].render();
  ctx.document.activeElement = input;
  for (const value of ['c', 'co', 'cores']) {
    input.value = value;
    input.oninput({target: input});
  }
  await Promise.resolve();
  assert.equal(pageCount, 1, 'typing should not rebuild the page');
  assert.equal(focusCount, 0, 'typing should not move focus');
  assert.equal(ctx.document.activeElement, input);
  assert.equal(controls['#specsCount'].textContent, '1 項目');
  assert.match(controls['#specsTable'].innerHTML, /物理コア/);
  assert.doesNotMatch(controls['#specsTable'].innerHTML, /論理スレッド/);
});
