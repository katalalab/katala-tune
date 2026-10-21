'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const UI = require('../renderer/ui');

// DOM-operation fixture only; no browser layout or native application acceptance.
class Element {
  constructor(tag = 'div', attrs = {}) { this.tagName = tag; this.attrs = attrs; this.children = []; this.disabled = false; }
  set innerHTML(html) {
    this.html = html; this.children = []; const stack = [this];
    for (const token of html.match(/<[^>]+>|[^<]+/g) || []) {
      if (token.startsWith('</')) { if (stack.length > 1) stack.pop(); }
      else if (token.startsWith('<')) {
        const tag = /^<([\w-]+)/.exec(token)?.[1]; if (!tag) continue;
        const attrs = Object.fromEntries([...token.matchAll(/([\w-]+)="([^"]*)"/g)].map(m => [m[1], m[2]]));
        if (/\sopen(?:\s|>)/.test(token)) attrs.open = true;
        const e = new Element(tag, attrs); e.disabled = /\sdisabled(?:\s|>)/.test(token); stack.at(-1).children.push(e);
        if (!/\/>$/.test(token) && !['input','br','hr','img','meta','link'].includes(tag)) stack.push(e);
      } else stack.at(-1).children.push(token);
    }
  }
  get innerHTML() { return this.html || ''; }
  get textContent() { return this.children.map(c => typeof c === 'string' ? c.replace(/&lt;/g,'<').replace(/&gt;/g,'>').replace(/&amp;/g,'&') : c.textContent).join(''); }
  get open() { return !!this.attrs.open; }
  set open(v) { this.attrs.open = !!v; }
  get className() { return this.attrs.class || ''; }
  querySelector(s) { return this.querySelectorAll(s)[0] || null; }
  querySelectorAll(s) {
    const match = e => s.startsWith('#') ? e.attrs.id === s.slice(1) : s.startsWith('.') ? e.className.split(' ').includes(s.slice(1))
      : s.startsWith('[') ? s.slice(1,-1) in e.attrs : e.tagName === s;
    return this.children.filter(c => c instanceof Element).flatMap(c => [c,...c.descendants()]).filter(match);
  }
  descendants() { return this.children.filter(c => c instanceof Element).flatMap(c => [c,...c.descendants()]); }
}
const flush = () => new Promise(setImmediate);
const deferred = () => { let resolve, reject; const promise = new Promise((a,b) => { resolve=a; reject=b; }); return {promise,resolve,reject}; };
const response = (events = [], health = {status:'unknown',summary:'収集記録は開始していません',dropped_total:null,db_failures:null}) => ({events,health,retention:{rows:events.length,oldest_at:events[0]?.ts ?? null,newest_at:events[0]?.ts ?? null,keep_days_max:14,keep_rows_max:50000}});
async function dashboard(read) {
  const root = new Element(), calls = [], intervals = [];
  const data = {at:Date.now(),collecting:true,monitor:{enabled:true},nodes:[]};
  let seq = 0;
  const tune = {dashboard:async () => data,onLive(){},onMonitor(){},liveStart:async()=>({}),liveStop:async()=>({})};
  if (read) tune.monitorEvents = f => { calls.push(structuredClone(f)); return read(f); };
  const context = {
    UI,Charts:{},state:{view:'dashboard',nodes:[]},window:{tune},
    document:{visibilityState:'visible',addEventListener(){},querySelector:s=>root.querySelector(s),querySelectorAll:s=>root.querySelectorAll(s),getElementById:id=>root.querySelector(`#${id}`)},
    page:html=>{root.innerHTML=html;},$:s=>s==='#page'?root:root.querySelector(s),ticket:()=>++seq,stale:t=>t!==seq,
    setCrumbs(){},bindGoto(){},ago:String,setInterval:fn=>{intervals.push(fn);return intervals.length;},clearInterval(){},
  };
  vm.createContext(context); vm.runInContext(fs.readFileSync(require.resolve('../renderer/dashboard'),'utf8'),context);
  const view=context.window.KT_VIEWS[0];view.render();await flush();
  const toggle = open => { const d=root.querySelector('#dashJournal');assert.ok(d,'read-only journal details must be mounted');d.open=open;d.ontoggle(); };
  return {root,calls,context,view,toggle,refresh:async()=>{intervals.at(-1)();await flush();},read:()=>root.querySelector('#dashJournalRead').onclick()};
}

test('journal reads only on explicit action and distinguishes unread, unstarted, empty and unknown counters', async () => {
  const d=await dashboard(async()=>response());
  assert.ok(d.root.textContent.includes('まだ読んでいません'));
  await d.refresh();assert.equal(d.calls.length,0);
  d.toggle(true);await d.read();
  assert.deepEqual(d.calls,[{limit:50}]);
  assert.ok(d.root.textContent.includes('開始していません'));
  assert.ok(d.root.textContent.includes('保存された記録はありません'));
  assert.ok(d.root.textContent.includes('欠落 不明'));
  assert.ok(d.root.textContent.includes('最古 未記録'));
});

test('latest fifty escaped summaries and loss/retention survive existing dashboard redraw without new journal poll', async () => {
  const events=Array.from({length:62},(_,id)=>({id,ts:1700000000000+id,node_id:'<img src=x onerror=bad>',summary:'<script>bad()</script>',data:{committed:false}}));
  const d=await dashboard(async()=>response(events,{status:'degraded',summary:'保存失敗があります',dropped_total:3,db_failures:2,unconfirmed_acks:1,overflow_total:0}));
  d.toggle(true);await d.read();
  assert.equal(d.root.querySelectorAll('[data-monitor-event]').length,50);
  assert.equal(d.root.querySelector('img'),null);assert.equal(d.root.querySelector('script'),null);
  assert.ok(d.root.textContent.includes('<script>bad()</script>'));
  assert.ok(d.root.textContent.includes('欠落 3'));assert.ok(d.root.textContent.includes('保存失敗 2'));
  assert.ok(d.root.textContent.includes('未確認ACK 1'));assert.ok(d.root.textContent.includes('保存失敗'));
  assert.ok(!d.root.textContent.includes('最古 未記録'));
  await d.refresh();
  assert.equal(d.root.querySelector('#dashJournal').open,true);
  assert.equal(d.root.querySelectorAll('[data-monitor-event]').length,50);
  assert.equal(d.calls.length,1);
  d.toggle(false);await d.refresh();assert.equal(d.root.querySelector('#dashJournal').open,false);
});

test('inflight read is single and a failure permits retry without displaying exception text', async () => {
  const pending=deferred();let count=0;
  const d=await dashboard(()=>++count===1?pending.promise:Promise.resolve(response()));d.toggle(true);
  const first=d.read();await d.read();assert.equal(d.calls.length,1);
  assert.equal(d.root.querySelector('#dashJournalRead').disabled,true);
  pending.reject(new Error('dummy-secret-canary'));await first;
  assert.ok(d.root.textContent.includes('読み出せません'));
  assert.ok(!d.root.textContent.includes('dummy-secret-canary'));
  assert.equal(d.root.querySelector('#dashJournalRead').disabled,false);
  await d.read();assert.equal(d.calls.length,2);assert.ok(d.root.textContent.includes('保存された記録はありません'));
});

test('response from closed details or a departed view never replaces journal data or another page', async () => {
  const first=deferred(),second=deferred();let count=0;
  const d=await dashboard(()=>++count===1?first.promise:second.promise);d.toggle(true);
  const a=d.read();d.toggle(false);first.resolve(response([{summary:'obsolete-closed'}]));await a;
  d.toggle(true);assert.ok(d.root.textContent.includes('まだ読んでいません'));assert.ok(!d.root.textContent.includes('obsolete-closed'));
  const b=d.read();d.view.leave();d.context.state.view='logs';d.root.innerHTML='<p>other-page</p>';
  second.resolve(response([{summary:'obsolete-other-view'}]));await b;
  assert.equal(d.root.textContent,'other-page');
  d.context.state.view='dashboard';d.view.render();await flush();assert.ok(!d.root.textContent.includes('obsolete-other-view'));
  assert.ok(d.root.textContent.includes('まだ読んでいません'));
});

test('legacy renderer without monitorEvents shows a bounded unsupported hint', async () => {
  const d=await dashboard();d.toggle(true);
  assert.ok(d.root.textContent.includes('この版では収集記録を読めません'));
  assert.equal(d.root.querySelector('#dashJournalRead'),null);
});

test('Tauri bridge invokes monitor_events with the exact typed filter object or null', async () => {
  const calls=[];
  const context={__KT_PLATFORM__:'darwin',__KT_DEBUG__:false,window:{__TAURI_INTERNALS__:{invoke:async(cmd,args)=>{calls.push({cmd,args});return {};}}},document:{addEventListener(){},styleSheets:[]},MutationObserver:class{observe(){}},getComputedStyle:()=>({getPropertyValue:()=>''})};
  vm.createContext(context);vm.runInContext(fs.readFileSync(require.resolve('../src-tauri/bridge/tune.js'),'utf8'),context);
  await context.window.tune.monitorEvents({limit:50});await context.window.tune.monitorEvents();
  assert.deepEqual(JSON.parse(JSON.stringify(calls)),[{cmd:'monitor_events',args:{filter:{limit:50}}},{cmd:'monitor_events',args:{filter:null}}]);
});
