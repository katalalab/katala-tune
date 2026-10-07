'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { ledgerTerms, scanText } = require('../scripts/oss-check');

test('台帳から探す語を作る（短すぎる語・一般的な語は除く）', () => {
  const cfg = { nodes: [{ id: 'gpu-box-7', alias: 'gpu-box-7', local_hostname: 'Office-PC.local' }, { id: 'mac', alias: 'pc' }], fleet: { repo: '~/src/fleet-x', env_file: 'secret.tpl' } };
  assert.deepEqual(ledgerTerms(cfg, ['someone']).sort(), ['Office-PC', 'gpu-box-7', 'someone', '~/src/fleet-x', 'secret.tpl'].sort());
  assert.deepEqual(ledgerTerms(null), []);
});

test('台帳の語は単語の境目でだけ一致する', () => {
  const rules = (t) => scanText(t, ['gpu-box-7']).map((x) => x.rule);
  assert.deepEqual(rules('ssh GPU-BOX-7 uptime'), ['ledger']);
  assert.deepEqual(rules('gpu-box-70 と my-gpu-box-7'), []);
});

test('一般的な目印を見つけ、例示用の名前は見逃す', () => {
  // 検出される形の文字列は実行時に組み立てる（ソースに置くと oss-check 自身がこのファイルを検出する）
  const j = (...p) => p.join('');
  const ip = ['100', '101', '2', '3'].join('.');
  const bad = [ip, j('box.tail', '1234.ts', '.net'), j('op:', '//Vault/item/field'), j('/Us', 'ers/realname/x'), j('C:', '\\Us', 'ers\\realname\\x'), j('real.person', '@corp.example', '.jp')];
  assert.deepEqual(scanText(bad.join('\n')).map((x) => x.rule), ['tailscale-ip', 'tailnet-host', 'op-ref', 'home-path', 'home-path', 'email']);
  const ok = ['/Users/me/Library', 'C:\\Users\\a\\x.txt', '10.0.0.1', 'foo@example.com', j('1', '@users.noreply.github', '.com'), j('noreply', '@anthropic', '.com'), '100.1.2.3'];
  assert.deepEqual(scanText(ok.join('\n')), []);
  assert.deepEqual(scanText(j('"author": "a.b', '@corp.example', '.jp"'), [], ['email']), []);
});
