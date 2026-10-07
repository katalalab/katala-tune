#!/usr/bin/env node
// パッケージ後の Electron 本体の fuse を切り替える: node scripts/harden.js <Katala Tune.app | Katala Tune.exe>
// このアプリは自分を Node として起動しないので、外から JS を注入できる入口（ELECTRON_RUN_AS_NODE・NODE_OPTIONS・--inspect）を閉じる。
// @electron/packager 20 はアプリを app.asar にまとめるので、asar 以外からは読み込まない（OnlyLoadAppFromAsar）。
// 改ざんの検知（EnableEmbeddedAsarIntegrityValidation）は、packager が Info.plist に ElectronAsarIntegrity を書いた macOS だけで有効にする
// （ハッシュが無いまま有効にすると起動できない）。
'use strict';
const fs = require('node:fs');
const path = require('node:path');

// macOS の .app に asar のハッシュが書かれているか
function hasAsarIntegrity(app) {
  if (!app.endsWith('.app')) return false;
  try { return /<key>ElectronAsarIntegrity<\/key>/.test(fs.readFileSync(path.join(app, 'Contents', 'Info.plist'), 'utf8')); } catch { return false; }
}

async function main(target) {
  if (!target) throw new Error('使い方: node scripts/harden.js <パッケージ済みの .app または .exe>');
  const { flipFuses, getCurrentFuseWire, FuseVersion, FuseV1Options, FuseState } = await import('@electron/fuses');
  const want = {
    [FuseV1Options.RunAsNode]: false,
    [FuseV1Options.EnableNodeOptionsEnvironmentVariable]: false,
    [FuseV1Options.EnableNodeCliInspectArguments]: false,
    [FuseV1Options.EnableCookieEncryption]: true,
    [FuseV1Options.OnlyLoadAppFromAsar]: true,
  };
  if (hasAsarIntegrity(target)) want[FuseV1Options.EnableEmbeddedAsarIntegrityValidation] = true;
  // macOS (arm64) は書き換えで ad-hoc 署名が壊れるので付け直す
  await flipFuses(target, { version: FuseVersion.V1, resetAdHocDarwinSignature: target.endsWith('.app'), ...want });
  const wire = await getCurrentFuseWire(target);
  const bad = Object.entries(want).filter(([k, v]) => wire[k] !== (v ? FuseState.ENABLE : FuseState.DISABLE)).map(([k]) => FuseV1Options[k]);
  if (bad.length) throw new Error(`fuse が反映されていない: ${bad.join(', ')}`);
  console.log(`fuse を設定した: ${Object.keys(want).map((k) => `${FuseV1Options[k]}=${want[k] ? 'on' : 'off'}`).join(' ')}`);
}

main(process.argv[2]).catch((e) => { console.error(e.message); process.exitCode = 1; });
