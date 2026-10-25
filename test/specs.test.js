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
  n.report.sections.storage.components[0].official.url='https://support.logi.com/hc/en-us/example';
  assert(model.officialDetails([n]).includes('href="https://support.logi.com/hc/en-us/example"'));
  n.report.sections.storage.components[0].official.url='https://support.logi.com.evil.example/hc/example';
  assert(!model.officialDetails([n]).includes('href="https://support.logi.com.evil.example'));
  n.report.sections.storage.components[0].official.url='https://evilcrucial.com/example';
  assert(!model.officialDetails([n]).includes('href="https://evilcrucial.com'));
  n.report.sections.storage.references[0].url='https://learn.microsoft.com.evil.example/test';
  assert(!model.officialDetails([n]).includes('href="https://learn.microsoft.com.evil'));
});

test('gap reasons are displayed safely and counted from components instead of incoming summaries',()=>{
  const n={node_id:'Example',report:{summary:{gap_reasons:{identity:999,missing_field:999,source_body:999}},sections:{usb:{components:[
    {name:'Unresolved',fields:{driver_name:'example.sys'},official:{status:'unverified'},missing_fields:['driver_name'],gap_reasons:[
      {kind:'identity',code:'model_not_specific',detail:'Exact <img src=x onerror=alert(1)> model required'},
      {kind:'missing_field',code:'provider_unavailable',field:'driver_name',detail:'Provider did not expose the driver name'},
      {kind:'unrecognized',code:'ignored',detail:'Unknown reason kind'}
    ]},
    {name:'Excerpt',fields:{},official:{status:'matched',url:'https://www.razer.com/example',source_access:'manufacturer_page_indexed_excerpt'},missing_fields:[],gap_reasons:[
      {kind:'source_body',code:'indexed_excerpt_only',detail:'Only the manufacturer index is available'}
    ]}
  ]}}}};
  const html=model.officialDetails([n]);
  assert(html.includes('Exact &lt;img src=x onerror=alert(1)&gt; model required'));
  assert(!html.includes('<img'));
  assert(html.includes('model_not_specific'));
  assert(html.includes('ドライバー名'));
  assert(html.includes('Only the manufacturer index is available'));
  assert.match(html,/型番・IDの特定 1/);
  assert.match(html,/取得項目の不足 1/);
  assert.match(html,/資料本文の確認 1/);
  assert(!html.includes('999'));
  assert(!html.includes('Unknown reason kind'));
  assert.equal(model.label('driver_name'),'ドライバー名');
  const gaps=model.officialDetails([n],'gaps');
  assert(gaps.includes('<strong>Unresolved</strong>'));
  assert(!gaps.includes('<strong>Excerpt</strong>'));
  const limits=model.officialDetails([n],'body_limits');
  assert(limits.includes('<strong>Excerpt</strong>'));
  assert(!limits.includes('<strong>Unresolved</strong>'));
});

test('rendered text and supporting original PDFs retain distinct evidence scope and trusted links',()=>{
  const n={node_id:'Example',report:{sections:{usb:{components:[{name:'Mouse',fields:{},missing_fields:[],official:{
    status:'matched',name:'Official mouse',url:'https://mysupport.razer.com/example',source_access:'manufacturer_page_full_body',
    source_evidence:{sha256:'a'.repeat(64),coverage:'full_page',material:'rendered_dom_text',notes:'Current selected Specifications tab'},
    supporting_source_evidence:[
      {url:'https://dl.razerzone.com/example.pdf',sha256:'b'.repeat(64),coverage:'full_document',material:'pdf_document',pages:[7],claim_scope:['polling_rate_up_to_hz','cable_length_m'],notes:'Manual <script>alert(1)</script>'},
      {url:'https://assets.micron.com/example.pdf',sha256:'c'.repeat(64),coverage:'full_document',material:'pdf_document',pages:[2],claim_scope:['capacity_tb']},
      {url:'https://dl.razerzone.com.evil.example/bad.pdf',sha256:'d'.repeat(64),coverage:'full_document',notes:'Unsafe supplementary evidence'}
    ]
  }}]}}}};
  const html=model.officialDetails([n]);
  assert(html.includes('ブラウザー描画後の本文テキスト'));
  assert(html.includes('PDF文書（原本）'));
  assert(html.includes('補足の公式資料'));
  assert(html.includes('確認ページ 7'));
  assert(html.includes('確認対象: 公称最大ポーリングレート (Hz)、ケーブル長 (m)'));
  assert(html.includes('b'.repeat(64)));
  assert(html.includes('href="https://dl.razerzone.com/example.pdf"'));
  assert(html.includes('href="https://assets.micron.com/example.pdf"'));
  assert(html.includes('Manual &lt;script&gt;alert(1)&lt;/script&gt;'));
  assert(!html.includes('<script>'));
  assert(!html.includes('Unsafe supplementary evidence'));
  assert(!html.includes('href="https://dl.razerzone.com.evil.example'));
  assert(!model.officialDetails([n],'body_limits').includes('<strong>Mouse</strong>'));
  n.report.sections.usb.components[0].official={status:'matched',url:'https://www.tp-link.com/example',source_access:'manufacturer_document_full_body',source_evidence:{sha256:'e'.repeat(64),coverage:'full_document',material:'pdf_document',pages:[1]}};
  assert(model.officialDetails([n]).includes('href="https://www.tp-link.com/example"'));
});

