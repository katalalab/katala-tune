'use strict';
/* renderer/tools.js — 「道具」の画面（道具の棚卸しと Do-gu の照合）
 *
 * app.js より先に読み込み、window.KT_VIEWS に画面を登録する（app.js がサイドバーと描画に足す）。
 * app.js の関数（page・setCrumbs・ticket・stale・toast・bindSearch・fmtTime・ago・state など）は、描くときに呼ぶ。
 * データは window.tune.inventory / inventoryRun / doguRefresh / doguExclude / doguPublish だけから受け取る（Electron 版と Tauri 版で同じ形）。
 * HTML へは必ず UI.esc を通して入れる。
 */
(() => {
  const inv = { tab: 'matrix', all: false, q: '', node: '', drift: false, show: 200, running: false, fetching: false, selected: null, data: null };
  const KIND = { added: ['追加', 'green'], removed: ['削除', 'red'], updated: ['更新', 'blue'] };
  const HOURS = [6, 12, 24, 48, 168];

  const srcLabel = (d, s) => d.sources?.[s] || s;
  // 機体どうしで最も多い版（版の違いを強調する基準）
  function majority(g) {
    const c = new Map();
    for (const cell of Object.values(g.nodes)) for (const v of cell.versions) c.set(v, (c.get(v) || 0) + 1);
    return [...c.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] ?? null;
  }

  function cellOf(d, g, node) {
    const x = g.nodes[node];
    if (!x) return {};
    const srcs = x.sources.map((s) => srcLabel(d, s)).join('・');
    if (!x.versions.length) return { html: '<span class="inv-has" aria-label="あり">●</span>', title: `${srcs}（版なし）` };
    const maj = majority(g);
    const diff = g.drift && maj != null && !x.versions.includes(maj);
    return {
      html: `<span class="inv-ver num">${UI.esc(x.versions.join(' / '))}</span>`,
      diff,
      title: `${srcs}: ${x.versions.join(' / ')}${diff ? `（多数派は ${maj}）` : ''}`,
    };
  }

  function filtered(d) {
    const q = inv.q.trim().toLowerCase();
    return d.groups.filter((g) => (!q || `${g.name} ${g.slug || ''} ${g.key}`.toLowerCase().includes(q))
      && (!inv.node || g.nodes[inv.node]) && (!inv.drift || g.drift));
  }

  function matrixHtml(d) {
    const rows = filtered(d);
    const shown = rows.slice(0, inv.show);
    const drift = d.groups.filter((g) => g.drift).length;
    return `
      <div class="filters">
        ${UI.search({ id: 'invQ', placeholder: '道具の名前で絞り込む', label: '道具の検索', value: inv.q })}
        ${UI.select({ id: 'invNode', label: '機体', options: [['', 'すべて'], ...d.nodes.map((n) => [n.id, n.id])], value: inv.node })}
        ${UI.seg({ attr: 'invall', options: [['0', '自分で入れたもの'], ['1', 'すべて']], value: inv.all ? '1' : '0' })}
        ${UI.seg({ attr: 'invdrift', options: [['0', 'すべて'], ['1', `版が違うものだけ ${drift}`]], value: inv.drift ? '1' : '0' })}
        <span class="count">${rows.length} 件${rows.length > shown.length ? `（${shown.length} 件を表示）` : ''}</span>
      </div>
      ${rows.length ? UI.matrix({
        corner: '道具',
        cls: 'scroll-x inv-matrix',
        rows: shown.map((g) => ({
          key: g.key,
          g,
          html: `<span class="inv-name">${UI.esc(g.name)}</span>${g.slug ? UI.chip(g.category || 'Do-gu', 'purple', { title: `Do-gu: ${g.slug}`, cls: 'inv-cat' }) : ''}`,
          sub: g.sources.map((s) => srcLabel(d, s)).join('・'),
        })),
        cols: d.nodes.map((n) => ({ key: n.id, html: `<span class="inv-col">${UI.icon(n.os === 'windows' ? 'win' : 'mac')}${UI.esc(n.id)}</span>`, attrs: `title="${UI.esc(`${n.id}: 自分で入れた道具 ${n.count} 件`)}"` })),
        cell: (r, c) => cellOf(d, r.g, c.key),
      }) : UI.empty(d.groups.length ? '該当する道具はありません' : 'まだ棚卸ししていません。「棚卸しする」で全機体の道具を集めます。')}
      ${rows.length > shown.length ? `<p class="note-line"><button class="btn small" id="invMore">さらに ${Math.min(300, rows.length - shown.length)} 件を表示</button></p>` : ''}
      <p class="note-line">黄色のセルは、その道具を入れている機体のうち多数派と版が違うもの（同じ機体の中の書き方の違いは数えない）。● は版の無いもの。</p>`;
  }

  function historyHtml(d) {
    return UI.table({
      cols: [{ label: '日時' }, { label: '機体' }, { label: '変化' }, { label: '道具' }, { label: '種類' }, { label: '版', cls: 'n' }],
      rows: d.events,
      empty: 'まだ変化はありません（初回の棚卸しは記録しません）',
      row: (e) => {
        const [label, tone] = KIND[e.kind] || [e.kind, 'default'];
        const ver = e.kind === 'updated' ? `${e.from_version ?? '-'} → ${e.to_version ?? '-'}` : (e.to_version ?? e.from_version ?? '');
        return `<td class="muted nowrap">${UI.esc(fmtTime(e.ts))}</td><td class="nowrap"><b>${UI.esc(e.node_id)}</b></td><td>${UI.chip(label, tone)}</td>`
          + `<td>${UI.esc(e.name)}</td><td class="muted nowrap">${UI.esc(srcLabel(d, e.source))}</td><td class="n num">${UI.esc(ver)}</td>`;
      },
    });
  }

  function doguHtml(d) {
    const g = d.dogu;
    const intro = '<a href="https://do-gu.niwa.dev" target="_blank" rel="noopener">Do-gu</a> は仕事道具のデッキを見せ合うサービス。共通の道具の一覧（公開）と照合して、自分で入れた道具からデッキの下書きを作ります。';
    if (!g) {
      return `${UI.callout({ tone: 'gray', icon: 'info', body: `${intro}<p class="note-line">一覧の取得は、このボタンを押したときだけ行います（送るものはありません。取得した一覧は1日使い回します）。</p>` })}
        <div class="empty"><p>まだ Do-gu の一覧を取得していません。</p><button class="btn primary" id="doguRefresh"${inv.fetching ? ' disabled' : ''}>${UI.icon('sync')}${inv.fetching ? '取得中…' : 'Do-gu の一覧を取得'}</button></div>`;
    }
    const draft = g.draft || [];
    const sel = inv.selected ?? new Set(draft.map((x) => x.slug));
    const picked = draft.filter((x) => sel.has(x.slug));
    const age = Date.now() - g.at;
    return `
      ${UI.props([
        ['Do-gu の一覧', `<span>${UI.esc(g.tools)} 件</span><span class="faint">${UI.esc(ago(g.at))}に取得${age > 86400e3 ? '（1日以上前）' : ''}</span><button class="btn small" id="doguRefresh"${inv.fetching ? ' disabled' : ''}>${UI.icon('sync')}${inv.fetching ? '取得中…' : '取り直す'}</button>`, 'sync'],
        ['照合できた道具', `<span>${UI.esc(g.matched)} 件</span><span class="faint">自分で入れたもののうち Do-gu にある道具</span>`, 'link'],
        ['デッキの下書き', `<span>${UI.esc(draft.length)} 件</span>${g.exclude.length ? `<span class="faint">除外 ${UI.esc(g.exclude.length)} 件</span>` : ''}`, 'tag'],
      ])}
      ${UI.callout({ tone: 'orange', icon: 'alert', title: '登録すると公開ページに載ります', body: '登録は確認ダイアログで全件を見せてから送ります。送るのは既にある道具への紐づけだけで、新しい道具は作りません。確認のあいだに下書きが変わったら送りません。失敗してもやり直さず、結果は「実行記録」に残ります。' })}
      ${UI.section('デッキの下書き', '登録する道具を選び、要らないものは除外します')}
      ${UI.table({
        cols: [{ label: '', cls: 'c', width: '36px' }, { label: '道具' }, { label: '分類' }, { label: '機体' }, { label: '種類' }, { label: '', cls: 'acts' }],
        rows: draft,
        empty: '下書きは空です（照合できた道具が無いか、すべて除外しています）',
        row: (x) => `<td class="c mid"><input type="checkbox" data-pick="${UI.esc(x.slug)}" aria-label="${UI.esc(x.name)} を登録する"${sel.has(x.slug) ? ' checked' : ''}></td>`
          + `<td><b>${UI.esc(x.name)}</b><span class="sub">${UI.esc(x.slug)}</span></td><td>${UI.chip(x.category ?? '-', 'purple')}</td>`
          + `<td class="muted">${UI.esc(x.nodes.join('、'))}</td><td class="muted">${UI.esc(x.sources.map((s) => srcLabel(d, s)).join('・'))}</td>`
          + `<td class="acts"><button class="btn small" data-exclude="${UI.esc(x.slug)}">除外</button></td>`,
      })}
      <div class="inv-publish">
        <button class="btn primary" id="doguPublish"${picked.length ? '' : ' disabled'}>${UI.icon('link')}選んだ ${picked.length} 件を Do-gu に登録…</button>
        <span class="faint">登録の前に確認します。API キーは環境変数 DO_GU_API_KEY か ~/.local/share/do-gu/api_key・~/.config/do-gu/api_key から読みます</span>
      </div>
      ${g.exclude.length ? `${UI.section('除外した道具', '下書きに出しません')}<div class="inv-excluded">${g.exclude.map((s) => `<span class="inv-ex">${UI.chip(s, 'gray')}<button class="link" data-restore="${UI.esc(s)}">戻す</button></span>`).join('')}</div>` : ''}`;
  }

  function scheduleRow() {
    const sch = state.status?.schedule || state.cfg?.schedule || {};
    const h = sch.inventory_hours ?? 24;
    const opts = [...new Set([...HOURS, h])].sort((a, b) => a - b).map((v) => [v, v % 24 === 0 ? `${v / 24} 日` : `${v} 時間`]);
    return `<div class="settings inv-settings"><label class="row"><span class="row-label">自動の棚卸し<small>自動スキャンがオンのとき、この間隔で全機体の道具を集めます（インストール先を読むだけ）</small></span>${UI.select({ id: 'invHours', options: opts, value: h, active: false })}</label></div>`;
  }

  async function render() {
    const t = ticket();
    setCrumbs([{ icon: 'tool', label: '道具' }]);
    let d;
    try {
      d = await window.tune.inventory({ all: inv.all });
    } catch (e) {
      if (stale(t)) return;
      page(`${UI.head({ icon: 'tool', title: '道具' })}${UI.callout({ tone: 'red', icon: 'alert', title: '道具の一覧を読めません', body: `<div class="err">${UI.esc(e?.message || e)}</div>` })}`);
      return;
    }
    if (stale(t)) return;
    inv.data = d;
    const explicit = d.nodes.reduce((s, n) => s + (n.count || 0), 0);
    const errs = d.nodes.filter((n) => n.error);
    const running = inv.running || d.inventorying;
    const tabs = [['matrix', '機体×道具', d.groups.length, 'tool'], ['history', '追加・削除の履歴', d.events.length, 'history'], ['dogu', 'Do-gu', d.dogu ? d.dogu.draft.length : null, 'link']];
    const sch = state.status?.schedule || state.cfg?.schedule || {};
    let body = '';
    if (inv.tab === 'history') body = historyHtml(d);
    else if (inv.tab === 'dogu') body = doguHtml(d);
    else body = matrixHtml(d);
    page(`
      ${UI.head({
        icon: 'tool', title: '道具',
        desc: '全機体の CLI・パッケージ・アプリと版を集め、機体ごとの違いと追加・削除を記録します。調査はインストール先を読むだけで、パッケージマネージャもネットワークも使いません。',
        props: UI.props([
          ['前回の棚卸し', `${UI.esc(d.lastInventoryAt ? `${fmtTime(d.lastInventoryAt)}（${ago(d.lastInventoryAt)}）` : 'まだ')}${running ? UI.chip('棚卸し中', 'blue', { dot: true }) : ''}`, 'history'],
          ['自動', `${UI.chip(sch.enabled === false ? 'オフ' : 'オン', sch.enabled === false ? 'gray' : 'green', { dot: true })}<span class="faint">${UI.esc(sch.inventory_hours ?? 24)} 時間ごと</span>`, 'actions'],
          ['道具', `<span>${UI.esc(d.groups.length)} 種類</span><span class="faint">自分で入れたもの 延べ ${UI.esc(explicit)} 件</span>${d.groups.some((g) => g.drift) ? UI.chip(`版の違い ${d.groups.filter((g) => g.drift).length}`, 'yellow') : ''}`, 'tool'],
        ]),
        actions: `<button class="btn primary" id="invRun"${running ? ' disabled' : ''}>${UI.icon('play')}${running ? '棚卸し中…' : '棚卸しする'}</button>`,
      })}
      ${errs.length ? UI.callout({ tone: 'red', icon: 'alert', title: `${errs.length} 台で棚卸しできませんでした`, body: `<div class="list">${errs.map((n) => `<div class="li"><div class="li-main"><div class="li-title"><span class="node">${UI.esc(n.id)}</span></div><div class="li-sub err">${UI.esc(String(n.error).slice(0, 300))}</div></div></div>`).join('')}</div><p class="note-line">前回の一覧はそのまま残しています（失敗を「削除」とは見なしません）。</p>` }) : ''}
      ${UI.tabs(tabs, inv.tab)}
      ${body}
      ${inv.tab === 'matrix' ? scheduleRow() : ''}`);
    bind(d);
  }

  function bind(d) {
    $$('[data-tab]').forEach((b) => { b.onclick = () => { inv.tab = b.dataset.tab; render(); }; });
    const run = $('#invRun');
    if (run) run.onclick = runInventory;
    if ($('#invQ')) bindSearch('#invQ', 'tools', (v) => { inv.q = v; inv.show = 200; }, render);
    const node = $('#invNode');
    if (node) node.onchange = (e) => { inv.node = e.target.value; inv.show = 200; render(); };
    $$('[data-invall]').forEach((b) => { b.onclick = () => { inv.all = b.dataset.invall === '1'; inv.show = 200; render(); }; });
    $$('[data-invdrift]').forEach((b) => { b.onclick = () => { inv.drift = b.dataset.invdrift === '1'; inv.show = 200; render(); }; });
    const more = $('#invMore');
    if (more) more.onclick = () => { inv.show += 300; render(); };
    const hours = $('#invHours');
    if (hours) hours.onchange = async (e) => { await window.tune.setSchedule({ inventory_hours: +e.target.value }); state.status = await window.tune.status(); toast(`自動の棚卸しを ${e.target.value} 時間ごとにしました`); render(); };
    const refresh = $('#doguRefresh');
    if (refresh) refresh.onclick = doguRefresh;
    const draft = d.dogu?.draft || [];
    $$('[data-pick]').forEach((c) => {
      c.onchange = () => {
        const sel = inv.selected ?? new Set(draft.map((x) => x.slug));
        if (c.checked) sel.add(c.dataset.pick); else sel.delete(c.dataset.pick);
        inv.selected = sel;
        render();
      };
    });
    $$('[data-exclude]').forEach((b) => { b.onclick = () => setExclude([...(d.dogu?.exclude || []), b.dataset.exclude]); });
    $$('[data-restore]').forEach((b) => { b.onclick = () => setExclude((d.dogu?.exclude || []).filter((s) => s !== b.dataset.restore)); });
    const pub = $('#doguPublish');
    if (pub) pub.onclick = () => publish(draft);
  }

  async function runInventory() {
    inv.running = true;
    render();
    const t0 = Date.now();
    try {
      const r = await window.tune.inventoryRun();
      const ng = (r.nodes || []).filter((n) => n.error).length;
      toast(r.busy ? '別の棚卸しが実行中です' : `棚卸しが ${((Date.now() - t0) / 1000).toFixed(1)} 秒で終わりました${ng ? `（${ng} 台失敗）` : ''}`);
    } catch (e) {
      toast(`棚卸しできませんでした: ${e?.message || e}`, 6000);
    }
    inv.running = false;
    if (state.view === 'tools') render();
  }

  async function doguRefresh() {
    inv.fetching = true;
    render();
    try {
      const r = await window.tune.doguRefresh();
      if (r?.error) toast(`Do-gu の一覧を取得できませんでした: ${r.error}`, 6000);
      else toast(`Do-gu の一覧を取得しました（${r?.dogu?.tools ?? 0} 件）`);
    } catch (e) {
      toast(`Do-gu の一覧を取得できませんでした: ${e?.message || e}`, 6000);
    }
    inv.fetching = false;
    inv.selected = null;
    if (state.view === 'tools') render();
  }

  async function setExclude(list) {
    await window.tune.doguExclude([...new Set(list)]);
    if (inv.selected) for (const s of list) inv.selected.delete(s);
    render();
  }

  async function publish(draft) {
    const sel = inv.selected ?? new Set(draft.map((x) => x.slug));
    const slugs = draft.filter((x) => sel.has(x.slug)).map((x) => x.slug);
    const r = await window.tune.doguPublish(slugs);
    if (r.cancelled) return toast('取り消しました');
    if (r.refused) return toast(`送りませんでした: ${r.refused}`, 6000);
    toast(r.ok ? `Do-gu に登録しました: ${r.url}` : `登録に失敗しました（やり直していません）: ${r.result?.error || ''}`, 7000);
    render();
  }

  // 1台ずつの棚卸しの結果（自動の棚卸しも含む）。この画面を開いているときだけ描き直す
  let resultTimer;
  window.tune.onInventoryResult?.(() => {
    clearTimeout(resultTimer);
    resultTimer = setTimeout(() => { if (state.view === 'tools' && !inv.running) render(); }, 500);
  });

  (window.KT_VIEWS = window.KT_VIEWS || []).push({ key: 'tools', label: '道具', icon: 'tool', render });
})();
