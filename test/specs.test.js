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

test('coverage separates exact products, documentation and unavailable setting values',()=>{
  const n={report:{sections:{settings:{components:[{official:{status:'not_applicable'},fields:{secure_boot:'permission_denied'},missing_fields:[]}]},storage:{components:[{official:{status:'matched'},fields:{model:'Example'},missing_fields:['firmware_version']},{official:{status:'unverified'},fields:{model:'Other'}}]}}}};
  assert.deepEqual(model.coverage(n),{matched:1,identified:0,unverified:1,notApplicable:1,missing:1,unavailable:1});
});

test('full-body evidence and hardware function identification are separate from exact product matching',()=>{
  const n={node_id:'Example',report:{sections:{pci:{components:[{name:'Controller',fields:{record_kind:'device'},official:{status:'identified',match_kind:'hardware_id',url:'https://edc.intel.com/example',source_access:'manufacturer_page_full_body',source_evidence:{url:'https://edc.intel.com/example',sha256:'a'.repeat(64),coverage:'full_page',notes:'Exact public ID table'},specifications:{controller_function:'SMBus'}},missing_fields:[]}]}}}};
  assert.equal(model.coverage(n).identified,1);
  assert.equal(model.coverage(n).matched,0);
  const html=model.officialDetails([n]);
  assert(html.includes('公式ID照合'));
  assert(html.includes('本文取得・確認済み'));
  assert(html.includes('a'.repeat(64)));
  assert.equal(model.format('present',false),'未検出');
  assert.equal(model.format('driver_signed',false),'署名なし');
});

test('evidence filters retain gaps and distinguish recorded full bodies from excerpt-only sources',()=>{
  const n={node_id:'Example',report:{sections:{usb:{components:[
    {name:'Unresolved',fields:{},official:{status:'unverified'},missing_fields:[]},
    {name:'Complete',fields:{},official:{status:'matched',url:'https://www.rme-audio.de/example',source_access:'manufacturer_page_full_body',source_evidence:{sha256:'b'.repeat(64)}},missing_fields:[]},
    {name:'Excerpt',fields:{},official:{status:'matched',url:'https://www.razer.com/example',source_access:'manufacturer_page_indexed_excerpt'},missing_fields:[]}
  ]}}}};
  const gaps=model.officialDetails([n],'gaps');
  assert(gaps.includes('<strong>Unresolved</strong>'));
  assert(!gaps.includes('<strong>Complete</strong>'));
  const limits=model.officialDetails([n],'body_limits');
  assert(limits.includes('<strong>Excerpt</strong>'));
  assert(limits.includes('索引の抜粋'));
  assert(!limits.includes('<strong>Complete</strong>'));
  assert(!limits.includes('<strong>Unresolved</strong>'));
});

test('official details accept new manufacturer roots, show access limits and keep method references separate',()=>{
  const n={node_id:'Example',report:{sections:{storage:{components:[{name:'SSD',fields:{},official:{status:'matched',url:'https://content.crucial.com/example.pdf',source_access:'manufacturer_document_indexed_excerpt',scope:'Excerpt only'},missing_fields:[]}],references:[{name:'Method',url:'https://learn.microsoft.com/en-us/windows/example'}]},settings:{components:[{name:'Settings',fields:{},official:{status:'not_applicable'},missing_fields:[]}]}}}};
  const html=model.officialDetails([n]);
  assert(html.includes('href="https://content.crucial.com/example.pdf"'));
  assert(html.includes('索引の抜粋'));
  assert(html.includes('公式の項目説明・取得方法'));
  assert(html.includes('製品仕様の照合対象外'));
  n.report.sections.storage.components[0].official.url='https://evilcrucial.com/example';
  assert(!model.officialDetails([n]).includes('href="https://evilcrucial.com'));
  n.report.sections.storage.references[0].url='https://learn.microsoft.com.evil.example/test';
  assert(!model.officialDetails([n]).includes('href="https://learn.microsoft.com.evil'));
});
