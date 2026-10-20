'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const UI = require('../renderer/ui');

const flush = () => new Promise(setImmediate);
const entity = (kind, label, n = 1) => ({id:`kt.topology.v1.${kind}.${String(n).padStart(64,'0')}`,kind,label,attributes:{content_sha256:'a'.repeat(64)}});
const graph = () => ({schema:'katala_topology.v1',entities:[entity('host','fixture-host'),entity('environment','fixture-environment'),entity('package','fixture-package'),entity('workflow','fixture-workflow'),entity('job','fixture-job')],edges:[],scopes:[{scope_id:'fixture:native',status:'partial',observed_at:1700000000000,counters:{entries:7,files:3,bytes:99},failures:[{code:'limit_reached',message:'untrusted-hidden-message'}]}],history:[{scope_id:'fixture:native',status:'complete'}],changes:[{scope_id:'fixture:native',kind:'entity',change_kind:'add',at:1700000000000}]});

// A control/event fixture, not a browser or native-WebView substitute. It records
// table output and the controls recreated by page() so asynchronous navigation,
// clipboard use and continuous input can be tested without layout assumptions.
async function mount(options = {}) {
  const state = {html:'',pages:0,view:'topology',loads:0,controls:[],copies:[],exports:[],event:null,graph:options.graph || graph()};
  const document = {
    visibilityState:'visible',activeElement:null,listeners:{},
    addEventListener(name,fn){this.listeners[name]=fn;},
    querySelector(selector){
      if(selector === '.side-item.sel') return {dataset:{view:state.view}};
      return state.controls.find(control => selector === '#'+control.id) || null;
    },
    querySelectorAll(selector){
      const key = /^\[data-([\w-]+)\]$/.exec(selector)?.[1]?.replace(/-([a-z])/g,(_,c)=>c.toUpperCase());
      return key ? state.controls.filter(control => key in control.dataset) : [];
    },
  };
  const page = html => {
    state.html=html;state.pages++;state.controls=[];
    for(const m of html.matchAll(/<(button|input|select)\b([^>]*)>/g)){
      const attrs=Object.fromEntries([...m[2].matchAll(/([\w-]+)="([^"]*)"/g)].map(a=>[a[1],a[2]]));
      const control={id:attrs.id,dataset:{},value:attrs.value || '',listeners:{},selectionStart:0,selectionEnd:0,
        addEventListener(name,fn){this.listeners[name]=fn;},
        focus(){document.activeElement=this;},
        setSelectionRange(start,end){this.selectionStart=start;this.selectionEnd=end;},
      };
      for(const [key,value] of Object.entries(attrs)) if(key.startsWith('data-')) control.dataset[key.slice(5).replace(/-([a-z])/g,(_,c)=>c.toUpperCase())]=value;
      state.controls.push(control);
    }
  };
  const tune = options.unavailable ? {} : {
    topologyView:async()=>{state.loads++;return options.load ? options.load(state.loads) : state.graph;},
    topologyRefresh:options.refresh || (async()=>({status:'partial',changes:0,scopes:[]})),
    topologyExport:options.export || (async(view,format)=>{state.exports.push({view,format});if(options.exportFailure) throw Error(options.exportFailure);return 'public-only-'+view+'-'+format;}),
    copy:options.copy || (async(text)=>state.copies.push(text)),
    onTopologyUpdated:fn=>{state.event=fn;},
  };
  const context={UI,window:{tune},document,page,setCrumbs(){}};
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(require.resolve('../renderer/topology'),'utf8'),context);
  const view=context.window.KT_VIEWS[0];
  view.render();
  await flush();
  const click = async(key,value)=>{
    const control=state.controls.find(c=>c.dataset[key]===value || (key==='id' && c.id===value));
    assert.ok(control,`${key}=${value} control exists`);
    await control.listeners.click?.({target:control});await flush();
  };
  const change=async(id,value,name='change')=>{
    const control=document.querySelector('#'+id);assert.ok(control,id);control.value=value;control.focus();control.selectionStart=control.selectionEnd=value.length;
    await control.listeners[name]?.({target:control});await flush();
  };
  return {state,document,view,click,change,event:async receipt=>{assert.equal(typeof state.event,'function');state.event(receipt);await flush();}};
}

