'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const collect = require('../lib/collect');
let result;
collect.run = async () => result;
const { inventoryNode } = require('../lib/inventory');

test('failed or unconfirmed process cannot authorize inventory replacement', async () => {
  for (const code of [1, null]) {
    result = { code, out: '{"items":[]}', err: 'synthetic failure' };
    const got = await inventoryNode({ id: 'fixture', os: 'macos', local: true });
    assert.equal(got.ok, false);
    assert.equal(got.items, undefined);
  }
  result = { code: 0, out: '{"items":[{"source":"app","name":"fixture"}]}', err: '' };
  const got = await inventoryNode({ id: 'fixture', os: 'macos', local: true });
  assert.equal(got.ok, true);
  assert.equal(got.items.length, 1);
});
