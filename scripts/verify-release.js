#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { SEMVER } = require('./version');

// 署名の暗号検証は updater が行う。ここでは配布物と feed の対応・版・URL・欠落を検証する。
function verifyRelease(root, version, repo) {
  if (!SEMVER.test(version) || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repo)) throw new Error('invalid release identity');
  const mac = `KatalaTune_${version}_universal.app.tar.gz`;
  const win = `KatalaTune_${version}_x64-setup.exe`;
  const files = [mac, `${mac}.sig`, `KatalaTune_${version}_universal.dmg`, win, `${win}.sig`, 'latest.json'];
  for (const file of files) {
    if (!fs.statSync(path.join(root, file)).isFile() || !fs.statSync(path.join(root, file)).size) throw new Error(`missing/empty: ${file}`);
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(root, 'latest.json'), 'utf8'));
  if (manifest.version !== version || !Number.isFinite(Date.parse(manifest.pub_date))) throw new Error('feed version/date mismatch');
  const platforms = { 'darwin-aarch64': mac, 'darwin-x86_64': mac, 'windows-x86_64': win };
  if (Object.keys(manifest.platforms || {}).sort().join() !== Object.keys(platforms).sort().join()) throw new Error('feed platforms mismatch');
  for (const [platform, file] of Object.entries(platforms)) {
    const entry = manifest.platforms[platform];
    const signature = fs.readFileSync(path.join(root, `${file}.sig`), 'utf8').trim();
    if (signature.length < 64 || !/^[A-Za-z0-9+/=\r\n]+$/.test(signature) || entry.signature !== signature) throw new Error(`feed signature mismatch: ${platform}`);
    if (entry.url !== `https://github.com/${repo}/releases/download/v${version}/${file}`) throw new Error(`feed URL mismatch: ${platform}`);
  }
  return files.map(file => `${createHash('sha256').update(fs.readFileSync(path.join(root, file))).digest('hex')}  ${file}`).join('\n') + '\n';
}
if (require.main === module) {
  try { const [root, version, repo] = process.argv.slice(2); const sums = verifyRelease(root, version, repo); fs.writeFileSync(path.join(root, 'SHA256SUMS'), sums); console.log(sums.trim()); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
module.exports = { verifyRelease };
