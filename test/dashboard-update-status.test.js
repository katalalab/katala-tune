'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const UI = require('../renderer/ui');

async function renderStatus(app) {
  let html = '';
  const root = {querySelectorAll: () => []};
  const data = {at: Date.now(), collecting: true, monitor: {enabled: true}, nodes: [], app};
  const ctx = {
    UI, Charts: {spark: () => '', bars: () => ''}, state: {view: 'dashboard', nodes: []},
    document: {visibilityState: 'visible', addEventListener(){}, querySelector: () => null, querySelectorAll: () => []},
    window: {tune: {dashboard: async () => data, onLive(){}, liveStart: async () => {}, liveStop: async () => {}}},
    page: value => { html = value; }, $: s => s === '#page' ? root : null,
    ticket: () => 1, stale: () => false, setCrumbs(){}, bindGoto(){}, ago: () => '直前',
    setInterval: () => 1, clearInterval(){}, requestAnimationFrame() {},
  };
  vm.runInNewContext(fs.readFileSync('renderer/dashboard.js', 'utf8'), ctx);
  ctx.window.KT_VIEWS[0].render();
  await new Promise(setImmediate);
  return html;
}

test('old downloaded version is not reported as a pending install after a no-update check', async () => {
  const html = await renderStatus({version:'0.6.5-specs.8', auto_update:true, update:{current:'0.6.5-specs.8', available:null, downloaded:'0.6.4', checked_at:123}});
  assert.match(html, /保存された確認記録: 更新なし（直前）/);
  assert.doesNotMatch(html, /0\.6\.4 を用意済み|閉じると入れる/);
});

test('a previous download does not claim a pending install after auto update is switched off', async () => {
  const html = await renderStatus({version:'0.6.4', auto_update:false, update:{current:'0.6.4', available:'0.7.0', downloaded:'0.7.0', checked_at:123}});
  assert.match(html, /自動更新オフ/);
  assert.match(html, /保存された確認記録: 0\.7\.0 あり/);
  assert.doesNotMatch(html, /用意済み|閉じると入れる/);
});

test('a failed check does not report an earlier available version as current', async () => {
  const html = await renderStatus({version:'0.6.4', auto_update:true, update:{current:'0.6.4', available:'0.7.0', downloaded:'0.7.0', checked_at:123, error:'接続できません'}});
  assert.match(html, /保存された更新エラー: 接続できません/);
  assert.doesNotMatch(html, /0\.7\.0 あり|最新（/);
});


test('a previous version’s update result is not reported for the installed version', async () => {
  const html = await renderStatus({version:'0.7.0', auto_update:true, update:{current:'0.6.4', available:'0.6.5', checked_at:123}});
  assert.match(html, /確認情報が古い/);
  assert.doesNotMatch(html, /0\.6\.5 あり|更新なし/);
});

test('a legacy update record without its originating version is not treated as current', async () => {
  const html = await renderStatus({version:'0.7.0', auto_update:true, update:{available:null, checked_at:123}});
  assert.match(html, /確認情報が不明/);
  assert.doesNotMatch(html, /更新なし/);
});


test('an available version equal to the installed version is still only a stored record', async () => {
  const html = await renderStatus({version:'0.7.0', auto_update:true, update:{current:'0.7.0', available:'0.7.0', checked_at:123}});
  assert.match(html, /保存された確認記録: 0\.7\.0 あり/);
  assert.doesNotMatch(html, /更新なし|最新（/);
});

test('an app without update history reports no saved check', async () => {
  const html = await renderStatus({version:'0.7.0', auto_update:true});
  assert.match(html, /確認記録なし/);
});
