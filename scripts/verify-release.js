#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { createHash, createPublicKey, verify: verifyEd25519 } = require('node:crypto');
const { SEMVER } = require('./version');

const TAURI_CONF = path.join(__dirname, '..', 'src-tauri', 'tauri.conf.json');

// ---- 更新署名（minisign）の検証 ----
// Tauri の公開鍵（tauri.conf.json の plugins.updater.pubkey）と .sig は、minisign の公開鍵・署名のファイルの中身を base64 にしたもの。
//   公開鍵: "untrusted comment: …" / base64("Ed" + 鍵 ID 8 バイト + Ed25519 の公開鍵 32 バイト)
//   署名:   "untrusted comment: …" / base64("ED" か "Ed" + 鍵 ID 8 バイト + 署名 64 バイト) / "trusted comment: …" / base64(global signature 64 バイト)
// アプリの updater（tauri-plugin-updater・minisign-verify）と同じく、鍵 ID の一致・本体の署名（"ED" は BLAKE2b-512 で前ハッシュ、
// "Ed" は旧形式でファイルそのもの）・global signature（署名 64 バイト + trusted comment）の3つを確かめる。
// requireSignedVersion: true と同じく、trusted comment の version: がリリースの版と同じことも確かめる（違えばアプリが入れない）。
const ED25519_SPKI = Buffer.from('302a300506032b6570032100', 'hex');
const UNTRUSTED = 'untrusted comment: ';
const TRUSTED = 'trusted comment: ';

// パディングつきの標準の base64 だけを受け付ける（Buffer.from は不正な文字を黙って読み飛ばすので、読み直して一致を見る）
function strictBase64(text, what) {
  if (typeof text !== 'string' || !text || text.length % 4 || !/^[A-Za-z0-9+/]+={0,2}$/.test(text)) throw new Error(`${what}: not base64`);
  const bin = Buffer.from(text, 'base64');
  if (bin.toString('base64') !== text) throw new Error(`${what}: not base64`);
  return bin;
}
const boxLines = (b64, what) => strictBase64(String(b64).trim(), what).toString('utf8').split('\n').map((l) => l.replace(/\r$/, ''));

function parsePublicKey(b64) {
  const [comment, line] = boxLines(b64, 'public key');
  if (!comment?.startsWith(UNTRUSTED) || line == null) throw new Error('public key: invalid format');
  const bin = strictBase64(line, 'public key');
  if (bin.length !== 42 || bin[0] !== 0x45 || (bin[1] !== 0x64 && bin[1] !== 0x44)) throw new Error('public key: unsupported format');
  return { keyId: bin.subarray(2, 10), key: createPublicKey({ key: Buffer.concat([ED25519_SPKI, bin.subarray(10)]), format: 'der', type: 'spki' }) };
}

function parseSignature(b64) {
  const [comment, line, trusted, globalLine] = boxLines(b64, 'signature');
  if (!comment?.startsWith(UNTRUSTED) || line == null || !trusted?.startsWith(TRUSTED) || globalLine == null) throw new Error('signature: invalid format');
  const bin = strictBase64(line, 'signature');
  const global = strictBase64(globalLine, 'signature');
  if (bin.length !== 74 || global.length !== 64) throw new Error('signature: invalid length');
  const alg = bin.subarray(0, 2).toString('latin1');
  if (alg !== 'ED' && alg !== 'Ed') throw new Error(`signature: unsupported algorithm ${JSON.stringify(alg)}`);
  return { prehashed: alg === 'ED', keyId: bin.subarray(2, 10), signature: bin.subarray(10), trusted: trusted.slice(TRUSTED.length), global };
}

