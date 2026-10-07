'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { versions, FILES } = require('../scripts/version');

const read = (f) => fs.readFileSync(path.join(__dirname, '..', f), 'utf8');

test('版: package.json・Cargo・tauri.conf.json の版がそろっている', () => {
  const vs = versions();
  assert.equal(new Set(Object.values(vs)).size, 1, JSON.stringify(vs));
});

test('版: 書き換えは版の行だけで、読み直すと新しい版になる', () => {
  for (const [f, h] of Object.entries(FILES)) {
    const before = read(f);
    const after = h.set(before, '9.8.7-rc.1');
    assert.equal(h.get(after), '9.8.7-rc.1', f);
    const changed = before.split('\n').filter((l, i) => l !== after.split('\n')[i]);
    assert.ok(changed.length >= 1 && changed.length <= 3, `${f}: ${changed.length} 行`);
    assert.equal(before.split('\n').length, after.split('\n').length, f);
  }
});

test('版: SemVer でない形（先頭の 0・空のプレリリース識別子）は受け付けない', () => {
  const { main, SEMVER } = require('../scripts/version');
  for (const ok of ['0.4.0', '1.2.3-alpha.1', '1.0.0+build.5']) assert.ok(SEMVER.test(ok), ok);
  for (const bad of ['01.2.3', '1.02.3', '1.2.3-alpha..1', '1.2.3-01', '1.2.3-', '1.2']) {
    assert.ok(!SEMVER.test(bad), bad);
    const err = console.error; console.error = () => {};
    try { assert.equal(main([bad]), 2, bad); } finally { console.error = err; }
  }
});

test('版: 1 つでも書き換えられないファイルがあれば、どのファイルも変えない', () => {
  const { main } = require('../scripts/version');
  const files = Object.keys(FILES);
  const before = Object.fromEntries(files.map((f) => [f, read(f)]));
  const orig = FILES['Cargo.lock'].set;
  FILES['Cargo.lock'].set = (t) => t; // 書式が変わって版の行が見つからない場合を作る
  const err = console.error; const log = console.log; console.error = () => {}; console.log = () => {};
  try {
    assert.equal(main(['9.9.9']), 1);
    for (const f of files) assert.equal(read(f), before[f], `${f} が書き換わった`);
  } finally {
    FILES['Cargo.lock'].set = orig; console.error = err; console.log = log;
    for (const f of files) if (read(f) !== before[f]) fs.writeFileSync(path.join(__dirname, '..', f), before[f]); // 念のため戻す
  }
});
