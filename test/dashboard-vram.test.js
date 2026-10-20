'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const UI = require('../renderer/ui');
const fixtures = require('./fixtures/vram-slots.json');

// A small element tree for the renderer's DOM operations. No browser or layout is
// involved: the test drives onLive, queues animation frames and observes mutations
// of the same mounted element, including initially empty containers.
class Element {
  constructor(tag = 'div', attrs = {}) {
    this.tagName = tag; this.attrs = attrs; this.children = []; this.dataset = {};
    this.classList = {toggle:() => {}};
  }
  setAttribute(k, v) { this.attrs[k] = v; }
  set innerHTML(html) {
    this.html = html;
    const stack = [this]; this.children = [];
    for (const token of html.match(/<[^>]+>|[^<]+/g) || []) {
      if (token.startsWith('</')) { if (stack.length > 1) stack.pop(); }
      else if (token.startsWith('<')) {
        const tag = /^<([\w-]+)/.exec(token)?.[1]; if (!tag) continue;
        const attrs = Object.fromEntries([...token.matchAll(/([\w-]+)="([^"]*)"/g)].map(m => [m[1], m[2]]));
        const e = new Element(tag, attrs); stack.at(-1).children.push(e);
        if (!/\/>$/.test(token) && !['input','br','hr','img','meta','link'].includes(tag)) stack.push(e);
      } else stack.at(-1).children.push(token);
    }
  }
  get innerHTML() { return this.html ?? this.children.map(c => typeof c === 'string' ? c : c.innerHTML).join(''); }
  get textContent() { return this.children.map(c => typeof c === 'string' ? c : c.textContent).join(''); }
  get firstElementChild() { return this.children.find(c => c instanceof Element); }
  get className() { return this.attrs.class || ''; }
  set className(v) { this.attrs.class = v; }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  querySelectorAll(selector) {
    const matches = e => selector.startsWith('.') ? e.className.split(' ').includes(selector.slice(1))
      : selector.startsWith('#') ? e.attrs.id === selector.slice(1)
      : selector.startsWith('[') ? (() => { const m = /^\[([\w-]+)(?:="([^"]*)")?\]$/.exec(selector); return m && (m[2] == null ? m[1] in e.attrs : e.attrs[m[1]] === m[2]); })()
      : e.tagName === selector;
    const all = this.children.filter(c => c instanceof Element).flatMap(c => [c, ...c.descendants()]);
    return all.filter(matches);
  }
  descendants() { return this.children.filter(c => c instanceof Element).flatMap(c => [c, ...c.descendants()]); }
}

async function dashboard(initial = {}, minuteValues = {}) {
  const root = new Element(); let loads = 0, pages = 0; const listeners = [], frames = [];
  const data = {at:Date.now(), collecting:true, monitor:{enabled:true}, nodes:[{id:'fixture',os:'windows',latest:{minute:Date.now(),v:minuteValues},series:{},live:{state:'running'}}]};
  const context = {
    UI, Charts:{spark:() => '', bars:() => ''}, state:{view:'dashboard',nodes:[]},
    document:{visibilityState:'visible',addEventListener(){},createElement:() => new Element(),querySelector:s => root.querySelector(s),querySelectorAll:s => root.querySelectorAll(s),getElementById:id => root.querySelector(`#${id}`)},
    window:{tune:{dashboard:async () => { loads++; return data; },onLive:fn => listeners.push(fn),liveStart:async()=>({}),liveStop:async()=>({})}},
    CSS:{escape:String}, page:html => { pages++; root.innerHTML = html; }, $:s => s === '#page' ? root : root.querySelector(s),
    ticket:() => 1, stale:() => false, setCrumbs(){}, bindGoto(){}, ago:String,
    setInterval:() => 1, clearInterval(){}, requestAnimationFrame:fn => { frames.push(fn); return frames.length; },
  };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(require.resolve('../renderer/live'),'utf8'), context);
  const live = vm.runInContext('Live', context);
  Object.assign(live.store, initial);
  vm.runInContext(fs.readFileSync(require.resolve('../renderer/dashboard'),'utf8'), context);
  context.window.KT_VIEWS[0].render();
  await new Promise(setImmediate);
  return {root,live,counts:()=>({loads,pages}),mountLive:() => {
    root.innerHTML = '<button data-live-toggle="node"></button><div id="livePanel"></div>';
    live.mount('node',['fixture'],[{id:'fixture',os:'windows'}]);
    root.querySelector('[data-live-toggle]').onclick();
    return root.querySelector('.lv-main');
  },send:node => { for (const fn of listeners) fn({nodes:{fixture:node}}); while (frames.length) frames.shift()(); }};
}

test('dashboard live event fills the initially empty VRAM DOM and updates used/free/percent without reloading', async () => {
  const d = await dashboard();
  const tile = d.root.querySelector('[data-dash="fixture"]');
  const vram = tile.querySelector('.dt-vram');
  assert.ok(vram, 'an empty persistent VRAM target must exist before the first value');
  assert.equal(vram.textContent, '');
  d.send({state:'running',gpus:[{adapter_index:0,storage_slot:0}],points:[{t:Date.now(),gpu_0_vram_used_mib:0,gpu_0_vram_total_mib:100,gpu_0_vram_free_mib:99,gpu_0_vram_pct:0}]});
  assert.equal(tile.querySelector('.dt-vram'), vram, 'live updates reuse the mounted target');
  assert.ok(vram.textContent.includes('VRAM 0 / 100 MiB'));
  assert.ok(vram.textContent.includes('空き 99 MiB'));
  assert.ok(vram.textContent.includes('0%'));
  d.send({points:[{t:Date.now()+1,gpu_0_vram_used_mib:50,gpu_0_vram_total_mib:100,gpu_0_vram_free_mib:49,gpu_0_vram_pct:50}]});
  assert.ok(vram.textContent.includes('VRAM 50 / 100 MiB'));
  assert.ok(vram.textContent.includes('空き 49 MiB'));
  assert.ok(vram.textContent.includes('50%'));
  d.send({points:[{t:Date.now()+2,cpu:0}],gpus:[{adapter_index:0,storage_slot:0,memory_status:'unavailable',memory_reason:'query-timeout'}]});
  assert.ok(vram.textContent.includes('未取得'), 'a fresh missing value must clear the previous numbers');
  assert.ok(!vram.textContent.includes('50 / 100'));
  assert.deepEqual(d.counts(),{loads:1,pages:1});
});

test('shared slot fixtures use bounded storage keys while keeping physical labels and rejection reasons', async () => {
  for (const f of fixtures) {
    const gpus = f.gpus.map((g,i) => ({...g,storage_slot:f.slots[i],...(f.reasons[i] ? {mem_used_mb:null,mem_total_mb:null,mem_free_mb:null,mem_used_pct:null,memory_status:'unavailable',memory_reason:f.reasons[i]} : {})}));
    const d = await dashboard();
    d.send({gpus,points:[{t:Date.now(),...f.metrics}]});
    const tile = d.root.querySelector('[data-dash="fixture"]');
    const text = tile.querySelector('.dt-vram')?.textContent || '';
    for (const g of gpus) {
      if (g.storage_slot != null && g.mem_used_mb != null) {
        assert.ok(text.includes(`GPU ${g.adapter_index ?? '?'} VRAM ${g.mem_used_mb} /`),f.name);
        assert.ok(text.includes(`空き ${g.mem_free_mb} MiB`),f.name);
      } else assert.ok(text.includes('未取得'),f.name);
    }
    if (f.reasons.some(Boolean)) assert.ok(text.includes('GPU番号'),`${f.name}: identity rejection must be explained`);
    assert.deepEqual(d.counts(),{loads:1,pages:1});
    const history = await dashboard({},Object.fromEntries(Object.entries(f.metrics).map(([k,v]) => [k,[v,v]])));
    const historical = history.root.querySelector('.dt-vram').textContent;
    for (const g of gpus.filter(g => g.storage_slot != null && g.mem_used_mb != null)) {
      assert.ok(historical.includes(`GPU ${g.storage_slot} VRAM ${g.mem_used_mb} /`),`${f.name}: minute projection`);
      assert.ok(historical.includes(`空き ${g.mem_free_mb} MiB`),f.name);
    }
    const live = d.mountLive();
    const gpuRows = live.querySelectorAll('.lv-row').filter(r => r.querySelector('.lv-k').textContent.startsWith('GPU ') && !r.querySelector('.lv-k').textContent.includes('電力'));
    assert.equal(gpuRows.length,gpus.length,f.name);
    for (let i=0; i<gpus.length; i++) {
      const g=gpus[i], row=gpuRows[i];
      assert.ok(row.querySelector('.lv-k').textContent.startsWith(`GPU ${g.adapter_index ?? '?'}`),f.name);
      const expected=g.storage_slot == null ? null : f.metrics[`gpu_${g.storage_slot}_vram_pct`];
      assert.ok(row.querySelector('.lv-v').textContent.startsWith(expected == null ? '–' : `${expected}%`),f.name);
      if(f.reasons[i]) assert.ok(row.textContent.includes('GPU番号'),f.name);
    }
  }
});
