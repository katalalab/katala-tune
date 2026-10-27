'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { report } = require('../scripts/native-diagnostic-report.js');

test('diagnostic report emits only closed numeric role observations', () => {
  const input = [
    'native_role=topology-supervise elapsed_ms=1977 code=Some(84) stdout_len=0 stderr_len=0',
    'panic: path /private/example and pid 12345',
    'native_role=arbitrary elapsed_ms=1 code=Some(84) stdout_len=0 stderr_len=0',
    'native_role=topology-verify elapsed_ms=1 code=Some(84) stdout_len=0 stderr_len=0 extra',
  ].join('\n');
  assert.deepEqual(report(input), ['native_role=topology-supervise elapsed_ms=1977 code=Some(84) stdout_len=0 stderr_len=0']);
});

test('no matching observation exposes no raw error data', () => {
  assert.deepEqual(report('error: secret or path'), []);
});

test('decodes Windows UTF-16 logs without exposing unrelated content', () => {
  const { decode } = require('../scripts/native-diagnostic-report.js');
  const text = 'native_role=topology-supervise elapsed_ms=1 code=Some(84) stdout_len=0 stderr_len=0\r\nprivate path';
  const buffer = Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, 'utf16le')]);
  assert.deepEqual(report(decode(buffer)), [text.split('\r\n')[0]]);
});
