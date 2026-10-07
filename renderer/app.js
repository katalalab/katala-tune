'use strict';
// 画面側。データは window.tune（preload）からだけ受け取り、HTML へは必ず esc() を通して入れる。
// 部品は ui.js（ページ見出し・表・チップ・コールアウト・トグルなど）と charts.js（棒・リング・折れ線など）。
// 本体（Electron / Tauri）の API は直接呼ばない。呼び出しは window.tune.* だけ

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const { esc, icon } = UI;
// 文中の `…` をインラインのコードとして見せる（先にエスケープしてから置き換える）
const rich = (s) => esc(s).replace(/`([^`\n]+)`/g, '<code>$1</code>');
// 長い ID（com.example.job、\\Path\\Task など）は区切り記号の後ろで折り返す
const wbr = (s) => esc(s).replace(/([.\\/_-])/g, '$1<wbr>');
const fmtTime = (ms) => (ms ? new Date(ms).toLocaleString('ja-JP', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '-');
const ago = (ms) => { if (!ms) return '-'; const s = (Date.now() - ms) / 1000; return s < 90 ? `${Math.max(1, Math.round(s))}秒前` : s < 5400 ? `${Math.round(s / 60)}分前` : s < 172800 ? `${Math.round(s / 3600)}時間前` : `${Math.round(s / 86400)}日前`; };
const SEV_LABEL = { critical: '重大', warn: '注意', info: '提案', error: 'エラー' };
const SEV_TONE = { critical: 'red', warn: 'orange', info: 'blue' };
const LEVEL_LABEL = { critical: '重大', error: 'エラー', warn: '注意', info: '情報' };
const LEVEL_TONE = { critical: 'red', error: 'red', warn: 'orange', info: 'default' };
const LEVEL_SERIES = [
  { key: 'critical', label: '重大', tone: 'crit' }, { key: 'error', label: 'エラー', tone: 'err' },
  { key: 'warn', label: '注意', tone: 'warn' }, { key: 'info', label: '情報', tone: 'unknown' },
];
const LEVEL_BAR = { critical: 'crit', error: 'err', warn: 'warn', info: 'unknown' };
const STATUS_LABEL = { ok: '正常', warn: '注意', fail: '異常', unknown: '不明' };
const CAT_LABEL = { cpu: 'CPU', memory: 'メモリ', disk: 'ディスク', thermal: '熱', power: '電源', stability: '安定性', background: '常駐・その他', security: 'セキュリティ' };
const SOURCE_LABEL = { win_system: 'System', win_application: 'Application', neonmonitor: 'NeonMonitor', mac_diag: 'DiagnosticReports', mac_kernel: 'カーネル' };
// メーターのしきい値（lib/rules.js の判定に合わせる。ディスクとメモリは使用率に直したもの）
const TH = { cpu: { warn: 60, crit: 85 }, mem: { warn: 80, crit: 90 }, disk: { warn: 90, crit: 95 } };
const NAV = [['overview', '概要', 'overview'], ['status', '状態', 'status'], ['resources', 'リソース', 'resources'], ['procs', 'プロセス', 'procs'],
  ['jobs', 'スケジュール', 'jobs'], ['logs', 'ログ', 'logs'], ['actions', '実行記録', 'actions']];

const state = {
  cfg: null, nodes: [], results: {}, fleet: null, view: 'overview', tab: 'findings', busy: new Set(), syncing: false,
  logFilter: { q: '', node_id: '', level: '', since: 7 },
  jobFilter: { node_id: '', failing: false, q: '' },
  procFilter: { node_id: '', q: '', sort: 'cpu' },
  status: null,
};

// 非同期で描く画面が、待っている間に別の画面へ移ったら古い結果で上書きしない
let renderSeq = 0;
const ticket = () => ++renderSeq;
const stale = (t) => t !== renderSeq;

let toastTimer;
function toast(t, ms = 3200) {
  const el = $('#toast');
  el.textContent = t;
  el.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('show'), ms);
}

const fleetNode = (id) => state.fleet?.nodes?.find((n) => n.node_id === id);
const counts = (r) => { const c = { critical: 0, warn: 0, info: 0 }; (r?.findings || []).forEach((f) => { c[f.severity]++; }); return c; };
const osIcon = (n) => (n.os === 'windows' ? 'win' : 'mac');
const sysDisk = (d) => (d?.disk || []).find((x) => x.mount === '/' || /^C:/i.test(x.mount));
const scoreClass = (s) => `sc-${Charts.scoreTone(s)}`;
function nodeDot(n) {
  if (state.busy.has(n.id)) return 'busy';
  const r = state.results[n.id];
  if (r && !r.ok) return 'crit';
  if (!r) { const fl = fleetNode(n.id); return fl ? (fl.state === 'online' ? 'ok' : 'warn') : ''; }
  const c = counts(r);
  return c.critical ? 'crit' : c.warn ? 'warn' : 'ok';
}

// 検索欄: 打ち終わってから描き直し、カーソルを末尾に戻す。待つ間に別の画面へ移っていたら、値だけ覚えて描き直さない
function bindSearch(sel, view, apply, rerender, ms = 250) {
  let t;
  $(sel).oninput = (e) => {
    clearTimeout(t);
    t = setTimeout(async () => {
      apply(e.target.value);
      if (state.view !== view) return;
      await rerender();
      const i = $(sel);
      if (i) { i.focus(); i.setSelectionRange(i.value.length, i.value.length); }
    }, ms);
  };
}

function page(html) { $('#page').innerHTML = `<div class="page-inner">${html}</div>`; }
function bindGoto(root = document) { $$('[data-goto]', root).forEach((el) => { el.onclick = () => go('node:' + el.dataset.goto); }); }

// ---- サイドバーとパンくず ----
function renderSidebar() {
  const c = state.status?.counts;
  const badge = c ? (c.fail ? `<span class="badge bad" title="異常 ${c.fail} 件">${c.fail}</span>` : c.warn ? `<span class="badge warn" title="注意 ${c.warn} 件">${c.warn}</span>` : '') : '';
  $('#sideNav').innerHTML = NAV.map(([k, label, ic]) => `<button class="side-item${state.view === k ? ' sel' : ''}" data-view="${k}"${state.view === k ? ' aria-current="page"' : ''}>${icon(ic)}<span>${label}</span>${k === 'status' ? badge : ''}</button>`).join('');
  $('#sideNodes').innerHTML = state.nodes.map((n) => {
    const r = state.results[n.id];
    const sel = state.view === 'node:' + n.id;
    return `<button class="side-item${sel ? ' sel' : ''}" data-view="node:${esc(n.id)}"${sel ? ' aria-current="page"' : ''} title="${esc(`${n.id}${n.role ? `（${n.role}）` : ''}`)}">`
      + `<span class="dot ${nodeDot(n)}"></span><span>${esc(n.id)}</span><span class="badge">${r?.ok ? r.score : r ? '!' : ''}</span></button>`;
  }).join('');
  $('#sideCount').textContent = state.nodes.length ? String(state.nodes.length) : '';
  $$('.sidebar [data-view]').forEach((b) => { b.onclick = () => go(b.dataset.view); });
}

// list = [{ icon, label, view }]。最後が今の画面
function setCrumbs(list) {
  $('#crumbs').innerHTML = list.map((c, i) => {
    const last = i === list.length - 1;
    const inner = `${c.icon ? icon(c.icon) : ''}<span>${esc(c.label)}</span>`;
    return (i ? '<span class="crumb-sep">/</span>' : '') + (c.view && !last ? `<button class="crumb" data-crumb="${esc(c.view)}">${inner}</button>` : `<span class="crumb${last ? ' cur' : ''}">${inner}</span>`);
  }).join('');
  $$('[data-crumb]').forEach((b) => { b.onclick = () => go(b.dataset.crumb); });
  document.title = `${list.at(-1)?.label || ''} — Katala Tune`;
}

function go(view) {
  const moved = view !== state.view;
  state.view = view;
  if (!view.startsWith('node:')) state.tab = 'findings';
  render();
  if (moved) $('#page').scrollTop = 0;
}

function render() {
  renderSidebar();
  if (state.view === 'overview') return renderOverview();
  if (state.view === 'status') return renderStatus();
  if (state.view === 'resources') return renderResources();
  if (state.view === 'procs') return renderProcs();
  if (state.view === 'jobs') return renderJobs();
  if (state.view === 'logs') return renderLogs();
  if (state.view === 'actions') return renderActions();
  if (state.view.startsWith('node:')) return renderNode(state.view.slice(5));
}

// ---- 概要 ----
function stat(label, value, { sub = '', cls = '', ic = '' } = {}) {
  return `<div class="stat ${cls}"><div class="stat-k">${ic ? icon(ic) : ''}${esc(label)}</div><div class="stat-v">${value}</div>${sub ? `<div class="stat-s">${esc(sub)}</div>` : ''}</div>`;
}

function statusParts(c) {
  return [{ label: '異常', value: c.fail || 0, tone: 'crit' }, { label: '注意', value: c.warn || 0, tone: 'warn' }, { label: '正常', value: c.ok || 0, tone: 'ok' }, { label: '不明', value: c.unknown || 0, tone: 'unknown' }];
}

function cardHtml(n) {
  const r = state.results[n.id];
  const d = r?.ok ? r.data : null;
  const fl = fleetNode(n.id);
  const c = counts(r);
  const disk = sysDisk(d);
  const memUsed = d?.memory?.available_pct != null ? 100 - d.memory.available_pct : fl?.metrics?.mem ?? null;
  const cpu = d?.cpu_busy ?? fl?.metrics?.cpu ?? null;
  const diskUsed = disk ? 100 - disk.free_pct : fl?.metrics?.disk ?? null;
  const busy = state.busy.has(n.id);
  const cmp = r?.compare;
  const delta = cmp ? ` <span class="delta ${cmp.verdict}">${cmp.bench_ratio >= 1 ? '+' : ''}${((cmp.bench_ratio - 1) * 100).toFixed(0)}%</span>` : '';
  const tags = [n.local ? UI.chip('この機体', 'blue') : '', n.shared ? UI.chip('提案のみ', 'gray') : ''].join('');
  const sev = [c.critical ? UI.chip(`重大 ${c.critical}`, 'red') : '', c.warn ? UI.chip(`注意 ${c.warn}`, 'orange') : '', c.info ? UI.chip(`提案 ${c.info}`, 'blue') : '',
    r?.ok && !c.critical && !c.warn ? UI.chip('良好', 'green') : ''].join('');
  return `<div class="card${busy ? ' busy' : ''}" data-id="${esc(n.id)}" tabindex="0" role="button" aria-label="${esc(n.id)} の詳細を開く">
    <div class="card-top">
      <div class="card-id"><div class="card-name">${icon(osIcon(n))}<span>${esc(n.id)}</span></div>
        <div class="card-role">${n.role ? `<span>${esc(n.role)}</span>` : ''}${tags}</div></div>
      ${Charts.ring(r?.ok ? r.score : null)}
    </div>
    <div class="card-meters">
      ${Charts.meter(cpu, { label: 'CPU', ...TH.cpu, text: cpu != null ? `${Math.round(cpu)}%` : '-' })}
      ${Charts.meter(memUsed, { label: 'メモリ', ...TH.mem, text: memUsed != null ? `${Math.round(memUsed)}%` : '-' })}
      ${Charts.meter(diskUsed, { label: 'ディスク', ...TH.disk, text: disk ? `空き ${disk.free_gb} GB` : diskUsed != null ? `${Math.round(diskUsed)}%` : '-' })}
    </div>
    <div class="card-hw">${esc(d ? `${d.host.cpu} · ${d.host.cores} スレッド · ${d.memory.total_gb} GB` : n.os === 'macos' ? 'macOS' : 'Windows')}</div>
    <div class="card-foot">${sev}${d?.bench ? `<span title="1スレッドの固定計算。小さいほど速い">計測 ${esc(d.bench.median_ms)} ms${delta}</span>` : ''}
      <span class="right">${busy ? '分析中…' : r ? esc(ago(r.at)) : '未分析'}</span></div>
    ${r && !r.ok ? `<div class="err"><b>分析できませんでした</b>\n${esc(r.error)}</div>` : ''}
  </div>`;
}

function renderOverview() {
  ticket();
  const rs = state.nodes.map((n) => state.results[n.id]).filter((r) => r?.ok);
  const all = rs.flatMap((r) => r.findings);
  const crit = all.filter((f) => f.severity === 'critical').length;
  const warn = all.filter((f) => f.severity === 'warn').length;
  const avg = rs.length ? Math.round(rs.reduce((s, r) => s + r.score, 0) / rs.length) : null;
  const last = Math.max(0, ...rs.map((r) => r.at || 0));
  const fl = state.fleet?.summary;
  const sc = state.status?.counts;
  const sch = state.status?.schedule || state.cfg.schedule;
  const worst = rs.length ? rs.reduce((a, b) => (b.score < a.score ? b : a)) : null;
  setCrumbs([{ icon: 'overview', label: '概要' }]);
  page(`
    ${UI.head({ icon: 'overview', title: '概要', desc: `${state.nodes.length} 台の機体の状態と、優先して見るもの。${last ? `最終分析は ${fmtTime(last)}（${ago(last)}）` : 'まだ分析していません'}${sch ? `。自動スキャンは${sch.enabled ? ` ${sch.probe_minutes} 分ごと` : 'オフ'}` : ''}。` })}
    ${state.cfg.example ? UI.callout({ tone: 'orange', icon: 'info', title: '機体台帳がまだ見本のままです', body: `サイドバー下の「台帳を開く」で <code>${esc(state.cfg.file)}</code> を開き、~/.ssh/config の Host 名で機体を書いてください。保存したら「全機を分析」で読み直します。` }) : ''}
    ${state.cfg.error ? UI.callout({ tone: 'red', icon: 'alert', title: '台帳を読めません', body: esc(state.cfg.error) }) : ''}
    <div class="summary">
      <div class="summary-gauge">${Charts.gauge(avg, { label: '平均スコア', sub: worst && worst.score < 80 ? `最低 ${worst.node_id} ${worst.score}` : '' })}</div>
      <div>
        <div class="stats">
          ${stat('分析済み', `${rs.length}<small>/ ${state.nodes.length} 台</small>`, { sub: rs.length < state.nodes.length ? `${state.nodes.length - rs.length} 台は結果なし` : 'すべての機体', ic: 'machine' })}
          ${stat('重大', String(crit), { cls: crit ? 'crit' : '', sub: crit ? `${new Set(rs.filter((r) => counts(r).critical).map((r) => r.node_id)).size} 台` : 'なし', ic: 'alert' })}
          ${stat('注意', String(warn), { cls: warn ? 'warn' : '', sub: warn ? `${new Set(rs.filter((r) => counts(r).warn).map((r) => r.node_id)).size} 台` : 'なし', ic: 'info' })}
          ${fl ? stat('katala-fleet 要対応', esc(fl.attention), { ic: 'fleet' }) : ''}
        </div>
        ${sc ? `<div class="breakdown"><div class="bd-head"><span>状態の内訳（全機体とアプリの機能）</span><button class="link" data-view-link="status">「状態」を開く</button></div>${Charts.stacked(statusParts(sc), { label: '状態の内訳' })}</div>` : ''}
      </div>
    </div>
    ${UI.section('機体', 'クリックで詳細')}
    <div class="gallery">${state.nodes.map(cardHtml).join('')}</div>
    ${crit + warn ? `${UI.section('優先して見るもの', '重大と注意。クリックで機体の所見へ')}${topFindings()}` : ''}`);
  $$('.card').forEach((el) => {
    el.onclick = () => go('node:' + el.dataset.id);
    el.onkeydown = (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); go('node:' + el.dataset.id); } };
  });
  $$('[data-view-link]').forEach((el) => { el.onclick = () => go(el.dataset.viewLink); });
  bindGoto();
}

function topFindings() {
  const items = state.nodes.flatMap((n) => (state.results[n.id]?.findings || []).filter((f) => f.severity !== 'info').map((f) => ({ n, f })));
  items.sort((a, b) => (a.f.severity === 'critical' ? 0 : 1) - (b.f.severity === 'critical' ? 0 : 1));
  return `<div class="list">${items.slice(0, 12).map(({ n, f }) => `<div class="li" data-goto="${esc(n.id)}" tabindex="0">
      ${UI.chip(SEV_LABEL[f.severity], SEV_TONE[f.severity])}
      <div class="li-main"><div class="li-title"><span class="node">${esc(n.id)}</span><span>${esc(f.title)}</span></div>${f.detail ? `<div class="li-sub one">${esc(f.detail)}</div>` : ''}</div>
      <span class="right">${esc(CAT_LABEL[f.category] || '')}</span>${icon('chevron', 'chev')}</div>`).join('')}</div>
    ${items.length > 12 ? `<p class="note-line">ほか ${items.length - 12} 件。各機体の「所見」に全件あります。</p>` : ''}`;
}

// ---- 機体の詳細 ----
const TABS = [['findings', '所見', 'bulb'], ['procs', 'プロセス', 'procs'], ['jobs', '定期処理', 'jobs'], ['logs', 'ログ', 'logs'], ['history', '履歴', 'history'], ['machine', '機体情報', 'machine']];

function compareHtml(cmp) {
  const pct = `${cmp.bench_ratio >= 1 ? '+' : ''}${((cmp.bench_ratio - 1) * 100).toFixed(1)}%`;
  const verdict = { slower: '遅くなった', faster: '速くなった', same: '誤差の範囲' }[cmp.verdict];
  return `<span>計測 <span class="delta ${cmp.verdict}">${pct}</span>（${verdict}）</span>`
    + `${cmp.cpu_delta != null ? `<span class="faint">·</span><span>CPU ${cmp.cpu_delta >= 0 ? '+' : ''}${esc(cmp.cpu_delta)}pt</span>` : ''}`
    + `${cmp.mem_delta != null ? `<span class="faint">·</span><span>空きメモリ ${cmp.mem_delta >= 0 ? '+' : ''}${esc(cmp.mem_delta)}pt</span>` : ''}`;
}

function nodeProps(n, r) {
  const d = r?.ok ? r.data : null;
  const c = counts(r);
  const stateChip = !r ? UI.chip('未分析', 'gray', { dot: true }) : !r.ok ? UI.chip('分析できない', 'red', { dot: true })
    : c.critical ? UI.chip(`重大 ${c.critical}`, 'red', { dot: true }) : c.warn ? UI.chip(`注意 ${c.warn}`, 'orange', { dot: true }) : UI.chip('良好', 'green', { dot: true });
  const more = r?.ok ? [c.critical && c.warn ? UI.chip(`注意 ${c.warn}`, 'orange') : '', c.info ? UI.chip(`提案 ${c.info}`, 'blue') : ''].join('') : '';
  const cmp = r?.compare;
  return UI.props([
    ['状態', stateChip + more, 'status'],
    r?.ok && ['スコア', `${Charts.ring(r.score, { size: 18, stroke: 3, label: false })}<span class="num"><b class="${scoreClass(r.score)}">${esc(r.score)}</b> / 100</span>`, 'gauge'],
    cmp && ['前回比', compareHtml(cmp), 'history'],
    ['OS', esc(d?.host?.os || (n.os === 'macos' ? 'macOS' : 'Windows')), osIcon(n)],
    d && ['CPU・メモリ', esc(`${d.host.cpu} · ${d.host.cores} スレッド · ${d.memory.total_gb} GB`), 'machine'],
    ['接続', `<code>${esc(n.alias)}</code>${n.local ? UI.chip('この機体（ローカルで実行）', 'blue') : ''}${n.shared ? UI.chip('共用機・提案のみ', 'gray') : ''}`, 'link'],
    r?.at && ['最終分析', `<span>${esc(fmtTime(r.at))}</span><span class="faint">${esc(ago(r.at))}</span>${r.stale ? UI.chip('前回の結果', 'default', { title: 'アプリを開いたときに保存済みの結果を表示しています' }) : ''}`, 'actions'],
    d?.bench && ['計測', `<span class="num">${esc(d.bench.median_ms)} ms</span><span class="faint">1スレッドの固定計算。小さいほど速い</span>`, 'resources'],
  ]);
}

async function renderNode(id) {
  const t = ticket();
  const n = state.nodes.find((x) => x.id === id);
  if (!n) return go('overview');
  const r = state.results[n.id];
  const d = r?.ok ? r.data : null;
  const busy = state.busy.has(n.id);
  const tabs = [...TABS, ...(state.cfg.fleet ? [['fleet', 'katala-fleet', 'fleet']] : [])]
    .map(([k, label, ic]) => [k, label, k === 'findings' && r?.ok ? r.findings.length : k === 'jobs' && d?.jobs ? d.jobs.length : null, ic]);
  setCrumbs([{ icon: 'overview', label: '概要', view: 'overview' }, { icon: osIcon(n), label: n.id }]);

  let body = '';
  if (state.tab === 'logs') body = await nodeLogsHtml(n.id);
  else if (state.tab === 'history') body = await historyHtml(n.id);
  else if (state.tab === 'fleet') body = fleetHtml(n.id);
  else if (!r) body = UI.empty('まだ分析していません。', `<button class="btn primary" id="btnOne">${icon('play')}この機体を分析</button>`);
  else if (!r.ok) body = UI.callout({ tone: 'red', icon: 'alert', title: '分析できませんでした', body: `<div class="err">${esc(r.error)}</div><p class="note-line">SSH で入れるか（~/.ssh/config の Host 名と鍵）、機体の電源とネットワークを確かめてから「この機体を分析し直す」を押してください。</p>` });
  else if (state.tab === 'findings') body = findingsHtml(r.findings);
  else if (state.tab === 'procs') body = procsHtml(d);
  else if (state.tab === 'jobs') body = jobsTable(jobsOf([n]), false);
  else if (state.tab === 'machine') body = machineHtml(d, r);
  if (stale(t)) return;

  const actions = r ? `<button class="btn" id="btnOne" ${busy ? 'disabled' : ''}>${icon('play')}${busy ? '分析中…' : 'この機体を分析し直す'}</button>` : '';
  page(`
    ${UI.head({ icon: osIcon(n), title: n.id, desc: n.role || '', props: nodeProps(n, r), actions })}
    ${n.note ? UI.callout({ tone: 'gray', icon: 'info', body: esc(n.note) }) : ''}
    ${UI.tabs(tabs, state.tab)}
    ${body}`);
  $$('[data-tab]').forEach((b) => { b.onclick = () => { state.tab = b.dataset.tab; renderNode(id); }; });
  const one = $('#btnOne');
  if (one) one.onclick = () => probe([n.id]);
  bindJobActions();
  $$('.f').forEach((el) => {
    const f = r?.findings?.find((x) => x.id === el.dataset.fid);
    if (!f) return;
    $$('[data-copy]', el).forEach((b) => { b.onclick = async () => { await window.tune.copy(f.commands[+b.dataset.copy]); b.textContent = 'コピー済み'; }; });
    const a = $('[data-act]', el);
    if (a) a.onclick = () => runAction(n, f);
    const lq = $('[data-logq]', el);
    if (lq) lq.onclick = () => { state.logFilter = { ...state.logFilter, q: f.log_query?.q || '', node_id: n.id, level: f.log_query?.level || '' }; go('logs'); };
  });
}

function findingsHtml(list) {
  if (!list.length) return UI.callout({ tone: 'green', icon: 'check', title: '目立つ問題はありません', body: '次の分析で変化があれば、ここに出ます。' });
  return [['critical', '重大'], ['warn', '注意'], ['info', '提案']].map(([sev, label]) => {
    const xs = list.filter((f) => f.severity === sev);
    if (!xs.length) return '';
    return `<div class="group-head">${UI.chip(label, SEV_TONE[sev])}<span>${xs.length} 件</span>${sev === 'info' ? '<span>· 開くと詳細</span>' : ''}</div><div class="flist">${xs.map(findingHtml).join('')}</div>`;
  }).join('');
}

function findingHtml(f) {
  const cmds = (f.commands || []).map((c, i) => `<div class="code">${esc(c)}<button class="btn small copy" data-copy="${i}">${icon('copy')}コピー</button></div>`).join('');
  let act = '';
  if (f.action) {
    act = f.action.blocked
      ? `<div class="act"><span class="blocked">${icon('alert')}${esc(f.action.blocked)}</span></div>`
      : `<div class="act"><button class="btn ${f.action.type === 'kill-process' ? 'danger' : 'primary'}" data-act="1">${esc(f.action.label)}</button><span>実行前に確認します。直前に状態を確かめ直し、変わっていれば中止します</span></div>`;
  }
  if (f.log_query) act += `<div class="act"><button class="btn small" data-logq="1">${icon('logs')}関連するログを見る</button></div>`;
  return UI.toggle({
    cls: `f sev-${f.severity}`, attrs: `data-fid="${esc(f.id)}"`, open: f.severity !== 'info',
    summary: `<span class="f-title">${esc(f.title)}</span>${f.category ? `<span class="f-cat">${esc(CAT_LABEL[f.category] || f.category)}</span>` : ''}`,
    body: `${f.detail ? `<div class="f-detail">${rich(f.detail)}</div>` : ''}${f.advice ? `<p class="f-advice">${rich(f.advice)}</p>` : ''}${cmds}${act}`,
  });
}

function procsHtml(d) {
  const p = d.processes;
  const win = d.probe === 'windows';
  const cpuOf = (x) => (win ? x.cpu * d.host.cores : x.cpu);
  const maxMem = Math.max(1, ...p.apps.map((a) => a.mem_mb));
  const maxCpu = Math.max(100, ...p.top_cpu.map(cpuOf));
  return `
    ${UI.section('アプリ別', '同名のプロセスを合算、メモリ順')}
    ${UI.table({
      cols: [{ label: 'アプリ' }, { label: 'メモリ', cls: 'n', width: '24%' }, { label: 'CPU（1コア換算）', cls: 'n' }, { label: '数', cls: 'n' }],
      rows: p.apps,
      row: (a) => `<td>${esc(a.app)}</td><td class="n">${(a.mem_mb / 1024).toFixed(2)} GB${Charts.dataBar(a.mem_mb, maxMem)}</td><td class="n">${cpuOf(a).toFixed(0)}%</td><td class="n">${esc(a.count)}</td>`,
    })}
    ${UI.section('CPU 上位', win ? '1.5秒の瞬間値と起動からの平均' : 'ps の直近平均')}
    ${UI.table({
      cols: [{ label: 'PID', cls: 'n' }, { label: '名前' }, { label: 'CPU', cls: 'n', width: '22%' }, win ? { label: '平均', cls: 'n' } : { label: '起動から' }, { label: 'メモリ', cls: 'n' }],
      rows: p.top_cpu,
      row: (x) => `<td class="n muted">${esc(x.pid)}</td><td>${esc(x.name)}${x.app && x.app !== x.name ? ` <span class="muted">· ${esc(x.app)}</span>` : ''}</td>`
        + `<td class="n">${cpuOf(x).toFixed(0)}%${Charts.dataBar(cpuOf(x), maxCpu, { tone: cpuOf(x) >= 80 ? 'warn' : 'neutral' })}</td>`
        + `${win ? `<td class="n">${esc(x.avg_core ?? '-')}%</td>` : `<td class="muted">${esc(x.etime)}</td>`}<td class="n">${esc(x.mem_mb)} MB</td>`,
    })}
    <p class="note-line">全 ${esc(p.count ?? '-')} プロセス · AI エージェント ${esc(p.agent_processes ?? '-')}</p>`;
}

function machineHtml(d, r) {
  const rows = [
    ['OS', d.host.os], ['CPU', `${d.host.cpu}（${d.host.cores} スレッド${d.host.p_cores ? ` · P ${d.host.p_cores} / E ${d.host.e_cores}` : ''}）`],
    ['稼働時間', d.host.uptime_h != null ? `${(d.host.uptime_h / 24).toFixed(1)} 日` : '-'],
    ['メモリ', `${d.memory.total_gb} GB · 空き ${d.memory.available_pct}%${d.memory.swap_used_gb != null ? ` · swap ${d.memory.swap_used_gb} GB` : ''}${d.memory.commit_pct != null ? ` · コミット ${d.memory.commit_pct}%` : ''}`],
    ['計測（5回）', d.bench ? `${d.bench.runs_ms.join(' / ')} ms（中央値 ${d.bench.median_ms}）` : 'python が無いため省略'],
  ];
  if (d.load) rows.push(['load', d.load.join(' / ')]);
  if (d.power?.plan_name) rows.push(['電源プラン', d.power.plan_name]);
  if (d.cpu_perf_pct != null) rows.push(['CPU クロック', `定格の ${d.cpu_perf_pct}%`]);
  if (d.wsl) rows.push(['WSL 上限', d.wsl.config ? `memory=${d.wsl.memory ?? '未設定'} processors=${d.wsl.processors ?? '未設定'}` : '.wslconfig なし']);
  if (d.stability_7d) rows.push(['7日間の停止', `BugCheck ${d.stability_7d.bugcheck_1001} · Kernel-Power ${d.stability_7d.kernel_power_41} · 6008 ${d.stability_7d.unexpected_6008}`]);
  if (d.defender) rows.push(['Defender', `リアルタイム ${d.defender.realtime ? '有効' : '無効'} · 除外 ${d.defender.exclusions ?? '不明'}`]);
  if (d.containers?.colima) rows.push(['colima', d.containers.colima.map((v) => `${v.name} ${v.status} · CPU ${v.cpus} · ${v.memory_gb} GB`).join('、')]);
  if (d.containers?.docker_desktop) rows.push(['Docker Desktop', `${d.containers.docker_desktop.memory_gb} GB`]);
  if (d.caches) rows.push(['キャッシュ', d.caches.map((c) => `${c.path} ${c.gb != null ? c.gb + ' GB' : '計測打ち切り'}`).join('\n')]);
  if (d.services) rows.push(['サービス', d.services.map((s) => `${s.name} ${s.status}`).join(' · ')]);
  rows.push(['調査時間', `機体内 ${d.elapsed_s} 秒 · 往復 ${r.wall_s?.toFixed?.(1) ?? '-'} 秒`]);
  const disks = (d.disk || []).map((x) => Charts.meter(100 - x.free_pct, {
    label: x.mount, ...((x.mount === '/' || /^C:/i.test(x.mount)) ? TH.disk : { warn: 90 }),
    text: `空き ${x.free_gb} / ${x.total_gb} GB${x.raw_free_gb != null ? `（即時 ${x.raw_free_gb}）` : ''}`,
  })).join('');
  const gpus = (d.gpus || []).map((g) => `<div class="gpu"><div class="gpu-name">${esc(g.name)}<span class="faint">driver ${esc(g.driver)}</span></div><div class="meter-list">
      ${Charts.meter(g.util, { label: '使用率', text: `${g.util}%` })}
      ${Charts.meter(g.mem_total_mb ? (g.mem_used_mb / g.mem_total_mb) * 100 : null, { label: 'メモリ', warn: 90, text: `${g.mem_used_mb} / ${g.mem_total_mb} MB` })}
      ${Charts.meter(g.temp_c, { label: '温度', warn: 80, crit: 85, text: `${g.temp_c}°C` })}
      ${Charts.meter(g.power_limit_w ? (g.power_w / g.power_limit_w) * 100 : null, { label: '電力', text: `${g.power_w} / ${g.power_limit_w} W` })}</div></div>`).join('');
  return `${UI.props(rows.map(([k, v]) => [k, `<span class="pre">${esc(v)}</span>`]))}
    ${disks ? `${UI.section('ディスク', '使用率。macOS は実効の空き（自動で空く分を含む）')}<div class="meter-list wide">${disks}</div>` : ''}
    ${gpus ? `${UI.section('GPU', '分析した瞬間の値')}${gpus}` : ''}`;
}

const withUnit = (v, unit) => (v == null ? '<span class="faint">-</span>' : `${esc(v)}${unit}`);
const box = (title, hint, inner) => `<div class="chart-box"><div class="chart-title">${esc(title)}${hint ? `<small>${esc(hint)}</small>` : ''}</div>${inner}</div>`;

async function historyHtml(id) {
  const h = await window.tune.history(id);
  if (!h.length) return UI.empty('履歴はまだありません。分析するたびに 1 点ずつ増えます。');
  const last = h.slice(-40);
  const pts = (k) => last.map((x) => ({ t: x.at, v: x[k] }));
  const latest = last.at(-1);
  return `<div class="charts">
      ${box('スコア', '100 点満点', Charts.line(pts('score'), { min: 0, max: 100, tone: Charts.scoreTone(latest.score), marks: [{ v: 80, tone: 'ok' }, { v: 55, tone: 'warn' }], better: 'high', label: 'スコア' }))}
      ${box('計測', 'ms', Charts.line(pts('bench_ms'), { unit: 'ms', tone: 'accent', better: 'low', label: '計測' }))}
      ${box('CPU 使用率', '%', Charts.line(pts('cpu'), { unit: '%', min: 0, max: 100, tone: 'info', marks: [{ v: TH.cpu.crit, tone: 'crit' }, { v: TH.cpu.warn, tone: 'warn' }], label: 'CPU 使用率' }))}
      ${box('空きメモリ', '%', Charts.line(pts('mem_avail'), { unit: '%', min: 0, max: 100, tone: 'info', marks: [{ v: 20, tone: 'warn' }, { v: 10, tone: 'crit' }], better: 'high', label: '空きメモリ' }))}
    </div>
    ${UI.section('直近の分析', `${last.length} 回・新しい順`)}
    ${UI.table({
      cols: [{ label: '日時' }, { label: 'スコア', cls: 'n' }, { label: '計測', cls: 'n' }, { label: 'CPU', cls: 'n' }, { label: '空きメモリ', cls: 'n' }, { label: 'swap', cls: 'n' }, { label: '重大 / 注意', cls: 'n' }],
      rows: last.slice().reverse(),
      row: (x) => `<td class="muted">${esc(fmtTime(x.at))}</td><td class="n"><b class="${scoreClass(x.score)}">${esc(x.score ?? '-')}</b></td><td class="n">${withUnit(x.bench_ms, ' ms')}</td><td class="n">${withUnit(x.cpu, '%')}</td><td class="n">${withUnit(x.mem_avail, '%')}</td><td class="n">${withUnit(x.swap_gb, ' GB')}</td>`
        + `<td class="n">${x.critical ? `<span class="sc-crit">${x.critical}</span>` : '0'} / ${x.warn ? `<span class="sc-warn">${x.warn}</span>` : '0'}</td>`,
    })}`;
}

function fleetHtml(id) {
  if (!state.fleet) return UI.empty('ツールバーの「概況を更新」で katala-fleet から読み込みます。');
  if (state.fleet.error) return UI.callout({ tone: 'red', icon: 'alert', title: 'katala-fleet を読めません', body: `<div class="err">${esc(state.fleet.error)}</div>` });
  const fl = fleetNode(id);
  const att = (state.fleet.attention || []).filter((a) => a.node_id === id);
  const stTone = !fl ? 'gray' : fl.state === 'online' ? 'green' : fl.state === 'offline' ? 'gray' : 'orange';
  return `${UI.props([
      ['状態', `${UI.chip(fl?.state ?? '台帳に無い', stTone, { dot: true })}${fl?.last_seen ? `<span class="faint">最終受信 ${esc(ago(fl.last_seen * 1000))}</span>` : ''}`, 'status'],
      ['OS', esc(fl?.os ?? '-'), 'machine'],
      ['失敗中の定期処理', esc(fl?.failing ?? '-'), 'jobs'],
      ['稼働コンテナ', esc(fl?.running_containers ?? '-'), 'procs'],
      ['AI エージェント', esc(fl?.agent_processes ?? '-'), 'procs'],
    ])}
    ${UI.section('要対応', '失敗している定期処理は運用の問題なので、ここでは止めない')}
    ${att.length ? att.map((a) => UI.callout({ tone: a.severity === 'error' ? 'red' : a.severity === 'warn' ? 'orange' : 'blue', icon: 'alert', title: `${a.type} ${a.subject}`, body: `${UI.chip(a.severity, a.severity === 'error' ? 'red' : 'orange')} ${esc(a.state)} · ${esc(a.detail)}` })).join('') : '<p class="note-line">なし</p>'}`;
}

// ---- 状態 ----
function matrixCell(x) {
  if (!x) return {};
  const title = `${STATUS_LABEL[x.status]}: ${x.detail || ''}`;
  if (x.status === 'ok') return { html: UI.dot('ok'), title };
  return { html: `<span class="mx-cell ${esc(x.status)}">${UI.dot(x.status)}${STATUS_LABEL[x.status]}</span>`, title };
}

async function renderStatus() {
  const t = ticket();
  const st = state.status = await window.tune.status();
  if (stale(t)) return;
  renderSidebar();
  const c = st.counts;
  setCrumbs([{ icon: 'status', label: '状態' }]);
  const app = st.checks.filter((x) => x.scope === '_app');
  const nodeIds = state.nodes.map((n) => n.id);
  const byNode = Object.fromEntries(nodeIds.map((id) => [id, Object.fromEntries(st.checks.filter((x) => x.scope === id).map((x) => [x.id, x]))]));
  const rowIds = [];
  for (const id of nodeIds) for (const k of Object.keys(byNode[id])) if (!rowIds.includes(k)) rowIds.push(k);
  const rowName = (k) => nodeIds.map((id) => byNode[id][k]?.name).find(Boolean) || k;
  const scopeName = (s) => (s === '_app' ? 'アプリ' : s);
  const fails = st.checks.filter((x) => x.status === 'fail');
  const warns = st.checks.filter((x) => x.status === 'warn');
  const mins = (v) => [v, v < 60 ? `${v} 分` : `${v / 60} 時間`];
  const group = (list, tone, title) => (list.length ? UI.callout({
    tone, icon: 'alert', title: `${title} ${list.length} 件`,
    body: `<div class="list">${list.map((x) => `<div class="li"${x.scope !== '_app' ? ` data-goto="${esc(x.scope)}" tabindex="0"` : ''}>
      <div class="li-main"><div class="li-title"><span class="node">${esc(scopeName(x.scope))}</span><span>${esc(x.name)}</span></div><div class="li-sub">${esc(x.detail || '')}</div></div>
      <span class="right">${esc(ago(x.since))}から</span></div>`).join('')}</div>`,
  }) : '');
  const sch = st.schedule;
  page(`
    ${UI.head({
      icon: 'status', title: '状態',
      desc: 'アプリ自身の機能と、機体ごとの機能を 正常／注意／異常／不明 で表示します。どの項目も根拠と「いつからその状態か」を持ち、変わったときだけ履歴に残ります。異常になったときと、異常から戻ったときに通知します。',
      props: UI.props([
        ['自動スキャン', `${UI.chip(sch.enabled ? 'オン' : 'オフ', sch.enabled ? 'green' : 'gray', { dot: true })}<span class="faint">分析 ${esc(sch.probe_minutes)} 分・ログ ${esc(sch.logs_minutes)} 分ごと</span>`, 'actions'],
        ['前回の分析', `${esc(ago(st.lastProbeAt))}${st.probing ? UI.chip('分析中', 'blue', { dot: true }) : ''}`, 'gauge'],
        ['前回のログ取り込み', `${esc(ago(st.lastLogsAt))}${st.syncing ? UI.chip('取り込み中', 'blue', { dot: true }) : ''}`, 'logs'],
      ]),
    })}
    <div class="status-breakdown">${Charts.stacked(statusParts(c), { label: '状態の内訳' })}</div>
    ${fails.length + warns.length ? `${UI.section('要確認', '異常と注意。いつからその状態か')}${group(fails, 'red', '異常')}${group(warns, 'orange', '注意')}` : ''}
    ${UI.section('自動スキャン', `アプリを閉じてもメニューバー${state.cfg.platform === 'win32' ? '（通知領域）' : ''}に残って動きます`)}
    <div class="settings">
      <label class="row"><span class="row-label">自動スキャン<small>決めた間隔で全機体を分析し、ログを取り込みます</small></span><input type="checkbox" class="switch" id="swAuto" ${sch.enabled ? 'checked' : ''}></label>
      <label class="row"><span class="row-label">分析の間隔</span>${UI.select({ id: 'selProbe', options: [15, 30, 60, 120, 240].map(mins), value: sch.probe_minutes, active: false })}</label>
      <label class="row"><span class="row-label">ログ取り込みの間隔</span>${UI.select({ id: 'selLogs', options: [5, 15, 30, 60].map(mins), value: sch.logs_minutes, active: false })}</label>
      <label class="row"><span class="row-label">ログイン時に起動<small>ウィンドウは開かずに常駐します</small></span><input type="checkbox" class="switch" id="swLogin" ${st.openAtLogin ? 'checked' : ''}></label>
    </div>
    ${UI.section('機体の機能', '点にカーソルを合わせると根拠を表示。列の名前で機体の詳細へ')}
    ${rowIds.length ? UI.matrix({
      corner: '項目',
      rows: rowIds.map((k) => ({ key: k, label: rowName(k) })),
      cols: nodeIds.map((id) => ({ key: id, html: `<button data-goto="${esc(id)}">${esc(id)}</button>` })),
      cell: (row, col) => matrixCell(byNode[col.key][row.key]),
    }) : UI.empty('まだ機体の状態がありません。分析すると表示します。')}
    ${UI.section('アプリの機能')}
    <div class="list">${app.map((x) => `<div class="li">${UI.status(x.status)}<div class="li-main"><div class="li-title"><span class="node">${esc(x.name)}</span></div><div class="li-sub">${esc(x.detail || '')}</div></div><span class="right">${esc(ago(x.since))}から</span></div>`).join('')}</div>
    ${UI.section('状態の変化', '変わったときだけ記録')}
    ${UI.table({
      cols: [{ label: '日時' }, { label: '対象' }, { label: '項目' }, { label: '変化' }, { label: '根拠' }],
      rows: st.events.slice(0, 80),
      empty: 'まだ変化はありません',
      row: (e) => `<td class="muted nowrap">${esc(fmtTime(e.ts))}</td><td class="nowrap">${esc(scopeName(e.scope))}</td><td>${esc(e.name)}</td>`
        + `<td class="nowrap">${e.from_status ? `${UI.status(e.from_status)} <span class="faint">→</span> ` : ''}${UI.status(e.to_status)}</td><td class="muted">${esc((e.detail || '').slice(0, 120))}</td>`,
    })}
    <p class="note-line">期待するサービス・定期処理・プロセスは、台帳の各機体に <code>"expect": { "services": [...], "jobs": [...], "processes": [...] }</code> と書くと、ここで見張ります。</p>`);
  bindGoto();
  $('#swAuto').onchange = async (e) => { await window.tune.setSchedule({ enabled: e.target.checked }); toast(`自動スキャンを${e.target.checked ? 'オン' : 'オフ'}にしました`); renderStatus(); };
  $('#selProbe').onchange = async (e) => { await window.tune.setSchedule({ probe_minutes: +e.target.value }); renderStatus(); };
  $('#selLogs').onchange = async (e) => { await window.tune.setSchedule({ logs_minutes: +e.target.value }); renderStatus(); };
  $('#swLogin').onchange = async (e) => { const on = await window.tune.setLogin(e.target.checked); toast(on ? 'ログイン時に起動します' : 'ログイン時の起動をやめました'); renderStatus(); };
}

// ---- リソース ----
function renderResources() {
  ticket();
  setCrumbs([{ icon: 'resources', label: 'リソース' }]);
  const rows = state.nodes.map((n) => ({ n, r: state.results[n.id] })).filter((x) => x.r?.ok);
  const missing = state.nodes.filter((n) => !state.results[n.id]?.ok);
  const m = (v, th) => `<td class="mid">${Charts.meter(v, { ...th })}</td>`;
  page(`
    ${UI.head({ icon: 'resources', title: 'リソース', desc: '全機体の CPU・メモリ・swap／コミット・ディスク・GPU・稼働日数・計測を、最新の分析結果で並べます。行をクリックすると機体の詳細へ移ります。' })}
    ${rows.length ? UI.table({
      cols: [{ label: '機体' }, { label: 'CPU', width: '13%' }, { label: 'メモリ使用', width: '13%' }, { label: 'swap / コミット', cls: 'n' }, { label: 'ディスク使用', width: '13%' }, { label: '空き', cls: 'n' },
        { label: 'GPU', cls: 'n' }, { label: '稼働', cls: 'n' }, { label: '計測', cls: 'n' }, { label: 'スコア', cls: 'n' }],
      rows,
      rowAttrs: ({ n }) => `data-goto="${esc(n.id)}" class="row-link"`,
      row: ({ n, r }) => {
        const d = r.data;
        const disk = sysDisk(d);
        const g = d.gpus?.[0];
        const commit = d.memory.commit_pct;
        const sw = d.probe === 'windows'
          ? `<span class="${commit >= 90 ? 'sc-crit' : commit >= 80 ? 'sc-warn' : ''}">${esc(commit)}%</span>`
          : `${esc(d.memory.swap_used_gb)} GB`;
        return `<td><b>${esc(n.id)}</b><span class="sub">${esc(d.host.cpu)}</span></td>
          ${m(d.cpu_busy, TH.cpu)}${m(d.memory.available_pct != null ? 100 - d.memory.available_pct : null, TH.mem)}
          <td class="n mid">${sw}</td>${m(disk ? 100 - disk.free_pct : null, TH.disk)}<td class="n mid">${disk ? esc(disk.free_gb) + ' GB' : '-'}</td>
          <td class="n mid">${g ? `${esc(g.util)}% · ${esc(g.temp_c)}°C` : '<span class="faint">-</span>'}</td><td class="n mid">${d.host.uptime_h != null ? (d.host.uptime_h / 24).toFixed(1) + ' 日' : '-'}</td>
          <td class="n mid">${esc(d.bench?.median_ms ?? '-')} ms</td><td class="n mid"><span class="score-cell">${Charts.ring(r.score, { size: 26, stroke: 3 })}</span></td>`;
      },
    }) : UI.empty('まだ分析していません。ツールバーの「全機を分析」で始めます。')}
    ${missing.length && rows.length ? `<p class="note-line">分析結果が無いため表示していない機体: ${missing.map((n) => `${esc(n.id)}${state.results[n.id] ? '（分析できない）' : '（未分析）'}`).join('、')}</p>` : ''}`);
  bindGoto();
}

// ---- プロセス（全機体） ----
const nodeOptions = (all = 'すべて') => [['', all], ...state.nodes.map((n) => [n.id, n.id])];

function renderProcs() {
  ticket();
  const f = state.procFilter;
  setCrumbs([{ icon: 'procs', label: 'プロセス' }]);
  let rows = [];
  for (const n of state.nodes) {
    const r = state.results[n.id];
    if (!r?.ok || (f.node_id && f.node_id !== n.id)) continue;
    const d = r.data, win = d.probe === 'windows', seen = new Set();
    for (const p of [...d.processes.top_cpu, ...(d.processes.top_mem || [])]) {
      if (seen.has(p.pid)) continue; seen.add(p.pid);
      rows.push({ n, p, core: win ? p.cpu * d.host.cores : p.cpu });
    }
  }
  if (f.q) rows = rows.filter((x) => `${x.p.name} ${x.p.app || ''}`.toLowerCase().includes(f.q.toLowerCase()));
  rows.sort((a, b) => (f.sort === 'mem' ? b.p.mem_mb - a.p.mem_mb : b.core - a.core));
  const shown = rows.slice(0, 300);
  const maxCore = Math.max(100, ...shown.map((x) => x.core));
  const maxMem = Math.max(1, ...shown.map((x) => x.p.mem_mb));
  page(`
    ${UI.head({ icon: 'procs', title: 'プロセス', desc: '各機体の最新の分析から、CPU 上位とメモリ上位を横断して検索・並べ替えます。終了は確認のうえ、直前に同じプロセスかを確かめてから行います。' })}
    <div class="filters">
      ${UI.search({ id: 'procQ', placeholder: '名前で絞り込む', value: f.q })}
      ${UI.select({ id: 'procNode', label: '機体', options: nodeOptions(), value: f.node_id })}
      ${UI.seg({ attr: 'sort', options: [['cpu', 'CPU 順'], ['mem', 'メモリ順']], value: f.sort })}
      <span class="count">${rows.length} 件${rows.length > 300 ? '（上位 300 件を表示）' : ''}</span>
    </div>
    ${UI.table({
      cols: [{ label: '機体' }, { label: 'PID', cls: 'n' }, { label: '名前' }, { label: 'CPU（1コア換算）', cls: 'n', width: '17%' }, { label: 'メモリ', cls: 'n', width: '15%' }, { label: '', cls: 'acts' }],
      rows: shown,
      empty: '該当するプロセスはありません',
      row: ({ n, p, core }, i) => `<td class="nowrap"><b>${esc(n.id)}</b></td><td class="n muted">${esc(p.pid)}</td><td>${esc(p.name)}${p.app && p.app !== p.name ? ` <span class="muted">· ${esc(p.app)}</span>` : ''}</td>`
        + `<td class="n">${core.toFixed(0)}%${Charts.dataBar(core, maxCore, { tone: core >= 80 ? 'warn' : 'neutral' })}</td>`
        + `<td class="n">${p.mem_mb >= 1024 ? (p.mem_mb / 1024).toFixed(1) + ' GB' : esc(p.mem_mb) + ' MB'}${Charts.dataBar(p.mem_mb, maxMem)}</td>`
        + `<td class="acts mid">${n.shared ? '<span class="faint">提案のみ</span>' : `<button class="btn small danger" data-kill="${i}">終了…</button>`}</td>`,
    })}`);
  bindSearch('#procQ', 'procs', (v) => { state.procFilter.q = v; }, renderProcs);
  $('#procNode').onchange = (e) => { state.procFilter.node_id = e.target.value; renderProcs(); };
  $$('[data-sort]').forEach((b) => { b.onclick = () => { state.procFilter.sort = b.dataset.sort; renderProcs(); }; });
  $$('[data-kill]').forEach((b) => {
    b.onclick = async () => {
      const { n, p } = shown[+b.dataset.kill];
      const r = await window.tune.action(n.id, { type: 'kill-process', params: { pid: p.pid, name: p.name, start: p.start ?? null, min_cpu: 0 } }, `${p.name}（PID ${p.pid}）を終了`);
      if (r.cancelled) return toast('取り消しました');
      if (r.refused) return toast(`実行しませんでした: ${r.refused}`, 5000);
      toast(`${n.id}: ${r.outcome}`, 5000);
      probe([n.id]);
    };
  });
}

// ---- スケジュール（定期処理） ----
function jobsOf(nodes) {
  const out = [];
  for (const n of nodes) {
    const r = state.results[n.id];
    for (const j of (r?.ok && r.data.jobs) || []) out.push({ n, j });
  }
  return out;
}
const SCHED_OK = (v) => v == null || v === 0 || (v >= 0x41300 && v <= 0x4130f);
const isFailing = (j) => !['disabled', 'unknown', 'running'].includes(j.state) && (j.kind === 'schtask' ? !SCHED_OK(j.last_result) : j.scope === 'user' && j.last_result != null && j.last_result !== 0);
const JOB_STATE = { ready: '待機', running: '実行中', disabled: '無効', queued: '待ち', loaded: '読み込み済み', 'not-loaded': '未読み込み', unknown: '不明' };
const JOB_TONE = { ready: 'default', running: 'blue', disabled: 'gray', queued: 'yellow', loaded: 'default', 'not-loaded': 'gray', unknown: 'gray' };
let jobRows = [];

function jobActions(n, j, i) {
  if (n.shared) return '<span class="faint">提案のみ</span>';
  const b = (act, label, cls = '') => `<button class="btn small ${cls}" data-job="${i}" data-jact="${act}">${label}</button>`;
  if (j.kind === 'schtask') return [j.state === 'disabled' ? b('task-enable', '有効化') : b('task-disable', '無効化'), j.state !== 'disabled' ? b('task-run', '今すぐ実行') : ''].join(' ');
  if (j.scope !== 'user') return '<span class="faint">system（扱わない）</span>';
  return [j.state === 'not-loaded' && j.plist ? b('launchd-load', '読み込む') : j.plist ? b('launchd-unload', '止める') : '', j.state !== 'not-loaded' ? b('launchd-kickstart', '今すぐ実行') : ''].join(' ');
}

function jobsTable(rows, showNode = true) {
  jobRows = rows;
  if (!rows.length) return UI.empty('定期処理の情報がありません（分析し直すと取得します）。');
  return UI.table({
    cols: [...(showNode ? [{ label: '機体' }] : []), { label: '名前' }, { label: '予定' }, { label: '状態' }, { label: '前回' }, { label: '次回' }, { label: '実行するもの' }, { label: '', cls: 'acts' }],
    rows,
    row: ({ n, j }, i) => {
      const fail = isFailing(j);
      return `${showNode ? `<td class="nowrap"><b>${esc(n.id)}</b></td>` : ''}
        <td><b>${wbr(j.name)}</b>${j.kind === 'schtask' && j.path !== '\\' ? `<span class="sub">${wbr(j.path)}</span>` : ''}${j.scope && j.scope !== 'user' ? ` ${UI.chip(j.scope, 'gray')}` : ''}</td>
        <td class="muted">${esc(j.schedule)}</td><td>${UI.chip(JOB_STATE[j.state] || j.state, JOB_TONE[j.state] || 'default')}</td>
        <td class="nowrap">${j.last_result == null ? '<span class="faint">-</span>' : fail ? UI.chip(`失敗 ${j.last_result}`, 'red') : UI.chip('成功', 'green')}${j.last_run ? `<span class="sub">${esc(fmtTime(j.last_run))}</span>` : ''}</td>
        <td class="muted nowrap">${j.next_run ? esc(fmtTime(j.next_run)) : '-'}</td><td class="muted">${wbr(j.program || '')}</td><td class="acts">${jobActions(n, j, i)}</td>`;
    },
  });
}

function bindJobActions() {
  $$('[data-jact]').forEach((b) => {
    b.onclick = async () => {
      const { n, j } = jobRows[+b.dataset.job];
      const type = b.dataset.jact;
      const params = j.kind === 'schtask' ? { path: j.path, name: j.name } : { label: j.id, plist: j.plist || null };
      const r = await window.tune.action(n.id, { type, params }, `${b.textContent}: ${j.name}`);
      if (r.cancelled) return toast('取り消しました');
      if (r.refused) return toast(`実行しませんでした: ${r.refused}`, 5000);
      toast(`${n.id}: ${r.ok ? '完了' : '失敗'} — ${(r.output || '').split('\n').pop()}`, 5000);
      probe([n.id]);
    };
  });
}

function renderJobs() {
  ticket();
  const f = state.jobFilter;
  let rows = jobsOf(state.nodes.filter((n) => !f.node_id || n.id === f.node_id));
  const total = rows.length;
  const failing = rows.filter(({ j }) => isFailing(j)).length;
  if (f.failing) rows = rows.filter(({ j }) => isFailing(j));
  if (f.q) rows = rows.filter(({ j }) => `${j.name} ${j.program || ''} ${j.schedule}`.toLowerCase().includes(f.q.toLowerCase()));
  rows.sort((a, b) => (isFailing(b.j) - isFailing(a.j)) || a.n.id.localeCompare(b.n.id) || a.j.name.localeCompare(b.j.name));
  setCrumbs([{ icon: 'jobs', label: 'スケジュール' }]);
  page(`
    ${UI.head({ icon: 'jobs', title: 'スケジュール', desc: 'launchd（ユーザーの LaunchAgents）とタスクスケジューラ（Windows 標準・Apple 標準は除く）を横断して、予定・状態・前回の結果・次回を表示します。無効化・有効化・今すぐ実行は確認のうえで行い、元に戻せるものは「実行記録」から戻せます。' })}
    ${failing ? UI.callout({ tone: 'orange', icon: 'alert', title: `前回失敗した定期処理が ${failing} 件あります`, body: '「前回失敗のみ」で絞り込めます。失敗は運用の問題のことが多いので、止める前に何を実行しているかを確かめてください。' }) : ''}
    <div class="filters">
      ${UI.search({ id: 'jobQ', placeholder: '名前・実行ファイル・予定で絞り込む', value: f.q })}
      ${UI.select({ id: 'jobNode', label: '機体', options: nodeOptions(), value: f.node_id })}
      ${UI.seg({ attr: 'jf', options: [['0', `すべて ${total}`], ['1', `前回失敗のみ ${failing}`]], value: f.failing ? '1' : '0' })}
      <span class="count">${rows.length} 件</span>
    </div>
    ${jobsTable(rows)}`);
  bindSearch('#jobQ', 'jobs', (v) => { state.jobFilter.q = v; }, renderJobs);
  $('#jobNode').onchange = (e) => { state.jobFilter.node_id = e.target.value; renderJobs(); };
  $$('[data-jf]').forEach((b) => { b.onclick = () => { state.jobFilter.failing = b.dataset.jf === '1'; renderJobs(); }; });
  bindJobActions();
}

// ---- ログ ----
function logRow(l, showNode = true) {
  const span = l.repeat > 1 ? UI.chip(`×${l.repeat}（${fmtTime(l.first_ts)} から）`, 'default') : '';
  return `<div class="log"><div class="log-when">${esc(fmtTime(l.ts))}</div><div>
    <div class="log-meta">${UI.chip(LEVEL_LABEL[l.level] || l.level, LEVEL_TONE[l.level] || 'default')}${showNode ? `<b>${esc(l.node_id)}</b>` : ''}<span>${esc(l.provider || '')}${l.event_id ? ` · ${esc(l.event_id)}` : ''}</span><span class="faint" title="${esc(l.source)}">${esc(SOURCE_LABEL[l.source] || l.source)}</span>${span}</div>
    <div class="log-msg">${esc(l.message)}</div></div></div>`;
}

// 連続する同種のログ（同じ機体・同じ指紋）を1行にまとめる。洪水のようなログで一覧が埋まらないように
function collapse(rows) {
  const out = [];
  for (const r of rows) {
    const prev = out.at(-1);
    if (prev && prev.node_id === r.node_id && prev.fingerprint === r.fingerprint) { prev.repeat++; prev.first_ts = r.ts; }
    else out.push({ ...r, repeat: 1, first_ts: r.ts });
  }
  return out;
}

const sigHtml = (s, max) => `<div class="sig"><div class="sig-row"><span class="sig-n">${esc(s.n)}</span>${UI.chip(LEVEL_LABEL[s.level] || s.level, LEVEL_TONE[s.level] || 'default')}<span class="sig-src">${esc(s.provider || s.source)}</span>${s.nodes > 1 ? UI.chip(`${s.nodes} 台`, 'blue') : ''}</div>`
  + `${Charts.dataBar(s.n, max, { tone: LEVEL_BAR[s.level] || 'neutral' })}<div class="sig-msg">${esc(s.sample)}</div><div class="sig-meta">${esc(s.node_ids)} · 最終 ${esc(ago(s.last_ts))}</div></div>`;

// 期間に合わせた区切り。1日は1時間ごと、7日は6時間ごと、30日は1日ごと（どれも区切りの良い時刻にそろえる）
function logWindow(days, now = Date.now()) {
  const H = 3600e3;
  const d = new Date(now);
  if (days <= 1) { d.setMinutes(0, 0, 0); const to = d.getTime() + H; return { from: to - 24 * H, to, count: 24, step: H }; }
  if (days <= 7) { d.setHours(Math.floor(d.getHours() / 6) * 6, 0, 0, 0); const to = d.getTime() + 6 * H; return { from: to - 28 * 6 * H, to, count: 28, step: 6 * H }; }
  d.setHours(0, 0, 0, 0); const to = d.getTime() + 24 * H; return { from: to - days * 24 * H, to, count: days, step: 24 * H };
}

function logTrend(rows, days, { heat = true, limited = false } = {}) {
  const w = logWindow(days);
  const md = (t) => { const x = new Date(t); return `${x.getMonth() + 1}/${x.getDate()}`; };
  const buckets = Charts.bucketize(rows.map((l) => ({ ts: l.ts, series: l.level })), w);
  const label = days <= 1
    ? (b, i) => (i % 4 === 0 ? `${new Date(b.t0).getHours()}時` : '')
    : days <= 7 ? (b) => (new Date(b.t0).getHours() === 0 ? md(b.t0) : '') : (b, i) => (i % 5 === 0 ? md(b.t0) : '');
  const cols = Charts.columns(buckets, { series: LEVEL_SERIES, label, title: 'ログ件数の推移' });
  const hint = `表示中の ${rows.length} 件${limited ? '（上限に達したので、古い分は含まない）' : ''}`;
  if (!heat) return `<div class="charts one">${box('件数の推移', hint, cols)}</div>`;
  // 機体×日（1日表示なら機体×時）の濃淡。情報は数えず、重大・エラー・注意だけ
  const step = days <= 1 ? 3600e3 : 86400e3;
  const n = days <= 1 ? 24 : days;
  const end = new Date();
  if (days <= 1) end.setMinutes(0, 0, 0); else end.setHours(0, 0, 0, 0);
  const start = end.getTime() - (n - 1) * step;
  const hmCols = Array.from({ length: n }, (_, i) => {
    const t0 = start + i * step;
    const label = days <= 1 ? (i % 6 === 0 ? `${new Date(t0).getHours()}時` : '') : (n <= 7 || i % 5 === 0 ? md(t0) : '');
    return { key: i, t0, label };
  });
  const grid = new Map();
  for (const l of rows) {
    if (l.level === 'info') continue;
    const i = Math.floor((l.ts - start) / step);
    if (i < 0 || i >= n) continue;
    grid.set(`${l.node_id}|${i}`, (grid.get(`${l.node_id}|${i}`) || 0) + 1);
  }
  const hm = Charts.heatmap({
    rows: state.nodes.map((x) => ({ key: x.id, label: x.id })), cols: hmCols, tone: 'crit',
    value: (r, c) => grid.get(`${r.key}|${c.key}`) || 0,
    title: (r, c, v) => `${r.label} ${days <= 1 ? `${new Date(c.t0).getHours()}時台` : md(c.t0)}: 重大・エラー・注意 ${v} 件`,
  });
  return `<div class="charts">${box('件数の推移', hint, cols)}${box(days <= 1 ? '機体ごと（時間別）' : '機体ごと（日別）', '重大・エラー・注意の件数', hm)}</div>`;
}

async function nodeLogsHtml(id) {
  const since = Date.now() - 7 * 86400e3;
  const [sigs, rows] = await Promise.all([window.tune.logsSignatures({ since, node_id: id, limit: 15 }), window.tune.logsQuery({ node_id: id, since, limit: 1000 })]);
  if (!rows.rows?.length) return UI.empty('まだログを取り込んでいません。ツールバーの「ログを取り込む」で取り込みます。');
  const max = Math.max(1, ...sigs.map((s) => s.n));
  return `${logTrend(rows.rows, 7, { heat: false, limited: rows.rows.length >= 1000 })}
    <div class="logs-grid">
      <div>${UI.section('同種ログ', '7日')}<div class="list">${sigs.map((s) => sigHtml(s, max)).join('') || '<p class="note-line">なし</p>'}</div></div>
      <div>${UI.section('新しい順', `${Math.min(80, rows.rows.length)} 件`)}<div class="list">${collapse(rows.rows.slice(0, 80)).map((l) => logRow(l, false)).join('')}</div></div>
    </div>`;
}

async function renderLogs() {
  const t = ticket();
  const f = state.logFilter;
  setCrumbs([{ icon: 'logs', label: 'ログ' }]);
  const since = Date.now() - f.since * 86400e3;
  const [sigs, res, cursors] = await Promise.all([
    window.tune.logsSignatures({ since, node_id: f.node_id || undefined, limit: 25 }),
    window.tune.logsQuery({ q: f.q || undefined, node_id: f.node_id || undefined, level: f.level || undefined, since, limit: 1000 }),
    window.tune.logsCursors(),
  ]);
  if (stale(t)) return;
  const rows = res.rows || [];
  const max = Math.max(1, ...sigs.map((s) => s.n));
  const cursorErrors = cursors.filter((c) => c.last_error).length;
  page(`
    ${UI.head({ icon: 'logs', title: 'ログ', desc: '各機体のイベントログ・クラッシュ・NeonMonitor を、前回の続きから取り込みます。数字・ID・パスを伏せて同じ形のものを「同種ログ」にまとめ、何台で出ているかを数えます。⌘K（Ctrl+K）で検索へ移ります。' })}
    <div class="filters">
      ${UI.search({ id: 'logQ', placeholder: '全文検索（例: WHEA、beszel、crash）', value: f.q })}
      ${UI.select({ id: 'logNode', label: '機体', options: nodeOptions(), value: f.node_id })}
      ${UI.select({ id: 'logLevel', label: 'レベル', options: [['', 'すべて'], ...['critical', 'error', 'warn', 'info'].map((l) => [l, LEVEL_LABEL[l]])], value: f.level })}
      ${UI.select({ id: 'logSince', label: '期間', options: [1, 7, 30].map((d) => [d, `${d} 日`]), value: f.since, active: f.since !== 7 })}
      <span class="count">${rows.length} 件</span>
    </div>
    ${res.error ? UI.callout({ tone: 'red', icon: 'alert', title: '検索式を読めません', body: `<div class="err">${esc(res.error)}</div><p class="note-line">語をそのまま並べると「すべて含む」、OR で「どれか」、"…" で語句として探します。</p>` }) : ''}
    ${rows.length ? logTrend(rows, f.since, { limited: rows.length >= 1000 }) : ''}
    <div class="logs-grid">
      <div>
        ${UI.section('同種ログ', '数字・ID・パスを伏せて同じ形のもの')}
        <div class="list">${sigs.length ? sigs.map((s) => sigHtml(s, max)).join('') : '<p class="note-line">なし</p>'}</div>
        ${UI.section('取り込みの状態', cursorErrors ? `${cursorErrors} か所で失敗` : '')}
        ${UI.table({
          cols: [{ label: '機体 · 取り込み元' }, { label: '最終成功' }, { label: '前回', cls: 'n' }, { label: '捨てた数', cls: 'n' }],
          rows: cursors,
          empty: 'まだ取り込んでいません',
          row: (c) => `<td><b>${esc(c.node_id)}</b> <span class="muted">· ${esc(SOURCE_LABEL[c.source] || c.source)}</span>${c.last_error ? `<div class="err cursor-err">${esc(c.last_error.slice(0, 140))}</div>` : ''}</td>`
            + `<td class="nowrap">${c.last_error ? UI.chip('失敗', 'red', { dot: true }) + ' ' : ''}${c.last_ok_at ? esc(ago(c.last_ok_at)) : '<span class="faint">-</span>'}</td><td class="n">${esc(c.last_count ?? '-')}</td><td class="n">${c.dropped_total ? `<span class="sc-warn">${esc(c.dropped_total)}</span>` : ''}</td>`,
        })}
      </div>
      <div>
        ${UI.section('新しい順', `${rows.length} 件`)}
        <div class="list">${rows.length ? collapse(rows).map((l) => logRow(l)).join('') : '<p class="note-line">該当なし</p>'}</div>
      </div>
    </div>`);
  bindSearch('#logQ', 'logs', (v) => { state.logFilter.q = v.trim(); }, renderLogs, 300);
  $('#logNode').onchange = (e) => { state.logFilter.node_id = e.target.value; renderLogs(); };
  $('#logLevel').onchange = (e) => { state.logFilter.level = e.target.value; renderLogs(); };
  $('#logSince').onchange = (e) => { state.logFilter.since = +e.target.value; renderLogs(); };
}

// ---- 実行記録 ----
async function renderActions() {
  const t = ticket();
  setCrumbs([{ icon: 'actions', label: '実行記録' }]);
  const log = await window.tune.actionsLog();
  if (stale(t)) return;
  page(`
    ${UI.head({ icon: 'actions', title: '実行記録', desc: 'このアプリから実行した操作と、その結果。元に戻せる操作は、ここから戻せます（戻すときも確認します）。' })}
    ${log.length ? UI.table({
      cols: [{ label: '日時' }, { label: '機体' }, { label: '操作' }, { label: '', cls: 'acts' }],
      rows: log,
      row: (a) => `<td class="muted nowrap">${esc(fmtTime(a.at))}</td><td class="nowrap"><b>${esc(a.node_id)}</b></td>`
        + `<td><div class="act-title">${a.ok ? UI.chip('成功', 'green', { dot: true }) : UI.chip('失敗', 'red', { dot: true })}<span>${esc(a.label)}</span></div>${a.output ? `<div class="act-out">${esc(a.output)}</div>` : ''}</td>`
        + `<td class="acts">${a.undo && !log.some((b) => b.undo_of === a.id && b.ok) ? `<button class="btn small" data-undo="${esc(a.id)}">元に戻す</button>` : a.undo ? UI.chip('戻し済み', 'gray') : a.undo_of ? UI.chip('戻し', 'default') : ''}</td>`,
    }) : UI.empty('まだ何も実行していません。所見の提案から実行すると、ここに残ります。')}`);
  $$('[data-undo]').forEach((b) => {
    b.onclick = async () => { const r = await window.tune.undo(b.dataset.undo); toast(r.ok ? '元に戻しました' : r.refused || (r.cancelled ? '取り消しました' : `失敗: ${r.output}`)); renderActions(); };
  });
}

async function runAction(n, f) {
  const r = await window.tune.action(n.id, f.action, `${f.action.label}（${f.title}）`);
  if (r.cancelled) return toast('取り消しました');
  if (r.refused) return toast(`実行しませんでした: ${r.refused}`, 5000);
  toast(`${n.id}: ${r.outcome}`, 5000);
  await probe([n.id]);
}

// ---- 分析・同期 ----
async function probe(ids) {
  const targets = ids || state.nodes.map((n) => n.id);
  targets.forEach((id) => state.busy.add(id));
  $('#btnProbe').disabled = true;
  render();
  const t0 = Date.now();
  const r = await window.tune.probe(ids);
  const cfg = await window.tune.config();
  if (JSON.stringify(cfg.nodes) !== JSON.stringify(state.nodes)) { state.cfg = cfg; state.nodes = cfg.nodes; }
  targets.forEach((id) => state.busy.delete(id));
  $('#btnProbe').disabled = false;
  toast(r.busy ? '別の分析が実行中です' : `${targets.length} 台の分析が ${((Date.now() - t0) / 1000).toFixed(1)} 秒で終わりました`);
  render();
}

async function syncLogs() {
  $('#btnSync').disabled = true;
  const t0 = Date.now();
  const r = await window.tune.logsSync();
  $('#btnSync').disabled = false;
  if (r.busy) return toast('ログの取り込みが実行中です');
  const added = r.results.reduce((s, x) => s + Object.values(x.sources || {}).reduce((a, v) => a + (v.inserted || 0), 0), 0);
  const ng = r.results.filter((x) => x.error).length;
  toast(`ログを ${added} 件取り込みました（${((Date.now() - t0) / 1000).toFixed(1)} 秒${ng ? `、${ng} 台失敗` : ''}）`);
  if (state.view === 'logs' || state.tab === 'logs') render();
}

async function loadFleet() {
  if (!state.cfg.fleet) return;
  state.fleet = await window.tune.fleet();
  if (state.fleet.error) toast(`katala-fleet: ${state.fleet.error.split('\n').pop()}`, 5000);
  render();
}

// OS のアクセントカラー。明るい色なら文字を黒にして読めるようにする
function setAccent(hex) {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex || '');
  if (!m) return;
  const v = parseInt(m[1], 16);
  const lin = (c) => { c /= 255; return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
  const L = 0.2126 * lin((v >> 16) & 255) + 0.7152 * lin((v >> 8) & 255) + 0.0722 * lin(v & 255);
  document.documentElement.style.setProperty('--accent', `#${m[1]}`);
  document.documentElement.style.setProperty('--accent-text', L > 0.3 ? '#1f1f1f' : '#ffffff');
}

