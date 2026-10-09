'use strict';
/* renderer/agents.js — 「AI」の画面（Claude Code・Codex のセッションの要約）
 *
 * app.js より先に読み込み、window.KT_VIEWS に画面を登録する（app.js がサイドバーと描画に足す）。
 * 集計は Rust 側（tune-core）で行い、ここには集計結果（aiSummary）とページングした一覧（aiSessions）だけが来る。
 * Electron 版は取り込まないので、aiSummary が無いか { unsupported } を返したら、その旨だけを出す。
 * HTML へは必ず UI.esc を通して入れる。
 */
(() => {
  UI.ICON.agent = '<svg viewBox="0 0 20 20"><rect x="3.5" y="5.5" width="13" height="10" rx="3"/><path d="M10 2.75v2.75M7.5 10h.01M12.5 10h.01M7.75 13h4.5"/></svg>';

  const ai = { days: 30, tool: '', node: '', list: { q: '', kind: '', sort: 'last', errors: false, offset: 0, limit: 30 }, syncing: false };
  const TOOL = { claude: 'Claude Code', codex: 'Codex' };
  const TOOL_TONE = { claude: 'orange', codex: 'blue' };
  const MINUTES = [15, 30, 60];

  // 数の表示（万・億）
  function fmtN(n) {
    if (n == null || !Number.isFinite(+n)) return '-';
    const a = Math.abs(n);
    if (a >= 1e8) return `${(n / 1e8).toFixed(a >= 1e9 ? 0 : 1)}億`;
    if (a >= 1e4) return `${(n / 1e4).toFixed(a >= 1e5 ? 0 : 1)}万`;
    return Math.round(n).toLocaleString('ja-JP');
  }
  const fmtBytes = (b) => (b == null ? '-' : b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : b >= 1e6 ? `${(b / 1e6).toFixed(0)} MB` : `${Math.max(0, Math.round(b / 1e3))} KB`);
  function fmtDur(ms) {
    if (ms == null || ms < 0) return '-';
    const m = Math.round(ms / 60000);
    if (m < 60) return `${m}分`;
    const h = Math.floor(m / 60);
    return h < 48 ? `${h}時間${m % 60 ? `${m % 60}分` : ''}` : `${Math.floor(h / 24)}日${h % 24 ? `${h % 24}時間` : ''}`;
  }
  const io = (x) => (x.tok_in || 0) + (x.tok_out || 0);
  const toolChip = (t) => UI.chip(TOOL[t] || t, TOOL_TONE[t] || 'default');
  function prLabel(u) {
    const m = /^https:\/\/github\.com\/([^/]+\/[^/]+)\/pull\/(\d+)/.exec(u || '');
    return m ? `${m[1]} #${m[2]}` : u;
  }
  // 版の新しさ（数字の並びで比べる）
  function newer(a, b) {
    const p = (v) => String(v).split(/[^0-9]+/).filter(Boolean).map(Number);
    const x = p(a), y = p(b);
    for (let i = 0; i < Math.max(x.length, y.length); i++) if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) > (y[i] || 0);
    return false;
  }

  const filter = () => ({ days: ai.days, tz: -new Date().getTimezoneOffset(), node_id: ai.node || undefined, tool: ai.tool || undefined });

  function usageHtml(s) {
    // 期間の日の並び（Rust が返す day は tz の 0 時）
    const DAY = 86400e3;
    const days = Array.from({ length: s.days }, (_, i) => s.since + i * DAY);
    const by = new Map();
    for (const r of s.daily) {
      const k = r.day;
      const v = by.get(k) || { in: 0, out: 0, sessions: 0, nodes: {} };
      v.in += r.tok_in || 0; v.out += r.tok_out || 0; v.sessions += r.sessions || 0;
      v.nodes[r.node_id] = r;
      by.set(k, v);
    }
    const max = Math.max(0, ...[...by.values()].map((v) => v.in + v.out));
    const [unit, uname] = max >= 1e7 ? [1e6, '百万'] : max >= 1e4 ? [1e3, '千'] : [1, ''];
    const buckets = days.map((d) => { const v = by.get(d); return { t0: d, t1: d + DAY, values: { in: Math.round((v?.in || 0) / unit), out: Math.round((v?.out || 0) / unit) } }; });
    const md = (t) => { const x = new Date(t); return `${x.getMonth() + 1}/${x.getDate()}`; };
    const every = s.days <= 7 ? 1 : s.days <= 31 ? 5 : 15;
    const cols = Charts.columns(buckets, {
      series: [{ key: 'in', label: '入力', tone: 'info' }, { key: 'out', label: '出力', tone: 'accent' }],
      label: (b, i) => (i % every === 0 ? md(b.t0) : ''),
      tip: (b) => `${md(b.t0)}（${uname}トークン）`,
      title: 'トークンの推移',
    });
    const nodes = state.nodes.filter((n) => !ai.node || n.id === ai.node);
    const hm = Charts.heatmap({
      rows: nodes.map((n) => ({ key: n.id, label: n.id })),
      cols: days.map((d, i) => ({ key: d, label: s.days <= 7 || i % every === 0 ? md(d) : '' })),
      tone: 'accent',
      // 凡例の最大値が読めるよう、左のグラフと同じ単位（小数1桁）にする
      value: (r, c) => { const x = by.get(c.key)?.nodes[r.key]; return x ? Math.round((io(x) / unit) * 10) / 10 : 0; },
      title: (r, c) => { const x = by.get(c.key)?.nodes[r.key]; return `${r.label} ${md(c.key)}: ${x ? `入出力 ${fmtN(io(x))} トークン・${x.sessions} セッション・指示 ${x.prompts}` : 'なし'}`; },
    });
    return `<div class="charts">${box('トークンの推移', `入力＋出力（${uname || ''}トークン）。キャッシュは含まない`, cols)}${box('機体×日', `入力＋出力（${uname || ''}トークン）。セルにカーソルでセッション数`, hm)}</div>`;
  }

  function modelsHtml(s) {
    const max = Math.max(1, ...s.models.map(io));
    return UI.table({
      cols: [{ label: 'モデル' }, { label: 'セッション', cls: 'n' }, { label: '入力＋出力', cls: 'n', width: '38%' }],
      rows: s.models,
      empty: 'この期間の使用はありません',
      row: (m) => `<td><span class="ai-model">${UI.esc(m.model || '（不明）')}</span>${toolChip(m.tool)}</td><td class="n">${UI.esc(m.sessions)}</td>`
        + `<td class="n">${UI.esc(fmtN(io(m)))}${Charts.dataBar(io(m), max, { tone: 'accent' })}<span class="sub">キャッシュ読み ${UI.esc(fmtN(m.tok_cache_read))}</span></td>`,
    });
  }

  function toolsHtml(s) {
    const max = Math.max(1, ...s.tools.map((t) => t.errors));
    return UI.table({
      cols: [{ label: 'ツール' }, { label: '失敗', cls: 'n', width: '30%' }, { label: '呼び出し', cls: 'n' }, { label: 'セッション', cls: 'n' }],
      rows: s.tools.slice(0, 12),
      empty: 'この期間に失敗したツールはありません',
      row: (t) => {
        const rate = t.calls ? (t.errors / t.calls) * 100 : null;
        return `<td class="wrap-any">${wbr(t.name)}</td><td class="n">${UI.esc(fmtN(t.errors))}${Charts.dataBar(t.errors, max, { tone: 'crit' })}</td><td class="n muted">${UI.esc(fmtN(t.calls))}<span class="sub${rate >= 10 ? ' sc-warn' : ''}">${rate == null ? '' : `失敗 ${rate.toFixed(rate < 10 ? 1 : 0)}%`}</span></td>`
          + `<td class="n muted">${UI.esc(fmtN(t.sessions))}</td>`;
      },
    });
  }

  function sessionRow(x) {
    const errs = (x.tool_errors || 0) + (x.turn_errors || 0) + (x.api_errors || 0) + (x.hook_errors || 0);
    const prs = Array.isArray(x.prs) ? x.prs : [];
    return `<td class="muted nowrap">${UI.esc(fmtTime(x.last_ts))}</td><td class="nowrap"><b>${UI.esc(x.node_id)}</b></td>`
      + `<td class="nowrap">${toolChip(x.tool)}${x.parent_id ? ` ${UI.chip('子', 'gray', { title: 'サブエージェント' })}` : ''}</td>`
      + `<td class="wrap-any"><span class="ai-cwd">${wbr(x.cwd || '-')}</span><span class="sub">${UI.esc(x.model || '')}${x.version ? ` · ${UI.esc(x.version)}` : ''}</span></td>`
      + `<td class="n">${UI.esc(fmtDur(x.duration_ms))}</td><td class="n">${UI.esc(fmtN(x.prompts))}</td>`
      + `<td class="n">${UI.esc(fmtN(x.tool_calls))}${errs ? `<span class="sub sc-warn">失敗 ${UI.esc(errs)}</span>` : ''}</td>`
      + `<td class="n">${UI.esc(fmtN(io(x)))}<span class="sub">キャッシュ ${UI.esc(fmtN(x.tok_cache_read))}</span></td>`
      + `<td>${prs.slice(0, 3).map((u) => `<a href="${UI.esc(u)}" target="_blank" rel="noopener" class="ai-pr">${UI.esc(prLabel(u))}</a>`).join('')}${prs.length > 3 ? `<span class="faint">ほか ${prs.length - 3}</span>` : ''}</td>`;
  }
  const SESSION_COLS = [{ label: '最終' }, { label: '機体' }, { label: 'ツール' }, { label: '作業場所 · モデル' }, { label: '長さ', cls: 'n' }, { label: '指示', cls: 'n' }, { label: 'ツール呼び出し', cls: 'n' }, { label: 'トークン', cls: 'n' }, { label: 'PR' }];

  function versionsHtml(s) {
    const tools = [...new Set(s.versions.map((v) => v.tool))];
    if (!tools.length) return UI.empty('まだ版の情報がありません');
    const latest = Object.fromEntries(tools.map((t) => [t, s.versions.filter((v) => v.tool === t).reduce((a, v) => (a == null || newer(v.version, a) ? v.version : a), null)]));
    return UI.matrix({
      corner: 'ツール',
      rows: tools.map((t) => ({ key: t, html: toolChip(t), sub: `最新 ${latest[t]}` })),
      cols: state.nodes.map((n) => ({ key: n.id, label: n.id })),
      cell: (r, c) => {
        const v = s.versions.find((x) => x.tool === r.key && x.node_id === c.key);
        if (!v) return {};
        const diff = v.version !== latest[r.key];
        return { html: `<span class="num">${UI.esc(v.version)}</span>`, diff, title: `${c.key}: ${v.version}（${ago(v.last_ts)}に使用）${diff ? `。最新は ${latest[r.key]}` : ''}` };
      },
    });
  }

  function statusHtml(s) {
    return UI.table({
      cols: [{ label: '機体' }, { label: '状態' }, { label: '最終成功' }, { label: 'セッション', cls: 'n' }, { label: 'ファイル', cls: 'n' }, { label: '前回読んだ量', cls: 'n' }, { label: '残り', cls: 'n' }],
      rows: s.nodes,
      row: (n) => {
        const rest = n.truncated ? Math.max(0, (n.bytes_pending || 0) - (n.bytes_read || 0)) : 0;
        const st = n.enabled === false ? UI.chip('取り込まない（台帳）', 'gray')
          : n.no_python ? UI.chip('python が無い', 'orange', { dot: true })
            : n.last_error ? UI.chip('失敗', 'red', { dot: true })
              : n.truncated ? UI.chip('続きあり', 'blue', { dot: true })
                : n.last_ok_at ? UI.chip('最新', 'green', { dot: true }) : UI.chip('未取り込み', 'gray', { dot: true });
        const err = n.last_error ? `<div class="err cursor-err">${UI.esc(String(n.last_error).slice(0, 200))}</div>` : '';
        const ferr = (n.file_errors || []).length ? `<div class="faint">読めないファイル ${UI.esc(n.file_errors.length)} 件</div>` : '';
        return `<td><b>${UI.esc(n.id)}</b>${n.shared ? ` ${UI.chip('共用機', 'gray')}` : ''}${err}${ferr}</td><td class="nowrap">${st}</td><td class="nowrap">${UI.esc(n.last_ok_at ? ago(n.last_ok_at) : '-')}</td>`
          + `<td class="n">${UI.esc(n.sessions ?? '-')}</td><td class="n">${UI.esc(n.files_total ?? '-')}${n.files_changed ? `<span class="sub">変化 ${UI.esc(n.files_changed)}</span>` : ''}</td>`
          + `<td class="n">${UI.esc(fmtBytes(n.bytes_read))}${n.elapsed_s != null ? `<span class="sub">${UI.esc(n.elapsed_s)} 秒</span>` : ''}</td><td class="n">${rest ? UI.esc(fmtBytes(rest)) : '<span class="faint">-</span>'}</td>`;
      },
    });
  }

  async function listHtml() {
    const f = ai.list;
    const res = await window.tune.aiSessions({ ...filter(), q: f.q || undefined, kind: f.kind || undefined, sort: f.sort, errors: f.errors || undefined, offset: f.offset, limit: f.limit });
    const rows = res.rows || [];
    const from = rows.length ? res.offset + 1 : 0;
    return `
      <div class="filters">
        ${UI.search({ id: 'aiQ', placeholder: '作業場所・モデル・版で絞り込む', label: 'セッションの検索', value: f.q })}
        ${UI.select({ id: 'aiKind', label: '種類', options: [['', 'すべて'], ['main', '親のみ'], ['sub', 'サブエージェント']], value: f.kind })}
        ${UI.seg({ attr: 'aisort', options: [['last', '新しい順'], ['duration', '長い順'], ['tokens', 'トークン順'], ['errors', '失敗順']], value: f.sort })}
        ${UI.seg({ attr: 'aierr', options: [['0', 'すべて'], ['1', '失敗ありのみ']], value: f.errors ? '1' : '0' })}
        <span class="count">${UI.esc(res.total)} 件中 ${from}〜${res.offset + rows.length}</span>
      </div>
      ${UI.table({ cols: SESSION_COLS, rows, empty: '該当するセッションはありません', row: sessionRow, cls: 'scroll-x ai-sessions' })}
      <div class="ai-pager"><button class="btn small" id="aiPrev"${f.offset > 0 ? '' : ' disabled'}>前へ</button><button class="btn small" id="aiNext"${res.offset + rows.length < res.total ? '' : ' disabled'}>次へ</button></div>`;
  }

  async function render() {
    const t = ticket();
    setCrumbs([{ icon: 'agent', label: 'AI エージェント' }]);
    const head = (props = '', actions = '') => UI.head({
      icon: 'agent', title: 'AI エージェント',
      desc: 'Claude Code・Codex のセッションの要約。各機体のセッションのファイルを続きから読み、数・時刻・モデル・トークン・PR だけを取り出します。会話の本文やツールの入出力は取り出さず、保存もしません。',
      props, actions,
    });
    const s = typeof window.tune.aiSummary === 'function' ? await window.tune.aiSummary(filter()).catch((e) => ({ error: String(e?.message || e) })) : { unsupported: true };
    if (stale(t)) return;
    if (!s || s.unsupported) {
      page(`${head()}${UI.callout({ tone: 'gray', icon: 'info', title: 'Tauri 版で表示します', body: 'AI エージェントのセッションの取り込みと集計は、Rust の分析エンジン（Tauri 版）で行います。Electron 版では取り込みません。' })}`);
      return;
    }
    if (s.error) {
      page(`${head()}${UI.callout({ tone: 'red', icon: 'alert', title: '集計を読めません', body: `<div class="err">${UI.esc(s.error)}</div>` })}`);
      return;
    }
    const list = await listHtml().catch((e) => UI.callout({ tone: 'red', icon: 'alert', title: 'セッションの一覧を読めません', body: `<div class="err">${UI.esc(e?.message || e)}</div>` }));
    if (stale(t)) return;
    const ingesting = ai.syncing || s.ingesting;
    const rest = s.nodes.filter((n) => n.truncated).reduce((a, n) => a + Math.max(0, (n.bytes_pending || 0) - (n.bytes_read || 0)), 0);
    const noPy = s.nodes.filter((n) => n.no_python);
    const errs = s.nodes.filter((n) => n.last_error && !n.no_python);
    const tot = s.totals || {};
    const sch = s.schedule || {};
    const stateChip = ingesting ? UI.chip('取り込み中', 'blue', { dot: true }) : s.pending ? UI.chip('続きあり', 'orange', { dot: true }) : s.lastAiAt ? UI.chip('最新', 'green', { dot: true }) : UI.chip('未取り込み', 'gray', { dot: true });
    const mins = sch.ai_minutes ?? 30;
    page(`
      ${head(UI.props([
        ['取り込み', `${stateChip}<span class="faint">${UI.esc(s.lastAiAt ? `前回 ${fmtTime(s.lastAiAt)}（${ago(s.lastAiAt)}）` : 'まだ取り込んでいません')}</span>`, 'sync'],
        ['自動', `${UI.chip(sch.enabled === false ? 'オフ' : 'オン', sch.enabled === false ? 'gray' : 'green', { dot: true })}<span class="faint">${UI.esc(mins)} 分ごと（続きがあれば ${UI.esc(sch.catch_up_minutes ?? 2)} 分後）</span>`, 'actions'],
      ]), `<button class="btn primary" id="aiSync"${ingesting ? ' disabled' : ''}>${UI.icon('sync')}${ingesting ? '取り込み中…' : '今すぐ取り込む'}</button>`)}
      ${s.pending || ingesting ? UI.callout({ tone: 'blue', icon: 'sync', title: ingesting ? '取り込み中です' : '取り込みの続きがあります', body: `1回の取り込みは 25 秒で区切り、続きは次の回に読みます（初回は大きいので数回に分かれます）。${rest ? `残りはおよそ <b>${UI.esc(fmtBytes(rest))}</b>。` : ''}自動では ${UI.esc(sch.catch_up_minutes ?? 2)} 分ごとに続きを読みます。` }) : ''}
      ${noPy.length ? UI.callout({ tone: 'orange', icon: 'alert', title: `${noPy.length} 台に python が無いため取り込めません`, body: `${UI.esc(noPy.map((n) => n.id).join('、'))}: python 3.8 以上を入れると取り込めます（Windows は python3 → python → py の順に探します）。` }) : ''}
      ${errs.length ? UI.callout({ tone: 'red', icon: 'alert', title: `${errs.length} 台で取り込みに失敗しました`, body: `<div class="list">${errs.map((n) => `<div class="li"><div class="li-main"><div class="li-title"><span class="node">${UI.esc(n.id)}</span></div><div class="li-sub err">${UI.esc(String(n.last_error).slice(0, 300))}</div></div></div>`).join('')}</div>` }) : ''}
      <div class="filters">
        ${UI.select({ id: 'aiDays', label: '期間', options: [[7, '7 日'], [30, '30 日'], [90, '90 日']], value: ai.days, active: ai.days !== 30 })}
        ${UI.select({ id: 'aiTool', label: 'ツール', options: [['', 'すべて'], ['claude', 'Claude Code'], ['codex', 'Codex']], value: ai.tool })}
        ${UI.select({ id: 'aiNode', label: '機体', options: nodeOptions(), value: ai.node })}
      </div>
      <div class="stats ai-stats">
        ${stat('トークン（入力＋出力）', UI.esc(fmtN(io(tot))), { sub: `入力 ${fmtN(tot.tok_in)}・出力 ${fmtN(tot.tok_out)}`, ic: 'agent' })}
        ${stat('キャッシュ読み', UI.esc(fmtN(tot.tok_cache_read)), { sub: `書き ${fmtN(tot.tok_cache_write)}`, ic: 'data' })}
        ${stat('セッション', UI.esc(fmtN(tot.sessions)), { sub: `指示 ${fmtN(tot.prompts)}`, ic: 'history' })}
        ${stat('ツール呼び出し', UI.esc(fmtN(tot.tool_calls)), { sub: `失敗 ${fmtN(s.tools.reduce((a, x) => a + x.errors, 0))}`, ic: 'tool' })}
      </div>
      ${UI.section('使用量の推移', `直近 ${s.days} 日・日ごと（長いセッションも日をまたいで割り振る）`)}
      ${usageHtml(s)}
      <div class="logs-grid ai-grid">
        <div>${UI.section('モデル別', 'この期間に使ったトークン')}${modelsHtml(s)}</div>
        <div>${UI.section('失敗の多いツール', 'ツールの結果がエラーだった回数')}${toolsHtml(s)}</div>
      </div>
      ${UI.section('長いセッション', '開始から最後の記録まで')}
      ${UI.table({ cols: SESSION_COLS, rows: s.long, empty: 'この期間のセッションはありません', row: sessionRow, cls: 'scroll-x ai-sessions' })}
      <div class="logs-grid ai-grid">
        <div>${UI.section('PR', '新しい順')}<div class="list">${s.prs.length ? s.prs.slice(0, 15).map((p) => `<div class="li"><div class="li-main"><div class="li-title"><a href="${UI.esc(p.url)}" target="_blank" rel="noopener">${UI.esc(prLabel(p.url))}</a></div><div class="li-sub one">${UI.esc(p.node_id)} · ${UI.esc(TOOL[p.tool] || p.tool)} · ${UI.esc(p.cwd || '-')}</div></div><span class="right">${UI.esc(ago(p.last_ts))}</span></div>`).join('') : '<p class="note-line">この期間の PR はありません</p>'}</div></div>
        <div>${UI.section('機体ごとの版', '黄色は最新ではない')}${versionsHtml(s)}</div>
      </div>
      ${UI.section('取り込みの状態', '機体ごと。続きの位置は DB が覚え、変化の無いファイルは読まない')}
      ${statusHtml(s)}
      ${UI.section('セッション', '一覧はページングして読み込む')}
      ${list}
      <div class="settings ai-settings"><label class="row"><span class="row-label">自動の取り込み<small>自動スキャンがオンのとき、この間隔で続きを読みます（変化の無いファイルは読みません）</small></span>${UI.select({ id: 'aiMinutes', options: [...new Set([...MINUTES, mins])].sort((a, b) => a - b).map((v) => [v, `${v} 分`]), value: mins, active: false })}</label></div>
      <p class="note-line">台帳の機体に <code>"ai_sessions": false</code> と書くと、その機体からは取り込みません。</p>`);
    bind();
  }

  function bind() {
    const sync = $('#aiSync');
    if (sync) sync.onclick = runSync;
    $('#aiDays').onchange = (e) => { ai.days = +e.target.value; ai.list.offset = 0; render(); };
    $('#aiTool').onchange = (e) => { ai.tool = e.target.value; ai.list.offset = 0; render(); };
    $('#aiNode').onchange = (e) => { ai.node = e.target.value; ai.list.offset = 0; render(); };
    if ($('#aiQ')) bindSearch('#aiQ', 'ai', (v) => { ai.list.q = v; ai.list.offset = 0; }, render, 300);
    const aiKind = $('#aiKind');
    if (aiKind) aiKind.onchange = (e) => { ai.list.kind = e.target.value; ai.list.offset = 0; render(); };
    $$('[data-aisort]').forEach((b) => { b.onclick = () => { ai.list.sort = b.dataset.aisort; ai.list.offset = 0; render(); }; });
    $$('[data-aierr]').forEach((b) => { b.onclick = () => { ai.list.errors = b.dataset.aierr === '1'; ai.list.offset = 0; render(); }; });
    const aiPrev = $('#aiPrev');
    if (aiPrev) aiPrev.onclick = () => { ai.list.offset = Math.max(0, ai.list.offset - ai.list.limit); render(); };
    const aiNext = $('#aiNext');
    if (aiNext) aiNext.onclick = () => { ai.list.offset += ai.list.limit; render(); };
    $('#aiMinutes').onchange = async (e) => { await window.tune.setSchedule({ ai_minutes: +e.target.value }); toast(`自動の取り込みを ${e.target.value} 分ごとにしました`); render(); };
  }

  async function runSync() {
    ai.syncing = true;
    render();
    const t0 = Date.now();
    try {
      const r = await window.tune.aiSync();
      if (r.busy) toast('取り込みが実行中です');
      else {
        const ok = (r.results || []).filter((x) => x.ok);
        const n = ok.reduce((a, x) => a + (x.sessions || 0), 0);
        const more = ok.some((x) => x.truncated);
        const ng = (r.results || []).length - ok.length;
        toast(`${ok.length} 台から ${n} セッション分を取り込みました（${((Date.now() - t0) / 1000).toFixed(1)} 秒${more ? '、続きあり' : ''}${ng ? `、${ng} 台失敗` : ''}）`, 5000);
      }
    } catch (e) {
      toast(`取り込めませんでした: ${e?.message || e}`, 6000);
    }
    ai.syncing = false;
    if (state.view === 'ai') render();
  }

  // 1台ずつの取り込みの結果（自動の取り込みも含む）。この画面を開いているときだけ描き直す
  let syncedTimer;
  window.tune.onAiSynced?.(() => {
    clearTimeout(syncedTimer);
    syncedTimer = setTimeout(() => { if (state.view === 'ai' && !ai.syncing) render(); }, 600);
  });

  (window.KT_VIEWS = window.KT_VIEWS || []).push({ key: 'ai', label: 'AI', icon: 'agent', render });
})();
