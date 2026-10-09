'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { verifyRelease, verifySignature, parsePublicKey, updaterPublicKey } = require('../scripts/verify-release');

// test/fixtures/release: 使い捨ての鍵で `cargo tauri signer sign --app-version 0.4.0` が作った署名と公開鍵（秘密鍵は作ってすぐ消した）。
// app.tar.gz・setup.exe は同じ鍵 A、app.tar.gz.other-key.sig は別の鍵 B の署名
const FIX = path.join(__dirname, 'fixtures', 'release');
const read = (f, enc) => fs.readFileSync(path.join(FIX, f), enc);
const PUBKEY = read('updater.pub', 'utf8');
const pub = parsePublicKey(PUBKEY);
const sigOf = (f) => read(`${f}.sig`, 'utf8').trim();

// 署名（base64 の箱）の行を書き換えて作り直す
const lines = (b64) => Buffer.from(b64, 'base64').toString('utf8').split('\n');
const box = (ls) => Buffer.from(ls.join('\n')).toString('base64');
const binLine = (b64, i) => Buffer.from(lines(b64)[i], 'base64');
function withLine(b64, i, value) { const ls = lines(b64); ls[i] = value; return box(ls); }

function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'release-check-')); t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const mac = 'KatalaTune_0.4.0_universal.app.tar.gz', win = 'KatalaTune_0.4.0_x64-setup.exe';
  for (const [name, src] of [[mac, 'app.tar.gz'], [win, 'setup.exe']]) {
    fs.copyFileSync(path.join(FIX, src), path.join(dir, name));
    fs.writeFileSync(path.join(dir, `${name}.sig`), `${sigOf(src)}\n`);
  }
  fs.writeFileSync(path.join(dir, 'KatalaTune_0.4.0_universal.dmg'), 'fixture:dmg');
  const feed = { version: '0.4.0', pub_date: '2026-01-01T00:00:00Z', platforms: {} };
  for (const [platform, name, src] of [['darwin-aarch64', mac, 'app.tar.gz'], ['darwin-x86_64', mac, 'app.tar.gz'], ['windows-x86_64', win, 'setup.exe']]) {
    feed.platforms[platform] = { signature: sigOf(src), url: `https://github.com/example/tune/releases/download/v0.4.0/${name}` };
  }
  const save = () => fs.writeFileSync(path.join(dir,'latest.json'),JSON.stringify(feed)); save(); return {dir, feed, save, mac, win};
}
const check = (dir, version = '0.4.0') => verifyRelease(dir, version, 'example/tune', { pubkey: PUBKEY });

test('release verifies all three targets and hashes all six assets', t => {
  const { dir } = fixture(t); const sums = check(dir); assert.equal(sums.trim().split('\n').length,6); assert.match(sums,/^[a-f0-9]{64}  KatalaTune_/);
});
test('release rejects wrong versions, origins, signatures and missing artifacts', t => {
  const {dir,feed,save,mac}=fixture(t);
  feed.version='0.3.0';save();assert.throws(()=>check(dir),/version/);feed.version='0.4.0';
  const entry=feed.platforms['darwin-aarch64']; const url=entry.url; entry.url='https://example.invalid/replacement';save();assert.throws(()=>check(dir),/URL/);entry.url=url;
  const sig=entry.signature; entry.signature='bad';save();assert.throws(()=>check(dir),/signature/);entry.signature=sig;save();
  fs.unlinkSync(path.join(dir,mac));assert.throws(()=>check(dir));
  assert.throws(()=>check(dir,'../../escape'),/identity/);
});

test('更新署名: cargo tauri signer の署名を公開鍵で暗号として検証する（前ハッシュ "ED"・署名に入った版）', () => {
  assert.equal(lines(sigOf('app.tar.gz'))[0].startsWith('untrusted comment: '), true);
  const r = verifySignature(read('app.tar.gz'), sigOf('app.tar.gz'), pub, '0.4.0');
  assert.equal(r.prehashed, true);
  assert.match(r.trusted, /\tversion:0\.4\.0$/);
  verifySignature(read('setup.exe'), sigOf('setup.exe'), pub, '0.4.0');
  // 署名に入った版と違う版としては通さない（アプリの requireSignedVersion と同じ）
  assert.throws(() => verifySignature(read('app.tar.gz'), sigOf('app.tar.gz'), pub, '0.4.1'), /signed version 0\.4\.0 is not 0\.4\.1/);
  // 本物の tauri.conf.json の公開鍵も読める
  assert.equal(parsePublicKey(updaterPublicKey()).keyId.length, 8);
});

test('更新署名: 改ざんしたファイル・別の鍵の署名・書き換えた trusted comment を拒否する', () => {
  const file = read('app.tar.gz'), sig = sigOf('app.tar.gz');
  const tampered = Buffer.concat([file, Buffer.from('x')]);
  assert.throws(() => verifySignature(tampered, sig, pub), /does not verify for this file/);
  // 別のファイル（setup.exe）の正しい署名を付け替えても通らない
  assert.throws(() => verifySignature(file, sigOf('setup.exe'), pub), /does not verify for this file/);
  // 別の鍵の署名: 鍵 ID が違う
  const other = read('app.tar.gz.other-key.sig', 'utf8').trim();
  assert.throws(() => verifySignature(file, other, pub), /key ID/);
  // 別の鍵の署名の鍵 ID だけをこちらの鍵 ID に書き換えても、本体の署名で落ちる
  const forged = binLine(other, 1); pub.keyId.copy(forged, 2);
  assert.throws(() => verifySignature(file, withLine(other, 1, forged.toString('base64')), pub), /does not verify for this file/);
  // trusted comment（版）を書き換えると global signature で落ちる
  const relabeled = withLine(sig, 2, lines(sig)[2].replace('version:0.4.0', 'version:0.5.0'));
  assert.throws(() => verifySignature(file, relabeled, pub, '0.5.0'), /global signature/);
  // 別の鍵の公開鍵では通らない
  const { publicKey } = crypto.generateKeyPairSync('ed25519');
  const otherPub = { keyId: pub.keyId, key: publicKey };
  assert.throws(() => verifySignature(file, sig, otherPub), /does not verify for this file/);
});

