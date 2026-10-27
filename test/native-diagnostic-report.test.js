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

test('extracts the first observation when libtest prefixes its fixed test name', () => {
  const line = 'native_role=topology-native-reader elapsed_ms=984 code=Some(0) stdout_len=1971 stderr_len=0';
  assert.deepEqual(report(`test native_windows::native_three_entries_use_real_git_and_process_one_frame_without_eof ... ${line}`), [line]);
  assert.deepEqual(report(`untrusted text ${line}`), []);
});
