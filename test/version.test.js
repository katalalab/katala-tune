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
