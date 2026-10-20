'use strict';
/* Topology is an internal, read-only view. Public JSON/Mermaid exports apply a
 * separate anonymous-label policy; an empty public label is never a missing
 * collected entity. */
(() => {
  UI.ICON.topology = '<svg viewBox="0 0 20 20"><circle cx="4" cy="10" r="1.8"/><circle cx="15.5" cy="4" r="1.8"/><circle cx="15.5" cy="16" r="1.8"/><path d="M5.7 9.1 13.8 4.9M5.7 10.9l8.1 4.2"/></svg>';
  const T = window.tune;
  const esc = UI.esc;
  const DISPLAY_LIMIT = 200;
  const state = { graph: null, receipt: null, loading: false, reloadPending: false, tab: 'all', filter: '', kind: 'all', exportStatus: '' };
  const active = () => document.querySelector('.side-item.sel')?.dataset.view === 'topology' && document.visibilityState === 'visible';
  const status = (value) => ({ complete: ['完了', 'green'], partial: ['一部のみ', 'orange'], failed: ['失敗', 'red'], excluded: ['対象外', 'gray'] }[value] || ['未取得', 'gray']);
  const visible = (rows) => rows.slice(0, DISPLAY_LIMIT);
  const noteLimit = (shown, total) => total > shown ? UI.callout({ tone: 'gray', title: `表示 ${shown} / 収集 ${total}`, body: '未表示の項目があります。全件は public JSON または Mermaid export で確認できます。' }) : '';
  const match = (value) => !state.filter || String(value || '').toLocaleLowerCase().includes(state.filter.toLocaleLowerCase());

  function scopeFailure(scope) {
    const failures = Array.isArray(scope.failures) ? scope.failures : [];
    return failures.map((failure) => failure?.code || failure).filter(Boolean).join(', ');
  }
  function rowScope(scope) {
    const [label, tone] = status(scope.status);
    const counters = scope.counters || {};
    const observed = scope.observed_at || scope.at;
    return `<td><code>${esc(scope.scope_id || '')}</code></td><td>${UI.chip(label, tone, { dot: true })}</td><td class="muted">${esc(observed ? new Date(observed).toLocaleString('ja-JP') : '')}</td><td class="muted">${esc(scopeFailure(scope) || '-')}</td><td class="muted">${esc(counters.entries ?? '-')} / ${esc(counters.files ?? '-')} / ${esc(counters.bytes ?? '-')}</td>`;
  }
  function rowEntity(entity) {
    const evidence = entity.attributes?.content_sha256 || entity.attributes?.value_hash || entity.attributes?.revision || '';
    return `<td>${esc(entity.kind || '')}</td><td>${esc(entity.label || '')}</td><td class="muted"><code>${esc((entity.id || '').slice(-16))}</code></td><td class="muted"><code>${esc(String(evidence).slice(-16))}</code></td>`;
  }
  function rowChange(change) {
    return `<td><code>${esc(change.scope_id || '')}</code></td><td>${esc(change.kind || '')}</td><td>${esc(change.change_kind || '')}</td><td class="muted">${esc(change.at ? new Date(change.at).toLocaleString('ja-JP') : '')}</td>`;
  }
  function rowEdge(edge) {
    return `<td>${esc(edge.kind || '')}</td><td><code>${esc((edge.from_id || edge.from || '').slice(-16))}</code></td><td><code>${esc((edge.to_id || edge.to || '').slice(-16))}</code></td>`;
  }
  function tabButton(id, label) {
    return `<button class="btn${state.tab === id ? ' primary' : ''}" data-topology-tab="${id}">${label}</button>`;
  }
  function filters(entities) {
    const kinds = [...new Set(entities.map((entity) => entity.kind).filter(Boolean))].sort();
    return `<div class="toolbar" data-no-drag><input id="topologyFilter" type="search" placeholder="機体・repo・名前で絞り込み" value="${esc(state.filter)}" aria-label="構成図を絞り込み"><select id="topologyKind" aria-label="実体の種類"><option value="all">すべての種類</option>${kinds.map((kind) => `<option value="${esc(kind)}"${state.kind === kind ? ' selected' : ''}>${esc(kind)}</option>`).join('')}</select></div>`;
  }
  function graphBody(graph) {
    const scopes = graph?.scopes || [];
    const history = graph?.history || [];
    const changes = graph?.changes || [];
    const entities = graph?.entities || [];
    const edges = graph?.edges || [];
    const views = {
      all: { label: '全体', entities: null, edges: null, hint: '収集済みの全関係' },
      hosts: { label: '機体配置', entities: new Set(['host', 'environment', 'repository', 'checkout', 'worktree']), edges: new Set(['contains_environment', 'contains_checkout', 'checkout_of', 'worktree_of', 'contains_workspace']), hint: '機体・環境・checkout の配置' },
      dependencies: { label: '依存', entities: new Set(['repository', 'checkout', 'workspace', 'package']), edges: new Set(['contains_checkout', 'contains_workspace', 'contains_package', 'workspace_member', 'declares_dependency', 'resolves_to']), hint: '宣言と観測済み解決先の依存' },
      cicd: { label: 'CI/CD', entities: new Set(['repository', 'workflow', 'job', 'artifact', 'deployment']), edges: new Set(['has_workflow', 'has_job', 'needs_job', 'uses_action', 'uses_workflow', 'runs_on', 'deploys_to', 'stores_in']), hint: 'workflow と job の観測済み関係' },
    };
    const view = views[state.tab] || views.all;
    const candidates = entities.filter((entity) => !view.entities || view.entities.has(entity.kind));
    const filteredEntities = candidates.filter((entity) => (state.kind === 'all' || entity.kind === state.kind) && match(`${entity.label} ${entity.id} ${entity.kind}`));
    const filteredIds = new Set(filteredEntities.map((entity) => entity.id));
    const filteredEdges = edges.filter((edge) => (!view.edges || view.edges.has(edge.kind)) && match(`${edge.kind} ${edge.from_id || edge.from} ${edge.to_id || edge.to}`) && (state.kind === 'all' || filteredIds.has(edge.from_id || edge.from) || filteredIds.has(edge.to_id || edge.to)));
    const shownEntities = visible(filteredEntities);
    const shownEdges = visible(filteredEdges);
    const incomplete = scopes.some((scope) => scope.status !== 'complete');
    const currentness = incomplete
      ? UI.callout({ tone: 'orange', title: '未完の範囲があります', body: '表示中の実体は直近の成功グラフを保持しています。未完範囲では、現在の存在・削除・変更を確認できていません。' })
      : UI.callout({ tone: 'gray', title: '観測時点の構成図です', body: '完了範囲でも、表示は最後に観測した時点の記録です。リアルタイムの存在判定ではありません。' });
    return `${UI.props([
      ['最新範囲', esc(scopes.length)], ['履歴', esc(history.length)], ['実体', esc(entities.length)], ['関係', esc(edges.length)], ['形式', `<code>${esc(graph?.schema || 'katala_topology.v1')}</code>`],
    ])}${currentness}<div class="toolbar" data-no-drag>${tabButton('all', '全体')}${tabButton('hosts', '機体配置')}${tabButton('dependencies', '依存')}${tabButton('cicd', 'CI/CD')}<button class="btn" data-topology-export="json">公開 JSON をコピー</button><button class="btn" data-topology-export="mermaid">Mermaid をコピー</button></div>
      ${filters(candidates)}
      ${UI.section(view.label, `${view.hint}。未観測の対象は推測で結びません`)}
      ${UI.table({ cols: [{label:'型'}, {label:'名前'}, {label:'ID'}, {label:'根拠 SHA / revision'}], rows: shownEntities, row: rowEntity, empty: '条件に合う実体はありません' })}
      ${noteLimit(shownEntities.length, filteredEntities.length)}
      ${UI.section('関係', '起点・終点は内部の opaque ID です')}
      ${UI.table({ cols: [{label:'種類'}, {label:'起点'}, {label:'終点'}], rows: shownEdges, row: rowEdge, empty: '条件に合う関係はありません' })}
      ${noteLimit(shownEdges.length, filteredEdges.length)}
      ${UI.section('最新の収集範囲', '失敗理由は安全な code、件数は entry / file / byte です。直近成功は未完更新で消しません')}
      ${UI.table({ cols: [{label:'範囲'}, {label:'状態'}, {label:'観測時刻'}, {label:'理由'}, {label:'entry / file / byte'}], rows: visible(scopes), row: rowScope, empty: 'まだ収集していません' })}
      ${noteLimit(visible(scopes).length, scopes.length)}
      ${UI.section('変更履歴', '保存済みの semantic change 行。観測時刻だけの更新は変更として数えません')}
      ${UI.table({ cols: [{label:'範囲'}, {label:'対象'}, {label:'変更'}, {label:'記録時刻'}], rows: visible(changes), row: rowChange, empty: '保存済みの変更はありません' })}
      ${noteLimit(visible(changes).length, changes.length)}`;
  }
  async function load() {
    if (typeof T?.topologyView !== 'function') return;
    if (state.loading) { state.reloadPending = true; return; }
    state.loading = true;
    try { state.graph = await T.topologyView(); } catch (error) { state.graph = { error: String(error?.message || error) }; }
    finally {
      state.loading = false;
      if (active()) render();
      if (state.reloadPending) { state.reloadPending = false; void load(); }
    }
  }
  async function refresh() {
    if (state.loading || typeof T?.topologyRefresh !== 'function') return;
    state.loading = true;
    render();
    try { state.receipt = await T.topologyRefresh(); state.graph = await T.topologyView(); }
    catch (error) { state.receipt = { status: 'failed', error: String(error?.message || error) }; }
    finally { state.loading = false; if (active()) render(); }
  }
  async function exportTopology(format) {
    if (typeof T?.topologyExport !== 'function' || typeof T?.copy !== 'function') return;
    try {
      await T.copy(await T.topologyExport(state.tab, format));
      state.exportStatus = `${format === 'json' ? '公開 JSON' : 'Mermaid'} をクリップボードへコピーしました`;
    } catch (error) { state.exportStatus = `export に失敗しました: ${String(error?.message || error)}`; }
    render();
  }
  function bindControls() {
    document.querySelector('#topologyRefresh')?.addEventListener('click', refresh);
    document.querySelectorAll('[data-topology-export]').forEach((button) => button.addEventListener('click', () => exportTopology(button.dataset.topologyExport)));
    document.querySelectorAll('[data-topology-tab]').forEach((button) => button.addEventListener('click', () => { state.tab = button.dataset.topologyTab; render(); }));
    document.querySelector('#topologyFilter')?.addEventListener('input', (event) => { state.filter = event.target.value; render(); });
    document.querySelector('#topologyKind')?.addEventListener('change', (event) => { state.kind = event.target.value; render(); });
  }
  function render() {
    if (typeof setCrumbs !== 'function' || typeof page !== 'function') return;
    const priorFilter = document.querySelector('#topologyFilter');
    const restoreFilter = priorFilter && document.activeElement === priorFilter ? { start: priorFilter.selectionStart, end: priorFilter.selectionEnd } : null;
    setCrumbs([{ icon: 'topology', label: '構成図' }]);
    const unavailable = typeof T?.topologyView !== 'function';
    const receipt = state.receipt;
    const receiptHtml = receipt ? UI.callout({ tone: receipt.status === 'complete' ? 'green' : receipt.status === 'disabled' ? 'gray' : 'orange', title: receipt.status === 'complete' ? '更新しました' : receipt.status === 'disabled' ? '収集は無効です' : '更新結果を確認してください', body: `<code>${esc(JSON.stringify({ status: receipt.status, scopes: receipt.scopes?.length, changes: receipt.changes, error: receipt.error }))}</code>` }) : '';
    const exportHtml = state.exportStatus ? UI.callout({ tone: state.exportStatus.includes('失敗') ? 'red' : 'gray', title: 'export', body: esc(state.exportStatus) }) : '';
    page(`${UI.head({ icon:'topology', title:'構成図', desc:'内部表示は収集済みの識別子を使います。公開 JSON/Mermaid export は匿名化した別形式です。更新は read-only です。', actions: unavailable ? '' : `<button class="btn primary" id="topologyRefresh" ${state.loading ? 'disabled' : ''}>${state.loading ? '更新中…' : '今すぐ更新'}</button>` })}
      ${unavailable ? UI.callout({ tone:'gray', title:'この画面は利用できません', body:'現在の実行環境は Topology API を提供していません。' }) : receiptHtml}${exportHtml}
      ${state.graph?.error ? UI.callout({ tone:'red', title:'構成図を読み込めません', body:esc(state.graph.error) }) : graphBody(state.graph)}`);
    bindControls();
    if (restoreFilter) {
      const filter = document.querySelector('#topologyFilter');
      filter?.focus();
      filter?.setSelectionRange(restoreFilter.start, restoreFilter.end);
    }
  }
  (window.KT_VIEWS = window.KT_VIEWS || []).push({ key: 'topology', label: '構成図', icon: 'topology', render, leave: () => {} });
  document.addEventListener('visibilitychange', () => { if (active()) load(); });
  if (typeof T?.onTopologyUpdated === 'function') T.onTopologyUpdated((receipt) => { state.receipt = receipt; load(); });
  load();
})();
