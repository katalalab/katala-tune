#!/usr/bin/env node
'use strict';
// Local GPUI .app, separate from the Tauri bundle and its Cargo binary.
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
if (process.platform !== 'darwin') throw new Error('GPUI .app packaging requires macOS');
const root = path.resolve(__dirname, '..');
const at = p => path.join(root, p);
const { version } = require('../package.json');
const revision = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
const target = at('target/gpui');
execFileSync('cargo', ['build', '--locked', '--release', '--package', 'tune-gpui', '--target-dir', target], { cwd: root, stdio: 'inherit' });
const app = at('dist/Katala Tune GPUI Preview.app');
const contents = path.join(app, 'Contents');
fs.mkdirSync(path.join(contents, 'MacOS'), { recursive: true });
fs.mkdirSync(path.join(contents, 'Resources'), { recursive: true });
fs.copyFileSync(path.join(target, 'release/katala-tune-gpui'), path.join(contents, 'MacOS/katala-tune-gpui'));
fs.chmodSync(path.join(contents, 'MacOS/katala-tune-gpui'), 0o755);
fs.copyFileSync(at('build/icon.icns'), path.join(contents, 'Resources/icon.icns'));
fs.writeFileSync(path.join(contents, 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Katala Tune GPUI Preview</string>
  <key>CFBundleDisplayName</key><string>Katala Tune GPUI Preview</string>
  <key>CFBundleIdentifier</key><string>org.katala.tune.gpui.preview</string>
  <key>CFBundleExecutable</key><string>katala-tune-gpui</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>icon.icns</string>
  <key>CFBundleShortVersionString</key><string>${version.split('-')[0]}</string>
  <key>CFBundleVersion</key><string>${version.split('-')[0]}</string>
  <key>KatalaTuneRevision</key><string>${revision}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
`);
// Ad-hoc signature for local verification; distribution signing is separate.
execFileSync('codesign', ['--force', '--sign', '-', app], { stdio: 'inherit' });
console.log(app);
