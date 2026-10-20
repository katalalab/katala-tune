'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { webcrypto } = require('node:crypto');

const valid = {
  schema: 'task-completion.v1',
  tasks: [{ id: 'safe', goal: '<img src=x>', target: 'repository', revision: 'head', delivery: 'repository',
    gates: { verification: { status: 'pass', evidence: [{ ref: 'https://github.com/example/tune/pull/4', result: '<script>x</script>' }] } },
    review: { status: 'accepted' } }], coverage: { expected: ['repository'], observed: ['repository'], unobserved: [] },
};

function loadView({ crypto = webcrypto } = {}) {
  let html = '';
  const controls = new Map();
  const document = {
    querySelector(selector) { if (!controls.has(selector)) controls.set(selector, {}); return controls.get(selector); },
  };
  const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
  const window = { KT_VIEWS: [], KT_REVIEW_MODEL: null };
  const context = vm.createContext({ window, document, UI: { ICON: {}, esc, head: ({ title, desc }) => `<h1>${esc(title)}</h1><p>${esc(desc)}</p>` }, state: { view: 'review' }, setCrumbs: () => {}, page: (value) => { html = value; }, Date, URL, JSON, TextDecoder, crypto, console });
  vm.runInContext(fs.readFileSync('renderer/review-model.js', 'utf8'), context);
  vm.runInContext(fs.readFileSync('renderer/review.js', 'utf8'), context);
  return { view: window.KT_VIEWS[0], controls, get html() { return html; } };
}

function file(source, name = 'ledger.json', onRead = () => {}) {
  const bytes = new TextEncoder().encode(source);
  return { name, size: bytes.byteLength, arrayBuffer: async () => { onRead(); return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength); } };
}

test('2 MiB超は読み込み前に拒否し、入力例はエスケープ済みの申告として表示する', async () => {
  const app = loadView();
  app.view.render();
  let read = 0;
  await app.controls.get('#reviewFile').onchange({ target: { files: [{ size: 2 * 1024 * 1024 + 1, arrayBuffer: async () => { read++; return new ArrayBuffer(0); } }] } });
  assert.equal(read, 0);
  assert.match(app.html, /2 MiB/);

  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify(valid))] } });
  assert.match(app.html, /成功の申告（未照合）/);
  assert.match(app.html, /受入の申告（未照合）/);
  assert.match(app.html, /台帳の申告のみ。PRの現在のheadとは照合していません。生成元は未確認です。/);
  assert.equal((app.html.match(/対象範囲/g) || []).length, 1);
  assert.match(app.html, /対象版/);
  assert.match(app.html, /依頼の終点/);
  assert.match(app.html, /正式ブランチ/);
  for (const label of ['実装', '検証', '正式反映', '配布', '受入']) assert.match(app.html, new RegExp(`<h3>${label}</h3>`));
  assert.doesNotMatch(app.html, /<h3>implementation<\/h3>/);
  assert.match(app.html, /&lt;img src=x&gt;/);
  assert.doesNotMatch(app.html, /<img src=x>|<script>/);
  assert.match(app.html, /結果（台帳の申告）: &lt;script&gt;x&lt;\/script&gt;/);
  assert.match(app.html, /成功の申告の証跡の result が pass ではありません/);
  assert.match(app.html, /target="_blank" rel="noopener noreferrer"/);
  const link = app.html.match(/<a href="([^"]+)" target="_blank" rel="noopener noreferrer">([^<]+)<\/a>/);
  assert.equal(link[1], link[2]);
});

test('読み込み中はエラー文言を出さず、失敗時だけ読み込めなかったと表示する', async () => {
  const app = loadView();
  app.view.render();
  let resolve;
  const pending = app.controls.get('#reviewFile').onchange({ target: { files: [{ name: 'loading.json', size: 1, arrayBuffer: () => new Promise((done) => { resolve = done; }) }] } });
  assert.match(app.html, /<div class="co-title">読み込み中<\/div>/);
  assert.doesNotMatch(app.html, /読み込めませんでした/);
  resolve(new TextEncoder().encode('{').buffer);
  await pending;
  assert.match(app.html, /<div class="co-title">読み込めませんでした<\/div>/);
  assert.doesNotMatch(app.html, /<div class="co-title">読み込み中<\/div>/);
});

