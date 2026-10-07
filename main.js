// Katala Tune: Electron のメインプロセス。調査・判定・実行・保存・自動スキャンはここで行い、画面には結果だけ渡す
'use strict';
const { app, BrowserWindow, ipcMain, dialog, clipboard, shell, nativeTheme, systemPreferences, Tray, Menu, Notification, nativeImage } = require('electron');
const os = require('node:os');
const path = require('node:path');
const fs = require('node:fs');
const { spawn } = require('node:child_process');
const { loadConfig, ensureConfig } = require('./lib/nodes');
const { probeAll } = require('./lib/collect');
const { analyze, score, compare } = require('./lib/rules');
const actions = require('./lib/actions');
const { openDb } = require('./lib/db');
const logs = require('./lib/logs');
const health = require('./lib/health');
const inventory = require('./lib/inventory');
const dogu = require('./lib/dogu');

const IS_MAC = process.platform === 'darwin';
const IS_WIN = process.platform === 'win32';

// Finder・スタートメニューから起動すると PATH が最小なので、ssh・op-agent・python が見えるよう足す
const extraPath = IS_WIN
  ? [path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'OpenSSH')]
  : ['/opt/homebrew/bin', '/usr/local/bin', path.join(os.homedir(), '.local/bin'), '/usr/bin', '/bin'];
process.env.PATH = [process.env.PATH, ...extraPath].join(path.delimiter);

const DEFAULT_SCHEDULE = { enabled: true, probe_minutes: 60, logs_minutes: 15, inventory_hours: 24 };
let cfg = { nodes: [], protect: [] };
let cfgError = null;
let db;
let win;
let tray;
let probing = false;
let syncing = false;
let inventorying = false;
let fleetCache = null;
let fleetAt = null;
const lastProbeError = {};
const lastInventoryError = {};

function reloadConfig() {
  try { cfg = loadConfig(); cfgError = null; } catch (e) { cfgError = String(e.message || e); }
  return cfg;
}

const schedule = () => ({ ...DEFAULT_SCHEDULE, ...(cfg.schedule || {}), ...(db?.getMeta('schedule') || {}) });
const isExample = () => cfg.nodes.some((n) => n.id === 'my-mac') && cfg.nodes.some((n) => n.id === 'family-pc');
const nodeById = (id) => cfg.nodes.find((n) => n.id === id);
const publicNode = ({ id, alias, os: o, role, note, shared, local, expect }) => ({ id, alias, os: o, role, note, shared: !!shared, local, expect: expect || null });

function summarize(e) {
  const d = e.data || {};
  const f = e.findings || [];
  return {
    bench_ms: d.bench?.median_ms ?? null, cpu: d.cpu_busy ?? null, mem_avail: d.memory?.available_pct ?? null, swap_gb: d.memory?.swap_used_gb ?? null,
    critical: f.filter((x) => x.severity === 'critical').length, warn: f.filter((x) => x.severity === 'warn').length,
  };
}

// snapshot に所見（probe 由来＋ログ由来）と前回比を付ける
function enrich(result, prevData) {
  if (!result.ok) return result;
  const node = nodeById(result.node_id);
  const findings = [...analyze(result.data, node), ...logs.logFindings(db, result.node_id)];
  const SEV = { critical: 0, warn: 1, info: 2 };
  findings.sort((a, b) => SEV[a.severity] - SEV[b.severity]);
  if (node?.shared) for (const f of findings) if (f.action) f.action.blocked = '共用機のため、この画面からは実行しない（持ち主と相談）';
  return { ...result, findings, score: score(findings), compare: compare(prevData, result.data) };
}

