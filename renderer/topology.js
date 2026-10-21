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
  const match = (value) => state.graph?.window ? true : !state.filter || String(value || '').toLocaleLowerCase().includes(state.filter.toLocaleLowerCase());

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
    const a = entity.attributes || {};
    const ports = Array.isArray(a.ports) && a.ports.length ? a.ports.map(p => `${p.host_port} → ${p.container_port}/${p.protocol}`).join(', ') : a.ports_total === 0 ? 'none observed' : 'unknown';
    const omitted = Number.isSafeInteger(a.ports_omitted) && a.ports_omitted > 0 ? `; 省略 ${a.ports_omitted}` : '';
    const docker = entity.kind === 'docker_container' ? `state: ${a.state ?? 'unknown'}; health: ${a.health ?? 'unknown'}; memory: ${a.memory_bytes ?? 'unknown'}; nano_cpus: ${a.nano_cpus ?? 'unknown'}; ports: ${ports}${omitted}; log candidate: ${a.log_unbounded_candidate === true ? 'yes' : a.log_unbounded_candidate === false ? 'no' : 'unknown'}${a.attributes_truncated === true ? '; 表示属性を限定' : ''}` : '';
    const evidence = entity.attributes?.content_sha256 || entity.attributes?.value_hash || entity.attributes?.revision || '';
    return `<td>${esc(entity.kind || '')}</td><td>${esc(entity.label || '')}</td><td class="muted"><code>${esc((entity.id || '').slice(-16))}</code></td><td class="muted"><code>${esc(docker || String(evidence).slice(-16))}</code></td>`;
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
    const storedKinds = Array.isArray(state.graph?.window?.available_kinds) ? state.graph.window.available_kinds.filter(k => typeof k === 'string').slice(0, 32) : [];
    const kinds = [...new Set([...storedKinds, ...entities.map((entity) => entity.kind).filter(Boolean), ...(state.kind === 'all' ? [] : [state.kind])])].sort();
    return `<div class="toolbar" data-no-drag><input id="topologyFilter" type="search" placeholder="機体・repo・名前で絞り込み" value="${esc(state.filter)}" aria-label="構成図を絞り込み"><select id="topologyKind" aria-label="実体の種類"><option value="all">すべての種類</option>${kinds.map((kind) => `<option value="${esc(kind)}"${state.kind === kind ? ' selected' : ''}>${esc(kind)}</option>`).join('')}</select></div>`;
  }
  const candidateGroups = (graph) => Array.isArray(graph?.candidates)
    ? graph.candidates.filter(c => c && typeof c === 'object' && c.status === 'partial' && Array.isArray(c.entities) && Array.isArray(c.edges)) : [];
  const factTime = (value) => Number.isSafeInteger(value) && value >= 0 && Number.isFinite(new Date(value).getTime()) ? new Date(value).toLocaleString('ja-JP') : '不明';
  function candidateBody(groups, view, entityBudget = DISPLAY_LIMIT, edgeBudget = DISPLAY_LIMIT) {
    if (!groups.length) return '';
    const tables = groups.slice(0, 64).map(candidate => {
      const entities = candidate.entities.filter(e => e && typeof e.id === 'string');
      const ids = new Set(entities.map(e => e.id));
      const filtered = entities.filter(e => (!view.entities || view.entities.has(e.kind)) && (state.kind === 'all' || e.kind === state.kind) && match(`${e.label} ${e.id} ${e.kind}`));
      const filteredIds = new Set(filtered.map(e => e.id));
      const edges = candidate.edges.filter(e => e && ids.has(e.from_id || e.from) && ids.has(e.to_id || e.to)
        && (!view.edges || view.edges.has(e.kind)) && match(`${e.kind} ${e.from_id || e.from} ${e.to_id || e.to}`)
        && (state.kind === 'all' || filteredIds.has(e.from_id || e.from) || filteredIds.has(e.to_id || e.to)));
      const shownEntities = filtered.slice(0, entityBudget);
      const selectedIds = new Set(shownEntities.map(e => e.id));
      const shownEdges = edges.filter(e => selectedIds.has(e.from_id || e.from) && selectedIds.has(e.to_id || e.to)).slice(0, edgeBudget);
      entityBudget -= shownEntities.length;
      edgeBudget -= shownEdges.length;
      const omitted = (shown, total) => total > shown ? UI.callout({ tone: 'gray', title: `表示 ${shown} / 候補 ${total}`, body: '候補の全件は公開 export に含まれません。絞り込みで確認できます。' }) : '';
      const latest = candidate.is_latest_attempt === true ? '最新の候補' : '以前の候補 — 最新の試行では更新されていません';
      return `${UI.section(latest, '候補にない対象の削除は判断できません')}${UI.props([
        ['範囲', esc(candidate.scope_id || '')], ['試行', esc(candidate.attempt_id || '')], ['状態', UI.chip('一部のみ', 'orange', { dot: true })],
        ['事実の観測時刻', esc(factTime(candidate.facts_observed_at_ms))], ['試行開始', esc(factTime(candidate.attempt_started_at_ms))], ['試行終了', esc(factTime(candidate.attempt_finished_at_ms))],
      ])}${UI.table({ cols: [{label:'型'}, {label:'名前'}, {label:'ID'}, {label:'根拠 / Docker 状態'}], rows: shownEntities, row: rowEntity, empty: '条件に合う候補の実体はありません' })}
        ${omitted(shownEntities.length, filtered.length)}
        ${UI.table({ cols: [{label:'種類'}, {label:'起点'}, {label:'終点'}], rows: shownEdges, row: rowEdge, empty: '条件に合う候補の関係はありません' })}
        ${omitted(shownEdges.length, edges.length)}`;
    }).join('');
    return `${UI.section('部分観測の候補', '正常記録とは独立した部分観測です。取得元と収集終了の確認を通った候補だけを表示します')}${tables}
      ${groups.length > 64 ? UI.callout({ tone: 'gray', title: `表示候補 64 / 保存候補 ${groups.length}`, body: '表示上限により未表示の候補があります。' }) : ''}`;
  }
  function graphBody(graph) {
    const scopes = graph?.scopes || [];
    const history = graph?.history || [];
    const changes = graph?.changes || [];
    const entities = graph?.entities || [];
    const edges = graph?.edges || [];
    const views = {
      all: { label: '全体', entities: null, edges: null, hint: '収集済みの全関係' },
      hosts: { label: '機体配置', entities: new Set(['docker_engine', 'docker_container', 'docker_network', 'docker_volume', 'docker_mount', 'host', 'environment', 'execution_environment', 'repository', 'checkout', 'worktree']), edges: new Set(['docker_contains', 'docker_network_member', 'docker_mounts', 'contains_environment', 'contains_checkout', 'checkout_of', 'worktree_of', 'contains_workspace']), hint: '機体・環境・checkout の配置' },
      dependencies: { label: '依存', entities: new Set(['repository', 'checkout', 'workspace', 'package']), edges: new Set(['contains_checkout', 'contains_workspace', 'contains_package', 'workspace_member', 'declares_dependency', 'resolves_to']), hint: '宣言と観測済み解決先の依存' },
      cicd: { label: 'CI/CD', entities: new Set(['repository', 'workflow', 'job', 'artifact', 'deployment']), edges: new Set(['has_workflow', 'has_job', 'needs_job', 'uses_action', 'uses_workflow', 'runs_on', 'deploys_to', 'stores_in']), hint: 'workflow と job の観測済み関係' },
    };
    const view = views[state.tab] || views.all;
    const groups = candidateGroups(graph);
    const candidateKinds = groups.flatMap(c => c.entities.filter(e => e && (!view.entities || view.entities.has(e.kind))));
    const candidates = entities.filter((entity) => !view.entities || view.entities.has(entity.kind));
    const filteredEntities = candidates.filter((entity) => (state.kind === 'all' || entity.kind === state.kind) && match(`${entity.label} ${entity.id} ${entity.kind}`));
    const filteredIds = new Set(filteredEntities.map((entity) => entity.id));
    const filteredEdges = edges.filter((edge) => (!view.edges || view.edges.has(edge.kind)) && match(`${edge.kind} ${edge.from_id || edge.from} ${edge.to_id || edge.to}`) && (state.kind === 'all' || filteredIds.has(edge.from_id || edge.from) || filteredIds.has(edge.to_id || edge.to)));
    const shownEntities = visible(filteredEntities);
    const shownIds = new Set(shownEntities.map(e => e.id));
    const shownEdges = visible(filteredEdges.filter(e => shownIds.has(e.from_id || e.from) && shownIds.has(e.to_id || e.to)));
    const receiptTimes = new Map();
    for (const scope of scopes) {
      if (Number.isSafeInteger(scope.observed_at) && scope.observed_at >= 0) receiptTimes.set(scope.scope_id, Math.max(receiptTimes.get(scope.scope_id) ?? -1, scope.observed_at));
    }
    const receiptTime = (row) => {
      const ids = Array.isArray(row.scope_ids) ? row.scope_ids : [row.scope_id];
      const latest = ids.reduce((at, id) => Math.max(at, receiptTimes.get(id) ?? -1), -1);
      return latest >= 0 ? latest : null;
    };
    const acceptedTimes = (row) => `<td class="muted">${esc(factTime(row.facts_observed_at_ms))}</td><td class="muted">不明</td><td class="muted">${esc(factTime(receiptTime(row)))}</td>`;
    const incomplete = scopes.some((scope) => scope.status !== 'complete');
    const currentness = incomplete
      ? UI.callout({ tone: 'orange', title: '未完の範囲があります', body: '表示中の実体は直近の成功グラフを保持しています。未完範囲では、現在の存在・削除・変更を確認できていません。' })
      : UI.callout({ tone: 'gray', title: '観測時点の構成図です', body: '完了範囲でも、表示は最後に観測した時点の記録です。リアルタイムの存在判定ではありません。' });
    const windowInfo = graph?.window;
    const count = (value) => Number.isSafeInteger(value) && value >= 0 ? String(value) : '不明';
    const totals = windowInfo?.stored_totals || {};
    const filteredTotals = windowInfo?.filtered_stored_totals || {};
    const windowNote = windowInfo ? UI.callout({ tone: 'gray', title: '表示範囲と検査について', body:
      `表示中の項目だけを検査しています。表示外の項目と保存原本全体は未検査です。保存項目数（全件を検査した件数ではありません）: 正常記録 ${count(totals.accepted_entities)} 件 / 関係 ${count(totals.accepted_edges)} 件、候補 ${count(totals.candidate_entities)} 件 / 関係 ${count(totals.candidate_edges)} 件。条件に合う保存項目数: 正常記録 ${count(filteredTotals.accepted_entities)} 件、候補 ${count(filteredTotals.candidate_entities)} 件。関係は表示中の項目どうしに限ります。${windowInfo.truncated ? '表示上限や端点の省略により、表示していない項目があります。' : ''}${Number.isSafeInteger(windowInfo.corrupt_scopes_hidden) && windowInfo.corrupt_scopes_hidden > 0 ? `候補の記録に不整合があるため ${windowInfo.corrupt_scopes_hidden} 件の範囲を表示していません。` : ''}${windowInfo.candidate_unavailable_reasons?.length ? '候補は未作成・旧形式・記録の不整合などにより確認できません。検証済みの 0 件を意味しません。' : ''}${windowInfo.candidate_omission_reasons?.includes('expired_projection') ? '期限切れの候補を省略しました。' : ''}${windowInfo.candidate_omission_reasons?.includes('superseded_projection') ? '新しい完了記録がある候補を省略しました。' : ''}検索は長さを制限した表示内容が対象です。属性・ports・履歴の一部を省略しています。`
    }) : '';

    return `${windowNote}${UI.props([
      ['最新範囲', esc(scopes.length)], ['履歴', esc(history.length)], ['実体', esc(entities.length)], ['関係', esc(edges.length)], ['形式', `<code>${esc(graph?.schema || 'katala_topology.v1')}</code>`],
    ])}${currentness}<div class="toolbar" data-no-drag>${tabButton('all', '全体')}${tabButton('hosts', '機体配置')}${tabButton('dependencies', '依存')}${tabButton('cicd', 'CI/CD')}<button class="btn" data-topology-export="json">公開 JSON をコピー</button><button class="btn" data-topology-export="mermaid">Mermaid をコピー</button></div>
      ${filters([...candidates, ...candidateKinds])}
      ${UI.section('最後の正常記録', '完了した観測だけを反映した構成です。実際の試行時刻は未取得です')}
      ${UI.section(view.label, `${view.hint}。未観測の対象は推測で結びません`)}
      ${UI.table({ cols: [{label:'型'}, {label:'名前'}, {label:'ID'}, {label:'根拠 / Docker 状態'}, {label:'事実の観測時刻'}, {label:'最新の試行時刻'}, {label:'収集記録の観測時刻'}], rows: shownEntities, row: entity => rowEntity(entity) + acceptedTimes(entity), empty: '条件に合う実体はありません' })}
      ${noteLimit(shownEntities.length, filteredEntities.length)}
      ${UI.section('関係', '起点・終点は内部の opaque ID です')}
      ${UI.table({ cols: [{label:'種類'}, {label:'起点'}, {label:'終点'}, {label:'事実の観測時刻'}, {label:'最新の試行時刻'}, {label:'収集記録の観測時刻'}], rows: shownEdges, row: edge => rowEdge(edge) + acceptedTimes(edge), empty: '条件に合う関係はありません' })}
      ${noteLimit(shownEdges.length, filteredEdges.length)}
      ${candidateBody(groups, view, DISPLAY_LIMIT - shownEntities.length, DISPLAY_LIMIT - shownEdges.length)}
      ${UI.section('最新の収集範囲', '失敗理由は安全な code、件数は entry / file / byte です。直近成功は未完更新で消しません')}
      ${UI.table({ cols: [{label:'範囲'}, {label:'状態'}, {label:'観測時刻'}, {label:'理由'}, {label:'entry / file / byte'}], rows: visible(scopes), row: rowScope, empty: 'まだ収集していません' })}
      ${noteLimit(visible(scopes).length, scopes.length)}
      ${UI.section('変更履歴', '保存済みの semantic change 行。観測時刻だけの更新は変更として数えません')}
      ${UI.table({ cols: [{label:'範囲'}, {label:'対象'}, {label:'変更'}, {label:'記録時刻'}], rows: visible(changes), row: rowChange, empty: '保存済みの変更はありません' })}
      ${noteLimit(visible(changes).length, changes.length)}`;
  }
  const request = () => ({ tab: state.tab, kind: state.kind, filter: state.filter });
  async function readGraph() {
    const query = request();
    let graph;
    try { graph = await T.topologyView(query); }
    catch (error) { if (JSON.stringify(query) === JSON.stringify(request())) throw error; state.reloadPending = true; return; }
    if (JSON.stringify(query) === JSON.stringify(request())) state.graph = graph;
    else state.reloadPending = true;
  }
  async function load() {
    if (typeof T?.topologyView !== 'function') return;
    if (state.loading) { state.reloadPending = true; return; }
    state.loading = true;
    try { await readGraph(); } catch (error) { state.graph = { error: String(error?.message || error) }; }
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
    try { state.receipt = await T.topologyRefresh(); await readGraph(); }
    catch (error) { state.receipt = { status: 'failed', error: String(error?.message || error) }; }
    finally { state.loading = false; if (active()) render(); if (state.reloadPending) { state.reloadPending = false; void load(); } }
  }
  async function exportTopology(format) {
    if (typeof T?.topologyExport !== 'function' || typeof T?.copy !== 'function') return;
    try {
      await T.copy(await T.topologyExport(state.tab, format));
      state.exportStatus = `${format === 'json' ? '公開 JSON' : 'Mermaid'} をクリップボードへコピーしました`;
    } catch (error) { state.exportStatus = `export に失敗しました: ${String(error?.message || error)}`; }
    if (active()) render();
  }
  function queryChanged() { state.graph = null; render(); void load(); }
  function bindControls() {
    document.querySelector('#topologyRefresh')?.addEventListener('click', refresh);
    document.querySelectorAll('[data-topology-export]').forEach((button) => button.addEventListener('click', () => exportTopology(button.dataset.topologyExport)));
    document.querySelectorAll('[data-topology-tab]').forEach((button) => button.addEventListener('click', () => { state.tab = button.dataset.topologyTab; state.kind = 'all'; queryChanged(); }));
    document.querySelector('#topologyFilter')?.addEventListener('input', (event) => { state.filter = event.target.value; queryChanged(); });
    document.querySelector('#topologyKind')?.addEventListener('change', (event) => { state.kind = event.target.value; queryChanged(); });
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
      ${state.graph?.error ? UI.callout({ tone:'red', title:'構成図を読み込めません', body:esc(state.graph.error) }) : ''}${graphBody(state.graph?.error ? null : state.graph)}`);
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
