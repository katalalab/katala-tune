#!/usr/bin/env node
'use strict';
// macOS authoring tool. One SVG supplies the native, web and tray assets.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const at = p => path.join(root, p);
const run = (command, args) => execFileSync(command, args, { stdio: 'inherit' });
const svg = fs.readFileSync(at('build/mark.svg'), 'utf8');
const shapes = svg.replace(/<svg[^>]*>|<\/svg>|<title>.*?<\/title>|<!--.*?-->/gs, '').trim();
const mono = shapes.replace(/fill="[^"]+"/g, 'fill="#000"');
const icon = `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="shell" x1="0" y1="0" x2="1" y2="1">
      <stop stop-color="#29384b"/><stop offset="1" stop-color="#111720"/>
    </linearGradient>
    <linearGradient id="edge" x1="0" y1="0" x2="0" y2="1">
      <stop stop-color="#61748d" stop-opacity=".55"/><stop offset="1" stop-color="#61748d" stop-opacity=".12"/>
    </linearGradient>
  </defs>
  <rect x="100" y="100" width="824" height="824" rx="190" fill="url(#shell)"/>
  <rect x="102" y="102" width="820" height="820" rx="188" fill="none" stroke="url(#edge)" stroke-width="4"/>
  <g transform="translate(96 96) scale(13)">${shapes}</g>
</svg>
`;
const tray = `<svg xmlns="http://www.w3.org/2000/svg" width="36" height="36" viewBox="0 0 64 64">\n${mono}\n</svg>\n`;
fs.writeFileSync(at('build/icon.svg'), icon);
fs.writeFileSync(at('src-tauri/icons/tray.svg'), tray);
fs.copyFileSync(at('build/mark.svg'), at('renderer/brand-mark.svg'));

function raster(source, dest, size) {
  run('rsvg-convert', ['--width', String(size), '--height', String(size), '--output', dest, source]);
}
for (const size of [32, 128, 256]) raster(at('build/icon.svg'), at(`src-tauri/icons/${size}x${size}.png`), size);
raster(at('build/mark.svg'), at('build/mark.png'), 128);
raster(at('build/icon.svg'), at('src-tauri/icons/icon.png'), 1024);
raster(at('src-tauri/icons/tray.svg'), at('src-tauri/icons/tray.png'), 36);
const templateBase = at('build/trayTemplate');
raster(at('src-tauri/icons/tray.svg'), `${templateBase}.png`, 18);
fs.copyFileSync(at('src-tauri/icons/tray.png'), `${templateBase}@2x.png`);

const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'katala-icons-'));
try {
  const iconset = path.join(temp, 'icon.iconset');
  fs.mkdirSync(iconset);
  for (const size of [16, 32, 128, 256, 512]) {
    raster(at('build/icon.svg'), path.join(iconset, `icon_${size}x${size}.png`), size);
    raster(at('build/icon.svg'), path.join(iconset, `icon_${size}x${size}@2x.png`), size * 2);
  }
  run('iconutil', ['--convert', 'icns', '--output', at('build/icon.icns'), iconset]);
  // Independent PNG frames preserve crisp small sizes in the Windows icon.
  const frames = [16, 24, 32, 48, 64, 128, 256].map(size => {
    const frame = path.join(temp, `${size}.png`);
    raster(at('build/icon.svg'), frame, size);
    return frame;
  });
  run('magick', [...frames, at('build/icon.ico')]);
} finally {
  fs.rmSync(temp, { recursive: true, force: true });
}

// Review sheet at native UI sizes, with both light and dark surroundings.
const iconUri = `data:image/svg+xml;base64,${Buffer.from(icon).toString('base64')}`;
const sizes = [16, 24, 32, 48, 64, 128];
const samples = (y, ink) => sizes.map((size, i) => {
  const x = 110 + i * 174;
  return `<image href="${iconUri}" x="${x + (128 - size) / 2}" y="${y + (128 - size) / 2}" width="${size}" height="${size}"/>
    <text x="${x + 64}" y="${y + 158}" text-anchor="middle" fill="${ink}" font-size="15">${size} px</text>`;
}).join('');
const sheet = `<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="1056" viewBox="0 0 1280 1056">
  <rect width="1280" height="1056" fill="#f3f4f6"/>
  <g font-family="Helvetica Neue,Arial,sans-serif">
    <text x="76" y="65" font-size="16" letter-spacing="3" fill="#617084">KATALA TUNE / IDENTITY</text>
    <text x="76" y="126" font-size="44" font-weight="600" fill="#182230">One system. One mark.</text>
    <text x="78" y="166" font-size="19" fill="#617084">A shared foundation. Connected capabilities.</text>
    <rect x="76" y="206" width="542" height="360" rx="24" fill="#e6eaf0"/>
    <rect x="642" y="206" width="562" height="360" rx="24" fill="#151d29"/>
    <image href="${iconUri}" x="177" y="223" width="332" height="332"/>
    <g transform="translate(804 241) scale(4.5)">${shapes}</g>
    <text x="76" y="620" font-size="15" letter-spacing="2" fill="#617084">APPLICATION / LIGHT</text>
    ${samples(640, '#617084')}
    <rect y="824" width="1280" height="232" fill="#151d29"/>
    <text x="76" y="864" font-size="15" letter-spacing="2" fill="#9daabd">APPLICATION / DARK</text>
    ${samples(880, '#9daabd')}
  </g>
</svg>`;
const previewSvg = path.join(os.tmpdir(), `katala-brand-preview-${process.pid}.svg`);
try {
  fs.writeFileSync(previewSvg, sheet);
  run('rsvg-convert', ['--output', at('docs/brand-preview.png'), previewSvg]);
} finally {
  fs.rmSync(previewSvg, { force: true });
}
console.log('Generated app, sidebar and template icons from build/mark.svg');
