'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { normalizeItems, normName, matrix } = require('../lib/inventory');
const dogu = require('../lib/dogu');
const { openDb } = require('../lib/db');

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'kt-inv-'));

test('棚卸しの結果を種類と名前で一意にし、空の名前を捨てる', () => {
  const items = normalizeItems([
    { source: 'brew', name: 'ripgrep', version: '14.1.0', explicit: true },
    { source: 'brew', name: 'ripgrep', version: '14.1.0' },
    { source: 'app', name: 'Example App', version: 3, id: 'com.example.app' },
    { source: 'brew', name: '  ' },
    { source: 'npm', name: 'tool', version: '' },
  ]);
  assert.equal(items.length, 3);
  assert.deepEqual(items[1], { source: 'app', name: 'Example App', version: '3', explicit: true, extra: { id: 'com.example.app' } });
  assert.equal(items[2].version, null);
});

test('名前の正規化（アーキテクチャ・版・記号を落とす）', () => {
  assert.equal(normName('Visual Studio Code.app'), 'visualstudiocode');
  assert.equal(normName('Example Tool (x64)'), 'exampletool');
  assert.equal(normName('Example Tool 2.4.1'), 'exampletool');
  assert.equal(normName('秀丸エディタ'), '秀丸エディタ');
});

test('機体×道具の表で版の違い（drift）を出す', () => {
  const rows = [
    { node_id: 'a', source: 'brew', name: 'jq', version: '1.7', explicit: true },
    { node_id: 'b', source: 'brew', name: 'jq', version: '1.8', explicit: true },
    { node_id: 'a', source: 'brew', name: 'libfoo', version: '1', explicit: false },
    { node_id: 'b', source: 'cask', name: 'gone', version: '1', explicit: true, removed_at: 5 },
  ];
  const m = matrix(rows);
  assert.equal(m.length, 1);
  assert.equal(m[0].node_count, 2);
  assert.equal(m[0].drift, true);
  assert.equal(matrix(rows, { explicitOnly: false }).length, 2);
});

test('棚卸しの保存: 初回は記録しない、以降は追加・削除・版の変化・再追加を記録する', () => {
  const db = openDb(tmp());
  const it = (name, version) => ({ source: 'brew', name, version, explicit: true, extra: null });
  assert.deepEqual(db.saveInventory('n1', [it('a', '1'), it('b', '1')], 1000), { added: 0, removed: 0, updated: 0, total: 2, baseline: true });
  assert.equal(db.inventoryEvents().length, 0);
  const c = db.saveInventory('n1', [it('a', '2'), it('c', '1')], 2000);
  assert.deepEqual({ ...c }, { added: 1, removed: 1, updated: 1, total: 2, baseline: false });
  assert.deepEqual(db.inventory({ node_id: 'n1' }).map((r) => `${r.name}@${r.version}`), ['a@2', 'c@1']);
  assert.equal(db.inventory({ node_id: 'n1', includeRemoved: true }).find((r) => r.name === 'b').removed_at, 2000);
  db.saveInventory('n1', [it('a', '2'), it('b', '1'), it('c', '1')], 3000);
  const b = db.inventory({ node_id: 'n1' }).find((r) => r.name === 'b');
  assert.equal(b.removed_at, null);
  assert.equal(b.first_seen, 3000);
  assert.deepEqual(db.inventoryEvents().map((e) => `${e.kind}:${e.name}`), ['added:b', 'removed:b', 'added:c', 'updated:a']);
  assert.equal(db.saveInventory('n2', [it('a', '1')], 4000).baseline, true);
});

test('Do-gu: マスターの slug・名前・別表記で照合し、デッキの下書きを作る', () => {
  const tools = [
    { slug: 'visual-studio-code', name: 'Visual Studio Code', category: 'editor' },
    { slug: 'ripgrep', name: 'ripgrep', category: 'cli' },
    { slug: 'node-js', name: 'Node.js', category: 'language' },
    { slug: 'secret-tool', name: 'Secret Tool', category: 'other' },
  ];
  const match = dogu.makeMatcher(tools);
  assert.equal(match({ name: 'Visual Studio Code' }), 'visual-studio-code');
  assert.equal(match({ name: 'code' }), 'visual-studio-code');
  assert.equal(match({ name: 'node' }), 'node-js');
  assert.equal(match({ name: '@scope/ripgrep' }), 'ripgrep');
  assert.equal(match({ name: 'unknown-thing' }), null);

  const rows = [
    { node_id: 'a', source: 'app', name: 'Visual Studio Code', version: '1', explicit: true },
    { node_id: 'b', source: 'winreg', name: 'Microsoft Visual Studio Code (User)', version: '1', explicit: true },
    { node_id: 'a', source: 'brew', name: 'ripgrep', version: '14', explicit: true },
    { node_id: 'a', source: 'cask', name: 'secret-tool', version: '1', explicit: true },
    { node_id: 'a', source: 'brew', name: 'unknown-thing', version: '1', explicit: true },
  ];
  const m = matrix(rows, { matchSlug: match });
  assert.equal(m.find((g) => g.slug === 'visual-studio-code').node_count, 2);
  const draft = dogu.deckDraft(m, tools, ['secret-tool']);
  assert.deepEqual(draft.map((d) => d.slug), ['ripgrep', 'visual-studio-code']);
  assert.deepEqual(dogu.deckPayload(['ripgrep', 'ripgrep', 'gh']), { items: [{ tool: { slug: 'ripgrep' } }, { tool: { slug: 'gh' } }] });
});

test('Do-gu の API キー: 環境変数 → 保存場所の順で探し、無ければ null', () => {
  const home = tmp();
  assert.equal(dogu.apiKey({}, home, 'darwin'), null);
  fs.mkdirSync(path.join(home, '.config', 'do-gu'), { recursive: true });
  fs.writeFileSync(path.join(home, '.config', 'do-gu', 'api_key'), 'k-config\n');
  assert.equal(dogu.apiKey({}, home, 'darwin'), 'k-config');
  fs.mkdirSync(path.join(home, '.local', 'share', 'do-gu'), { recursive: true });
  fs.writeFileSync(path.join(home, '.local', 'share', 'do-gu', 'api_key'), 'k-share');
  assert.equal(dogu.apiKey({}, home, 'darwin'), 'k-share');
  assert.equal(dogu.apiKey({ DO_GU_API_KEY: ' k-env ' }, home, 'darwin'), 'k-env');
});

test('Do-gu: 同じ名前の道具が重なったら公式サイトとアイコンのあるほう、単体の CLI の claude は Claude Code', () => {
  const tools = [
    { slug: 'visual-studio-code', name: 'Visual Studio Code', category: 'editor', website_url: null, icon_url: '/i.png' },
    { slug: 'vscode', name: 'Visual Studio Code', category: 'editor', website_url: 'https://code.example', icon_url: '/j.png' },
    { slug: 'claude', name: 'Claude', category: 'ai-chat' },
    { slug: 'claude-code', name: 'Claude Code', category: 'agent-harness' },
  ];
  const match = dogu.makeMatcher(tools);
  assert.equal(match({ source: 'app', name: 'Visual Studio Code' }), 'vscode');
  assert.equal(match({ source: 'app', name: 'Claude' }), 'claude');
  assert.equal(match({ source: 'bin', name: 'claude' }), 'claude-code');
});