test('deliveryがlocal/repositoryなら配布段階を必須欠測として表示しない', async () => {
  const app = loadView();
  app.view.render();
  const source = { schema: 'task-completion.v1', tasks: [
    { id: 'repo', delivery: 'repository' },
    { id: 'env', delivery: 'environment' },
  ] };
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify(source))] } });
  const statuses = [...app.html.matchAll(/<h3>配布<\/h3><p>([^<]*)<\/p>/g)].map((match) => match[1]);
  assert.match(statuses[0], /必須ではありません（依頼の終点: 正式ブランチ）/);
  assert.doesNotMatch(statuses[0], /未記載/);
  assert.equal(statuses[1], '未記載');
});

test('長い文字列はreview-propsの値を省略せず、折り返し可能にする', async () => {
  const app = loadView();
  app.view.render();
  const long = 'x'.repeat(500);
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify({ schema: 'task-completion.v1', tasks: [{ id: 'long', goal: long }] }))] } });
  assert.ok(app.html.includes(`<div class="prop-k">目的</div><div class="prop-v">${long}</div>`));
  assert.match(fs.readFileSync('renderer/style.css', 'utf8'), /\.review-props \.prop-v \{[^}]*overflow-wrap: anywhere;/);
});

test('証跡の警告は段階ごとに一度だけ表示する', async () => {
  const app = loadView();
  app.view.render();
  const source = { schema: 'task-completion.v1', tasks: [{
    id: 'warnings', target: 'repository', revision: 'head',
    gates: { deployment: { status: 'pass', evidence: [{ ref: 'text', result: 'pass', observed_at: '2026-10-21T00:00:00Z', revision: 'other', environment: 'local' }] } },
  }] };
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify(source))] } });
  assert.equal((app.html.match(/未来の日時です/g) || []).length, 1);
  assert.equal((app.html.match(/revision が対象と異なります/g) || []).length, 1);
  assert.equal((app.html.match(/対象環境が対象と異なります/g) || []).length, 1);
});

test('読込競合、離脱、無効JSONでは古い台帳を再表示しない', async () => {
  const app = loadView();
  app.view.render();
  let firstResolve;
  const first = new Promise((resolve) => { firstResolve = resolve; });
  const field = app.controls.get('#reviewFile');
  const pending = field.onchange({ target: { files: [{ name: 'first.json', size: 10, arrayBuffer: async () => first }] } });
  await field.onchange({ target: { files: [file(JSON.stringify({ ...valid, tasks: [{ ...valid.tasks[0], id: 'new' }] }))] } });
  firstResolve(new TextEncoder().encode(JSON.stringify(valid)).buffer);
  await pending;
  assert.match(app.html, /new/);
  assert.doesNotMatch(app.html, />safe</);

  app.view.leave();
  app.view.render();
  assert.doesNotMatch(app.html, /new/);
  await app.controls.get('#reviewFile').onchange({ target: { files: [file('{')] } });
  assert.match(app.html, /JSON/);
  assert.doesNotMatch(app.html, /new/);
});

test('レビューのモデルと画面はapp.jsより前に登録される', () => {
  const html = fs.readFileSync('renderer/index.html', 'utf8');
  assert.match(html, /<script src="review-model\.js"><\/script>\s*<script src="review\.js"><\/script>[\s\S]*<script src="app\.js"><\/script>/);
});

test('台帳は全件数を示したまま25件ずつ表示し、証跡を畳む', async () => {
  const app = loadView();
  app.view.render();
  const tasks = Array.from({ length: 26 }, (_, i) => ({ id: `task-${i + 1}`, gates: { implementation: { evidence: [{ ref: 'text' }] } } }));
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify({ schema: 'task-completion.v1', tasks }))] } });
  assert.match(app.html, /1–25 \/ 26件/);
  assert.match(app.html, /task-25/);
  assert.doesNotMatch(app.html, /task-26/);
  assert.match(app.html, /<details class="review-evidence"/);
  await app.controls.get('#reviewNext').onclick();
  assert.match(app.html, /26–26 \/ 26件/);
  assert.match(app.html, /task-26/);
});

test('原本の既知SHA-256と選択情報を表示し、1 byte差を別の指紋にする', async () => {
  const app = loadView();
  app.view.render();
  let reads = 0;
  const source = '{"schema":"task-completion.v1","tasks":[{"id":"hash"}]}';
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(source, 'receipt.json', () => { reads++; })] } });
  assert.equal(reads, 1);
  assert.match(app.html, /receipt\.json/);
  assert.match(app.html, /b32ae09b6425033fa24eff43d5aaa1be2cd27a68e48b821e5768cfd322d22ab8/);
  const firstHash = app.html.match(/[a-f0-9]{64}/)[0];
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(source.replace('hash', 'mash'))] } });
  assert.notEqual(app.html.match(/[a-f0-9]{64}/)[0], firstHash);
});