test('topology has four views and keeps hosts/environments, packages and CI jobs in the appropriate view',async()=>{
  const d=await mount();
  await d.click('topologyTab','hosts');assert.match(d.state.html,/fixture-host/);assert.match(d.state.html,/fixture-environment/);assert.doesNotMatch(d.state.html,/fixture-package|fixture-workflow|fixture-job/);
  await d.click('topologyTab','dependencies');assert.match(d.state.html,/fixture-package/);assert.doesNotMatch(d.state.html,/fixture-host|fixture-workflow/);
  await d.click('topologyTab','cicd');assert.match(d.state.html,/fixture-workflow/);assert.match(d.state.html,/fixture-job/);assert.doesNotMatch(d.state.html,/fixture-package|fixture-host/);
  await d.click('topologyTab','all');assert.match(d.state.html,/fixture-host/);assert.match(d.state.html,/fixture-package/);assert.match(d.state.html,/fixture-job/);
});

test('topology displays partial evidence, currentness and changes without treating old success as current',async()=>{
  const d=await mount();
  assert.match(d.state.html,/未完の範囲/);assert.match(d.state.html,/現在の存在・削除・変更を確認できていません/);assert.match(d.state.html,/limit_reached/);assert.match(d.state.html,/7 \/ 3 \/ 99/);assert.match(d.state.html,/変更履歴/);assert.match(d.state.html,/>add</);assert.match(d.state.html,/aaaaaaaaaaaaaaaa/);assert.doesNotMatch(d.state.html,/untrusted-hidden-message/);
  d.state.graph={...graph(),scopes:[{scope_id:'fixture:native',status:'complete'}]};
  await d.event({status:'complete',changes:0});assert.match(d.state.html,/最後に観測した時点/);assert.doesNotMatch(d.state.html,/未完の範囲/);
});

test('public copy requests the selected backend export and never copies internal graph values',async()=>{
  const d=await mount();await d.click('topologyTab','cicd');await d.click('topologyExport','json');await d.click('topologyExport','mermaid');
  assert.deepEqual(d.state.exports,[{view:'cicd',format:'json'},{view:'cicd',format:'mermaid'}]);
  assert.deepEqual(d.state.copies,['public-only-cicd-json','public-only-cicd-mermaid']);assert.match(d.state.html,/コピーしました/);
  const failure=await mount({exportFailure:'<unsafe export>'});await failure.click('topologyExport','json');assert.deepEqual(failure.state.copies,[]);assert.match(failure.state.html,/&lt;unsafe export&gt;/);assert.doesNotMatch(failure.state.html,/<unsafe export>/);
});

test('finishing an export or copy after navigation does not replace the other view',async()=>{
  for(const stage of ['export','copy']) {
    let complete;
    const pending=new Promise(resolve=>{complete=resolve;});
    const d=await mount(stage === 'export' ? {export:()=>pending} : {copy:()=>pending});
    const control=d.state.controls.find(c=>c.dataset.topologyExport === 'json');
    const exporting=control.listeners.click();await flush();
    d.state.view='dashboard';d.view.leave();const pages=d.state.pages;
    complete('public-only-all-json');await exporting;await flush();
    assert.equal(d.state.pages,pages,`${stage} completion must not render after navigation`);
  }
});

test('filter and kind selection limit results and retain safe escaped labels',async()=>{
  const source=graph();source.entities.push(entity('package','<img src=x onerror=alert(1)>',99));
  const d=await mount({graph:source});assert.match(d.state.html,/&lt;img src=x onerror=alert\(1\)&gt;/);assert.doesNotMatch(d.state.html,/<img src=x/);
  await d.change('topologyKind','package');assert.match(d.state.html,/fixture-package/);assert.doesNotMatch(d.state.html,/fixture-host/);
  await d.change('topologyFilter','not-found','input');assert.match(d.state.html,/条件に合う実体はありません/);assert.doesNotMatch(d.state.html,/fixture-package/);
});

test('changing topology tabs resets the kind filter to all',async()=>{
  const d=await mount();await d.change('topologyKind','package');
  await d.click('topologyTab','cicd');
  assert.match(d.state.html,/fixture-workflow/);
  assert.match(d.state.html,/fixture-job/);
});

test('continuous search retains focus and caret after filtering rerenders controls',async()=>{
  const d=await mount();await d.change('topologyFilter','fixture','input');
  const current=d.document.querySelector('#topologyFilter');assert.equal(d.document.activeElement,current,'the current search control stays focused for the next character');assert.equal(current.selectionStart,7);assert.equal(current.selectionEnd,7);
});

