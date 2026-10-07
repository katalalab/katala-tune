'use strict';
/* renderer/live.js — ライブ表示（Tauri 版だけ。window.tune.liveStart が無い Electron 版では何も出さない）
 *
 * 画面が見ているあいだだけ流す:
 *   - 「ライブ」を入れた画面（リソース・機体）を開いているあいだ liveStart(ids) を呼び、30 秒ごとに呼び直して合図にする
 *   - 画面を移る・ウィンドウが隠れる・切り替えを切ると liveStop(ids)
 *   - Rust 側は合図が 2 分途切れると自動で止める（ここが止め損ねても機体に残らない）
 *   - 接続切れ・自動停止で止まった機体は勝手につなぎ直さない（「再開」を押したときだけ）
 * 点は onLive のイベントで受け取り、描く時間（直近 3 分）の分だけ持つ。元データは Rust 側にある。
 *
 *   Live.supported                     使えるか
 *   Live.toggle(kind)                  ページ見出しに置く「ライブ」の切り替え（kind: 'resources' | 'node'）
 *   Live.panel(kind)                   中身を描く入れ物
 *   Live.mount(kind, ids, nodes)       ページを描いた後に呼ぶ: 切り替えを結び、流すものを合わせ、中身を描く
 *   Live.route(view)                   画面を移るときに呼ぶ（新しい画面で使わないものは止める）
 *   Live.merge(store, ev, now)         イベントを store に足し、変わった id を返す（テスト用に公開）
 *   Live.rate(bps)                     「1.2 MB/s」の形
 */