function setPlatform(p) {
  document.body.classList.remove('platform-darwin', 'platform-win32', 'platform-linux');
  document.body.classList.add(`platform-${p}`);
}

// 状態の更新通知は短時間にまとめて1回だけ反映する
let checksTimer;
window.tune.onChecksUpdated(() => {
  clearTimeout(checksTimer);
  checksTimer = setTimeout(async () => {
    if (state.view === 'status') return renderStatus();
    state.status = await window.tune.status();
    renderSidebar();
    if (state.view === 'overview') renderOverview();
  }, 400);
});
window.tune.onNavigate((v) => go(v));

window.tune.onProbeResult((r) => {
  state.results[r.node_id] = r;
  state.busy.delete(r.node_id);
  render();
});

(async function init() {
  // 起動時の platform は URL の ?platform= から（無ければ UA から推定）。台帳を読んだあとで本体の値に合わせ直す
  const ua = navigator.userAgent;
  setPlatform(new URLSearchParams(location.search).get('platform') || (/Windows/i.test(ua) ? 'win32' : /Mac/i.test(ua) ? 'darwin' : 'linux'));
  $('#page').addEventListener('scroll', () => $('#topbar').classList.toggle('scrolled', $('#page').scrollTop > 2), { passive: true });
  state.cfg = await window.tune.config();
  state.nodes = state.cfg.nodes;
  if (state.cfg.platform) setPlatform(state.cfg.platform);
  if (state.cfg.accent) setAccent(state.cfg.accent);
  if (state.cfg.error) toast(`台帳を読めません: ${state.cfg.error}`, 8000);
  for (const r of await window.tune.last()) state.results[r.node_id] = r;
  state.status = await window.tune.status();
  render();
  $('#btnProbe').onclick = () => probe();
  $('#btnSync').onclick = syncLogs;
  $('#btnFleet').onclick = loadFleet;
  $('#btnFleet').hidden = !state.cfg.fleet;
  $('#btnConfig').onclick = () => window.tune.openConfig();
  $('#btnData').onclick = () => window.tune.openDataDir();
  document.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'k') { e.preventDefault(); go('logs'); setTimeout(() => $('#logQ')?.focus(), 50); }
  });
  loadFleet();
})();