// ---- 状態（機能チェック） ----
// broadcast: 画面へ「更新された」を送るか。画面からの問い合わせ（status）では送らない（送ると問い合わせが往復し続ける）
function computeChecks({ notify = true, broadcast = true } = {}) {
  const now = Date.now();
  const cursors = db.cursors();
  const sch = schedule();
  const changed = [];
  for (const n of cfg.nodes) {
    const [last] = db.lastSnapshots(n.id, 1);
    const full = last ? enrich({ node_id: n.id, ok: true, data: last.data, at: last.at, wall_s: last.wall_s }) : null;
    const checks = health.nodeChecks(n, last || null, full?.findings || [], { now, cursors, expect: n.expect, schedule: sch, lastError: lastProbeError[n.id] });
    changed.push(...db.saveChecks(n.id, checks, now));
  }
  const integ = db.integrity();
  const appC = health.appChecks({
    now, configError: cfgError, example: isExample(), nodeCount: cfg.nodes.length, protectCount: cfg.protect.length,
    dbCheck: integ.check, dbBytes: integ.bytes, scheduler: { ...sch, lastProbeAt: db.getMeta('lastProbeAt'), lastLogsAt: db.getMeta('lastLogsAt') },
    fleet: cfg.fleet?.repo ? fleetCache : undefined, fleetAt, openAtLogin: app.getLoginItemSettings().openAtLogin,
  });
  changed.push(...db.saveChecks('_app', appC, now));
  if (notify) notifyChanges(changed.filter((c) => c.from !== null));
  updateTray();
  if (broadcast) win?.webContents.send('checks-updated');
  return changed;
}

// 悪くなって異常（fail）になったとき、異常から戻ったときだけ通知する（同じ状態が続いても繰り返さない）
function notifyChanges(changes) {
  const important = changes.filter((c) => c.to === 'fail' || (c.from === 'fail' && c.to === 'ok'));
  if (!important.length || !Notification.isSupported()) return;
  const body = important.slice(0, 4).map((c) => `${c.scope === '_app' ? 'アプリ' : c.scope}: ${c.name} ${c.to === 'fail' ? '異常' : '回復'}`).join('\n');
  const n = new Notification({ title: `Katala Tune: ${important.length} 件の状態変化`, body: body + (important.length > 4 ? '\n…' : '') });
  n.on('click', () => { showWindow(); win?.webContents.send('navigate', 'status'); });
  n.show();
}

function statusCounts() {
  const c = { ok: 0, warn: 0, fail: 0, unknown: 0 };
  for (const r of db.checks()) c[r.status] = (c[r.status] || 0) + 1;
  return c;
}

// ---- 分析・ログ取り込み・自動スキャン ----
async function runProbe(ids, { auto = false } = {}) {
  if (probing) return { busy: true };
  probing = true;
  updateTray();
  try {
    reloadConfig();
    const targets = cfg.nodes.filter((n) => !ids?.length || ids.includes(n.id));
    const results = await probeAll(targets, (r) => {
      const prev = db.lastSnapshots(r.node_id, 1)[0];
      const full = enrich(r, prev?.data);
      if (full.ok) {
        delete lastProbeError[r.node_id];
        db.addSnapshot(r.node_id, { at: full.at, wall_s: full.wall_s, score: full.score, findings: full.findings.map(({ id, severity }) => ({ id, severity })), data: full.data, summary: summarize(full) });
      } else lastProbeError[r.node_id] = full.error;
      win?.webContents.send('probe-result', full);
    });
    if (!ids?.length) db.setMeta('lastProbeAt', Date.now());
    return { done: results.length, auto };
  } finally {
    probing = false;
    computeChecks();
  }
}

async function syncLogs(targets = cfg.nodes) {
  if (syncing) return { busy: true };
  syncing = true;
  updateTray();
  try {
    const t0 = Date.now();
    const res = await logs.syncAll(db, targets, (r) => win?.webContents.send('logs-synced', r));
    db.prune();
    if (targets.length === cfg.nodes.length) db.setMeta('lastLogsAt', Date.now());
    return { done: res.length, ms: Date.now() - t0, results: res };
  } finally {
    syncing = false;
    computeChecks();
  }
}

// 道具の棚卸し（読み取り専用）。変化は inventory_events に残る
async function runInventory(ids) {
  if (inventorying) return { busy: true };
  inventorying = true;
  try {
    reloadConfig();
    const targets = cfg.nodes.filter((n) => !ids?.length || ids.includes(n.id));
    const results = await Promise.all(targets.map(async (n) => {
      const r = await inventory.inventoryNode(n).catch((e) => ({ node_id: n.id, ok: false, error: String(e) }));
      const out = r.ok ? { node_id: n.id, ok: true, wall_s: r.wall_s, errors: r.errors, ...db.saveInventory(n.id, r.items, r.at, { skipSources: r.failedSources }) } : { node_id: n.id, ok: false, error: r.error };
      if (r.ok) delete lastInventoryError[n.id]; else lastInventoryError[n.id] = r.error;
      win?.webContents.send('inventory-result', out);
      return out;
    }));
    if (!ids?.length) db.setMeta('lastInventoryAt', Date.now());
    return { results };
  } finally {
    inventorying = false;
  }
}

