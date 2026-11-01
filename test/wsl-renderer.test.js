'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

function render(wsl) {
  const source = fs.readFileSync(path.join(__dirname, '../renderer/app.js'), 'utf8');
  const code = source.slice(source.indexOf('const WSL_STATE ='), source.indexOf('\nfunction procsHtml('));
  const context = vm.createContext({
    data: { wsl },
    esc: (v) => String(v).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;'),
    UI: { section: (s) => s, chip: (s) => s, table: ({rows, row}) => rows.map(row).join('') },
  });
  return vm.runInContext(`${code}\nwslHtml(data)`, context);
}

test('一覧取得失敗は機体の詳細に残り、正常空一覧とは区別される', () => {
  assert.match(render({ list_error: 'timeout', distros: [] }), /一覧を読めなかった.*timeout/);
  assert.equal(render({ listed: true, distros: [] }), '');
});

test('部分取得の詳細は成功したメモリと欠測項目・理由を一緒に表示する', () => {
  const html = render({ distros: [{ name: 'dev-a', state: 'running', observed: true, inside: {
    mem_total_mb: 4096, mem_available_mb: 2048, top: [], disk: [],
    errors: { top: 'command-failed', disk: 'invalid-data', psi: 'missing-status' },
  } }] });
  assert.match(html, /メモリ 2.0 GB \/ 4.0 GB/);
  assert.match(html, /未取得.*PSI.*取得結果を確認できなかった.*上位プロセス.*取得コマンドが失敗.*ディスク.*値を読めなかった/);
});

test('旧 snapshot と正常な空プロセスには部分失敗を表示しない', () => {
  for (const errors of [undefined, {}, { top: null }]) {
    const html = render({ distros: [{ name: 'dev-a', state: 'running', observed: true, inside: { mem_total_mb: 4096, mem_available_mb: 2048, top: [], errors } }] });
    assert.doesNotMatch(html, /未取得/);
  }
});
