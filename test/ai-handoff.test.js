'use strict';
// 所見を AI エージェントへ渡す（Tauri 版だけ）。画面からの呼び方と、権限の宣言が揃っていることを確かめる。
// プロンプトの中身・断る条件・起動の引数は crates/tune-core/src/handoff.rs のテストが守る
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function bridge() {
  const calls = [];
  const context = { __KT_PLATFORM__: 'win32', __KT_DEBUG__: false, window: { __TAURI_INTERNALS__: { invoke: async (cmd, args) => { calls.push({ cmd, args }); return {}; } } }, document: { addEventListener() {}, styleSheets: [] } };
  vm.createContext(context);
  vm.runInContext(fs.readFileSync(require.resolve('../src-tauri/bridge/tune.js'), 'utf8'), context);
  return { tune: context.window.tune, calls };
}

test('Tauri bridge sends only ids and the agent name; the prompt is built on the Rust side', async () => {
  const { tune, calls } = bridge();
  await tune.aiHandoff('node-a', 'mem-high', 'claude');
  await tune.aiHandoffCopy('node-a', 'mem-high');
  await tune.aiHandoffReview('review-id', false);
  assert.deepEqual(JSON.parse(JSON.stringify(calls)), [
    { cmd: 'ai_handoff', args: { nodeId: 'node-a', findingId: 'mem-high', agent: 'claude' } },
    { cmd: 'ai_handoff_copy', args: { nodeId: 'node-a', findingId: 'mem-high' } },
    { cmd: 'ai_handoff_review', args: { id: 'review-id', proceed: false } },
  ]);
});

test('handoff commands are declared, permitted for the main window, and registered', () => {
  const build = fs.readFileSync(require.resolve('../src-tauri/build.rs'), 'utf8');
  const capability = JSON.parse(fs.readFileSync(require.resolve('../src-tauri/capabilities/default.json'), 'utf8'));
  const handlers = fs.readFileSync(require.resolve('../src-tauri/src/lib.rs'), 'utf8');
  for (const cmd of ['ai_handoff', 'ai_handoff_copy', 'ai_handoff_review']) {
    assert.match(build, new RegExp(`"${cmd}",`));
    assert.ok(capability.permissions.includes(`allow-${cmd.replace(/_/g, '-')}`), cmd);
    assert.match(handlers, new RegExp(`${cmd === 'ai_handoff_review' ? 'handoff_review' : 'commands'}::${cmd}\\s*,`));
  }
});

test('Electron preload does not expose handoff, so the renderer hides the buttons there', () => {
  const preload = fs.readFileSync(require.resolve('../preload.js'), 'utf8');
  assert.doesNotMatch(preload, /aiHandoff/);
  const app = fs.readFileSync(require.resolve('../renderer/app.js'), 'utf8');
  assert.match(app, /if \(window\.tune\.aiHandoff\) act \+=/);
});
