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

// 部品見本。ui.js と charts.js の部品を 1 ページに並べる（後で作る「道具」の画面で使う形も含む）。画面側で実行する
function partsPage() {
  const nodes = ['studio-mac', 'air-m3', 'build-mini', 'gpu-tower', 'media-pc', 'family-pc'];
  const tools = [
    ['git', ['2.51.0', '2.51.0', '2.50.1', '2.51.0', '2.51.0', null]],
    ['node', ['24.21.0', '24.21.0', '24.21.0', '22.20.0', '24.21.0', '24.21.0']],
    ['python3', ['3.13.7', '3.13.7', '3.12.11', '3.13.7', '3.13.7', '3.12.4']],
    ['ollama', ['0.12.3', null, null, '0.12.3', null, null]],
  ];
  const majority = (vs) => { const c = {}; vs.filter(Boolean).forEach((v) => { c[v] = (c[v] || 0) + 1; }); return Object.entries(c).sort((a, b) => b[1] - a[1])[0]?.[0]; };
  const now = Date.now();
  const pts = Array.from({ length: 24 }, (_, i) => ({ t: now - (23 - i) * 3600e3, v: i === 9 ? null : 40 + Math.round(20 * Math.sin(i / 3)) }));
  const box = (t, inner) => `<div class="chart-box"><div class="chart-title">${UI.esc(t)}</div>${inner}</div>`;
  const tones = ['default', 'gray', 'brown', 'orange', 'yellow', 'green', 'blue', 'purple', 'pink', 'red'];
  document.querySelector('#crumbs').innerHTML = '<span class="crumb cur">部品見本</span>';
  document.querySelector('#page').innerHTML = `<div class="page-inner">
    ${UI.head({ icon: 'tool', title: '部品見本', desc: 'renderer/ui.js と renderer/charts.js の部品。道具の台帳（道具×機体のマトリクス、版の違いの強調、検索、追加・削除の履歴）でも使い回す。', props: UI.props([['状態', UI.status('ok') + UI.status('warn') + UI.status('fail') + UI.status('unknown'), 'status'], ['タグ', tones.map((t) => UI.chip(t, t)).join(''), 'tag']]) })}
    ${UI.tabs([['matrix', 'マトリクス', 4, 'tool'], ['history', '追加・削除の履歴', 3, 'history']], 'matrix')}
    <div class="filters">${UI.search({ id: 'partsQ', placeholder: '道具の名前で絞り込む', value: '' })}${UI.select({ id: 'partsNode', label: '機体', options: [['', 'すべて'], ...nodes.map((n) => [n, n])], value: 'gpu-tower' })}${UI.seg({ attr: 'parts', options: [['all', 'すべて'], ['diff', '版が違うものだけ']], value: 'all' })}<span class="count">4 件</span></div>
    ${UI.matrix({
      corner: '道具', rows: tools.map(([k, vs]) => ({ key: k, label: k, sub: `多数派 ${majority(vs)}` })), cols: nodes.map((n) => ({ key: n, label: n })),
      cell: (r, c) => { const vs = tools.find((t) => t[0] === r.key)[1]; const v = vs[nodes.indexOf(c.key)]; if (!v) return {}; const m = majority(vs); return { html: `<span class="num">${UI.esc(v)}</span>`, diff: v !== m, title: v !== m ? `多数派は ${m}` : '' }; },
    })}
    ${UI.section('追加・削除の履歴')}
    ${UI.table({
      cols: [{ label: '日時' }, { label: '機体' }, { label: '変化' }, { label: '道具' }, { label: '版', cls: 'n' }],
      rows: [['10/7 09:12', 'gpu-tower', '更新', 'node', '22.20.0 → 24.21.0'], ['10/6 22:40', 'build-mini', '追加', 'ollama', '0.12.3'], ['10/5 18:03', 'family-pc', '削除', 'git', '2.49.0']],
      row: (r) => `<td class="muted nowrap">${r[0]}</td><td><b>${r[1]}</b></td><td>${UI.chip(r[2], { 追加: 'green', 削除: 'red', 更新: 'blue' }[r[2]])}</td><td>${r[3]}</td><td class="n">${r[4]}</td>`,
    })}
    ${UI.section('コールアウトとトグル')}
    ${['red', 'orange', 'yellow', 'green', 'blue', 'gray'].map((t) => UI.callout({ tone: t, icon: t === 'green' ? 'check' : t === 'gray' || t === 'blue' ? 'info' : 'alert', title: `${t} のコールアウト`, body: '本文はふつうの文字色で書く。' })).join('')}
    ${UI.toggle({ summary: '<span class="f-title">開いたトグル</span>', body: '<p class="f-advice">中身。<code>inline code</code> も使える。</p><div class="code">npm run ui-preview</div>', open: true })}
    ${UI.toggle({ summary: '<span class="f-title">閉じたトグル</span>', body: '見えない' })}
    ${UI.section('グラフ')}
    <div class="charts">
      ${box('横棒（しきい値 60 / 85）', [10, 64, 92, null].map((v, i) => Charts.meter(v, { label: ['正常', '注意', '異常', '値なし'][i], warn: 60, crit: 85 })).join(''))}
      ${box('リングとゲージ', `<div style="display:flex;gap:14px;align-items:center">${[null, 30, 70, 95].map((s) => Charts.ring(s)).join('')}${Charts.gauge(72, { label: '平均' })}</div>`)}
      ${box('折れ線（null で線が切れる）', Charts.line(pts, { unit: '%', min: 0, max: 100, marks: [{ v: 60, tone: 'warn' }] }))}
      ${box('積み上げと相対棒', `${Charts.stacked([{ label: '異常', value: 2, tone: 'crit' }, { label: '注意', value: 5, tone: 'warn' }, { label: '正常', value: 40, tone: 'ok' }, { label: '不明', value: 3, tone: 'unknown' }])}<div style="margin-top:14px">${Charts.dataBar(30, 100)}</div><div style="margin-top:8px">${Charts.dataBar(80, 100, { tone: 'warn' })}</div>`)}
    </div></div>`;
}

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
  { name: 'tools', js: [click('[data-view="tools"]')], wait: 500, slices: 2 },
  { name: 'tools-all', js: [click('[data-invall="1"]')], wait: 400 },
  { name: 'tools-drift', js: [click('[data-invall="0"]'), click('[data-invdrift="1"]')], wait: 400 },
  { name: 'tools-search', js: [click('[data-invdrift="0"]'), setInput('#invQ', 'code')], wait: 700 },
  { name: 'tools-history', js: [setInput('#invQ', ''), click('[data-tab="history"]')], wait: 700 },
  { name: 'tools-dogu-empty', js: [click('[data-tab="dogu"]')], wait: 400 },
  { name: 'tools-dogu', js: [click('#doguRefresh')], wait: 1100, slices: 2 },
  { name: 'ai', js: [click('[data-tab="matrix"]'), click('[data-view="ai"]')], wait: 700, slices: 5 },
  { name: 'ai-codex-7d', js: [setSelect('#aiTool', 'codex'), setSelect('#aiDays', '7')], wait: 700, slices: 2 },
  { name: 'ai-sessions-errors', js: [setSelect('#aiTool', ''), setSelect('#aiDays', '30'), click('[data-aierr="1"]')], wait: 700, slices: 5 },
  { name: 'security', js: [click('[data-aierr="0"]'), click('[data-view="security"]')], wait: 700, slices: 4 },
  { name: 'security-node', js: [setSelect('#secNode', 'gpu-tower')], wait: 500, slices: 2 },
  { name: 'parts', js: [click('[data-aierr="0"]'), `(${partsPage.toString()})()`], slices: 3 },
  { name: 'probing', js: [click('[data-view="overview"]'), click('#btnProbe')], wait: 450 },
  // ライブ表示（プレビューの preload が架空の値を 1 秒ごとに流す）。最後に置く（入れたままだと後の場面にもパネルが出る）
  { name: 'resources-live', js: [click('[data-view="resources"]'), click('[data-live-toggle]')], wait: 4200 },
  { name: 'node-live', js: [click('[data-view="node:gpu-tower"]'), click('[data-tab="findings"]'), click('[data-live-toggle]')], wait: 2500, slices: 2 },
];
// Windows の配置（キャプションボタンの逃げ）と、最小のウィンドウ幅。electron は AI の API が「未対応」を返す Electron 版の見え方
const VARIANTS = [
  { key: 'mac', platform: 'darwin', width: 1440, height: 920, scenes: SCENES },
  { key: 'win', platform: 'win32', width: 1440, height: 920, scenes: SCENES.filter((s) => ['overview', 'node-findings', 'logs'].includes(s.name)).map((s) => ({ ...s, slices: 1 })) },
  { key: 'narrow', platform: 'darwin', width: 1040, height: 700, scenes: SCENES.filter((s) => ['overview', 'status', 'resources', 'jobs', 'logs', 'node-findings', 'node-history', 'tools', 'tools-dogu-empty', 'tools-dogu', 'ai', 'security', 'probing', 'resources-live', 'node-live'].includes(s.name)).map((s) => ({ ...s, slices: 1 })) },
  { key: 'electron', platform: 'darwin', width: 1440, height: 920, query: { electron: '1' }, scenes: SCENES.filter((s) => ['ai', 'security'].includes(s.name)).map((s) => ({ ...s, slices: 1 })) },
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

  const query = { platform: v.platform, ...(v.query || {}), ...(ACCENT ? { accent: ACCENT } : {}) };
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