test('WebCryptoが無い場合も台帳を成功扱いにせず、指紋未計算を表示する', async () => {
  const app = loadView({ crypto: null });
  app.view.render();
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify(valid))] } });
  assert.match(app.html, /指紋未計算/);
  assert.match(app.html, /成功の申告（未照合）/);
});

test('SHA-256計算中に次の選択があれば古い表示を戻さない', async () => {
  let resolveFirst;
  let calls = 0;
  const crypto = { subtle: { digest: (...args) => (++calls === 1 ? new Promise((resolve) => { resolveFirst = resolve; }) : webcrypto.subtle.digest(...args)) } };
  const app = loadView({ crypto });
  app.view.render();
  const first = app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify({ ...valid, tasks: [{ ...valid.tasks[0], id: 'old' }] }))] } });
  await new Promise((resolve) => setImmediate(resolve));
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify({ ...valid, tasks: [{ ...valid.tasks[0], id: 'new' }] }))] } });
  resolveFirst(new Uint8Array(32).buffer);
  await first;
  assert.match(app.html, />new</);
  assert.doesNotMatch(app.html, />old</);
});

test('2 MiB以内でも原本読込エラーでは古い台帳を残さない', async () => {
  const app = loadView();
  app.view.render();
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify(valid))] } });
  await app.controls.get('#reviewFile').onchange({ target: { files: [{ name: 'broken.json', size: 12, arrayBuffer: async () => { throw Error('read failed'); } }] } });
  assert.match(app.html, /入力を受け付けられません/);
  assert.doesNotMatch(app.html, />safe</);
});

test('未照合の案内は初期、読込中、入力エラーでも常に表示する', async () => {
  const app = loadView();
  app.view.render();
  const notice = /台帳の申告のみ。PRの現在のheadとは照合していません。生成元は未確認です。/;
  assert.match(app.html, notice);
  let resolve;
  const pending = app.controls.get('#reviewFile').onchange({ target: { files: [{ name: 'loading.json', size: 1, arrayBuffer: () => new Promise((done) => { resolve = done; }) }] } });
  assert.match(app.html, notice);
  resolve(new TextEncoder().encode('{').buffer);
  await pending;
  assert.match(app.html, notice);
});

test('coverageは件数要約で畳み、ページャーとカードより前で全値を展開可能にする', async () => {
  const app = loadView();
  app.view.render();
  const source = { schema: 'task-completion.v1', tasks: [{ id: 'first' }], coverage: { expected: ['a', 'b'], observed: ['a'], unobserved: ['b'] } };
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify(source))] } });
  assert.match(app.html, /<details class="review-coverage"/);
  assert.match(app.html, /想定 2・観測 1・未観測 1/);
  assert.ok(app.html.indexOf('review-pager') < app.html.indexOf('review-coverage'));
  assert.ok(app.html.indexOf('review-coverage') < app.html.indexOf('>first</h2>'));
  assert.match(app.html, /申告された未観測: b/);
});

test('不正UTF-8は原本を解析せず、前の台帳を残さない', async () => {
  const app = loadView();
  app.view.render();
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify(valid))] } });
  await app.controls.get('#reviewFile').onchange({ target: { files: [{ name: 'bad-utf8.json', size: 1, arrayBuffer: async () => new Uint8Array([0xff]).buffer }] } });
  assert.match(app.html, /入力を受け付けられません/);
  assert.doesNotMatch(app.html, />safe</);
});

test('内部のnull欠測は未記載と表示し、literalの未記載はそのまま表示する', async () => {
  const app = loadView();
  app.view.render();
  const source = { schema: 'task-completion.v1', tasks: [{ id: 'literal', revision: '未記載', target: '未記載', review: { status: '未記載', ref: '未記載', observed_at: '未記載' } }] };
  await app.controls.get('#reviewFile').onchange({ target: { files: [file(JSON.stringify(source))] } });
  assert.match(app.html, /<div class="prop-k">目的<\/div><div class="prop-v">未記載<\/div>/);
  assert.match(app.html, /未対応の申告値: 未記載/);
  assert.match(app.html, /レビュー日時が不正/);
});