test('gap classification groups per-code counts without mixing source-body and identity denominators',()=>{
  const n={node_id:'Example',report:{summary:{identity_gap_reasons:{model_not_specific:900}},sections:{usb:{components:[
    {name:'A',fields:{},missing_fields:[],official:{status:'unverified'},gap_reasons:[{kind:'identity',code:'model_not_specific',detail:'Exact SKU needed'}]},
    {name:'B',fields:{},missing_fields:[],official:{status:'unverified'},gap_reasons:[{kind:'identity',code:'model_not_specific',detail:'Exact SKU needed'},{kind:'missing_field',code:'provider_unavailable',field:'model',detail:'No provider'}]},
    {name:'C',fields:{},missing_fields:[],official:{status:'matched',url:'https://www.razer.com/example'},gap_reasons:[{kind:'source_body',code:'<code-attack>',detail:'Missing full body'}]}
  ]}}}};
  const html=model.officialDetails([n]);
  assert(html.includes('理由コード別の内訳'));
  assert(html.includes('<code>model_not_specific</code> · 2件'));
  assert(html.includes('<code>provider_unavailable</code> · 1件'));
  assert(html.includes('<code>&lt;code-attack&gt;</code> · 1件'));
  assert(!html.includes('<code-attack>'));
  assert(!html.includes('900'));
});

test('driver archive hashes are identified as archive evidence rather than saved page text',()=>{
  const n={node_id:'Example',report:{sections:{network:{components:[{name:'Adapter',fields:{},missing_fields:[],official:{
    status:'identified',url:'https://www.tp-link.com/example.zip',source_access:'manufacturer_document_full_body',
    source_evidence:{sha256:'f'.repeat(64),coverage:'full_archive',notes:'Official driver INF archive'}
  }}]}}}};
  const html=model.officialDetails([n]);
  assert(html.includes('ドライバー配布アーカイブ'));
  assert(!html.includes('資料種別: 保存したページ本文'));
  n.report.sections.network.components[0].official.source_evidence.material='driver_archive';
  assert(model.officialDetails([n]).includes('ドライバー配布アーカイブ'));
});

test('official OS source, manufacturer text and CAB evidence retain distinct materials and lines',()=>{
  const component=(url,material,coverage,lines)=>({name:'Example',fields:{reported_model:'Public model'},missing_fields:[],official:{status:'identified',url,name:'Official source',source_access:coverage==='full_source_file'?'os_driver_source_full_body':'manufacturer_document_full_body',source_evidence:{url,material,coverage,lines,sha256:'a'.repeat(64)}}});
  const components=[
    component('https://raw.githubusercontent.com/torvalds/linux/v6.17/drivers/nvme/host/pci.c','source_code','full_source_file',[3783]),
    component('https://raw.githubusercontent.com/Logitech/fw_updates/'+'a'.repeat(40)+'/RQR12/example.txt','text_document','full_document',[14]),
    component('https://catalog.s.download.windowsupdate.com/example.cab','cab_archive','full_archive',[]),
    component('https://s3.amazonaws.com/plugable/bin/example.zip','driver_archive','full_archive',[])
  ];
  const nodes=[{node_id:'Example',report:{sections:{usb:{components}}}}];
  const html=model.officialDetails(nodes);
  for(const c of components)assert(html.includes(`href="${c.official.url}"`));
  assert(html.includes('OS公式ドライバーソース'));
  assert(html.includes('テキスト文書（原本）'));
  assert(html.includes('CABドライバー配布アーカイブ'));
  assert(html.includes('確認行 3783'));
  assert.equal(model.label('reported_model'),'バス報告モデル（登録済み型番）');
  for(const url of ['https://raw.githubusercontent.com/other/linux/v6.17/file','https://raw.githubusercontent.com/torvalds/linux/master/file','https://s3.amazonaws.com/another-bucket/bin/file','https://s3.amazonaws.com/plugable/bin/../../evil/file','https://catalog.s.download.windowsupdate.com.evil.example/file']) {
    components[0].official.url=url;
    assert(!model.officialDetails(nodes).includes(`href="${url}"`),url);
  }
});