// data（ファイルの中身）が、公開鍵 pub（parsePublicKey の戻り値）の鍵で署名されているか。version を渡すと署名に入った版も確かめる
function verifySignature(data, sigB64, pub, version) {
  const s = parseSignature(sigB64);
  if (!s.keyId.equals(pub.keyId)) throw new Error(`signature: key ID ${s.keyId.toString('hex')} does not match the public key`);
  const message = s.prehashed ? createHash('blake2b512').update(data).digest() : data;
  if (!verifyEd25519(null, message, pub.key, s.signature)) throw new Error('signature: does not verify for this file');
  if (!verifyEd25519(null, Buffer.concat([s.signature, Buffer.from(s.trusted, 'utf8')]), pub.key, s.global)) throw new Error('signature: global signature (trusted comment) does not verify');
  if (version != null) {
    const signed = s.trusted.split('\t').find((f) => f.startsWith('version:'))?.slice('version:'.length);
    if (signed == null || signed.replace(/^v/, '') !== version) throw new Error(`signature: signed version ${signed ?? '(none)'} is not ${version}`);
  }
  return { prehashed: s.prehashed, trusted: s.trusted };
}

// tauri.conf.json の更新の公開鍵（アプリに入る鍵と同じもの）
function updaterPublicKey(conf = TAURI_CONF) {
  const key = JSON.parse(fs.readFileSync(conf, 'utf8')).plugins?.updater?.pubkey;
  if (typeof key !== 'string' || !key) throw new Error(`updater pubkey not found: ${conf}`);
  return key;
}

// ---- 配布物の検証 ----
// 配布物と feed の対応・版・URL・欠落に加え、更新ファイルの .sig を tauri.conf.json の公開鍵で暗号として検証する
function verifyRelease(root, version, repo, { pubkey = updaterPublicKey() } = {}) {
  if (!SEMVER.test(version) || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repo)) throw new Error('invalid release identity');
  const pub = parsePublicKey(pubkey);
  const mac = `KatalaTune_${version}_universal.app.tar.gz`;
  const win = `KatalaTune_${version}_x64-setup.exe`;
  const files = [mac, `${mac}.sig`, `KatalaTune_${version}_universal.dmg`, win, `${win}.sig`, 'latest.json'];
  for (const file of files) {
    if (!fs.statSync(path.join(root, file)).isFile() || !fs.statSync(path.join(root, file)).size) throw new Error(`missing/empty: ${file}`);
  }
  const signatures = {};
  for (const file of [mac, win]) {
    signatures[file] = fs.readFileSync(path.join(root, `${file}.sig`), 'utf8').trim();
    try { verifySignature(fs.readFileSync(path.join(root, file)), signatures[file], pub, version); }
    catch (e) { throw new Error(`${file}.sig: ${e.message}`); }
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(root, 'latest.json'), 'utf8'));
  if (manifest.version !== version || !Number.isFinite(Date.parse(manifest.pub_date))) throw new Error('feed version/date mismatch');
  const platforms = { 'darwin-aarch64': mac, 'darwin-x86_64': mac, 'windows-x86_64': win };
  if (Object.keys(manifest.platforms || {}).sort().join() !== Object.keys(platforms).sort().join()) throw new Error('feed platforms mismatch');
  for (const [platform, file] of Object.entries(platforms)) {
    const entry = manifest.platforms[platform];
    // feed の署名は、上で検証した .sig と同じものだけ
    if (entry.signature !== signatures[file]) throw new Error(`feed signature mismatch: ${platform}`);
    if (entry.url !== `https://github.com/${repo}/releases/download/v${version}/${file}`) throw new Error(`feed URL mismatch: ${platform}`);
  }
  return files.map(file => `${createHash('sha256').update(fs.readFileSync(path.join(root, file))).digest('hex')}  ${file}`).join('\n') + '\n';
}
if (require.main === module) {
  try { const [root, version, repo] = process.argv.slice(2); const sums = verifyRelease(root, version, repo); fs.writeFileSync(path.join(root, 'SHA256SUMS'), sums); console.log(sums.trim()); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
module.exports = { verifyRelease, verifySignature, parsePublicKey, parseSignature, updaterPublicKey };
