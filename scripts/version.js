#!/usr/bin/env node
// 版をそろえる: package.json・package-lock.json・Cargo.toml（ワークスペース）・Cargo.lock（ワークスペースの crate）・src-tauri/tauri.conf.json
//   node scripts/version.js            … 今の版を表示（ずれていれば終了コード 1）
//   node scripts/version.js 0.4.0      … すべてを 0.4.0 にする
//   node scripts/version.js --check 0.4.0 … すべてが 0.4.0 か確かめる（リリースの CI がタグと照合する）
'use strict';
const fs = require('node:fs');
const path = require('node:path');

const ROOT = path.join(__dirname, '..');
const CRATES = ['katala-tune', 'tune-core', 'tune-cli'];
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
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
    get: (t) => { const vs = new Set(CRATES.map((c) => new RegExp(`\\nname = "${c}"\\nversion = "([^"]*)"`).exec(t)?.[1])); return vs.size === 1 ? [...vs][0] : [...vs].join(' / '); },
    set: (t, v) => CRATES.reduce((acc, c) => acc.replace(new RegExp(`(\\nname = "${c}"\\nversion = )"[^"]*"`), `$1"${v}"`), t),
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
    for (const [f, h] of Object.entries(FILES)) {
      const before = read(f);
      const after = h.set(before, want);
      if (h.get(after) !== want) {
        console.error(`${f} の版を書き換えられなかった`);
        return 1;
      }
      fs.writeFileSync(file(f), after);
    }
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
module.exports = { main, versions, FILES };
