'use strict';
/* renderer/security.js — 「セキュリティ」の画面（ネットワークとセキュリティ。docs/observability.md の 6）
 *
 * app.js より先に読み込み、window.KT_VIEWS に画面を登録する（app.js がサイドバーと描画に足す）。
 * データは window.tune.netsec だけ（Tauri 版。tune-core が DB と最新の分析から作る）。Electron 版には無いので、その旨だけを出す。
 * 画面からは何も変えない（読むだけ）。HTML へは必ず UI.esc を通して入れる。
 */
(() => {
  UI.ICON.shield = '<svg viewBox="0 0 20 20"><path d="M10 2.75l6 2.25v4.5c0 3.8-2.6 6.6-6 7.75-3.4-1.15-6-3.95-6-7.75V5z"/><path d="M7.4 10l1.9 1.9 3.4-3.6"/></svg>';

  const sec = { node: '' };
  // 機体×項目の表の列（tune-core の状態の id）
  const COLS = [['sec-listen', '待ち受け'], ['sec-defense', '防御'], ['sec-persist', '常駐の増減'], ['sec-login', 'ログイン'], ['sec-peers', '初めての接続先']];
  const KIND = { launchd: 'launchd', schtask: 'タスク', service: 'サービス', run: 'Run キー', startup: 'スタートアップ' };
  const CHANGE = { added: ['増えた', 'orange'], removed: ['消えた', 'gray'], changed: ['変わった', 'blue'] };
  const EXPOSURE = { any: ['全部の口', 'orange'], lan: ['特定のアドレス', 'yellow'], loopback: ['この機体の中', 'gray'] };

  const pick = (rows) => rows.filter((r) => !sec.node || r.node_id === sec.node || r.id === sec.node);
  const where = (pub) => (pub ? UI.chip('外部', 'red') : UI.chip('内側', 'gray'));

  function matrixHtml(d) {
    return UI.matrix({
      corner: '機体',
      cls: 'scroll-x sec-matrix',
      rows: d.nodes.map((n) => ({
        key: n.id, n,
        html: `<span class="sec-node" data-goto="${UI.esc(n.id)}" tabindex="0">${UI.icon(n.os === 'windows' ? 'win' : 'mac')}<b>${UI.esc(n.id)}</b></span>${n.shared ? UI.chip('共用機', 'gray') : ''}`,
        sub: n.enabled === false ? '台帳で止めている' : n.at ? `分析 ${fmtTime(n.at)}` : 'まだ分析していない',
      })),
      cols: COLS.map(([k, label]) => ({ key: k, label })),
      cell: (r, c) => {
        if (r.n.enabled === false) return { html: '<span class="muted">止めている</span>', title: '台帳の "network": false' };
        const ch = (r.n.checks || []).find((x) => x.id === c.key);
        if (!ch) return { html: '<span class="muted">–</span>', title: r.n.netsec ? 'この項目は Tauri 版の記録が要る' : 'まだ調べていない' };
        return { html: UI.status(ch.status), title: ch.detail || '' };
      },
    });
  }

  function attentionHtml(d) {
    const bad = [];
    for (const n of d.nodes) for (const c of n.checks || []) if (c.status === 'fail' || c.status === 'warn') bad.push({ n, c });
    if (!bad.length) return UI.callout({ tone: 'green', icon: 'check', title: '要対応はありません', body: '防御・待ち受け・常駐の増減・ログイン・初めての接続先に、異常も注意もありません。' });
    bad.sort((a, b) => (a.c.status === 'fail' ? 0 : 1) - (b.c.status === 'fail' ? 0 : 1));
    const fail = bad.some((x) => x.c.status === 'fail');
    return UI.callout({
      tone: fail ? 'red' : 'orange', icon: 'alert', title: `${bad.length} 件を確かめてください`,
      body: `<div class="list">${bad.map(({ n, c }) => `<div class="li"><div class="li-main"><div class="li-title">${UI.status(c.status)} <span class="node">${UI.esc(n.id)}</span> ${UI.esc(c.name)}</div><div class="li-sub">${UI.esc(c.detail || '')}</div></div></div>`).join('')}</div>`,
    });
  }

  function listenHtml(d) {
    const rows = [];
    for (const n of pick(d.nodes)) {
      const ns = n.netsec;
      if (!ns) continue;
      const fresh = new Set((ns.listen_new || []).map((l) => `${l.proto}|${l.port}|${l.proc}`));
      for (const l of ns.listen || []) if (l.exposure !== 'loopback') rows.push({ n, l, fresh: fresh.has(`${l.proto}|${l.port}|${l.proc}`) });
    }
    rows.sort((a, b) => Number(b.fresh) - Number(a.fresh) || a.n.id.localeCompare(b.n.id) || (a.l.proto === b.l.proto ? a.l.port - b.l.port : a.l.proto < b.l.proto ? -1 : 1));
    return UI.table({
      cols: [{ label: '機体' }, { label: 'プロセス' }, { label: '番号', cls: 'n' }, { label: '公開の範囲' }, { label: 'アドレス' }, { label: '' }],
      rows,
      empty: '外から届く待ち受けはありません（または、まだ分析していません）',
      cls: 'scroll-x sec-listen',
      row: ({ n, l, fresh }) => {
        const [lab, tone] = EXPOSURE[l.exposure] || [l.exposure, 'default'];
        return `<td class="nowrap"><b>${UI.esc(n.id)}</b></td><td>${UI.esc(l.proc)}</td><td class="n num">${UI.esc(l.proto.toUpperCase())} ${UI.esc(l.port)}</td>`
          + `<td>${UI.chip(lab, tone)}</td><td class="sec-addr">${UI.esc((l.addrs || []).join(', '))}</td><td>${fresh ? UI.chip('前回は無かった', l.proto === 'udp' ? 'orange' : 'red', { dot: true }) : ''}</td>`;
      },
    });
  }

  function defenseHtml(d) {
    return UI.table({
      cols: [{ label: '機体' }, { label: '状態' }, { label: '内容' }],
      rows: pick(d.nodes).filter((n) => n.enabled !== false),
      empty: 'まだ分析していません',
      row: (n) => {
        const c = (n.checks || []).find((x) => x.id === 'sec-defense');
        return `<td class="nowrap"><b>${UI.esc(n.id)}</b></td><td class="nowrap">${c ? UI.status(c.status) : UI.status('unknown')}</td><td>${UI.esc(c?.detail || (n.netsec ? '' : 'まだ調べていない'))}</td>`;
      },
    });
  }

  function persistHtml(d) {
    const rows = pick(d.events || []).slice(0, 100);
    return UI.table({
      cols: [{ label: '日時' }, { label: '機体' }, { label: '変化' }, { label: '種類' }, { label: '名前' }, { label: '実行ファイル' }],
      rows,
      empty: 'まだ増減はありません（各機体の初回は記録だけで、増減は2回目の分析から出ます）',
      cls: 'scroll-x',
      row: (e) => {
        const [lab, tone] = CHANGE[e.change] || [e.change, 'default'];
        const prog = e.change === 'changed' ? `${e.from_program || '-'} → ${e.program || '-'}` : e.program || '-';
        return `<td class="muted nowrap">${UI.esc(fmtTime(e.ts))}</td><td class="nowrap"><b>${UI.esc(e.node_id)}</b></td><td>${UI.chip(lab, tone)}</td><td class="nowrap">${UI.esc(KIND[e.kind] || e.kind)}</td>`
          + `<td class="sec-key">${wbr(e.key)}</td><td class="sec-addr">${UI.esc(prog)}</td>`;
      },
    });
  }

  function loginHtml(d) {
    const rows = pick(d.logins || []);
    return UI.table({
      cols: [{ label: '機体' }, { label: '結果' }, { label: '送り元' }, { label: '' }, { label: '24 時間', cls: 'n' }, { label: '7 日', cls: 'n' }, { label: '最後' }],
      rows,
      empty: 'ログインの失敗も、外からのログインもありません（Windows はセキュリティログを読める機体だけ）',
      row: (l) => {
        const ok = l.event_id === '4624';
        return `<td class="nowrap"><b>${UI.esc(l.node_id)}</b></td><td>${ok ? UI.chip('成功（ネットワーク）', 'blue') : UI.chip('失敗', 'orange')}</td><td class="sec-addr">${UI.esc(l.provider || '-')}</td><td>${where(l.public)}</td>`
          + `<td class="n num">${UI.esc(l.n_24h)}</td><td class="n num">${UI.esc(l.n_7d)}</td><td class="muted nowrap">${UI.esc(ago(l.last_ts))}</td>`;
      },
    });
  }

  function peersHtml(d) {
    const learning = d.nodes.filter((n) => n.netsec?.peers?.learning);
    const rows = pick(d.peers || []).slice(0, 150);
    const note = learning.length
      ? `<p class="note-line">覚えている途中（最初の ${UI.esc(d.learn_days)} 日は知らせない）: ${learning.map((n) => `${UI.esc(n.id)}（${UI.esc(fmtTime(n.netsec.peers.until))} まで）`).join('、')}</p>` : '';
    return `${note}${UI.table({
      cols: [{ label: '初めて見た' }, { label: '機体' }, { label: 'プロセス' }, { label: '宛先' }, { label: '番号', cls: 'n' }, { label: '' }, { label: '見た回数', cls: 'n' }],
      rows,
      empty: `直近 ${d.learn_days} 日に初めて見た接続先はありません`,
      cls: 'scroll-x',
      row: (p) => `<td class="muted nowrap">${UI.esc(fmtTime(p.first_seen))}</td><td class="nowrap"><b>${UI.esc(p.node_id)}</b></td><td>${UI.esc(p.proc)}</td><td class="sec-addr">${UI.esc(p.addr)}</td>`
        + `<td class="n num">${UI.esc(p.port)}</td><td>${where(p.public)}</td><td class="n num">${UI.esc(p.seen)}</td>`,
    })}`;
  }

  async function render() {
    const t = ticket();
    setCrumbs([{ icon: 'shield', label: 'セキュリティ' }]);
    const head = (props = '') => UI.head({
      icon: 'shield', title: 'セキュリティ',
      desc: '待ち受けているポート・防御の状態・自動起動（常駐）の増減・ログイン・初めての接続先。接続のメタデータと OS の記録だけを読み、パケットの中身は取りません。この画面から機体は変えません。',
      props,
    });
    const d = typeof window.tune.netsec === 'function' ? await window.tune.netsec({}).catch((e) => ({ error: String(e?.message || e) })) : { unsupported: true };
    if (stale(t)) return;
    if (!d || d.unsupported) {
      page(`${head()}${UI.callout({ tone: 'gray', icon: 'info', title: 'Tauri 版で表示します', body: '常駐の増減と初めての接続先の記録は、Rust の分析エンジン（Tauri 版）が持ちます。Electron 版では、防御と待ち受けの判定だけが「状態」と各機体の所見に出ます。' })}`);
      return;
    }
    if (d.error) {
      page(`${head()}${UI.callout({ tone: 'red', icon: 'alert', title: '読めません', body: `<div class="err">${UI.esc(d.error)}</div>` })}`);
      return;
    }
    const latest = Math.max(0, ...d.nodes.map((n) => n.at || 0));
    page(`
      ${head(UI.props([
        ['最新の分析', `<span class="faint">${UI.esc(latest ? `${fmtTime(latest)}（${ago(latest)}）` : 'まだ分析していません')}</span>`, 'history'],
        ['保持', `<span class="faint">宛先 ${UI.esc(d.keep_days)} 日・常駐の増減 180 日（手元の DB だけ）</span>`, 'data'],
      ]))}
      ${attentionHtml(d)}
      ${UI.section('機体×項目', '点にマウスを置くと根拠が出る。項目は 60 分ごとの分析とログの取り込みで更新される')}
      ${matrixHtml(d)}
      <div class="filters">${UI.select({ id: 'secNode', label: '機体', options: nodeOptions(), value: sec.node })}</div>
      ${UI.section('外から届く待ち受け', '全部の口（0.0.0.0・::）か、特定のアドレスで待っているもの。前回の分析と比べる')}
      ${listenHtml(d)}
      ${UI.section('防御の状態', 'macOS はファイアウォール・Gatekeeper・XProtect、Windows は Defender・ほかのウイルス対策・ファイアウォール')}
      ${defenseHtml(d)}
      ${UI.section('常駐の増減', 'LaunchAgents・LaunchDaemons／タスク・サービス・Run キー・スタートアップ（名前と実行ファイルの名前だけ）')}
      ${persistHtml(d)}
      ${UI.section('ログイン', 'sshd の失敗（macOS）・セキュリティログ 4625／4624（Windows）。送り元ごと')}
      ${loginHtml(d)}
      ${UI.section('初めての接続先', '分析のときの標本。機体・プロセスごとに覚えた宛先とポートに無いもの（直近 7 日に初めて見たもの）')}
      ${peersHtml(d)}
      <p class="note-line">台帳の機体に <code>"network": false</code> と書くと、その機体からは集めません。共用機の「初めての接続先」は提案だけにします。Windows のセキュリティログは、管理者か Event Log Readers のグループでないと読めません（読めないときは「不明」と出します）。</p>`);
    bind();
  }

  function bind() {
    $('#secNode').onchange = (e) => { sec.node = e.target.value; render(); };
    bindGoto($('#page'));
  }

  // 分析とログの取り込みのあと（状態が更新されたら）、この画面を開いているときだけ描き直す
  let timer;
  window.tune.onChecksUpdated?.(() => {
    clearTimeout(timer);
    timer = setTimeout(() => { if (state.view === 'security') render(); }, 800);
  });

  (window.KT_VIEWS = window.KT_VIEWS || []).push({ key: 'security', label: 'セキュリティ', icon: 'shield', render });
})();
