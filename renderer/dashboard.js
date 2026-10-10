'use strict';
/* renderer/dashboard.js — 「ダッシュボード」（Tauri 版。window.tune.dashboard が無い Electron 版では案内だけ）
 *
 * 全機体の今（ライブの流れで 1 秒ごと）と推移（常時監視の分ごとの集計）、MDM の表（OS・稼働時間・所見・ディスク・防御）、
 * 対応が要るもの。常時監視と、どの操作卓の数字を写すか（ハブ）をここで切り替える。
 * 常時監視が集めていないとき（止めている・ハブを写している）は、見ているあいだだけライブを流す（live.js と同じ合図の作法）。
 * app.js より先に読み込み、window.KT_VIEWS に登録する。HTML へは必ず UI.esc を通して入れる。
 */
(() => {
  UI.ICON.dashboard = '<svg viewBox="0 0 20 20"><path d="M3 13.5a7 7 0 1 1 14 0"/><path d="M10 13.5l3.6-4.2"/><circle cx="10" cy="13.5" r="1.3"/></svg>';
  const T = window.tune;
  const supported = typeof T?.dashboard === 'function';
  const RANGES = [[60, '1時間'], [360, '6時間'], [1440, '24時間']];
  const SEV = { critical: ['重大', 'red'], warn: ['注意', 'orange'] };
  const FRESH = 10e3; // ライブの点をこの時間まで「今」とみなす
  const REFRESH = 30e3; // 推移・表の読み直しとライブの合図
  const dash = { minutes: 60, data: null, refresh: 0, streaming: [], raf: 0, busy: false };
  const esc = UI.esc;
  const num = (v) => v != null && v !== '' && Number.isFinite(+v);
  const r0 = (v) => Math.round(+v);
  const active = () => state.view === 'dashboard' && document.visibilityState === 'visible';

  // ---- 値の取り出し ----
  // 今の値: ライブの最新の点（10 秒以内）か、無ければ一番新しい分の平均（温度は最大）
  function nowValues(n) {
    const pts = (typeof Live !== 'undefined' && Live.store[n.id]?.points) || [];
    const p = pts.at(-1);
    if (p && Date.now() - p.t <= FRESH) return { src: 'live', t: p.t, ...p };
    const v = n.latest?.v || {};
    const a = (k, i = 0) => (Array.isArray(v[k]) ? v[k][i] : null);
    return {
      src: 'minute', t: n.latest?.minute, cpu: a('cpu'), mem: a('mem'), gpu: a('gpu'), gtemp: a('gtemp', 1), commit: a('commit'), swap: a('swap'),
      power_cpu_w: a('power_cpu_w'), power_gpu_w: a('power_gpu_w'), power_soc_w: a('power_soc_w'), power_platform_w: a('power_platform_w'),
    };
  }

  // 電力: Mac は SMC の推定（機体全体）、Windows は CPU パッケージ＋GPU（ほかの部品は入らない）
  function power(v) {
    if (num(v.power_platform_w)) return { w: v.power_platform_w, label: '機体（推定）' };
    if (num(v.power_cpu_w) || num(v.power_gpu_w)) return { w: (+v.power_cpu_w || 0) + (+v.power_gpu_w || 0), label: num(v.power_gpu_w) ? 'CPU+GPU' : 'CPU' };
    if (num(v.power_soc_w)) return { w: v.power_soc_w, label: 'SoC' };
    return null;
  }

  const level = (v, warn, crit) => (!num(v) ? 'na' : v >= crit ? 'crit' : v >= warn ? 'warn' : 'ok');
  function metric(label, v, unit, { warn = 80, crit = 92, digits = 0 } = {}) {
    const val = num(v) ? (digits ? (+v).toFixed(digits) : r0(v)) : '–';
    return `<div class="dm lv-${level(v, warn, crit)}"><div class="dm-k">${esc(label)}</div><div class="dm-v">${esc(val)}<small>${num(v) ? esc(unit) : ''}</small></div></div>`;
  }

  function liveState(n) {
    const l = n.live;
    const st = (typeof Live !== 'undefined' && Live.store[n.id]?.state) || l?.state;
    const fresh = (Date.now() - (nowValues(n).t || 0)) <= FRESH;
    if (st === 'running' && fresh) return ['running', '1 秒ごと'];
    if (st === 'connecting' || st === 'retrying') return ['connecting', 'つないでいる'];
    if (n.latest?.minute && dash.data && dash.data.at - n.latest.minute <= 3 * 60e3) return ['minute', '分ごと'];
    return ['stopped', l?.detail || '止まっている'];
  }

  // ---- 描く ----
  function tile(n) {
    const v = nowValues(n);
    const pw = power(v);
    const [st, stLabel] = liveState(n);
    const s = n.series || {};
    const pts = (k) => (s.t || []).map((t, i) => ({ t, v: s[k]?.[i] })).filter((p) => num(p.v));
    const win = dash.minutes * 60e3;
    const spark = Charts.spark([{ points: pts('cpu'), tone: 'accent' }, { points: pts('mem'), tone: 'info' }, { points: pts('gpu'), tone: 'warn' }],
      { window: win, now: dash.data.at, min: 0, max: 100, gap: (win / 60) * 2.5, height: 40, label: `${n.id} の CPU・メモリ・GPU` });
    const impl = n.live?.impl ? UI.chip(n.live.impl === 'rust' ? 'Rust' : 'スクリプト', n.live.impl === 'rust' ? 'green' : 'gray', { title: `サンプラー: ${n.live.impl}${n.live.agent ? ` ${n.live.agent}` : ''}${num(n.live.load) ? `・自身の負荷 ${n.live.load}%（1 コア比）` : ''}` }) : '';
    const top = (k, unit) => {
      const x = n.live?.[k]?.[0] || (n.latest?.v?.[k] && { name: n.latest.v[k][0], [unit]: n.latest.v[k][1] });
      return x && x.name ? `<span title="${esc(k === 'top_cpu' ? 'CPU（1 コア比）' : 'メモリ')}">${esc(x.name)} <b>${esc(num(x[unit]) ? r0(x[unit]) : '–')}</b>${k === 'top_cpu' ? '%' : 'MB'}</span>` : '';
    };
    const al = (n.alerts || []).map((a) => UI.chip(a.title || a.id, SEV[a.severity]?.[1] || 'orange', { title: a.detail || '' })).join('');
    const last = n.last || {};
    return `<div class="dtile st-${st}" data-dash="${esc(n.id)}">
      <div class="dt-top"><span class="dt-name" data-goto="${esc(n.id)}" tabindex="0">${UI.icon(n.os === 'windows' ? 'win' : 'mac')}<b>${esc(n.id)}</b></span>
        <span class="dt-tags">${n.local ? UI.chip('この機体', 'blue') : ''}${n.shared ? UI.chip('共用機', 'gray') : ''}${impl}</span>
        <span class="dt-state" title="${esc(stLabel)}"><i class="live-dot"></i>${esc(stLabel)}</span></div>
      <div class="dt-metrics">
        ${metric('CPU', v.cpu, '%', { warn: 60, crit: 85 })}${metric('メモリ', v.mem, '%')}
        ${num(v.gpu) || num(v.gtemp) ? metric('GPU', v.gpu, '%', { warn: 85, crit: 97 }) + metric('GPU 温度', v.gtemp, '℃', { warn: 75, crit: 85 }) : num(v.commit) ? metric('コミット', v.commit, '%', { warn: 80, crit: 90 }) : metric('swap', v.swap, 'GB', { warn: 4, crit: 8, digits: 1 })}
        ${pw ? `<div class="dm" title="${esc(pw.label)}"><div class="dm-k">電力</div><div class="dm-v">${esc(r0(pw.w))}<small>W</small></div></div>` : ''}
      </div>
      <div class="dt-spark">${spark}</div>
      <div class="dt-top2">${top('top_cpu', 'cpu')}${top('top_mem', 'mem_mb')}</div>
      ${al ? `<div class="dt-alerts">${al}</div>` : ''}
      <div class="dt-foot"><span>${esc(last.os || n.os)}</span>${num(last.uptime_h) ? `<span>稼働 ${esc(fmtUptime(last.uptime_h))}</span>` : ''}${num(last.score) ? `<span>スコア <b>${esc(last.score)}</b></span>` : ''}${last.at ? `<span>分析 ${esc(ago(last.at))}</span>` : ''}</div>
    </div>`;
  }

  const fmtUptime = (h) => (h >= 48 ? `${Math.round(h / 24)}日` : `${Math.round(h)}時間`);

  function mdmTable(d) {
    const yes = (b, okText, ngText, ngTone = 'orange') => (b == null ? '<span class="muted">–</span>' : b ? UI.chip(okText, 'green') : UI.chip(ngText, ngTone));
    return UI.table({
      cols: [{ label: '機体' }, { label: 'OS' }, { label: '稼働', cls: 'n' }, { label: 'スコア', cls: 'n' }, { label: '所見' }, { label: '空きが最小のディスク' }, { label: '防御' }, { label: '電源' }, { label: '7 日の停止', cls: 'n' }, { label: 'サンプラー' }, { label: '最終分析' }],
      rows: d.nodes,
      cls: 'scroll-x dash-mdm',
      row: (n) => {
        const l = n.last || {};
        const disk = l.disk_low ? `${esc(l.disk_low.mount)} ${UI.chip(`${r0(l.disk_low.free_pct)}%`, l.disk_low.free_pct < 10 ? 'red' : l.disk_low.free_pct < 20 ? 'orange' : 'default')}` : '<span class="muted">–</span>';
        const counts = [l.critical ? UI.chip(`重大 ${l.critical}`, 'red') : '', l.warn ? UI.chip(`注意 ${l.warn}`, 'orange') : ''].join('') || (l.at ? UI.chip('良好', 'green') : '<span class="muted">–</span>');
        const stops = num(l.bugchecks_7d) ? (l.bugchecks_7d > 0 ? UI.chip(String(l.bugchecks_7d), 'red') : '0') : '<span class="muted">–</span>';
        return `<td class="nowrap"><b class="link" data-goto="${esc(n.id)}" tabindex="0">${esc(n.id)}</b></td><td class="nowrap">${esc(l.os || n.os)}</td>`
          + `<td class="n num">${num(l.uptime_h) ? esc(fmtUptime(l.uptime_h)) : '–'}</td><td class="n num">${num(l.score) ? esc(l.score) : '–'}</td><td>${counts}</td><td class="nowrap">${disk}</td>`
          + `<td>${n.os === 'windows' ? yes(l.defender_realtime, 'Defender 有効', 'リアルタイム保護 off') : '<span class="muted">セキュリティ画面</span>'}</td>`
          + `<td class="nowrap">${esc(l.power_plan || '–')}</td><td class="n num">${stops}</td>`
          + `<td>${n.live?.impl ? esc(n.live.impl === 'rust' ? `Rust ${n.live.agent || ''}` : 'スクリプト') : '<span class="muted">–</span>'}</td><td class="nowrap muted">${l.at ? esc(ago(l.at)) : '–'}</td>`;
      },
    });
  }

  function alertsList(d) {
    const rows = d.nodes.flatMap((n) => (n.alerts || []).map((a) => ({ ...a, node: n.id }))).sort((a, b) => (a.severity === 'critical' ? 0 : 1) - (b.severity === 'critical' ? 0 : 1));
    return UI.table({
      cols: [{ label: '' }, { label: '機体' }, { label: '内容' }, { label: '根拠' }],
      rows, empty: '対応が要るものはありません', cls: 'scroll-x',
      row: (a) => `<td>${UI.chip(SEV[a.severity]?.[0] || '注意', SEV[a.severity]?.[1] || 'orange')}</td><td class="nowrap"><b class="link" data-goto="${esc(a.node)}" tabindex="0">${esc(a.node)}</b></td><td>${esc(a.title || a.id)}</td><td class="muted">${esc(a.detail || '')}</td>`,
    });
  }

  function sourceText(d) {
    const p = d.hub_pull || {};
    if (d.collecting) return `この機体で集めている（全機体のサンプラーを流し続け、1 分ごとに集計を保存）`;
    if (d.monitor.hub) {
      const res = p.at ? (p.ok ? `${ago(p.at)}に写した${p.fresh === false ? '（ハブの数字が古い）' : ''}` : `写せなかった（${ago(p.at)}）: ${p.error || ''}`) : 'まだ写していない';
      return `${d.monitor.hub} の数字を写している — ${res}`;
    }
    return '常時監視は止めている（この画面を見ているあいだだけ 1 秒ごとに流す）';
  }

  function controls(d) {
    const hubs = [['', 'この機体で集める'], ...state.nodes.filter((n) => !n.local && (n.os === 'windows' || n.os === 'macos')).map((n) => [n.id, `${n.id} を写す`])];
    return `${UI.seg({ attr: 'drange', options: RANGES, value: dash.minutes })}
      ${UI.select({ id: 'dashHub', label: '数字の出どころ', options: hubs, value: d.monitor.hub || '', active: !!d.monitor.hub })}
      <button class="btn${d.monitor.enabled ? ' on' : ''}" id="dashMonitor" aria-pressed="${d.monitor.enabled}" title="アプリが開いているあいだ（閉じて常駐しているあいだも）全機体の数字を集め続ける">${UI.icon('gauge')}常時監視 ${d.monitor.enabled ? 'オン' : 'オフ'}</button>
      ${d.monitor.hub ? `<button class="btn" id="dashPull" title="ハブから今すぐ写す">${UI.icon('sync')}今すぐ写す</button>` : ''}`;
  }

  function head(d) {
    const nodes = d?.nodes || [];
    const live = nodes.filter((n) => ['running', 'minute'].includes(liveState(n)[0])).length;
    const alerts = nodes.reduce((s, n) => s + (n.alerts || []).length, 0);
    const sch = d?.schedule || {};
    return UI.head({
      icon: 'dashboard', title: 'ダッシュボード',
      desc: '全機体の今と推移。数字は 1 秒ごと（ライブ）と 1 分ごとの集計（14 日保存）、詳細な分析とログは 1 時間ごと。Mac・Windows のどの操作卓からでも、ハブを選べば同じ数字を見られます。この画面から機体は変えません。',
      actions: d ? controls(d) : '',
      props: d ? UI.props([
        ['届いている', `<b>${live}</b><span class="faint"> / ${nodes.length} 台</span>`, 'machine'],
        ['対応が要るもの', alerts ? `<b class="t-crit">${alerts}</b>` : '<span class="faint">なし</span>', 'alert'],
        ['数字の出どころ', `<span class="faint">${esc(sourceText(d))}</span>`, 'sync'],
        ['詳細の間隔', `<span class="faint">分析 ${esc(sch.probe_minutes ?? '–')} 分・ログ ${esc(sch.logs_minutes ?? '–')} 分ごと</span>`, 'history'],
      ]) : '',
    });
  }

  function draw() {
    const d = dash.data;
    page(`${head(d)}
      ${UI.section('機体', '大きな数字は今（ライブ）、線は CPU（青）・メモリ（水色）・GPU（橙）の推移')}
      <div class="dgrid">${d.nodes.map(tile).join('')}</div>
      ${UI.section('対応が要るもの', '直近 6 分の集計（続いている高負荷・メモリ・温度・数字が届かない）と、最新の分析の重大な所見')}
      ${alertsList(d)}
      ${UI.section('機体の一覧', 'MDM の表。OS・稼働時間・所見・ディスク・防御・電源・停止の回数（最新の分析から）')}
      ${mdmTable(d)}`);
    bind();
  }

  // ライブの点が届いたら、タイルの数字と状態だけを描き直す（推移の線と表は 30 秒ごとの読み直しで）
  function redrawLive() {
    dash.raf = 0;
    if (!active() || !dash.data) return;
    for (const n of dash.data.nodes) {
      const el = document.querySelector(`[data-dash="${CSS.escape(n.id)}"]`);
      if (!el) continue;
      const fresh = document.createElement('div');
      fresh.innerHTML = tile(n);
      const nt = fresh.firstElementChild;
      el.className = nt.className;
      for (const sel of ['.dt-metrics', '.dt-state', '.dt-top2']) {
        const a = el.querySelector(sel), b = nt.querySelector(sel);
        if (a && b && a.innerHTML !== b.innerHTML) a.innerHTML = b.innerHTML;
      }
    }
  }

  function bind() {
    const root = $('#page');
    root.querySelectorAll('[data-drange]').forEach((b) => { b.onclick = () => { dash.minutes = +b.dataset.drange; load(); }; });
    const hub = $('#dashHub');
    if (hub) hub.onchange = () => settings({ hub: hub.value || null });
    const mon = $('#dashMonitor');
    if (mon) mon.onclick = () => settings({ enabled: !dash.data.monitor.enabled });
    const pull = $('#dashPull');
    if (pull) pull.onclick = async () => { pull.disabled = true; await T.hubPull().catch((e) => toast(`写せなかった: ${e?.message || e}`)); load(); };
    bindGoto(root);
  }

  async function settings(patch) {
    try {
      await T.monitorSettings(patch);
    } catch (e) {
      toast(String(e?.message || e));
    }
    load();
  }

  // ---- ライブ（常時監視が集めていないときだけ、見ているあいだ流す）。合図は 30 秒ごとの読み直し（load）で送る ----
  function syncLive() {
    const want = active() && dash.data && !dash.data.collecting ? dash.data.nodes.map((n) => n.id) : [];
    const stop = dash.streaming.filter((id) => !want.includes(id));
    if (stop.length) T.liveStop(stop).catch(() => {});
    dash.streaming = want;
    if (want.length) T.liveStart(want).catch(() => {});
  }

  async function load() {
    if (dash.busy) return;
    dash.busy = true;
    const t = ticket();
    try {
      const d = await T.dashboard(dash.minutes).catch((e) => ({ error: String(e?.message || e) }));
      if (stale(t) || state.view !== 'dashboard') return;
      if (d.error) {
        page(`${head(null)}${UI.callout({ tone: 'red', icon: 'alert', title: '読めません', body: `<div class="err">${esc(d.error)}</div>` })}`);
        return;
      }
      dash.data = d;
      draw();
      syncLive();
    } finally {
      dash.busy = false;
    }
  }

  function render() {
    setCrumbs([{ icon: 'dashboard', label: 'ダッシュボード' }]);
    if (!supported) {
      page(`${head(null)}${UI.callout({ tone: 'gray', icon: 'info', title: 'Tauri 版で表示します', body: '常時監視とハブとの同期は Rust の分析エンジン（Tauri 版）が持ちます。' })}`);
      return;
    }
    if (dash.data) draw();
    load();
    clearInterval(dash.refresh);
    dash.refresh = setInterval(() => {
      if (state.view !== 'dashboard') { clearInterval(dash.refresh); dash.refresh = 0; syncLive(); return; }
      if (active()) load();
    }, REFRESH);
  }

  if (supported) {
    T.onLive(() => { if (!dash.raf && active()) dash.raf = requestAnimationFrame(redrawLive); });
    if (typeof T.onMonitor === 'function') T.onMonitor(() => { if (active() && !dash.busy) load(); });
    document.addEventListener('visibilitychange', () => { if (state.view === 'dashboard') (active() ? load() : syncLive()); });
  }

  (window.KT_VIEWS = window.KT_VIEWS || []).push({ key: 'dashboard', label: 'ダッシュボード', icon: 'dashboard', render, first: true, home: supported, leave: () => syncLive() });
})();
