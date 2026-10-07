// 画面の見た目を確かめる: npm run ui-preview（= npx electron scripts/ui-preview.js）
//   --show            ウィンドウを表示しながら撮る（既定は表示しない）
//   --only=a,b        名前に a か b を含む場面だけ撮る（例: --only=logs,node-history）
//   --theme=light     片方のテーマだけ撮る（light / dark）
//   --accent=8e44ad   アクセントカラーを変えて撮る（OS の設定の代わり）
// renderer/index.html を開き、scripts/ui-preview-preload.js が window.tune を test/fixtures/ui/*.json の架空データで差し替える。
// 各画面へ移動して dist/ui-preview/<テーマ>-<番号>-<場面>.png に保存する（長い画面は -2, -3 … と続けて撮る）。
// ライト／ダーク（nativeTheme.themeSource）、Windows 版の配置、最小のウィンドウ幅も撮る。
// コンソールのエラー・警告と読み込みの失敗を dist/ui-preview/report.json にまとめ、エラーがあれば終了コード 1。
// 撮るのは webContents の中身だけなので、vibrancy / Mica の代わりに近い色をウィンドウの背景に敷く。
'use strict';
const { app, BrowserWindow, nativeTheme } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const ROOT = path.join(__dirname, '..');
const OUT = path.join(ROOT, 'dist', 'ui-preview');
const arg = (k) => process.argv.find((a) => a.startsWith(`--${k}=`))?.split('=')[1];
const SHOW = process.argv.includes('--show');
const ONLY = arg('only')?.split(',').filter(Boolean);
const THEMES = arg('theme') ? [arg('theme')] : ['light', 'dark'];
const ACCENT = arg('accent');

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const click = (sel) => `document.querySelector(${JSON.stringify(sel)})?.click()`;
const setInput = (sel, v) => `(() => { const i = document.querySelector(${JSON.stringify(sel)}); if (!i) return; i.value = ${JSON.stringify(v)}; i.dispatchEvent(new Event('input')); })()`;
const setSelect = (sel, v) => `(() => { const s = document.querySelector(${JSON.stringify(sel)}); if (!s) return; s.value = ${JSON.stringify(v)}; s.dispatchEvent(new Event('change')); })()`;

// 場面。js を順に実行してから撮る。slices は長い画面を何枚に分けて撮るか
const SCENES = [
  { name: 'overview', js: [click('[data-view="overview"]')], slices: 3 },
  { name: 'status', js: [click('[data-view="status"]')], slices: 4 },
  { name: 'resources', js: [click('[data-view="resources"]')] },
  { name: 'procs', js: [click('[data-view="procs"]')], slices: 2 },
  { name: 'procs-search', js: [setInput('#procQ', 'chrome')], wait: 700 },
  { name: 'jobs', js: [setInput('#procQ', ''), click('[data-view="jobs"]')], slices: 2 },
  { name: 'jobs-failing', js: [click('[data-jf="1"]')] },
  { name: 'logs', js: [click('[data-jf="0"]'), click('[data-view="logs"]')], slices: 3 },
  { name: 'logs-search', js: [setInput('#logQ', 'WHEA')], wait: 900 },
  { name: 'logs-bad-query', js: [setInput('#logQ', '"WHEA')], wait: 900 },
  { name: 'logs-30d', js: [setInput('#logQ', ''), setSelect('#logSince', '30')], wait: 900 },
  { name: 'logs-1d', js: [setSelect('#logSince', '1')], wait: 600 },
  { name: 'actions', js: [setSelect('#logSince', '7'), click('[data-view="actions"]')] },
  { name: 'node-findings', js: [click('[data-view="node:gpu-tower"]'), click('[data-tab="findings"]')], slices: 3 },
  { name: 'node-procs', js: [click('[data-tab="procs"]')], slices: 2 },
  { name: 'node-jobs', js: [click('[data-tab="jobs"]')] },
  { name: 'node-logs', js: [click('[data-tab="logs"]')], slices: 2 },
  { name: 'node-history', js: [click('[data-tab="history"]')], slices: 2 },
  { name: 'node-machine', js: [click('[data-tab="machine"]')], slices: 2 },
  { name: 'node-fleet', js: [click('[data-tab="fleet"]')] },
  { name: 'node-mac', js: [click('[data-view="node:air-m3"]'), click('[data-tab="findings"]')], slices: 2 },
  { name: 'node-mac-history', js: [click('[data-tab="history"]')] },
  { name: 'node-healthy', js: [click('[data-view="node:build-mini"]'), click('[data-tab="findings"]')] },
  { name: 'node-unreachable', js: [click('[data-view="node:family-pc"]'), click('[data-tab="findings"]')] },
  { name: 'probing', js: [click('[data-view="overview"]'), click('#btnProbe')], wait: 450 },
];
// Windows の配置（キャプションボタンの逃げ）と、最小のウィンドウ幅
const VARIANTS = [
  { key: 'mac', platform: 'darwin', width: 1440, height: 920, scenes: SCENES },
  { key: 'win', platform: 'win32', width: 1440, height: 920, scenes: SCENES.filter((s) => ['overview', 'node-findings', 'logs'].includes(s.name)).map((s) => ({ ...s, slices: 1 })) },
  { key: 'narrow', platform: 'darwin', width: 1040, height: 700, scenes: SCENES.filter((s) => ['overview', 'status', 'resources', 'jobs', 'logs', 'node-findings', 'node-history'].includes(s.name)).map((s) => ({ ...s, slices: 1 })) },
];

