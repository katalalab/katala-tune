'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { verifyRelease } = require('../scripts/verify-release');
function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'release-check-')); t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const mac = 'KatalaTune_0.4.0_universal.app.tar.gz', win = 'KatalaTune_0.4.0_x64-setup.exe';
  const signature = Buffer.from('synthetic signature fixture '.repeat(8)).toString('base64');
  for (const name of [mac, win, 'KatalaTune_0.4.0_universal.dmg']) fs.writeFileSync(path.join(dir, name), `fixture:${name}`);
  for (const name of [mac, win]) fs.writeFileSync(path.join(dir, `${name}.sig`), signature+'\n');
  const feed = { version: '0.4.0', pub_date: '2026-01-01T00:00:00Z', platforms: {} };
  for (const [platform, name] of [['darwin-aarch64',mac],['darwin-x86_64',mac],['windows-x86_64',win]]) feed.platforms[platform] = { signature, url: `https://github.com/example/tune/releases/download/v0.4.0/${name}` };
  const save = () => fs.writeFileSync(path.join(dir,'latest.json'),JSON.stringify(feed)); save(); return {dir, feed, save, mac};
}
test('release verifies all three targets and hashes all six assets', t => {
  const { dir } = fixture(t); const sums = verifyRelease(dir,'0.4.0','example/tune'); assert.equal(sums.trim().split('\n').length,6); assert.match(sums,/^[a-f0-9]{64}  KatalaTune_/);
});
test('release rejects wrong versions, origins, signatures and missing artifacts', t => {
  const {dir,feed,save,mac}=fixture(t);
  feed.version='0.3.0';save();assert.throws(()=>verifyRelease(dir,'0.4.0','example/tune'),/version/);feed.version='0.4.0';
  const entry=feed.platforms['darwin-aarch64']; const url=entry.url; entry.url='https://example.invalid/replacement';save();assert.throws(()=>verifyRelease(dir,'0.4.0','example/tune'),/URL/);entry.url=url;
  const sig=entry.signature; entry.signature='bad';save();assert.throws(()=>verifyRelease(dir,'0.4.0','example/tune'),/signature/);entry.signature=sig;save();
  fs.unlinkSync(path.join(dir,mac));assert.throws(()=>verifyRelease(dir,'0.4.0','example/tune'));
  assert.throws(()=>verifyRelease(dir,'../../escape','example/tune'),/identity/);
});
