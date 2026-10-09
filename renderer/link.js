'use strict';
/* renderer/link.js — 「接続」の画面（tune-agent とのペアリングの骨組み）
 *
 * app.js より先に読み込み、window.KT_VIEWS に画面を登録する（app.js がサイドバーと描画に足す）。
 * できること: 機体のアドレスと 6 桁のコードを入れてペアリングする・ペア済みの機体（公開鍵の指紋）を見る。
 * 調査・ライブの経路は SSH のまま。tune-agent への切り替えは次の段階（docs/connectivity.md）。
 * コードは入力欄から window.tune.agentPair に渡すだけで、画面の状態にも残さない（送ったら欄を空にする）。
 * Electron 版には agentPeers が無いので、その旨だけを出す。HTML へは必ず UI.esc を通して入れる。
 */
(() => {
  const link = { addr: '', busy: false };

  const COLS = [
    { label: '機体', icon: 'link' },
    { label: '公開鍵の指紋' },
    { label: 'つなぎ先' },
    { label: 'ペアリング', cls: 'r' },
  ];

  function row(p) {
    return `<td><span class="node">${UI.esc(p.name)}</span></td>`
      + `<td><code>${UI.esc(p.fingerprint)}</code></td>`
      + `<td>${UI.esc(p.addr || '-')}</td>`
      + `<td class="r">${UI.esc(p.paired_at ? fmtTime(p.paired_at) : '-')}</td>`;
  }

  async function render() {
    const t = ticket();
    setCrumbs([{ icon: 'link', label: '接続' }]);
    const head = (props = '') => UI.head({
      icon: 'link', title: '接続（tune-agent）',
      desc: '各機体に置く tune-agent とペアリングします。機体で「tune-agent pair」を動かすと 6 桁のコードが出るので、その機体のアドレスとコードを入れてください。交換するのは公開鍵だけで、秘密鍵は機体の外に出ません。調べる経路は今は SSH のままです（tune-agent への切り替えは次の段階）。',
      props,
    });
    if (typeof window.tune.agentPeers !== 'function') {
      page(`${head()}${UI.callout({ tone: 'gray', icon: 'info', title: 'Tauri 版で使えます', body: 'tune-agent とのペアリングは、Rust の版（Tauri 版）だけで行います。' })}`);
      return;
    }
    const r = await window.tune.agentPeers().catch((e) => ({ error: String(e?.message || e) }));
    if (stale(t)) return;
    if (r.error) {
      page(`${head()}${UI.callout({ tone: 'red', icon: 'alert', title: '台帳を読めません', body: `<div class="err">${UI.esc(r.error)}</div>` })}`);
      return;
    }
    const peers = r.peers || [];
    page(`
      ${head(UI.props([
        ['この操作卓', r.me ? `<code>${UI.esc(r.me)}</code>` : '<span class="faint">まだ鍵がありません（最初のペアリングで作ります）</span>', 'link'],
        ['ペア済み', `${UI.esc(peers.length)} 台`, 'machine'],
      ]))}
      ${UI.section('ペアリング', 'コードは 5 分で失効・1 回限り。3 回続けて間違えると、その機体の受付が止まります')}
      <form class="filters link-form" id="linkForm" autocomplete="off">
        <label class="search"><input id="linkAddr" type="text" inputmode="url" placeholder="機体のアドレス（例: 127.0.0.1 か 127.0.0.1:47232）" aria-label="機体のアドレス" value="${UI.esc(link.addr)}" spellcheck="false"></label>
        <label class="search" style="max-width:180px;flex:0 1 180px"><input id="linkCode" type="text" inputmode="numeric" placeholder="6 桁のコード" aria-label="6 桁のコード" maxlength="7" autocomplete="one-time-code" spellcheck="false"></label>
        <button class="btn primary" id="linkPair" type="submit"${link.busy ? ' disabled' : ''}>${UI.icon('link')}${link.busy ? 'ペアリング中…' : 'ペアリング'}</button>
      </form>
      ${UI.section('ペア済みの機体', '公開鍵の指紋は、機体で「tune-agent status」を動かすと出る指紋と同じになります')}
      ${UI.table({ cols: COLS, rows: peers, row, empty: 'まだありません', cls: 'scroll-x' })}
      <p class="note-line">ペアを外すときは「tune agent-unpair 名前」（操作卓）と「tune-agent unpair 名前」（機体）を使います。</p>`);
    bind();
  }

  function bind() {
    const form = $('#linkForm');
    if (!form) return;
    $('#linkAddr').oninput = (e) => { link.addr = e.target.value; };
    form.onsubmit = async (e) => {
      e.preventDefault();
      const codeEl = $('#linkCode');
      const addr = $('#linkAddr').value.trim();
      const code = codeEl.value;
      codeEl.value = '';
      if (!addr) return toast('機体のアドレスを入れてください');
      if (!/^\s*\d{3}[\s-]?\d{3}\s*$/.test(code)) return toast('コードは 6 桁の数字です');
      link.busy = true;
      render();
      try {
        const r = await window.tune.agentPair(addr, code);
        toast(r.ok ? `${r.peer.name} とペアリングしました（指紋 ${r.peer.fingerprint}）` : `ペアリングできませんでした: ${r.error}`, r.ok ? 5000 : 7000);
      } catch (err) {
        toast(`ペアリングできませんでした: ${err?.message || err}`, 7000);
      }
      link.busy = false;
      if (state.view === 'link') render();
    };
  }

  (window.KT_VIEWS = window.KT_VIEWS || []).push({ key: 'link', label: '接続', icon: 'link', render });
})();