test('large graphs cap rendered entities at 200 and disclose the unshown count',async()=>{
  const source=graph();source.entities=Array.from({length:205},(_,i)=>entity('package',`unique-fixture-${i}`,i+1));source.scopes=[];source.changes=[];
  const d=await mount({graph:source});assert.match(d.state.html,/表示 200 \/ 収集 205/);assert.match(d.state.html,/unique-fixture-199/);assert.doesNotMatch(d.state.html,/unique-fixture-200</);assert.equal((d.state.html.match(/<td>package<\/td>/g)||[]).length,200);
});

test('an update arriving during a load is followed by another load rather than lost',async()=>{
  const waiting=[];let resolveFirst;
  const first=new Promise(resolve=>{resolveFirst=resolve;});
  const fresh={...graph(),entities:[entity('package','newer-observation')]};
  const d=await mount({load:async n=>{waiting.push(n);return n===1 ? first : fresh;}});
  await d.event({status:'complete',changes:1});resolveFirst({...graph(),entities:[entity('package','older-observation')]});await flush();await flush();
  assert.ok(d.state.loads>=2,'a pending update causes a follow-up fetch');assert.match(d.state.html,/newer-observation/);assert.doesNotMatch(d.state.html,/older-observation/);
});

test('finishing a manual refresh after navigation does not replace the other view',async()=>{
  let complete;const pending=new Promise(resolve=>{complete=resolve;});const d=await mount({refresh:()=>pending});
  const control=d.state.controls.find(c=>c.id==='topologyRefresh');const refreshing=control.listeners.click();await flush();d.state.view='dashboard';d.view.leave();const pages=d.state.pages;
  complete({status:'complete',changes:0});await refreshing;await flush();assert.equal(d.state.pages,pages,'the current view owns page rendering');
});

test('missing topology API and a failed read remain visible unavailable states',async()=>{
  const missing=await mount({unavailable:true});assert.match(missing.state.html,/この画面は利用できません/);
  const failure=await mount({load:async()=>{throw Error('<read failed>');}});assert.match(failure.state.html,/構成図を読み込めません/);assert.match(failure.state.html,/&lt;read failed&gt;/);
});

test('Tauri bridge routes the four-view export, clipboard and update-event payload to the registered backend',async()=>{
  const calls=[],callbacks=[];
  const context={__KT_PLATFORM__:'darwin',__KT_DEBUG__:false,URLSearchParams,
    location:{search:'',pathname:'/index.html',hash:''},history:{state:null,replaceState(){}},
    document:{addEventListener(){}},window:{__TAURI_INTERNALS__:{
      invoke:async(command,args)=>{calls.push({command,args});return command==='topology_export' ? 'safe-public-export' : {};},
      transformCallback:fn=>{callbacks.push(fn);return callbacks.length;},
    }},
  };
  vm.createContext(context);vm.runInContext(fs.readFileSync(require.resolve('../src-tauri/bridge/tune'),'utf8'),context);
  const tune=context.window.tune;
  await tune.topologyView();await tune.topologyRefresh();
  const text=await tune.topologyExport('dependencies','mermaid');await tune.copy(text);
  let received;tune.onTopologyUpdated(value=>{received=value;});await flush();callbacks.at(-1)({payload:{status:'partial',changes:2}});
  assert.deepEqual(JSON.parse(JSON.stringify(calls)),[
    {command:'topology_view',args:{}},{command:'topology_refresh',args:{}},
    {command:'topology_export',args:{view:'dependencies',format:'mermaid'}},
    {command:'copy',args:{text:'safe-public-export'}},
    {command:'plugin:event|listen',args:{event:'topology-updated',target:{kind:'Any'},handler:1}},
  ]);
  assert.deepEqual(JSON.parse(JSON.stringify(received)),{status:'partial',changes:2});
  const registration=fs.readFileSync(require.resolve('../src-tauri/src/lib.rs'),'utf8');
  for(const name of ['topology_view','topology_refresh','topology_export'])assert.match(registration,new RegExp('commands_topology::'+name+'\\s*,'));
});

test('the native command manifest and window capability both allow the topology IPC contract',()=>{
  const manifest=fs.readFileSync(require.resolve('../src-tauri/build.rs'),'utf8');
  const capability=JSON.parse(fs.readFileSync(require.resolve('../src-tauri/capabilities/default.json'),'utf8'));
  for(const command of ['topology_view','topology_refresh','topology_export']){
    assert.ok(manifest.includes('"'+command+'"'),`${command} must be declared to generate native permissions`);
    assert.ok(capability.permissions.includes('allow-'+command.replaceAll('_','-')),`${command} must be granted to the main window`);
  }
});
