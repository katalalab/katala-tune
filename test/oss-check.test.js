'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { ledgerTerms, scanText, scanFileText } = require('../scripts/oss-check');

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

test('GitHub のボットのアドレス（Dependabot の Signed-off-by など）はメールとして数えない', () => {
  const j = (...p) => p.join('');
  const bots = [
    j('Signed-off-by: dependabot[bot] <support', '@github', '.com>'),
    j('GitHub <noreply', '@github', '.com>'),
    j('dependabot[bot] <49699333+dependabot[bot]', '@users.noreply.github', '.com>'),
  ];
  assert.deepEqual(scanText(bots.join('\n')), []);
  // 同じドメインでも人のアドレス・似た別のドメインは見つける
  assert.deepEqual(scanText(j('real.person', '@github', '.com')).map((x) => x.rule), ['email']);
  assert.deepEqual(scanText(j('support', '@github', '.company.jp')).map((x) => x.rule), ['email']);
});

test('--history は HEAD から辿れるコミットだけ、--all-refs で全部の枝を見る', (t) => {
  const { execFileSync } = require('node:child_process');
  const fs = require('node:fs');
  const os = require('node:os');
  const path = require('node:path');
  // 使い捨てのリポジトリ（一時ディレクトリ）を作って、スクリプトをそこで動かす
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-oss-'));
  t.after(() => fs.promises.rm(dir, { recursive: true, force: true }));
  const cfg = ['-c', 'user.name=t', '-c', 'user.email=t@example.com', '-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=' + path.join(dir, 'no-hooks')];
  const vcs = (...a) => execFileSync('git', [...cfg, ...a], { cwd: dir, encoding: 'utf8' });
  vcs('init', '-q');
  vcs('checkout', '-q', '-b', 'trunk');
  fs.writeFileSync(path.join(dir, 'LICENSE'), 'MIT');
  fs.writeFileSync(path.join(dir, 'package.json'), '{ "license": "MIT" }');
  fs.mkdirSync(path.join(dir, 'vendor', 'taffy-0.9.0'), { recursive: true });
  for (const file of ['Cargo.toml', 'Cargo.toml.orig']) {
    fs.writeFileSync(path.join(dir, 'vendor', 'taffy-0.9.0', file), ['alice.i.cecile', '@gmail', '.com'].join(''));
  }
  vcs('add', '.');
  vcs('commit', '-q', '-m', 'init');
  // 別の枝のコミットメッセージにだけ、人のメールアドレスがある
  vcs('checkout', '-q', '-b', 'side');
  const email = ['real.person', '@corp.example', '.jp'].join('');
  vcs('commit', '-q', '--allow-empty', '-m', `side\n\nSigned-off-by: someone <${email}>`);
  vcs('checkout', '-q', 'trunk');
  const script = path.join(__dirname, '..', 'scripts', 'oss-check.js');
  const run = (...a) => {
    const env = { ...process.env, KATALA_TUNE_NODES: path.join(dir, 'none.json') };
    try { return { code: 0, out: execFileSync(process.execPath, [script, ...a], { cwd: dir, encoding: 'utf8', env }) }; }
    catch (e) { return { code: e.status, out: e.stdout }; }
  };
  const head = run('--history');
  assert.equal(head.code, 0, head.out);
  assert.match(head.out, /HEAD から辿れる 1 コミット/);
  const all = run('--history', '--all-refs');
  assert.equal(all.code, 1, all.out);
  assert.match(all.out, /全部の枝の 2 コミット/);
  assert.match(all.out, /NG email\s+commit [0-9a-f]{7} message:3/);
  assert.equal(run('--all-refs').code, 1, '--all-refs だけでも全部の枝の履歴を見る');
});

test('Taffy の公開作者メールは正確な二つの manifest だけで許可する', () => {
  const email = ['alice.i.cecile', '@gmail', '.com'].join('');
  const manifests = ['vendor/taffy-0.9.0/Cargo.toml', 'vendor/taffy-0.9.0/Cargo.toml.orig'];
  for (const file of manifests) assert.deepEqual(scanFileText(file, email), [], file);
  assert.deepEqual(scanFileText('package-lock.json', email), []);
  for (const file of ['vendor/other/Cargo.toml', 'vendor/taffy-0.9.0/src/lib.rs', 'crates/tune-gpui/Cargo.toml', 'src/app.js']) {
    assert.deepEqual(scanFileText(file, email).map((x) => x.rule), ['email'], file);
  }
});

test('Taffy manifest でもメール以外の目印と台帳の語は検出する', () => {
  const text = [
    ['100', '101', '2', '3'].join('.'),
    ['box.tail', '1234.ts', '.net'].join(''),
    ['op:', '//Vault/item/field'].join(''),
    ['/Us', 'ers/realname/x'].join(''),
    'gpu-box-7',
  ].join('\n');
  for (const file of ['vendor/taffy-0.9.0/Cargo.toml', 'vendor/taffy-0.9.0/Cargo.toml.orig']) {
    assert.deepEqual(scanFileText(file, text, ['gpu-box-7']).map((x) => x.rule),
      ['tailscale-ip', 'tailnet-host', 'op-ref', 'home-path', 'ledger'], file);
  }
});


test('同じ blob を Taffy manifest と別のパスに置いても例外を広げない', (t) => {
  const { execFileSync } = require('node:child_process');
  const fs = require('node:fs');
  const os = require('node:os');
  const path = require('node:path');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-oss-path-'));
  t.after(() => fs.promises.rm(dir, { recursive: true, force: true }));
  const vcs = (...args) => execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@example.com',
    '-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=' + path.join(dir, 'no-hooks'), ...args], { cwd: dir });
  vcs('init', '-q');
  fs.writeFileSync(path.join(dir, 'LICENSE'), 'MIT');
  fs.writeFileSync(path.join(dir, 'package.json'), '{ "license": "MIT" }');
  fs.mkdirSync(path.join(dir, 'vendor', 'taffy-0.9.0'), { recursive: true });
  const email = ['alice.i.cecile', '@gmail', '.com'].join('');
  fs.writeFileSync(path.join(dir, 'vendor', 'taffy-0.9.0', 'Cargo.toml'), email);
  fs.writeFileSync(path.join(dir, 'zz-app.txt'), email);
  vcs('add', '.');
  vcs('commit', '-q', '-m', 'init');
  const script = path.join(__dirname, '..', 'scripts', 'oss-check.js');
  for (const args of [[], ['--history']]) {
    let result;
    try { execFileSync(process.execPath, [script, ...args], { cwd: dir, encoding: 'utf8',
      env: { ...process.env, KATALA_TUNE_NODES: path.join(dir, 'none.json') } }); }
    catch (e) { result = e; }
    assert.equal(result?.status, 1);
    assert.match(result.stdout, /NG email\s+zz-app\.txt(?:@[0-9a-f]{7})?:1/);
    assert.doesNotMatch(result.stdout, /NG email\s+vendor\//);
  }
});
