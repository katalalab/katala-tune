// Katala Tune（Tauri 版）: 画面（renderer/）が使う window.tune を Tauri の IPC で用意する。
// preload.js と同じ名前・同じ引数・同じ戻り値の形。Tauri の初期化スクリプトとして、ページのスクリプトより先に動く。
// __KT_PLATFORM__ と __KT_DEBUG__ は Rust 側（src/window.rs）が埋める。
(function () {
  'use strict';
  if (window.tune) return;
  const PLATFORM = __KT_PLATFORM__;
  const DEBUG = __KT_DEBUG__;
  const T = () => window.__TAURI_INTERNALS__;
  const invoke = (cmd, args) => T().invoke(cmd, args || {});
  // Tauri のイベント（@tauri-apps/api の listen と同じ呼び方）。payload だけを渡す
  const listen = (event, fn) =>
    invoke('plugin:event|listen', { event, target: { kind: 'Any' }, handler: T().transformCallback((e) => fn(e.payload)) });

  // Electron 版は index.html?platform=darwin で開く（画面がこれで見た目を変える）。同じ形にする
  try {
    const q = new URLSearchParams(location.search);
    if (!q.get('platform')) {
      q.set('platform', PLATFORM);
      history.replaceState(history.state, '', `${location.pathname}?${q}${location.hash}`);
    }
  } catch (_) { /* 見た目だけの違いなので続ける */ }

  // 道具の棚卸し・Do-gu は Rust への移植待ち（呼ぶと理由つきで失敗する）
  const notPorted = (name) => () => Promise.reject(new Error(`Tauri 版では未移植: ${name}`));

  const tune = {
    config: () => invoke('config'),
    last: () => invoke('last'),
    probe: (ids) => invoke('probe', { ids: ids ?? null }),
    history: (id) => invoke('history', { id: id ?? null }),
    fleet: () => invoke('fleet'),
    action: (nodeId, action, label) => invoke('action', { nodeId, action, label }),
    undo: (entryId) => invoke('undo', { entryId }),
    actionsLog: () => invoke('actions_log'),
    logsSync: (ids) => invoke('logs_sync', { ids: ids ?? null }),
    logsQuery: (filter) => invoke('logs_query', { filter: filter ?? null }),
    logsSignatures: (filter) => invoke('logs_signatures', { filter: filter ?? null }),
    logsCursors: () => invoke('logs_cursors'),
    copy: (text) => invoke('copy', { text: String(text) }),
    openDataDir: () => invoke('open_data_dir'),
    openConfig: () => invoke('open_config'),
    status: () => invoke('status'),
    setSchedule: (patch) => invoke('set_schedule', { patch: patch ?? null }),
    setLogin: (on) => invoke('set_login', { on: !!on }),
    inventory: notPorted('inventory'),
    inventoryRun: notPorted('inventoryRun'),
    doguRefresh: notPorted('doguRefresh'),
    doguExclude: notPorted('doguExclude'),
    doguPublish: notPorted('doguPublish'),
    onInventoryResult: (fn) => { listen('inventory-result', (r) => fn(r)); },
    onChecksUpdated: (fn) => { listen('checks-updated', () => fn()); },
    onNavigate: (fn) => { listen('navigate', (v) => fn(v)); },
    onProbeResult: (fn) => { listen('probe-result', (r) => fn(r)); },
    onLogsSynced: (fn) => { listen('logs-synced', (r) => fn(r)); },
  };
  Object.defineProperty(window, 'tune', { value: Object.freeze(tune), enumerable: true });

  // ウィンドウを動かす: 画面の CSS にある Electron の `-webkit-app-region: drag | no-drag` を読み、
  // その範囲を押したら Tauri のドラッグを始める（画面の CSS は変えない）。`data-tauri-drag-region` は Tauri 自身が扱う
  const region = { drag: [], noDrag: [] };
  const RULE = /([^{}]+)\{([^{}]*)\}/g;
  const collect = (css) => {
    for (const m of css.replace(/\/\*[\s\S]*?\*\//g, '').matchAll(RULE)) {
      const v = /-webkit-app-region\s*:\s*(no-drag|drag)/.exec(m[2]);
      if (v) (v[1] === 'drag' ? region.drag : region.noDrag).push(m[1].trim());
    }
  };
  const loadRegions = () => {
    for (const link of document.querySelectorAll('link[rel="stylesheet"]')) {
      fetch(link.href).then((r) => r.text()).then(collect).catch(() => {});
    }
    for (const s of document.querySelectorAll('style')) collect(s.textContent || '');
  };
  document.addEventListener('DOMContentLoaded', loadRegions);
  const INTERACTIVE = 'button, input, select, textarea, a, label, [contenteditable], [data-no-drag]';
  const decide = (el) => {
    for (let n = el; n && n.nodeType === 1; n = n.parentElement) {
      if (n.matches(INTERACTIVE)) return false;
      if (region.noDrag.some((s) => { try { return n.matches(s); } catch (_) { return false; } })) return false;
      if (region.drag.some((s) => { try { return n.matches(s); } catch (_) { return false; } })) return true;
    }
    return false;
  };
  document.addEventListener('mousedown', (e) => {
    if (e.button !== 0 || e.detail > 1 || !decide(e.target)) return;
    e.preventDefault();
    invoke('plugin:window|start_dragging').catch(() => {});
  });
  document.addEventListener('dblclick', (e) => {
    if (decide(e.target)) invoke('plugin:window|internal_toggle_maximize').catch(() => {});
  });

  // 開発時だけ: 画面のエラー・警告・CSP 違反を Rust 側の標準エラーへ送る（検証用。リリースでは無効）
  if (DEBUG) {
    const report = (kind, msg) => { try { invoke('dev_report', { kind, message: String(msg).slice(0, 2000) }).catch(() => {}); } catch (_) { /* 送れなくても画面は止めない */ } };
    for (const level of ['warn', 'error']) {
      const orig = console[level].bind(console);
      console[level] = (...a) => { report(level, a.map((x) => (x && x.stack) || (typeof x === 'object' ? JSON.stringify(x) : x)).join(' ')); orig(...a); };
    }
    window.addEventListener('error', (e) => report('error', `${e.message} @${e.filename}:${e.lineno}`));
    window.addEventListener('unhandledrejection', (e) => report('unhandledrejection', (e.reason && e.reason.stack) || e.reason));
    document.addEventListener('securitypolicyviolation', (e) => report('csp', `${e.violatedDirective} ${e.blockedURI}`));
    window.addEventListener('load', () => setTimeout(() => {
      const ipc = performance.getEntriesByType('resource').filter((r) => /^ipc:|ipc\.localhost/.test(r.name)).length;
      report('info', `loaded ${location.href} cards=${document.querySelectorAll('.card').length} ipc-fetch=${ipc} drag=${region.drag.length}/${region.noDrag.length}`);
    }, 1500));
  }
})();
