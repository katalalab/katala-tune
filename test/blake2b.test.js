'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { createHash, getHashes } = require('node:crypto');
const { blake2b512 } = require('../scripts/blake2b');

test('BLAKE2b-512: RFC 7693 abc and empty known answers', () => {
  assert.equal(blake2b512(Buffer.from('abc')).toString('hex'),
    'ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1' +
    '7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923');
  assert.equal(blake2b512(Buffer.alloc(0)).toString('hex'),
    '786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419d2' +
    '5e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce');
});

test('BLAKE2b-512: block boundaries and multiple blocks match native crypto', {
  skip: !getHashes().includes('blake2b512'),
}, () => {
  for (const size of [1, 7, 8, 63, 64, 127, 128, 129, 255, 256, 257, 4096]) {
    const input = Buffer.from(Array.from({ length: size }, (_, i) => i % 251));
    const before = Buffer.from(input);
    assert.deepEqual(blake2b512(input), createHash('blake2b512').update(input).digest(), `size ${size}`);
    assert.deepEqual(input, before, 'hashing never mutates its input');
  }
});

test('BLAKE2b-512: block-boundary known answers also run without native support', () => {
  const answers = [
    [128, '2319e3789c47e2daa5fe807f61bec2a1a6537fa03f19ff32e87eecbfd64b7e0e8ccff439ac333b040f19b0c4ddd11a61e24ac1fe0f10a039806c5dcc0da3d115'],
    [129, 'f59711d44a031d5f97a9413c065d1e614c417ede998590325f49bad2fd444d3e4418be19aec4e11449ac1a57207898bc57d76a1bcf3566292c20c683a5c4648f'],
    [256, '93463ac058b6163eb43be3f5bb32b28541498f4e3366f1effe253ad44e1e076e41c3616046027c82a7124f8f4746668ad10b12e8e25a95ac8f3151df01cd5a93'],
    [4096, 'c7a3d6a53bd11772ecf077c1dc9633a39c6fe691ec07a530e0e765c0a9d5a01a16f00995536578b83e54c2821766ac7ac6ae86e22269a5d14208ccac954cc95f'],
  ];
  for (const [size, expected] of answers) {
    const input = Buffer.from(Array.from({ length: size }, (_, i) => i % 251));
    const before = Buffer.from(input);
    assert.equal(blake2b512(input).toString('hex'), expected, `size ${size}`);
    assert.deepEqual(input, before);
  }
});