const Live = (() => {
  const T = typeof window !== 'undefined' ? window.tune : null;
  const supported = !!T && typeof T.liveStart === 'function' && typeof T.liveStop === 'function' && typeof T.onLive === 'function';
  const WINDOW = 180e3; // 描く時間
  const KEEP = 300e3; // 持っておく時間
  const HEARTBEAT = 30e3;
  // メーターのしきい値（app.js の TH と同じ）
  const TH = { cpu: { warn: 60, crit: 85 }, mem: { warn: 80, crit: 90 } };
  const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const num = (v) => v != null && v !== '' && Number.isFinite(+v);

  // ---- 受け取ったイベントを貯める（DOM に触らない） ----
  const blank = () => ({ points: [], cores: [], mem: null, gpus: null, procs: null, info: null, state: 'stopped', reason: null, detail: null, attempt: 0, load: null, rss_mb: null });

  function merge(store, ev, now = Date.now()) {
    const changed = [];
    for (const [id, n] of Object.entries(ev?.nodes || {})) {
      const s = store[id] || (store[id] = blank());
      for (const k of ['state', 'reason', 'detail', 'attempt', 'load', 'rss_mb']) if (k in n) s[k] = n[k];
      for (const k of ['cores', 'mem', 'gpus', 'procs', 'info']) if (n[k] != null) s[k] = n[k];
      if (Array.isArray(n.points) && n.points.length) {
        // 開始したときの全体（full）は重ねる。どちらも時刻で並べ、同じ時刻は 1 つにする
        const byT = new Map((n.full ? [...s.points, ...n.points] : s.points.concat(n.points.filter((p) => p.t > (s.points.at(-1)?.t ?? -Infinity)))).map((p) => [p.t, p]));
        s.points = [...byT.values()].sort((a, b) => a.t - b.t);
      }
      s.points = s.points.filter((p) => p.t >= now - KEEP);
      changed.push(id);
    }
    return changed;
  }

  function rate(v) {
    if (!num(v)) return '–';
    const u = ['B/s', 'KB/s', 'MB/s', 'GB/s'];
    let x = +v, i = 0;
    while (x >= 1000 && i < u.length - 1) { x /= 1000; i++; }
    return `${x >= 100 || i === 0 ? Math.round(x) : x.toFixed(1)} ${u[i]}`;
  }
  const pctText = (v) => (num(v) ? `${Math.round(v)}%` : '–');
  const median = (xs) => { const a = xs.filter(num).sort((p, q) => p - q); return a.length ? a[Math.floor(a.length / 2)] : null; };

  // ---- 状態 ----
  const store = {};
  const want = { resources: false, node: false };
  let cur = { key: null, kind: null, ids: [], nodes: [] };
  const streaming = new Set();
  let maxStreams = 6;
  let hb = null;
  const visible = () => typeof document === 'undefined' || document.visibilityState !== 'hidden';
  const viewKey = (view) => (view === 'resources' ? 'resources' : String(view || ''));

  const STOP_LABEL = {
    user: '停止', idle: '自動停止', hidden: '停止', disconnected: '接続切れ', limit: '上限のため止めている', app_exit: '終了', unknown: '台帳に無い',
  };

  function stateChip(s) {
    if (s.state === 'running') return UI.chip('ライブ', 'green', { dot: true, cls: 'live-on' });
    if (s.state === 'connecting') return UI.chip('接続中…', 'blue', { dot: true, cls: 'live-wait' });
    if (s.state === 'retrying') return UI.chip(`つなぎ直し中 ${s.attempt}`, 'orange', { dot: true, cls: 'live-wait', title: s.detail || '' });
    const label = STOP_LABEL[s.reason] || '停止';
    return UI.chip(label, s.reason === 'disconnected' ? 'red' : 'gray', { dot: true, title: s.detail || '' });
  }

  // 機体の時刻と受け取った時刻の差。数秒を超えるのは届くのが遅いのではなく、機体の時計がずれている
  // （実機で 18 秒ずれた Windows 機があった。届く間隔は 1 秒ちょうどのまま）
  function lagHtml(lag) {
    if (!num(lag)) return '';
    if (lag > 3000 || lag < -1000) return `<span title="機体の時計とこの機体の時計の差。届くまでの時間は測れない">時計のずれ 約 ${Math.round(lag / 1000)} 秒</span>`;
    return `<span title="機体で測ってから届くまで（機体の時計が合っているとき）">遅延 ${Math.max(0, Math.round(lag))} ms</span>`;
  }

  // ---- 描く ----
  function row(label, series, opts, value, sub = '') {
    return `<div class="lv-row"><span class="lv-k">${esc(label)}</span><div class="lv-spark">${Charts.spark(series, opts)}</div>`
      + `<span class="lv-v">${value}${sub ? `<small>${sub}</small>` : ''}</span></div>`;
  }

  function cardBody(id, big) {
    const s = store[id] || blank();
    const n = cur.nodes.find((x) => x.id === id) || { id };
    const pts = s.points;
    const now = Math.max(Date.now(), pts.at(-1)?.t || 0);
    const last = pts.at(-1) || {};
    // 描く幅は直近 3 分。流し始めは 1 分から広げていく（右端が今）
    const span = Math.min(WINDOW, Math.max(60e3, now - (pts[0]?.t ?? now)));
    const opt = (extra) => ({ window: span, now, height: big ? 56 : 34, width: big ? 420 : 260, ...extra });
    const ser = (k, tone) => ({ points: pts.map((p) => ({ t: p.t, v: p[k] })), tone });
    const lvTone = (v, th) => (num(v) && v >= th.crit ? 'crit' : num(v) && v >= th.warn ? 'warn' : 'info');
    const mem = s.mem || {};
    const win = n.os === 'windows' || s.info?.os === 'windows';
    const memSub = win
      ? (num(mem.commit_pct) ? `コミット ${Math.round(mem.commit_pct)}%${big && num(mem.commit_gb) ? `（${mem.commit_gb.toFixed(1)} / ${(mem.commit_limit_gb ?? 0).toFixed(1)} GB）` : ''}` : '')
      : (num(mem.swap_used_gb) ? `swap ${mem.swap_used_gb.toFixed(1)} GB${mem.pressure && mem.pressure !== 'normal' ? ` · 圧迫 ${esc(mem.pressure)}` : ''}` : '');
    const gpu = (s.gpus || [])[0];
    const head = `<div class="lv-head">${big ? '' : `<span class="lv-name">${UI.icon(n.os === 'windows' ? 'win' : 'mac')}<b>${esc(id)}</b></span>`}${stateChip(s)}`
      + `<span class="lv-meta">${pts.length ? `${esc(Math.round(Math.min(WINDOW, now - pts[0].t) / 1000))} 秒分` : ''}</span></div>`;
    const stopped = s.state === 'stopped' && s.reason && s.reason !== 'user' && s.reason !== 'hidden';
    const note = stopped || s.state === 'retrying'
      ? `<div class="lv-note ${s.reason === 'disconnected' ? 'bad' : ''}"><span>${esc(s.detail || '')}</span>${stopped && s.reason !== 'unknown' ? `<button class="btn small" data-live-resume="${esc(id)}">${UI.icon('play')}再開</button>` : ''}</div>` : '';
    if (!pts.length) {
      return `${head}${note}${s.state === 'connecting' || s.state === 'retrying' ? '<div class="lv-empty">最初のサンプルを待っています…</div>' : note ? '' : '<div class="lv-empty">まだサンプルがありません</div>'}`;
    }
    const rows = [
      row('CPU', [ser('cpu', lvTone(last.cpu, TH.cpu))], opt({ min: 0, max: 100, label: 'CPU 使用率' }), pctText(last.cpu)),
      s.cores?.length ? `<div class="lv-cores">${Charts.bars(s.cores, { ...TH.cpu, label: 'コアごとの CPU' })}</div>` : '',
      row('メモリ', [ser('mem', lvTone(last.mem, TH.mem))], opt({ min: 0, max: 100, label: 'メモリ使用率' }), pctText(last.mem), memSub),
      row('ディスク', [ser('dr', 'info'), ser('dw', 'alt')], opt({ label: 'ディスクの読み書き', floor: 1e6 }), `<span class="rd">読 ${rate(last.dr)}</span><span class="wr">書 ${rate(last.dw)}</span>`),
      row('ネット', [ser('rx', 'info'), ser('tx', 'alt')], opt({ label: 'ネットワークの送受信', floor: 1e5 }), `<span class="rd">受 ${rate(last.rx)}</span><span class="wr">送 ${rate(last.tx)}</span>`),
      gpu || pts.some((p) => num(p.gpu))
        ? row('GPU', [ser('gpu', 'info')], opt({ min: 0, max: 100, label: 'GPU 使用率' }), pctText(pts.filter((p) => num(p.gpu)).at(-1)?.gpu),
          gpu && num(gpu.mem_total_mb) ? `${big ? 'メモリ ' : ''}${(gpu.mem_used_mb / 1024).toFixed(1)} / ${(gpu.mem_total_mb / 1024).toFixed(1)} GB` : '')
        : '',
    ].join('');
    const lag = median(pts.slice(-30).map((p) => p.lag));
    const foot = `<div class="lv-foot">${num(s.load) ? `<span title="このサンプラー自身の CPU（1 コア換算）。macOS は ps の分を含む">サンプラー ${s.load < 1 ? s.load.toFixed(2) : s.load.toFixed(1)}%${num(s.rss_mb) ? ` · ${Math.round(s.rss_mb)} MB` : ''}</span>` : ''}`
      + `${lagHtml(lag)}<span class="right">${esc(new Date(last.t).toLocaleTimeString('ja-JP'))}</span></div>`;
    return `${head}${note}<div class="lv-rows">${rows}</div>${foot}`;
  }

  function procsHtml(s) {
    const p = s.procs;
    if (!p) return '';
    const tbl = (title, list, key) => `<div class="lv-procs"><div class="lv-ptitle">${esc(title)}</div>${UI.table({
      cols: [{ label: '名前' }, { label: 'CPU', cls: 'n' }, { label: 'メモリ', cls: 'n' }],
      rows: list || [],
      empty: 'なし',
      row: (x) => `<td><span class="lv-pname">${esc(x.name)}</span>${x.app && x.app !== x.name ? ` <span class="muted">· ${esc(x.app)}</span>` : ''}<span class="faint"> ${esc(x.pid)}</span></td>`
        + `<td class="n${key === 'cpu' ? ' b' : ''}">${num(x.cpu) ? `${(+x.cpu).toFixed(0)}%` : '–'}</td><td class="n${key === 'mem' ? ' b' : ''}">${num(x.mem_mb) ? (x.mem_mb >= 1024 ? `${(x.mem_mb / 1024).toFixed(1)} GB` : `${Math.round(x.mem_mb)} MB`) : '–'}</td>`,
    })}</div>`;
    return `<div class="lv-procgrid">${tbl('CPU 上位（1 コア換算）', p.top_cpu, 'cpu')}${tbl('メモリ上位', p.top_mem, 'mem')}</div>`
      + `<p class="note-line lv-pnote">全 ${esc(p.count ?? '-')} プロセス · ${esc(new Date(p.at || Date.now()).toLocaleTimeString('ja-JP'))} 時点（5 秒ごと）</p>`;
  }

  function draw(ids) {
    const root = typeof document !== 'undefined' && document.getElementById('livePanel');
    if (!root || !cur.kind) return;
    if (!want[cur.kind]) { root.innerHTML = ''; root.hidden = true; return; }
    root.hidden = false;
    const big = cur.kind === 'node';
    if (!root.querySelector('.lv-grid')) {
      const skipped = cur.kind === 'resources' ? cur.nodes.filter((n) => n.shared) : [];
      root.innerHTML = `<div class="lv-grid${big ? ' big' : ''}">${cur.ids.map((id) => `<div class="lv-card" data-live="${esc(id)}"><div class="lv-main"></div>${big ? '<div class="lv-pwrap"></div>' : ''}</div>`).join('')}</div>`
        + `<p class="note-line">直近 ${WINDOW / 60000} 分を 1 秒ごとに更新します（見ているあいだだけ。上位プロセスは 5 秒ごと）。同時に流せるのは ${maxStreams} 台まで。`
        + `${skipped.length ? `共用機（${skipped.map((n) => esc(n.id)).join('、')}）は機体の画面から流せます。` : ''}</p>`;
      ids = cur.ids;
    }
    for (const id of ids || cur.ids) {
      const el = root.querySelector(`[data-live="${CSS.escape(id)}"]`);
      if (!el) continue;
      el.querySelector('.lv-main').innerHTML = cardBody(id, big);
      // 上位プロセスの表は 5 秒ごとにしか変わらないので、変わったときだけ描き直す
      const pw = el.querySelector('.lv-pwrap');
      const at = String(store[id]?.procs?.at ?? '');
      if (pw && pw.dataset.at !== at) { pw.dataset.at = at; pw.innerHTML = procsHtml(store[id] || blank()); }
    }
    root.querySelectorAll('[data-live-resume]').forEach((b) => { b.onclick = () => resume(b.dataset.liveResume); });
  }

  let pending = new Set();
  let raf = 0;
  function schedule(ids) {
    ids.forEach((id) => pending.add(id));
    if (raf || !visible()) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      const ids2 = [...pending].filter((id) => cur.ids.includes(id));
      pending = new Set();
      if (ids2.length) draw(ids2);
    });
  }

  // ---- 流すものを合わせる ----
  function onStart(r) {
    if (!r) return;
    if (r.limit) maxStreams = r.limit;
    merge(store, { nodes: r.snapshot || {} });
    for (const x of r.refused || []) {
      streaming.delete(x.id);
      Object.assign(store[x.id] || (store[x.id] = blank()), { state: 'stopped', reason: x.reason, detail: x.detail });
    }
    schedule([...Object.keys(r.snapshot || {}), ...(r.refused || []).map((x) => x.id)]);
  }

  async function sync() {
    if (!supported) return;
    const target = cur.kind && want[cur.kind] && visible() ? cur.ids : [];
    const stop = [...streaming].filter((id) => !target.includes(id));
    const start = target.filter((id) => !streaming.has(id) && !store[id]?.held);
    stop.forEach((id) => streaming.delete(id));
    start.forEach((id) => streaming.add(id));
    if (stop.length) await T.liveStop(stop).catch(() => {});
    if (start.length) onStart(await T.liveStart(start).catch(() => null));
    clearInterval(hb);
    hb = streaming.size ? setInterval(beat, HEARTBEAT) : null;
  }

  function beat() {
    if (!streaming.size || !visible()) return;
    T.liveStart([...streaming]).then(onStart).catch(() => {});
  }

  function resume(id) {
    if (store[id]) store[id].held = false;
    streaming.delete(id);
    sync();
  }

  function stopAll() {
    if (!supported || !streaming.size) return;
    const ids = [...streaming];
    streaming.clear();
    clearInterval(hb);
    hb = null;
    T.liveStop(ids).catch(() => {});
  }

  if (supported) {
    T.onLive((ev) => {
      const ids = merge(store, ev);
      for (const id of ids) {
        const s = store[id];
        // 接続切れ・自動停止・上限で止まったものは、合図で勝手につなぎ直さない（「再開」で流す）
        if (s.state === 'stopped' && s.reason && !['user', 'hidden'].includes(s.reason)) { streaming.delete(id); s.held = true; }
      }
      schedule(ids);
    });
    document.addEventListener('visibilitychange', () => {
      if (!cur.kind || !want[cur.kind]) return;
      if (visible()) { sync(); draw(); } else stopAll();
    });
  }

  // ---- app.js から呼ぶもの ----
  function toggle(kind) {
    if (!supported) return '';
    const on = !!want[kind];
    return `<button class="btn live-toggle${on ? ' on' : ''}" data-live-toggle="${esc(kind)}" aria-pressed="${on}" title="${on ? 'ライブ表示を止める' : '1 秒ごとの値を流す（見ているあいだだけ）'}"><span class="live-dot" aria-hidden="true"></span>ライブ</button>`;
  }

  const panel = () => (supported ? '<section class="live-panel" id="livePanel" aria-label="ライブ" hidden></section>' : '');

  function mount(kind, ids, nodes) {
    if (!supported) return;
    const key = kind === 'node' ? String(ids[0]) : 'resources';
    const moved = cur.key !== (kind === 'node' ? `node:${key}` : key);
    cur = { key: kind === 'node' ? `node:${key}` : key, kind, ids: [...ids], nodes: nodes || [] };
    document.querySelectorAll('[data-live-toggle]').forEach((b) => {
      b.onclick = () => {
        want[kind] = !want[kind];
        if (want[kind]) for (const id of cur.ids) if (store[id]) store[id].held = false;
        b.classList.toggle('on', want[kind]);
        b.setAttribute('aria-pressed', String(want[kind]));
        const root = document.getElementById('livePanel');
        if (root) root.innerHTML = '';
        sync();
        draw();
      };
    });
    if (moved) for (const id of cur.ids) if (store[id]) store[id].held = false;
    sync();
    draw();
  }

  function route(view) {
    if (!supported) return;
    if (viewKey(view) !== cur.key) {
      stopAll();
      cur = { key: null, kind: null, ids: [], nodes: [] };
    }
  }

  return { supported, toggle, panel, mount, route, merge, rate, store };
})();

if (typeof module !== 'undefined' && module.exports) module.exports = Live;
