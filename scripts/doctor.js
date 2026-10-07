#!/usr/bin/env node
'use strict';
// 読み取りだけ。ツールのインストール・グローバル設定は変えない。
const { spawnSync } = require('node:child_process');
const checks = [
  ['Node 24', process.execPath, ['--version'], /^v24\./],
  ['Rust 1.99.0', 'rustc', ['--version'], /^rustc 1\.99\.0 /],
  ['Tauri CLI 2.12.1', 'cargo', ['tauri', '--version'], /^tauri-cli 2\.12\.1/],
  ['rustfmt', 'cargo', ['fmt', '--version'], /rustfmt/],
  ['Clippy', 'cargo', ['clippy', '--version'], /clippy/],
];
if (process.platform === 'darwin') checks.push(['Xcode / Command Line Tools', 'xcrun', ['--find', 'clang'], /clang/]);
if (process.platform === 'win32') checks.push(['MSVC target', 'rustup', ['target', 'list', '--installed'], /x86_64-pc-windows-msvc/]);
let failed = false;
for (const [label, command, args, expected] of checks) {
  const result = spawnSync(command, args, { encoding: 'utf8', timeout: 30000 });
  const output = (result.stdout || '').trim();
  const ok = result.status === 0 && expected.test(output);
  console.log(`${ok ? 'OK' : 'MISSING'} ${label}${ok ? `: ${output}` : ''}`);
  failed ||= !ok;
}
if (process.platform === 'win32') console.log('MSVC C++ tools / WebView2 は cargo tauri build の実ビルドでも確認してください。');
process.exitCode = failed ? 1 : 0;