const report = { started: new Date().toISOString(), shots: [], console: [], problems: [] };

async function waitFor(wc, expr, ms = 5000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (await wc.executeJavaScript(expr).catch(() => false)) return true;
    await wait(80);
  }
  throw new Error(`待ちきれなかった: ${expr}`);
}

async function shoot(wc, base, slices) {
  const geo = await wc.executeJavaScript(`(() => { const p = document.querySelector('#page'); return { sh: p.scrollHeight, ch: p.clientHeight }; })()`);
  const step = Math.max(200, geo.ch - 90);
  const n = Math.max(1, Math.min(slices || 1, Math.ceil((geo.sh - geo.ch) / step) + 1));
  for (let i = 0; i < n; i++) {
    await wc.executeJavaScript(`(() => { const p = document.querySelector('#page'); p.scrollTop = ${i * step}; p.dispatchEvent(new Event('scroll')); })()`);
    await wait(120);
    const img = await wc.capturePage();
    const file = path.join(OUT, `${base}${i ? `-${i + 1}` : ''}.png`);
    fs.writeFileSync(file, img.toPNG());
    report.shots.push(path.relative(ROOT, file));
  }
  await wc.executeJavaScript(`document.querySelector('#page').scrollTop = 0`);
}

async function runVariant(theme, v) {
  nativeTheme.themeSource = theme;
  const win = new BrowserWindow({
    width: v.width, height: v.height, useContentSize: true, show: SHOW, paintWhenInitiallyHidden: true,
    // vibrancy / Mica の代わり（撮れるのは webContents だけなので、透ける部分に近い色を敷く）
    backgroundColor: theme === 'dark' ? '#2a2a2a' : '#e9e9e7',
    webPreferences: { preload: path.join(__dirname, 'ui-preview-preload.js'), contextIsolation: true, nodeIntegration: false, sandbox: false },
  });
  const wc = win.webContents;
  const tag = `${theme}/${v.key}`;
  wc.on('console-message', (e) => {
    report.console.push({ tag, level: e.level, message: e.message, at: `${path.basename(e.sourceId || '')}:${e.lineNumber ?? ''}` });
  });
  wc.on('preload-error', (_e, p, err) => report.problems.push({ tag, kind: 'preload-error', message: `${p}: ${err}` }));
  wc.on('did-fail-load', (_e, code, desc, url) => report.problems.push({ tag, kind: 'did-fail-load', message: `${code} ${desc} ${url}` }));
  wc.on('render-process-gone', (_e, d) => report.problems.push({ tag, kind: 'render-process-gone', message: JSON.stringify(d) }));
  wc.setWindowOpenHandler(() => ({ action: 'deny' }));
  wc.on('will-navigate', (e) => e.preventDefault());

  const query = { platform: v.platform, ...(ACCENT ? { accent: ACCENT } : {}) };
  await win.loadFile(path.join(ROOT, 'renderer', 'index.html'), { query });
  await waitFor(wc, `!!document.querySelector('#page .page-inner') && !!document.querySelector('.gallery .card')`);
  await wait(400); // katala-fleet の読み込みと描き直しを待つ
  let i = 0;
  for (const s of v.scenes) {
    i++;
    if (ONLY && !ONLY.some((o) => s.name.includes(o))) {
      for (const js of s.js) await wc.executeJavaScript(js);
      await wait(s.wait ?? 250);
      continue;
    }
    for (const js of s.js) { await wc.executeJavaScript(js); await wait(60); }
    await wait(s.wait ?? 350);
    await shoot(wc, `${theme}-${v.key}-${String(i).padStart(2, '0')}-${s.name}`, s.slices);
  }
  await wait(1700); // 分析中の場面の後始末（プレビューの分析は 1.5 秒で終わる）
  win.destroy();
}

async function main() {
  fs.mkdirSync(OUT, { recursive: true });
  for (const f of fs.readdirSync(OUT)) if (f.endsWith('.png') || f === 'report.json') fs.rmSync(path.join(OUT, f));
  for (const theme of THEMES) for (const v of VARIANTS) await runVariant(theme, v);
  const errors = report.console.filter((c) => c.level === 'error').length + report.problems.length;
  const warnings = report.console.filter((c) => c.level === 'warning').length;
  report.finished = new Date().toISOString();
  report.summary = { shots: report.shots.length, errors, warnings };
  fs.writeFileSync(path.join(OUT, 'report.json'), JSON.stringify(report, null, 1));
  for (const c of report.console.filter((x) => x.level === 'error' || x.level === 'warning')) console.log(`${c.level.toUpperCase()} [${c.tag}] ${c.message} (${c.at})`);
  for (const p of report.problems) console.log(`ERROR [${p.tag}] ${p.kind}: ${p.message}`);
  console.log(`${report.shots.length} 枚を ${path.relative(ROOT, OUT)} に保存。コンソールのエラー ${errors} 件・警告 ${warnings} 件`);
  return errors ? 1 : 0;
}

app.setName('Katala Tune Preview');
app.setPath('userData', path.join(os.tmpdir(), 'katala-tune-ui-preview'));
if (!SHOW) app.dock?.hide();
app.on('window-all-closed', () => {});
app.whenReady().then(main).then((code) => app.exit(code), (e) => { console.error(e); app.exit(2); });