// 道具の一覧（機体×道具）。Do-gu のマスターは取得済みのときだけ照合に使う（ここでは取りにいかない）。
// all: 依存として入ったものも含める（既定は自分で入れたものだけ）。下書きはいつも自分で入れたものだけから作る
function inventoryView({ all = false } = {}) {
  const cached = db.getMeta('dogu_tools');
  const matchSlug = cached ? dogu.makeMatcher(cached.tools) : null;
  const rows = db.inventory();
  const exclude = db.getMeta('dogu_exclude') || [];
  const m = inventory.matrix(rows, { matchSlug });
  const shown = all ? inventory.matrix(rows, { matchSlug, explicitOnly: false }) : m;
  const cats = cached ? new Map(cached.tools.map((t) => [t.slug, t.category])) : new Map();
  return {
    nodes: cfg.nodes.map(({ id, os: o }) => ({ id, os: o, count: rows.filter((r) => r.node_id === id && r.explicit).length, error: lastInventoryError[id] || null })),
    groups: shown.map((g) => ({ ...g, category: g.slug ? cats.get(g.slug) || null : null })), all,
    events: db.inventoryEvents(100), sources: inventory.SOURCES, lastInventoryAt: db.getMeta('lastInventoryAt'), inventorying,
    dogu: cached ? { at: cached.at, tools: cached.tools.length, matched: m.filter((g) => g.slug).length, draft: dogu.deckDraft(m, cached.tools, exclude), exclude } : null,
  };
}

// 1分ごとに予定を見て、期限が来たものだけ動かす（スリープ明けでも溜まった分を1回で済ませる）
function tick() {
  const s = schedule();
  if (!s.enabled || probing || syncing) return;
  const now = Date.now();
  if (now - (db.getMeta('lastProbeAt') || 0) >= s.probe_minutes * 60000) {
    runProbe(null, { auto: true }).then(() => syncLogs(reloadConfig().nodes)).catch(() => {});
  } else if (now - (db.getMeta('lastLogsAt') || 0) >= s.logs_minutes * 60000) {
    syncLogs(reloadConfig().nodes).catch(() => {});
  } else if (!inventorying && now - (db.getMeta('lastInventoryAt') || 0) >= s.inventory_hours * 3600e3) {
    runInventory().catch(() => {});
  }
}

// ---- IPC ----
ipcMain.handle('config', () => ({
  nodes: cfg.nodes.map(publicNode), error: cfgError, file: cfg.file, dataDir: path.join(app.getPath('userData'), 'data'),
  platform: process.platform, fleet: !!cfg.fleet?.repo, example: isExample(), schedule: schedule(),
  // システムのアクセントカラー（macOS / Windows）。取れなければ既定の青
  accent: (() => { try { const c = systemPreferences.getAccentColor?.(); return c ? `#${c.slice(0, 6)}` : null; } catch { return null; } })(),
}));

ipcMain.handle('last', () => cfg.nodes.map((n) => {
  const [last, prev] = db.lastSnapshots(n.id, 2);
  if (!last) return null;
  return { ...enrich({ node_id: n.id, ok: true, data: last.data, at: last.at, wall_s: last.wall_s }, prev?.data), stale: true };
}).filter(Boolean));

