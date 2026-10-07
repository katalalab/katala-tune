#!/usr/bin/env node
// パッケージ後の Electron 本体の fuse を切り替える: node scripts/harden.js <Katala Tune.app | Katala Tune.exe>
// このアプリは自分を Node として起動しないので、外から JS を注入できる入口（ELECTRON_RUN_AS_NODE・NODE_OPTIONS・--inspect）を閉じる。
// asar 系の fuse はアプリを asar にしていないので触らない。
'use strict';

async function main(target) {
  if (!target) throw new Error('使い方: node scripts/harden.js <パッケージ済みの .app または .exe>');
  const { flipFuses, getCurrentFuseWire, FuseVersion, FuseV1Options, FuseState } = await import('@electron/fuses');
  const want = {
    [FuseV1Options.RunAsNode]: false,
    [FuseV1Options.EnableNodeOptionsEnvironmentVariable]: false,
    [FuseV1Options.EnableNodeCliInspectArguments]: false,
    [FuseV1Options.EnableCookieEncryption]: true,
  };
  // macOS (arm64) は書き換えで ad-hoc 署名が壊れるので付け直す
  await flipFuses(target, { version: FuseVersion.V1, resetAdHocDarwinSignature: target.endsWith('.app'), ...want });
  const wire = await getCurrentFuseWire(target);
  const bad = Object.entries(want).filter(([k, v]) => wire[k] !== (v ? FuseState.ENABLE : FuseState.DISABLE)).map(([k]) => FuseV1Options[k]);
  if (bad.length) throw new Error(`fuse が反映されていない: ${bad.join(', ')}`);
  console.log(`fuse を設定した: ${Object.keys(want).map((k) => `${FuseV1Options[k]}=${want[k] ? 'on' : 'off'}`).join(' ')}`);
}

main(process.argv[2]).catch((e) => { console.error(e.message); process.exitCode = 1; });