test('更新署名: 壊れた署名・公開鍵は検証の前に拒否する', () => {
  const file = read('app.tar.gz'), sig = sigOf('app.tar.gz');
  const sigBin = binLine(sig, 1);
  const cases = [
    ['', /not base64/],
    ['not base64!', /not base64/],
    [`${sig.slice(0, -4)}`, /not base64|invalid/],
    [box(lines(sig).slice(0, 3)), /invalid format/],
    [withLine(sig, 0, 'comment without prefix'), /invalid format/],
    [withLine(sig, 2, 'timestamp:1'), /invalid format/],
    [withLine(sig, 1, sigBin.subarray(0, 73).toString('base64')), /invalid length/],
    [withLine(sig, 1, Buffer.concat([Buffer.from('XX'), sigBin.subarray(2)]).toString('base64')), /unsupported algorithm/],
    [withLine(sig, 3, 'AAAA'), /invalid length/],
    [withLine(sig, 1, `${sigBin.toString('base64').slice(0, -2)}!=`), /not base64/],
  ];
  for (const [bad, re] of cases) assert.throws(() => verifySignature(file, bad, pub), re, JSON.stringify(bad).slice(0, 80));
  assert.throws(() => parsePublicKey('REPLACE_WITH_TAURI_SIGNING_PUBLIC_KEY'), /public key/);
  assert.throws(() => parsePublicKey(Buffer.from('untrusted comment: x\nAAAA\n').toString('base64')), /unsupported format/);
});

test('更新署名: 旧形式（"Ed"、前ハッシュなし）の署名も検証する', () => {
  // その場で作った使い捨ての鍵（保存しない）で、minisign の旧形式の署名を組み立てる
  const { publicKey, privateKey } = crypto.generateKeyPairSync('ed25519');
  const raw = publicKey.export({ format: 'der', type: 'spki' }).subarray(12);
  const keyId = crypto.randomBytes(8);
  const legacyPub = parsePublicKey(box(['untrusted comment: test key', Buffer.concat([Buffer.from('Ed'), keyId, raw]).toString('base64'), '']));
  const data = Buffer.from('legacy payload');
  const signature = crypto.sign(null, data, privateKey);
  const trusted = 'timestamp:1\tfile:payload\tversion:0.4.0';
  const global = crypto.sign(null, Buffer.concat([signature, Buffer.from(trusted)]), privateKey);
  const sig = box(['untrusted comment: test', Buffer.concat([Buffer.from('Ed'), keyId, signature]).toString('base64'), `trusted comment: ${trusted}`, global.toString('base64'), '']);
  assert.equal(verifySignature(data, sig, legacyPub, '0.4.0').prehashed, false);
  assert.throws(() => verifySignature(Buffer.from('legacy payloaD'), sig, legacyPub), /does not verify for this file/);
});

test('release: feed と .sig が一致していても、署名が合わない配布物は通さない', t => {
  const { dir, feed, save, mac, win } = fixture(t);
  // 配布物を差し替えた（.sig と feed はそのまま）
  const original = fs.readFileSync(path.join(dir, mac));
  fs.writeFileSync(path.join(dir, mac), Buffer.concat([original, Buffer.from('tampered')]));
  assert.throws(() => check(dir), /app\.tar\.gz\.sig: signature: does not verify/);
  fs.writeFileSync(path.join(dir, mac), original);
  // 別の鍵の署名を .sig と feed の両方に入れた
  const other = read('app.tar.gz.other-key.sig', 'utf8').trim();
  fs.writeFileSync(path.join(dir, `${mac}.sig`), other);
  feed.platforms['darwin-aarch64'].signature = feed.platforms['darwin-x86_64'].signature = other; save();
  assert.throws(() => check(dir), /key ID/);
  // 公開鍵が違う（tauri.conf.json の鍵では、この使い捨ての鍵の署名は通らない）
  fs.writeFileSync(path.join(dir, `${mac}.sig`), sigOf('app.tar.gz'));
  feed.platforms['darwin-aarch64'].signature = feed.platforms['darwin-x86_64'].signature = sigOf('app.tar.gz'); save();
  assert.equal(check(dir).trim().split('\n').length, 6);
  assert.throws(() => verifyRelease(dir, '0.4.0', 'example/tune'), /key ID/);
  // 署名に入った版（0.4.0）と違う版のリリースとしては通さない
  for (const f of [mac, win]) fs.renameSync(path.join(dir, f), path.join(dir, f.replace('0.4.0', '0.4.1')));
  for (const f of [mac, win]) fs.renameSync(path.join(dir, `${f}.sig`), path.join(dir, `${f.replace('0.4.0', '0.4.1')}.sig`));
  fs.renameSync(path.join(dir, 'KatalaTune_0.4.0_universal.dmg'), path.join(dir, 'KatalaTune_0.4.1_universal.dmg'));
  assert.throws(() => check(dir, '0.4.1'), /signed version 0\.4\.0 is not 0\.4\.1/);
});