ipcMain.handle('probe', (_e, ids) => runProbe(ids).then((r) => { if (!ids?.length && !r.busy) syncLogs(cfg.nodes).catch(() => {}); return r; }));
ipcMain.handle('history', (_e, id) => db.history(id, 60));
ipcMain.handle('logs-sync', (_e, ids) => syncLogs(reloadConfig().nodes.filter((n) => !ids?.length || ids.includes(n.id))));
ipcMain.handle('logs-query', (_e, f) => {
  try { return { rows: db.queryLogs(f || {}) }; } catch (e) { return { error: String(e.message || e) }; }
});
ipcMain.handle('logs-signatures', (_e, f) => db.signatures(f || {}));
ipcMain.handle('logs-cursors', () => db.cursors());
ipcMain.handle('status', () => {
  computeChecks({ notify: false, broadcast: false });
  return { checks: db.checks(), events: db.checkEvents(150), schedule: schedule(), lastProbeAt: db.getMeta('lastProbeAt'), lastLogsAt: db.getMeta('lastLogsAt'), openAtLogin: app.getLoginItemSettings().openAtLogin, counts: statusCounts(), probing, syncing };
});
ipcMain.handle('set-schedule', (_e, patch) => {
  const s = { ...(db.getMeta('schedule') || {}) };
  if (typeof patch?.enabled === 'boolean') s.enabled = patch.enabled;
  for (const k of ['probe_minutes', 'logs_minutes']) if (Number.isInteger(patch?.[k]) && patch[k] >= 5 && patch[k] <= 1440) s[k] = patch[k];
  if (Number.isInteger(patch?.inventory_hours) && patch.inventory_hours >= 1 && patch.inventory_hours <= 168) s.inventory_hours = patch.inventory_hours;
  db.setMeta('schedule', s);
  computeChecks({ notify: false, broadcast: false });
  return schedule();
});
// ログイン時の起動は、操作者が画面で切り替えたときだけ変える
ipcMain.handle('set-login', (_e, on) => {
  app.setLoginItemSettings({ openAtLogin: !!on, openAsHidden: true, args: ['--hidden'] });
  computeChecks({ notify: false, broadcast: false });
  return app.getLoginItemSettings().openAtLogin;
});

// katala-fleet の概況（任意）。op-agent が秘密を子プロセスにだけ渡すので、この画面には値が来ない
ipcMain.handle('fleet', async () => {
  const repo = cfg.fleet?.repo;
  if (!repo) return { disabled: true };
  if (!fs.existsSync(path.join(repo, cfg.fleet.env_file))) return (fleetCache = { error: `katala-fleet が見つからない: ${repo}` });
  const res = await new Promise((resolve) => {
    const child = spawn('op-agent', ['run', `--env-file=${cfg.fleet.env_file}`, '--', 'python3', 'scripts/fleet-status.py', '--json'], { cwd: repo, stdio: ['ignore', 'pipe', 'pipe'] });
    let out = '', err = '';
    const t = setTimeout(() => child.kill('SIGKILL'), 60000);
    child.stdout.on('data', (d) => { out += d; });
    child.stderr.on('data', (d) => { err += d; });
    child.on('error', (e) => { err += String(e); });
    child.on('close', (code) => { clearTimeout(t); resolve({ code, out, err }); });
  });
  try {
    const v = JSON.parse(res.out);
    fleetCache = { now: v.now, summary: v.summary, nodes: v.nodes.map(({ node_id, state, last_seen, metrics, failing, running_containers, agent_processes, os: o }) => ({ node_id, state, last_seen, metrics, failing, running_containers, agent_processes, os: o })), attention: v.attention };
  } catch {
    fleetCache = { error: (res.err || `exit ${res.code}`).trim().split('\n').slice(-3).join('\n') };
  }
  fleetAt = Date.now();
  return fleetCache;
});

async function confirmAndRun(nodeId, action, title, undoOf = null) {
  // 実行の直前に台帳（共用機の指定・保護リスト）を読み直す。読めなければ実行しない
  let fresh;
  try { fresh = loadConfig(); } catch (e) { return { ok: false, refused: `台帳を読めないので実行しない: ${e.message}` }; }
  const node = fresh.nodes.find((n) => n.id === nodeId);
  if (!node) return { ok: false, refused: '台帳に無い機体' };
  let p;
  try { p = actions.plan(node, action, { protect: fresh.protect }); } catch (e) { return { ok: false, refused: String(e.message || e) }; }
  const { response } = await dialog.showMessageBox(win, {
    type: 'warning', buttons: ['実行する', 'やめる'], defaultId: 1, cancelId: 1,
    title: 'Katala Tune', message: title, detail: `${p.describe}\n\n実行するコマンド:\n${p.script}`,
  });
  if (response !== 0) return { ok: false, cancelled: true };
  let r;
  try { r = await actions.execute(node, action, { protect: loadConfig().protect }); } catch (e) { return { ok: false, refused: String(e.message || e) }; }
  const entry = { id: `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`, at: Date.now(), node_id: node.id, type: action.type, params: action.params, label: title, ok: r.ok, output: `${r.outcome}\n${r.output}`.trim(), undo: r.undo, undo_of: undoOf };
  db.addAction(entry);
  return { ...r, entry };
}

