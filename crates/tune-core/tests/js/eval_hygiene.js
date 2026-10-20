#!/usr/bin/env node
// リポジトリの衛生監査の JS 版（lib/hygiene.js）を仕様として、Rust 側のテスト（tests/parity_hygiene.rs）から渡された入力を評価して返す。
// 使い方: node eval_hygiene.js <cases.json>  → 標準出力に結果の JSON
'use strict';
const fs = require('node:fs');
const path = require('node:path');

const hygiene = require(path.join(__dirname, '..', '..', '..', '..', 'lib', 'hygiene'));
const cases = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const plain = (v) => JSON.parse(JSON.stringify(v === undefined ? null : v));

const out = {};
out.stripUrl = (cases.stripUrl || []).map((s) => plain(hygiene.stripUrl(s)));
out.normalize = (cases.normalize || []).map((d) => plain(hygiene.normalize(d)));
out.analyze = (cases.analyze || []).map((a) => plain(hygiene.analyze(a)));
out.pathStatus = (cases.pathStatus || []).map((r) => hygiene.pathStatus(r));
out.matrix = (cases.matrix || []).map((a) => plain(hygiene.matrix(a)));
out.script = (cases.script || []).map((o) => hygiene.script(o));
process.stdout.write(JSON.stringify(out));
