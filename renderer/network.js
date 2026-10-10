'use strict';
// 手動だけの軽量接続診断。表示中の結果はメモリに置き、DBや台帳を変更しない。
(() => {
  const results = new Map();
  let running = false;
  const flag = (v) => v === true ? UI.chip('あり', 'green') : v === false ? UI.chip('なし', 'orange') : '<span class="muted">未取得</span>';
  const errors = { dns_failed: '名前解決失敗', connect_failed: '接続失敗', timed_out: '時間切れ', tls_failed: 'TLS失敗', tls_certificate_failed: '証明書確認失敗', tool_missing: 'curl未導入', unavailable: '未取得' };
  function https(r) {
    if (!r) return '<span class="muted">未実行</span>';
    if (r.state !== 'reachable') return UI.chip(errors[r.state] || '未取得', 'orange');
    const t = r.timings?.total_ms;
    return `${UI.chip('到達', 'green')} <span class="muted">HTTP ${UI.esc(r.http_status)}${typeof t === 'number' ? ` · ${UI.esc(t.toFixed(1))} ms` : ''}</span>`;
  }
  async function run(ids, active) {
    if (running) return;
    running = true;
    render();
    let failed = 0;
    try {
      // 全機でも1台ずつ。中途で閉じても、完了した結果は次に開いた時に残る。
      for (const id of ids) {
        try {
          const out = await window.tune.networkCheck([id], active);
          const row = (out.nodes || []).find(r => r.node_id === id);
          if (!row) throw new Error('診断結果を取得できませんでした');
          results.set(id, row);
          if (!row.ok) failed++;
        } catch (e) {
          failed++;
          results.set(id, { node_id: id, at: Date.now(), ok: false, error: String(e.message || e) });
        }
        if (state.view === 'network') render();
      }
      toast(failed ? `接続診断が終わりました（${failed}台は未取得）` : '接続診断が終わりました');
    } catch (e) { toast(String(e.message || e), 6000); }
    finally { running = false; if (state.view === 'network') render(); }
  }
  function render() {
    setCrumbs([{ icon: 'sync', label: 'ネットワーク' }]);
    const supported = typeof window.tune.networkCheck === 'function';
    const head = UI.head({ icon: 'sync', title: 'ネットワーク', desc: '経路・IP設定・DNS設定とHTTPSの到達性を、その場で診断します。IP・SSID・DNS名・通信本文は保存しません。', props: supported ? `<button class="btn" id="netBasic"${running ? ' disabled' : ''}>${running ? '診断中…' : '全機の設定状態を診断'}</button><button class="btn primary" id="netActive"${running ? ' disabled' : ''}>HTTPSを含めて全機を診断</button>` : '' });
    if (!supported) { page(`${head}<p class="note-line">接続診断はTauri版で利用できます。</p>`); return; }
    page(`${head}<p class="note-line">HTTPS診断はAppleとCloudflareへ短いリクエストを各1回送ります。表示する時間は応答全体の所要時間で、帯域の測定ではありません。VPN・プロキシ・接続先の応答も含みます。定期実行はせず、結果はこのアプリを閉じるまで保持します。</p>${UI.table({ cols: [{ label: '機体' }, { label: '経路' }, { label: 'IP設定' }, { label: 'DNS設定' }, { label: 'Apple' }, { label: 'Cloudflare' }, { label: '測定時刻' }, { label: '' }], rows: state.nodes, cls: 'scroll-x', empty: '台帳に機体がありません', row: (n) => {
      const r = results.get(n.id), d = r?.ok ? r.data : null;
      return `<td><b>${UI.esc(n.id)}</b>${n.shared ? UI.chip('共用機', 'gray') : ''}</td><td>${flag(d?.default_route_present)}</td><td>${flag(d?.ip_address_present)}</td><td>${flag(d?.dns_configured)}</td><td>${https(d?.https?.named)}</td><td>${https(d?.https?.fixed_ip)}</td><td>${r ? `${UI.esc(fmtTime(r.at))}${!r.ok ? `<br>${UI.esc(r.error || '診断失敗')}` : ''}` : '未実行'}</td><td><button class="btn small" data-net-node="${UI.esc(n.id)}"${running ? ' disabled' : ''}>HTTPS診断</button></td>`;
    } })}`);
    document.querySelector('#netBasic').onclick = () => run(state.nodes.map(n => n.id), false);
    document.querySelector('#netActive').onclick = () => run(state.nodes.map(n => n.id), true);
    document.querySelectorAll('[data-net-node]').forEach(b => { b.onclick = () => run([b.dataset.netNode], true); });
  }
  (window.KT_VIEWS = window.KT_VIEWS || []).push({ key: 'network', label: 'ネットワーク', icon: 'sync', render });
})();