ipcMain.handle('action', (_e, nodeId, action, label) => confirmAndRun(nodeId, action, `${nodeId}: ${label}`));
ipcMain.handle('undo', async (_e, entryId) => {
  const all = db.actions(1000);
  const entry = all.find((a) => a.id === entryId);
  if (!entry?.undo) return { ok: false, refused: '元に戻せる記録が無い' };
  if (all.some((a) => a.undo_of === entryId && a.ok)) return { ok: false, refused: 'すでに元に戻した' };
  return confirmAndRun(entry.node_id, entry.undo, `${entry.node_id}: 元に戻す（${entry.label}）`, entryId);
});
ipcMain.handle('actions-log', () => db.actions(100));
ipcMain.handle('inventory', (_e, opts) => inventoryView({ all: !!opts?.all }));
ipcMain.handle('inventory-run', (_e, ids) => runInventory(ids).then(() => inventoryView()));
// Do-gu の共通マスターを取りにいく（画面のボタンを押したときだけ。送るものは無い）
ipcMain.handle('dogu-refresh', async () => {
  try { await dogu.tools(db, { force: true }); return inventoryView(); } catch (e) { return { error: String(e.message || e) }; }
});
ipcMain.handle('dogu-exclude', (_e, slugs) => {
  db.setMeta('dogu_exclude', [...new Set((Array.isArray(slugs) ? slugs : []).map(String))]);
  return inventoryView();
});
// デッキへの登録（外への送信）。機体を変える操作ではないが、同じ型で扱う:
// 検証（下書きにある slug だけ）→ 全件を見せた確認 → 確認後に下書きを作り直して再検証（変わっていたら送らない）→ 送信 → 実行記録。リトライしない
ipcMain.handle('dogu-publish', async (_e, slugs) => {
  const draftNow = () => inventoryView().dogu?.draft;
  if (!draftNow()) return { ok: false, refused: '先に Do-gu の一覧を取得してください' };
  const plan = dogu.planPublish(draftNow(), slugs);
  if (plan.refused) return { ok: false, refused: plan.refused };
  const key = dogu.apiKey();
  if (!key) return { ok: false, refused: `API キーが見つからない。${dogu.BASE}/howto で発行し、環境変数 DO_GU_API_KEY か ~/.config/do-gu/api_key に保存してください` };
  let login;
  try { login = (await dogu.me(key)).login; } catch (e) { return { ok: false, refused: String(e.message || e) }; }
  const list = plan.items.map((d) => `・${d.name}（${d.category}）`).join('\n');
  const { response } = await dialog.showMessageBox(win, {
    type: 'warning', buttons: ['登録する', 'やめる'], defaultId: 1, cancelId: 1, title: 'Katala Tune',
    message: `Do-gu のデッキに ${plan.pick.length} 件を登録します`,
    detail: `登録すると ${dogu.BASE}/@${login} で誰でも見られる公開ページに載ります。\n送るのは既にある道具への紐づけだけで、新しい道具は作りません。\n\n${list}`,
  });
  if (response !== 0) return { ok: false, cancelled: true };
  const fresh = dogu.planPublish(draftNow(), slugs);
  if (!dogu.samePlan(plan, fresh)) return { ok: false, refused: '確認のあいだに下書きが変わったので送らなかった。もう一度確認してください' };
  let res, ok = true;
  try { res = await dogu.publish(key, fresh.pick); } catch (e) { ok = false; res = { error: String(e.message || e) }; }
  const at = Date.now();
  db.addAction({ id: dogu.publishActionId(at), at, node_id: '_app', type: 'dogu_publish', params: { slugs: fresh.pick }, label: `Do-gu に ${fresh.pick.length} 件を登録`, ok, output: JSON.stringify(res).slice(0, 4000), undo: null, undo_of: null });
  return { ok, login, url: `${dogu.BASE}/@${login}`, result: res };
});
ipcMain.handle('copy', (_e, text) => { clipboard.writeText(String(text)); return true; });
ipcMain.handle('open-data-dir', () => shell.openPath(path.join(app.getPath('userData'), 'data')));
ipcMain.handle('open-config', () => shell.openPath(cfg.file || ensureConfig().file));

// ---- ウィンドウとメニューバー ----
function showWindow() {
  if (!win || win.isDestroyed()) createWindow();
  else { if (win.isMinimized()) win.restore(); win.show(); win.focus(); }
}

