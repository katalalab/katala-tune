#!/usr/bin/env node
// 版をそろえる: package.json・package-lock.json・Cargo.toml（ワークスペース）・Cargo.lock（ワークスペースの crate）・src-tauri/tauri.conf.json
//   node scripts/version.js            … 今の版を表示（ずれていれば終了コード 1）
//   node scripts/version.js 0.4.0      … すべてを 0.4.0 にする
//   node scripts/version.js --check 0.4.0 … すべてが 0.4.0 か確かめる（リリースの CI がタグと照合する）
'use strict';
const fs = require('node:fs');
const path = require('node:path');

const ROOT = path.join(__dirname, '..');
const CRATES = ['katala-tune', 'tune-core', 'tune-cli', 'tune-link', 'tune-agent'];
// SemVer 2.0.0 の正規表現（semver.org）。先頭が 0 の数字や空のプレリリース識別子（01.2.3・1.2.3-alpha..1）は通さない
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$/;
const file = (f) => path.join(ROOT, f);
const read = (f) => fs.readFileSync(file(f), 'utf8');

// 各ファイルの版を読む・書き換える（書き換えは版の行だけ。書式は崩さない）
const FILES = {
  'package.json': {
    get: (t) => JSON.parse(t).version,
    set: (t, v) => t.replace(/^( {2}"version": )"[^"]*"/m, `$1"${v}"`),
  },
  'package-lock.json': {
    get: (t) => { const j = JSON.parse(t); return j.version === j.packages?.['']?.version ? j.version : `${j.version} / ${j.packages?.['']?.version}`; },
    set: (t, v) => t.replace(/^( {2}"version": )"[^"]*"/m, `$1"${v}"`).replace(/("packages": \{\s*"": \{[^}]*?"version": )"[^"]*"/, `$1"${v}"`),
  },
  'Cargo.toml': {
    get: (t) => /\[workspace\.package\][^[]*?\nversion = "([^"]*)"/.exec(t)?.[1],
    set: (t, v) => t.replace(/(\[workspace\.package\][^[]*?\nversion = )"[^"]*"/, `$1"${v}"`),
  },
  'Cargo.lock': {
    get: (t) => { const vs = new Set(CRATES.map((c) => new RegExp(`\\r?\\nname = "${c}"\\r?\\nversion = "([^"]*)"`).exec(t)?.[1])); return vs.size === 1 ? [...vs][0] : [...vs].join(' / '); },
    set: (t, v) => CRATES.reduce((acc, c) => acc.replace(new RegExp(`(\\r?\\nname = "${c}"\\r?\\nversion = )"[^"]*"`), `$1"${v}"`), t),
  },
  'src-tauri/tauri.conf.json': {
    get: (t) => JSON.parse(t).version,
    set: (t, v) => t.replace(/^( {2}"version": )"[^"]*"/m, `$1"${v}"`),
  },
};

function versions() {
  return Object.fromEntries(Object.entries(FILES).map(([f, h]) => [f, h.get(read(f))]));
}

function main(argv) {
  const check = argv[0] === '--check';
  const want = check ? argv[1] : argv[0];
  if (want !== undefined && !SEMVER.test(want)) {
    console.error(`版の形が違う: ${want}（例: 0.4.0）`);
    return 2;
  }
  if (want !== undefined && !check) {
    // 先に全部を書き換えて確かめてから書く（途中で失敗して版がずれたまま残らないように）
    const staged = [];
    for (const [f, h] of Object.entries(FILES)) {
      const after = h.set(read(f), want);
      if (h.get(after) !== want) {
        console.error(`${f} の版を書き換えられなかった（どのファイルも変えていない）`);
        return 1;
      }
      staged.push([f, after]);
    }
    for (const [f, after] of staged) fs.writeFileSync(file(f), after);
  }
  const vs = versions();
  for (const [f, v] of Object.entries(vs)) console.log(`${v}\t${f}`);
  const all = new Set(Object.values(vs));
  if (all.size !== 1) {
    console.error('版がそろっていない');
    return 1;
  }
  if (check && !all.has(want)) {
    console.error(`版が ${want} ではない`);
    return 1;
  }
  return 0;
}

if (require.main === module) process.exitCode = main(process.argv.slice(2));
module.exports = { main, versions, FILES, SEMVER };