function createWindow() {
  const common = {
    width: 1440, height: 920, minWidth: 1040, minHeight: 640, title: 'Katala Tune', show: false,
    webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, nodeIntegration: false, sandbox: true },
  };
  // macOS: サイドバーだけ半透明（vibrancy）。Windows 11: Mica。どちらもタイトルバーは中身と一体にする
  const platformOpts = IS_MAC
    ? { titleBarStyle: 'hiddenInset', trafficLightPosition: { x: 18, y: 18 }, vibrancy: 'sidebar', visualEffectState: 'followWindow', backgroundColor: '#00000000' }
    : IS_WIN
      ? { titleBarStyle: 'hidden', titleBarOverlay: { color: '#00000000', symbolColor: nativeTheme.shouldUseDarkColors ? '#ffffff' : '#000000', height: 44 }, backgroundMaterial: 'mica', backgroundColor: '#00000000' }
      : { backgroundColor: nativeTheme.shouldUseDarkColors ? '#1e1e1e' : '#f5f5f7' };
  win = new BrowserWindow({ ...common, ...platformOpts });
  win.loadFile(path.join(__dirname, 'renderer', 'index.html'), { query: { platform: process.platform } });
  win.once('ready-to-show', () => { if (!process.argv.includes('--hidden')) win.show(); });
  win.webContents.setWindowOpenHandler(({ url }) => { if (/^https:\/\//.test(url)) shell.openExternal(url); return { action: 'deny' }; });
  win.webContents.on('will-navigate', (e) => e.preventDefault());
  if (IS_WIN) nativeTheme.on('updated', () => win?.setTitleBarOverlay?.({ symbolColor: nativeTheme.shouldUseDarkColors ? '#ffffff' : '#000000' }));
}

function updateTray() {
  if (!tray || !db) return;
  const c = statusCounts();
  const s = schedule();
  if (IS_MAC) tray.setTitle(c.fail ? ` ${c.fail}` : '');
  tray.setToolTip(`Katala Tune — 異常 ${c.fail}・注意 ${c.warn}・正常 ${c.ok}`);
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: `異常 ${c.fail}　注意 ${c.warn}　正常 ${c.ok}${probing ? '（分析中）' : syncing ? '（ログ取り込み中）' : ''}`, enabled: false },
    { type: 'separator' },
    { label: '状態を開く', click: () => { showWindow(); win?.webContents.send('navigate', 'status'); } },
    { label: '今すぐ分析', enabled: !probing, click: () => runProbe().then(() => syncLogs(cfg.nodes)).catch(() => {}) },
    { label: 'ログを取り込む', enabled: !syncing, click: () => syncLogs(cfg.nodes).catch(() => {}) },
    { type: 'separator' },
    { label: `自動スキャン（分析 ${s.probe_minutes} 分・ログ ${s.logs_minutes} 分ごと）`, type: 'checkbox', checked: s.enabled, click: (m) => { db.setMeta('schedule', { ...(db.getMeta('schedule') || {}), enabled: m.checked }); computeChecks({ notify: false }); } },
    { type: 'separator' },
    { label: '終了', click: () => app.quit() },
  ]));
}

function createTray() {
  const img = nativeImage.createFromPath(path.join(__dirname, 'build', 'trayTemplate.png'));
  img.setTemplateImage(true);
  tray = new Tray(img);
  tray.on('click', () => { if (IS_WIN) showWindow(); });
  updateTray();
}

// 二重に起動すると同じ DB に自動スキャンが2重に走るので、2つ目は1つ目のウィンドウを出して終わる
const singleInstance = app.requestSingleInstanceLock();
if (!singleInstance) app.quit();
else app.on('second-instance', () => showWindow());

app.whenReady().then(() => {
  if (!singleInstance) return;
  ensureConfig();
  reloadConfig();
  const dataDir = path.join(app.getPath('userData'), 'data');
  db = openDb(dataDir);
  db.importLegacy(dataDir, (e) => summarize({ data: e.data, findings: e.findings }));
  createWindow();
  createTray();
  computeChecks({ notify: false });
  setTimeout(tick, 5000);
  setInterval(tick, 60000).unref();
  app.on('activate', showWindow);
});
// ウィンドウを閉じても、自動スキャンのためにメニューバー（Windows は通知領域）に残る。終了はメニューから
app.on('window-all-closed', () => {});
