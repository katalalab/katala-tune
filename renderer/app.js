'use strict';
// 画面側。データは window.tune（preload）からだけ受け取り、HTML へは必ず esc() を通して入れる

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const fmtTime = (ms) => (ms ? new Date(ms).toLocaleString('ja-JP', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '-');
const ago = (ms) => { if (!ms) return '-'; const s = (Date.now() - ms) / 1000; return s < 90 ? `${Math.max(1, Math.round(s))}秒前` : s < 5400 ? `${Math.round(s / 60)}分前` : s < 172800 ? `${Math.round(s / 3600)}時間前` : `${Math.round(s / 86400)}日前`; };
const SEV_LABEL = { critical: '重大', warn: '注意', info: '提案', error: 'エラー' };
const LEVEL_PILL = { critical: 'critical', error: 'critical', warn: 'warn', info: 'tag' };

const ICON = {
  overview: '<svg viewBox="0 0 20 20"><rect x="3" y="3" width="6" height="6" rx="1.5"/><rect x="11" y="3" width="6" height="6" rx="1.5"/><rect x="3" y="11" width="6" height="6" rx="1.5"/><rect x="11" y="11" width="6" height="6" rx="1.5"/></svg>',
  logs: '<svg viewBox="0 0 20 20"><path d="M4 5h12M4 10h12M4 15h8"/></svg>',
  actions: '<svg viewBox="0 0 20 20"><circle cx="10" cy="10" r="7"/><path d="M10 6v4l2.5 2"/></svg>',
  mac: '<svg viewBox="0 0 20 20"><rect x="3" y="4" width="14" height="9.5" rx="1.5"/><path d="M7.5 16.5h5M10 13.5v3"/></svg>',
  win: '<svg viewBox="0 0 20 20"><rect x="3" y="4" width="14" height="10" rx="1.5"/><path d="M6 17h8"/></svg>',
  search: '<svg viewBox="0 0 20 20"><circle cx="9" cy="9" r="5"/><path d="M13 13l4 4"/></svg>',
  status: '<svg viewBox="0 0 20 20"><path d="M3 10h3l2-5 4 10 2-5h3"/></svg>',
  resources: '<svg viewBox="0 0 20 20"><rect x="3" y="11" width="3" height="6" rx="1"/><rect x="8.5" y="7" width="3" height="10" rx="1"/><rect x="14" y="3" width="3" height="14" rx="1"/></svg>',
  procs: '<svg viewBox="0 0 20 20"><rect x="3" y="3" width="14" height="14" rx="2"/><path d="M7 7h6M7 10h6M7 13h4"/></svg>',
  jobs: '<svg viewBox="0 0 20 20"><rect x="3" y="4" width="14" height="13" rx="2"/><path d="M3 8h14M7 2.5v3M13 2.5v3"/></svg>',
};
const STATUS_LABEL = { ok: '正常', warn: '注意', fail: '異常', unknown: '不明' };

const state = {
  cfg: null, nodes: [], results: {}, fleet: null, view: 'overview', tab: 'findings', busy: new Set(), syncing: false,
  logFilter: { q: '', node_id: '', level: '', since: 7 },
  jobFilter: { node_id: '', failing: false, q: '' },
  procFilter: { node_id: '', q: '', sort: 'cpu' },
  status: null,
};

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
function nodeDot(n) {
  if (state.busy.has(n.id)) return 'busy';
  const r = state.results[n.id];
  if (r && !r.ok) return 'crit';
  if (!r) { const fl = fleetNode(n.id); return fl ? (fl.state === 'online' ? 'ok' : 'warn') : ''; }
  const c = counts(r);
  return c.critical ? 'crit' : c.warn ? 'warn' : 'ok';
}

// ---- サイドバー ----
function renderSidebar() {
  const c = state.status?.counts;
  const navs = [['overview', '概要', ICON.overview], ['status', '状態', ICON.status, c ? (c.fail ? `<span class="badge bad">${c.fail}</span>` : c.warn ? `<span class="badge">${c.warn}</span>` : '') : ''],
    ['resources', 'リソース', ICON.resources], ['procs', 'プロセス', ICON.procs], ['jobs', 'スケジュール', ICON.jobs], ['logs', 'ログ', ICON.logs], ['actions', '実行記録', ICON.actions]];
  $('#sideNav').innerHTML = navs.map(([k, label, ic, badge]) => `<button class="side-item ${state.view === k ? 'sel' : ''}" data-view="${k}">${ic}<span>${label}</span>${badge || ''}</button>`).join('');
  $('#sideNodes').innerHTML = state.nodes.map((n) => {
    const r = state.results[n.id];
    return `<button class="side-item ${state.view === 'node:' + n.id ? 'sel' : ''}" data-view="node:${esc(n.id)}">
      <span class="dot ${nodeDot(n)}"></span><span>${esc(n.id)}</span><span class="badge">${r?.ok ? r.score : ''}</span></button>`;
  }).join('');
  $$('.side-item').forEach((b) => { b.onclick = () => go(b.dataset.view); });
}

function go(view) {
  state.view = view;
  if (!view.startsWith('node:')) state.tab = 'findings';
  render();
}

function setToolbar(title, sub = '', center = '') {
  $('#tbTitle').textContent = title;
  $('#tbSub').textContent = sub;
  $('#tbCenter').innerHTML = center;
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
function ring(score) {
  const r = 19, c = 2 * Math.PI * r;
  const color = score == null ? 'var(--line2)' : score >= 80 ? 'var(--ok)' : score >= 55 ? 'var(--warn)' : 'var(--crit)';
  const off = score == null ? c : c * (1 - score / 100);
  return `<div class="ring"><svg viewBox="0 0 46 46"><circle class="track" cx="23" cy="23" r="${r}"/><circle class="val" cx="23" cy="23" r="${r}" stroke="${color}" stroke-dasharray="${c}" stroke-dashoffset="${off}"/></svg><span>${score ?? '–'}</span></div>`;
}

function meter(label, used, text) {
  if (used == null) return `<div class="meter"><span>${label}</span><div class="bar"></div><span class="n">-</span></div>`;
  const cls = used >= 90 ? 'crit' : used >= 75 ? 'warn' : '';
  return `<div class="meter"><span>${label}</span><div class="bar"><i class="${cls}" style="width:${Math.min(100, Math.max(2, used))}%"></i></div><span class="n">${esc(text)}</span></div>`;
}

function cardHtml(n) {
  const r = state.results[n.id];
  const d = r?.ok ? r.data : null;
  const fl = fleetNode(n.id);
  const c = counts(r);
  const disk = d?.disk?.find((x) => x.mount === '/' || /^C:/i.test(x.mount));
  const memUsed = d?.memory?.available_pct != null ? 100 - d.memory.available_pct : fl?.metrics?.mem ?? null;
  const cpu = d?.cpu_busy ?? fl?.metrics?.cpu ?? null;
  const cmp = r?.compare;
  const delta = cmp ? ` <span class="delta ${cmp.verdict}">${cmp.bench_ratio >= 1 ? '+' : ''}${((cmp.bench_ratio - 1) * 100).toFixed(0)}%</span>` : '';
  return `<div class="card ${state.busy.has(n.id) ? 'busy' : ''}" data-id="${esc(n.id)}">
    <div class="top">
      <div><div class="name">${esc(n.id)}</div><div class="role">${esc(n.role || '')}${n.shared ? ' · 提案のみ' : ''}${n.local ? ' · この機体' : ''}</div></div>
      ${ring(r?.ok ? r.score : null)}
    </div>
    <div class="hw">${esc(d ? `${d.host.cpu} · ${d.host.cores} スレッド · ${d.memory.total_gb} GB` : n.os === 'macos' ? 'macOS' : 'Windows')}</div>
    <div class="meters">
      ${meter('CPU', cpu, cpu != null ? `${Math.round(cpu)}%` : '-')}
      ${meter('メモリ', memUsed, memUsed != null ? `${Math.round(memUsed)}%` : '-')}
      ${meter('ディスク', disk ? 100 - disk.free_pct : fl?.metrics?.disk ?? null, disk ? `空き ${disk.free_gb} GB` : '-')}
    </div>
    <div class="foot">
      ${c.critical ? `<span class="pill critical">重大 ${c.critical}</span>` : ''}${c.warn ? `<span class="pill warn">注意 ${c.warn}</span>` : ''}${c.info ? `<span class="pill info">提案 ${c.info}</span>` : ''}
      ${r?.ok && !c.critical && !c.warn ? '<span class="pill ok">良好</span>' : ''}
      ${d?.bench ? `<span title="1スレッドの固定計算。小さいほど速い">計測 ${d.bench.median_ms} ms${delta}</span>` : ''}
      <span class="right">${state.busy.has(n.id) ? '分析中…' : r ? ago(r.at) : '未分析'}</span>
    </div>
    ${r && !r.ok ? `<div class="err">${esc(r.error)}</div>` : ''}
  </div>`;
}

function renderOverview() {
  const rs = state.nodes.map((n) => state.results[n.id]).filter((r) => r?.ok);
  const all = rs.flatMap((r) => r.findings);
  const crit = all.filter((f) => f.severity === 'critical').length;
  const warn = all.filter((f) => f.severity === 'warn').length;
  const avg = rs.length ? Math.round(rs.reduce((s, r) => s + r.score, 0) / rs.length) : null;
  const last = Math.max(0, ...rs.map((r) => r.at || 0));
  const fl = state.fleet?.summary;
  setToolbar('概要', last ? `最終分析 ${fmtTime(last)}` : 'まだ分析していません');
  $('#page').innerHTML = `
    ${state.cfg.example ? `<div class="note">機体台帳がまだ見本のままです。サイドバー下の「台帳」で ${esc(state.cfg.file)} を開き、~/.ssh/config の Host 名で機体を書いてください。保存したら「全機を分析」で読み直します。</div>` : ''}
    ${state.cfg.error ? `<div class="note">台帳を読めません: ${esc(state.cfg.error)}</div>` : ''}
    <div class="kpis">
      <div class="kpi"><div class="k">機体</div><div class="v">${rs.length}<small>/ ${state.nodes.length}</small></div></div>
      <div class="kpi"><div class="k">平均スコア</div><div class="v">${avg ?? '–'}</div></div>
      <div class="kpi ${crit ? 'crit' : ''}"><div class="k">重大</div><div class="v">${crit}</div></div>
      <div class="kpi ${warn ? 'warn' : ''}"><div class="k">注意</div><div class="v">${warn}</div></div>
      ${fl ? `<div class="kpi"><div class="k">katala-fleet 要対応</div><div class="v">${fl.attention}</div></div>` : ''}
    </div>
    <h2 class="sec">機体 <span class="hint">クリックで詳細</span></h2>
    <div class="cards">${state.nodes.map(cardHtml).join('')}</div>
    ${crit + warn ? `<h2 class="sec">優先して見るもの</h2>${topFindings()}` : ''}`;
  $$('.card').forEach((el) => { el.onclick = () => go('node:' + el.dataset.id); });
  $$('[data-goto]').forEach((el) => { el.onclick = () => go('node:' + el.dataset.goto); });
}

function topFindings() {
  const items = state.nodes.flatMap((n) => (state.results[n.id]?.findings || []).filter((f) => f.severity !== 'info').map((f) => ({ n, f })));
  items.sort((a, b) => (a.f.severity === 'critical' ? 0 : 1) - (b.f.severity === 'critical' ? 0 : 1));
  return `<div class="panel">${items.slice(0, 12).map(({ n, f }) => `<div class="sig" data-goto="${esc(n.id)}"><div class="row"><span class="pill ${f.severity}">${SEV_LABEL[f.severity]}</span><b>${esc(n.id)}</b><span>${esc(f.title)}</span></div></div>`).join('')}</div>`;
}

// ---- 機体の詳細 ----
const TABS = [['findings', '所見'], ['procs', 'プロセス'], ['jobs', '定期処理'], ['logs', 'ログ'], ['history', '履歴'], ['machine', '機体']];

async function renderNode(id) {
  const n = state.nodes.find((x) => x.id === id);
  if (!n) return go('overview');
  const r = state.results[n.id];
  const d = r?.ok ? r.data : null;
  const tabs = [...TABS, ...(state.cfg.fleet ? [['fleet', 'katala-fleet']] : [])];
  setToolbar(n.id, `${n.alias} · ${n.role || ''}${r?.at ? ` · ${r.stale ? '前回 ' : ''}${fmtTime(r.at)}` : ''}`,
    `<div class="seg">${tabs.map(([k, v]) => `<button data-tab="${k}" class="${state.tab === k ? 'on' : ''}">${v}</button>`).join('')}</div>`);
  $$('[data-tab]').forEach((b) => { b.onclick = () => { state.tab = b.dataset.tab; renderNode(id); }; });

  let body = '';
  if (state.tab === 'logs') body = await nodeLogsHtml(n.id);
  else if (state.tab === 'history') body = await historyHtml(n.id);
  else if (state.tab === 'fleet') body = fleetHtml(n.id);
  else if (!r) body = '<div class="empty">まだ分析していません。<br><br><button class="btn primary" id="btnOne">この機体を分析</button></div>';
  else if (!r.ok) body = `<div class="err">${esc(r.error)}</div>`;
  else if (state.tab === 'findings') body = r.findings.length ? r.findings.map(findingHtml).join('') : '<div class="empty">目立つ問題はありません。</div>';
  else if (state.tab === 'procs') body = procsHtml(d);
  else if (state.tab === 'jobs') body = jobsTable(jobsOf([n]), false);
  else if (state.tab === 'machine') body = machineHtml(d, r);

  const cmp = r?.compare;
  $('#page').innerHTML = `
    ${n.note ? `<div class="note">${esc(n.note)}</div>` : ''}
    ${cmp && state.tab === 'findings' ? `<div class="compare">前回比: 計測 ${cmp.bench_ratio >= 1 ? '+' : ''}${((cmp.bench_ratio - 1) * 100).toFixed(1)}%（${{ slower: '遅くなった', faster: '速くなった', same: '誤差の範囲' }[cmp.verdict]}）${cmp.cpu_delta != null ? ` · CPU ${cmp.cpu_delta >= 0 ? '+' : ''}${cmp.cpu_delta}pt` : ''}${cmp.mem_delta != null ? ` · 空きメモリ ${cmp.mem_delta >= 0 ? '+' : ''}${cmp.mem_delta}pt` : ''}</div>` : ''}
    ${body}
    ${r && state.tab === 'findings' ? `<div style="margin-top:14px"><button class="btn" id="btnOne" ${state.busy.has(n.id) ? 'disabled' : ''}>この機体を分析し直す</button></div>` : ''}`;
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

function findingHtml(f) {
  const cmds = (f.commands || []).map((c, i) => `<div class="code">${esc(c)}<button class="btn small" data-copy="${i}">コピー</button></div>`).join('');
  let act = '';
  if (f.action) {
    act = f.action.blocked
      ? `<div class="act"><span class="blocked">${esc(f.action.blocked)}</span></div>`
      : `<div class="act"><button class="btn ${f.action.type === 'kill-process' ? 'danger' : 'primary'}" data-act="1">${esc(f.action.label)}</button><span>実行前に確認します。直前に状態を確かめ直し、変わっていれば中止します</span></div>`;
  }
  if (f.log_query) act += '<div class="act"><button class="btn small" data-logq="1">関連するログを見る</button></div>';
  return `<div class="f ${f.severity}" data-fid="${esc(f.id)}"><div>
    <div class="t"><span class="pill ${f.severity}">${SEV_LABEL[f.severity]}</span>${esc(f.title)}</div>
    ${f.detail ? `<div class="d">${esc(f.detail)}</div>` : ''}
    ${f.advice ? `<div class="a">${esc(f.advice)}</div>` : ''}${cmds}${act}</div></div>`;
}

function procsHtml(d) {
  const p = d.processes;
  const win = d.probe === 'windows';
  const cpuOf = (x) => (win ? (x.cpu * d.host.cores).toFixed(0) : x.cpu.toFixed(0));
  return `
    <h2 class="sec">アプリ別 <span class="hint">同名のプロセスを合算、メモリ順</span></h2>
    <div class="panel"><table><tr><th>アプリ</th><th class="n">メモリ</th><th class="n">CPU（1コア換算）</th><th class="n">数</th></tr>
    ${p.apps.map((a) => `<tr><td>${esc(a.app)}</td><td class="n">${(a.mem_mb / 1024).toFixed(2)} GB</td><td class="n">${cpuOf(a)}%</td><td class="n">${a.count}</td></tr>`).join('')}</table></div>
    <h2 class="sec">CPU 上位 <span class="hint">${win ? '1.5秒の瞬間値と起動からの平均' : 'ps の直近平均'}</span></h2>
    <div class="panel"><table><tr><th>PID</th><th>名前</th><th class="n">CPU</th>${win ? '<th class="n">平均</th>' : '<th>起動から</th>'}<th class="n">メモリ</th></tr>
    ${p.top_cpu.map((x) => `<tr><td class="muted">${x.pid}</td><td>${esc(x.name)}${x.app && x.app !== x.name ? ` <span style="color:var(--text2)">· ${esc(x.app)}</span>` : ''}</td><td class="n">${cpuOf(x)}%</td>${win ? `<td class="n">${x.avg_core ?? '-'}%</td>` : `<td class="muted">${esc(x.etime)}</td>`}<td class="n">${x.mem_mb} MB</td></tr>`).join('')}</table></div>
    <div class="compare" style="margin-top:10px">全 ${p.count} プロセス · AI エージェント ${p.agent_processes}</div>`;
}

function machineHtml(d, r) {
  const rows = [
    ['OS', d.host.os], ['CPU', `${d.host.cpu}（${d.host.cores} スレッド${d.host.p_cores ? ` · P ${d.host.p_cores} / E ${d.host.e_cores}` : ''}）`],
    ['稼働時間', d.host.uptime_h != null ? `${(d.host.uptime_h / 24).toFixed(1)} 日` : '-'],
    ['メモリ', `${d.memory.total_gb} GB · 空き ${d.memory.available_pct}%${d.memory.swap_used_gb != null ? ` · swap ${d.memory.swap_used_gb} GB` : ''}${d.memory.commit_pct != null ? ` · コミット ${d.memory.commit_pct}%` : ''}`],
    ['ディスク', (d.disk || []).map((x) => `${x.mount} 空き ${x.free_gb}/${x.total_gb} GB${x.raw_free_gb != null ? `（即時 ${x.raw_free_gb} GB）` : ''}`).join('\n')],
    ['計測（5回）', d.bench ? `${d.bench.runs_ms.join(' / ')} ms（中央値 ${d.bench.median_ms}）` : 'python が無いため省略'],
  ];
  if (d.load) rows.push(['load', d.load.join(' / ')]);
  if (d.power?.plan_name) rows.push(['電源プラン', d.power.plan_name]);
  if (d.cpu_perf_pct != null) rows.push(['CPU クロック', `定格の ${d.cpu_perf_pct}%`]);
  if (d.gpus?.length) rows.push(['GPU', d.gpus.map((g) => `${g.name} · ${g.util}% · ${g.mem_used_mb}/${g.mem_total_mb} MB · ${g.temp_c}°C · ${g.power_w}/${g.power_limit_w} W · driver ${g.driver}`).join('\n')]);
  if (d.wsl) rows.push(['WSL 上限', d.wsl.config ? `memory=${d.wsl.memory ?? '未設定'} processors=${d.wsl.processors ?? '未設定'}` : '.wslconfig なし']);
  if (d.stability_7d) rows.push(['7日間の停止', `BugCheck ${d.stability_7d.bugcheck_1001} · Kernel-Power ${d.stability_7d.kernel_power_41} · 6008 ${d.stability_7d.unexpected_6008}`]);
  if (d.defender) rows.push(['Defender', `リアルタイム ${d.defender.realtime ? '有効' : '無効'} · 除外 ${d.defender.exclusions ?? '不明'}`]);
  if (d.containers?.colima) rows.push(['colima', d.containers.colima.map((v) => `${v.name} ${v.status} · CPU ${v.cpus} · ${v.memory_gb} GB`).join('、')]);
  if (d.containers?.docker_desktop) rows.push(['Docker Desktop', `${d.containers.docker_desktop.memory_gb} GB`]);
  if (d.caches) rows.push(['キャッシュ', d.caches.map((c) => `${c.path} ${c.gb != null ? c.gb + ' GB' : '計測打ち切り'}`).join('\n')]);
  if (d.services) rows.push(['サービス', d.services.map((s) => `${s.name} ${s.status}`).join(' · ')]);
  rows.push(['調査時間', `機体内 ${d.elapsed_s} 秒 · 往復 ${r.wall_s?.toFixed?.(1) ?? '-'} 秒`]);
  return `<div class="panel kv">${rows.map(([k, v]) => `<div>${esc(k)}</div><div style="white-space:pre-wrap">${esc(v)}</div>`).join('')}</div>`;
}

function sparkline(values, color) {
  const v = values.filter((x) => x != null);
  if (v.length < 2) return '<div class="compare">2回以上分析すると推移を表示します</div>';
  const w = 600, h = 72, min = Math.min(...v), max = Math.max(...v), span = max - min || 1;
  const pts = values.map((x, i) => (x == null ? null : [(i / (values.length - 1)) * (w - 10) + 5, h - 8 - ((x - min) / span) * (h - 16)])).filter(Boolean);
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none"><polyline stroke="${color}" points="${pts.map((p) => p.join(',')).join(' ')}"/></svg>
    <div class="compare" style="margin:4px 0 0">最小 ${min} · 最大 ${max} · 最新 ${v.at(-1)}</div>`;
}

async function historyHtml(id) {
  const h = await window.tune.history(id);
  if (!h.length) return '<div class="empty">履歴はまだありません。</div>';
  const last = h.slice(-40);
  return `
    <h2 class="sec">計測 <span class="hint">ms・小さいほど速い</span></h2><div class="panel spark">${sparkline(last.map((x) => x.bench_ms), 'var(--accent)')}</div>
    <h2 class="sec">スコア</h2><div class="panel spark">${sparkline(last.map((x) => x.score), 'var(--ok)')}</div>
    <h2 class="sec">直近の分析</h2>
    <div class="panel"><table><tr><th>日時</th><th class="n">スコア</th><th class="n">計測</th><th class="n">CPU</th><th class="n">空きメモリ</th><th class="n">swap</th><th class="n">重大/注意</th></tr>
    ${last.slice().reverse().map((x) => `<tr><td>${fmtTime(x.at)}</td><td class="n">${x.score ?? '-'}</td><td class="n">${x.bench_ms ?? '-'} ms</td><td class="n">${x.cpu ?? '-'}%</td><td class="n">${x.mem_avail ?? '-'}%</td><td class="n">${x.swap_gb ?? '-'}</td><td class="n">${x.critical}/${x.warn}</td></tr>`).join('')}</table></div>`;
}

function fleetHtml(id) {
  if (!state.fleet) return '<div class="empty">ツールバーの「概況」で katala-fleet から読み込みます。</div>';
  if (state.fleet.error) return `<div class="err">${esc(state.fleet.error)}</div>`;
  const fl = fleetNode(id);
  const att = (state.fleet.attention || []).filter((a) => a.node_id === id);
  const sev = { error: 'critical', warn: 'warn' };
  return `<div class="panel kv">
      <div>状態</div><div>${esc(fl?.state ?? '台帳に無い')}${fl?.last_seen ? `（最終受信 ${ago(fl.last_seen * 1000)}）` : ''}</div>
      <div>OS</div><div>${esc(fl?.os ?? '-')}</div>
      <div>失敗中の定期処理</div><div>${fl?.failing ?? '-'}</div>
      <div>稼働コンテナ</div><div>${fl?.running_containers ?? '-'}</div>
      <div>AI エージェント</div><div>${fl?.agent_processes ?? '-'}</div></div>
    <h2 class="sec">要対応 <span class="hint">失敗している定期処理は運用の問題なので、ここでは止めない</span></h2>
    ${att.length ? att.map((a) => `<div class="f ${sev[a.severity] || 'info'}"><div><div class="t"><span class="pill ${sev[a.severity] || 'info'}">${esc(a.severity)}</span>${esc(a.type)} ${esc(a.subject)}</div><div class="d">${esc(a.state)} · ${esc(a.detail)}</div></div></div>`).join('') : '<div class="compare">なし</div>'}`;
}

// ---- 状態 ----
const dotHtml = (st, title = '') => `<span class="sdot ${st}" title="${esc(title)}"></span>`;

async function renderStatus() {
  const st = state.status = await window.tune.status();
  renderSidebar();
  const c = st.counts;
  setToolbar('状態', `自動スキャン ${st.schedule.enabled ? 'オン' : 'オフ'} · 分析 ${ago(st.lastProbeAt)} · ログ ${ago(st.lastLogsAt)}${st.probing ? ' · 分析中' : ''}${st.syncing ? ' · ログ取り込み中' : ''}`);
  const app = st.checks.filter((x) => x.scope === '_app');
  const nodeIds = state.nodes.map((n) => n.id);
  const byNode = Object.fromEntries(nodeIds.map((id) => [id, Object.fromEntries(st.checks.filter((x) => x.scope === id).map((x) => [x.id, x]))]));
  const rowIds = [];
  for (const id of nodeIds) for (const k of Object.keys(byNode[id])) if (!rowIds.includes(k)) rowIds.push(k);
  const rowName = (k) => nodeIds.map((id) => byNode[id][k]?.name).find(Boolean) || k;
  const bad = st.checks.filter((x) => x.status === 'fail' || x.status === 'warn').sort((a, b) => (a.status === 'fail' ? 0 : 1) - (b.status === 'fail' ? 0 : 1));
  const opt = (v, cur) => `<option value="${v}" ${v === cur ? 'selected' : ''}>${v < 60 ? v + ' 分' : v / 60 + ' 時間'}</option>`;
  $('#page').innerHTML = `
    <div class="kpis">
      <div class="kpi ${c.fail ? 'crit' : ''}"><div class="k">異常</div><div class="v">${c.fail}</div></div>
      <div class="kpi ${c.warn ? 'warn' : ''}"><div class="k">注意</div><div class="v">${c.warn}</div></div>
      <div class="kpi"><div class="k">正常</div><div class="v">${c.ok}</div></div>
      <div class="kpi"><div class="k">不明</div><div class="v">${c.unknown}</div></div>
    </div>
    <h2 class="sec">自動スキャン <span class="hint">アプリを閉じてもメニューバー${state.cfg.platform === 'win32' ? '（通知領域）' : ''}に残って動きます</span></h2>
    <div class="panel settings">
      <label class="row"><span>自動スキャン</span><input type="checkbox" class="switch" id="swAuto" ${st.schedule.enabled ? 'checked' : ''}></label>
      <label class="row"><span>分析の間隔</span><select class="sel" id="selProbe">${[15, 30, 60, 120, 240].map((v) => opt(v, st.schedule.probe_minutes)).join('')}</select></label>
      <label class="row"><span>ログ取り込みの間隔</span><select class="sel" id="selLogs">${[5, 15, 30, 60].map((v) => opt(v, st.schedule.logs_minutes)).join('')}</select></label>
      <label class="row"><span>ログイン時に起動（ウィンドウは開かずに常駐）</span><input type="checkbox" class="switch" id="swLogin" ${st.openAtLogin ? 'checked' : ''}></label>
    </div>
    ${bad.length ? `<h2 class="sec">要確認 <span class="hint">異常・注意の項目。いつからその状態か</span></h2>
    <div class="panel">${bad.map((x) => `<div class="sig" ${x.scope !== '_app' ? `data-goto="${esc(x.scope)}"` : ''}><div class="row">${dotHtml(x.status)}<b>${esc(x.scope === '_app' ? 'アプリ' : x.scope)}</b><span>${esc(x.name)}</span><span class="pill tag">${ago(x.since)}から</span></div><div class="msg">${esc(x.detail || '')}</div></div>`).join('')}</div>` : ''}
    <h2 class="sec">機体の機能 <span class="hint">点にカーソルを合わせると根拠を表示</span></h2>
    <div class="panel matrix"><table><tr><th>項目</th>${nodeIds.map((id) => `<th class="c"><a data-goto="${esc(id)}">${esc(id)}</a></th>`).join('')}</tr>
      ${rowIds.map((k) => `<tr><td>${esc(rowName(k))}</td>${nodeIds.map((id) => { const x = byNode[id][k]; return `<td class="c">${x ? dotHtml(x.status, `${STATUS_LABEL[x.status]}: ${x.detail || ''}`) : '<span class="muted">–</span>'}</td>`; }).join('')}</tr>`).join('')}
    </table></div>
    <h2 class="sec">アプリの機能</h2>
    <div class="panel">${app.map((x) => `<div class="sig"><div class="row">${dotHtml(x.status)}<b>${esc(x.name)}</b><span class="pill tag">${STATUS_LABEL[x.status]}</span></div><div class="msg">${esc(x.detail || '')}</div></div>`).join('')}</div>
    <h2 class="sec">状態の変化 <span class="hint">変わったときだけ記録</span></h2>
    <div class="panel"><table><tr><th>日時</th><th>対象</th><th>項目</th><th>変化</th><th>根拠</th></tr>
      ${st.events.slice(0, 80).map((e) => `<tr><td class="muted">${fmtTime(e.ts)}</td><td>${esc(e.scope === '_app' ? 'アプリ' : e.scope)}</td><td>${esc(e.name)}</td><td>${e.from_status ? `${dotHtml(e.from_status)} → ` : ''}${dotHtml(e.to_status)} ${STATUS_LABEL[e.to_status]}</td><td class="muted">${esc((e.detail || '').slice(0, 120))}</td></tr>`).join('')}
    </table></div>
    <div class="compare" style="margin-top:12px">期待するサービス・定期処理・プロセスは、台帳の各機体に <code>"expect": { "services": [...], "jobs": [...], "processes": [...] }</code> と書くと、ここで見張ります。</div>`;
  $$('[data-goto]').forEach((el) => { el.onclick = () => go('node:' + el.dataset.goto); });
  $('#swAuto').onchange = async (e) => { await window.tune.setSchedule({ enabled: e.target.checked }); toast(`自動スキャンを${e.target.checked ? 'オン' : 'オフ'}にしました`); renderStatus(); };
  $('#selProbe').onchange = async (e) => { await window.tune.setSchedule({ probe_minutes: +e.target.value }); renderStatus(); };
  $('#selLogs').onchange = async (e) => { await window.tune.setSchedule({ logs_minutes: +e.target.value }); renderStatus(); };
  $('#swLogin').onchange = async (e) => { const on = await window.tune.setLogin(e.target.checked); toast(on ? 'ログイン時に起動します' : 'ログイン時の起動をやめました'); renderStatus(); };
}

// ---- リソース ----
function renderResources() {
  setToolbar('リソース', '最新の分析結果。数字は機体ごとの最新値');
  const rows = state.nodes.map((n) => ({ n, r: state.results[n.id] })).filter((x) => x.r?.ok);
  const pct = (v, warnAt, critAt, invert = false) => { if (v == null) return '<td class="n muted">-</td>'; const load = invert ? 100 - v : v; const cls = load >= critAt ? 'crit' : load >= warnAt ? 'warn' : ''; return `<td class="n"><div class="cellbar"><div class="bar"><i class="${cls}" style="width:${Math.min(100, Math.max(2, load))}%"></i></div><span>${Math.round(v)}%</span></div></td>`; };
  $('#page').innerHTML = rows.length ? `<div class="panel"><table>
    <tr><th>機体</th><th>CPU</th><th>メモリ使用</th><th class="n">swap / コミット</th><th>ディスク使用</th><th class="n">空き</th><th class="n">GPU</th><th class="n">稼働</th><th class="n">計測</th><th class="n">スコア</th></tr>
    ${rows.map(({ n, r }) => { const d = r.data; const disk = (d.disk || []).find((x) => x.mount === '/' || /^C:/i.test(x.mount)); const g = d.gpus?.[0];
      return `<tr data-goto="${esc(n.id)}" class="link"><td><b>${esc(n.id)}</b><div class="muted" style="font-size:11.5px">${esc(d.host.cpu)}</div></td>
        ${pct(d.cpu_busy, 60, 85)}${pct(d.memory.available_pct != null ? 100 - d.memory.available_pct : null, 75, 90)}
        <td class="n">${d.probe === 'windows' ? `${d.memory.commit_pct}%` : `${d.memory.swap_used_gb} GB`}</td>
        ${pct(disk ? 100 - disk.free_pct : null, 85, 92)}<td class="n">${disk ? disk.free_gb + ' GB' : '-'}</td>
        <td class="n">${g ? `${g.util}% · ${g.temp_c}°C` : '-'}</td><td class="n">${d.host.uptime_h != null ? (d.host.uptime_h / 24).toFixed(1) + ' 日' : '-'}</td>
        <td class="n">${d.bench?.median_ms ?? '-'} ms</td><td class="n">${r.score}</td></tr>`; }).join('')}
  </table></div>` : '<div class="empty">まだ分析していません。</div>';
  $$('[data-goto]').forEach((el) => { el.onclick = () => go('node:' + el.dataset.goto); });
}

// ---- プロセス（全機体） ----
function renderProcs() {
  const f = state.procFilter;
  setToolbar('プロセス', 'CPU 上位とメモリ上位（各機体の最新の分析）。終了は確認のうえ、直前に同じプロセスかを確かめてから');
  let rows = [];
  for (const n of state.nodes) {
    const r = state.results[n.id];
    if (!r?.ok || (f.node_id && f.node_id !== n.id)) continue;
    const d = r.data, win = d.probe === 'windows', seen = new Set();
    for (const p of [...d.processes.top_cpu, ...d.processes.top_mem]) {
      if (seen.has(p.pid)) continue; seen.add(p.pid);
      rows.push({ n, p, core: win ? p.cpu * d.host.cores : p.cpu });
    }
  }
  if (f.q) rows = rows.filter((x) => `${x.p.name} ${x.p.app || ''}`.toLowerCase().includes(f.q.toLowerCase()));
  rows.sort((a, b) => (f.sort === 'mem' ? b.p.mem_mb - a.p.mem_mb : b.core - a.core));
  $('#page').innerHTML = `
    <div class="logbar">
      <label class="search">${ICON.search}<input id="procQ" placeholder="名前で絞り込む" value="${esc(f.q)}"></label>
      <select class="sel" id="procNode"><option value="">全機体</option>${state.nodes.map((n) => `<option ${f.node_id === n.id ? 'selected' : ''}>${esc(n.id)}</option>`).join('')}</select>
      <div class="seg"><button data-sort="cpu" class="${f.sort === 'cpu' ? 'on' : ''}">CPU 順</button><button data-sort="mem" class="${f.sort === 'mem' ? 'on' : ''}">メモリ順</button></div>
    </div>
    <div class="panel"><table><tr><th>機体</th><th>PID</th><th>名前</th><th class="n">CPU（1コア換算）</th><th class="n">メモリ</th><th></th></tr>
    ${rows.slice(0, 300).map(({ n, p, core }, i) => `<tr><td>${esc(n.id)}</td><td class="muted">${p.pid}</td><td>${esc(p.name)}${p.app && p.app !== p.name ? ` <span class="muted">· ${esc(p.app)}</span>` : ''}</td><td class="n">${core.toFixed(0)}%</td><td class="n">${p.mem_mb >= 1024 ? (p.mem_mb / 1024).toFixed(1) + ' GB' : p.mem_mb + ' MB'}</td>
      <td>${n.shared ? '<span class="muted">提案のみ</span>' : `<button class="btn small danger" data-kill="${i}">終了…</button>`}</td></tr>`).join('')}
    </table></div>`;
  let t;
  $('#procQ').oninput = (e) => { clearTimeout(t); t = setTimeout(() => { state.procFilter.q = e.target.value; renderProcs(); const i = $('#procQ'); i.focus(); i.setSelectionRange(i.value.length, i.value.length); }, 250); };
  $('#procNode').onchange = (e) => { state.procFilter.node_id = e.target.value; renderProcs(); };
  $$('[data-sort]').forEach((b) => { b.onclick = () => { state.procFilter.sort = b.dataset.sort; renderProcs(); }; });
  $$('[data-kill]').forEach((b) => {
    b.onclick = async () => {
      const { n, p } = rows[+b.dataset.kill];
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
let jobRows = [];

function jobActions(n, j, i) {
  if (n.shared) return '<span class="muted">提案のみ</span>';
  const b = (act, label, cls = '') => `<button class="btn small ${cls}" data-job="${i}" data-jact="${act}">${label}</button>`;
  if (j.kind === 'schtask') return [j.state === 'disabled' ? b('task-enable', '有効化') : b('task-disable', '無効化'), j.state !== 'disabled' ? b('task-run', '今すぐ実行') : ''].join(' ');
  if (j.scope !== 'user') return '<span class="muted">system（扱わない）</span>';
  return [j.state === 'not-loaded' && j.plist ? b('launchd-load', '読み込む') : j.plist ? b('launchd-unload', '止める') : '', j.state !== 'not-loaded' ? b('launchd-kickstart', '今すぐ実行') : ''].join(' ');
}

function jobsTable(rows, showNode = true) {
  jobRows = rows;
  if (!rows.length) return '<div class="empty">定期処理の情報がありません（分析し直すと取得します）。</div>';
  return `<div class="panel"><table><tr>${showNode ? '<th>機体</th>' : ''}<th>名前</th><th>予定</th><th>状態</th><th>前回</th><th>次回</th><th>実行するもの</th><th></th></tr>
    ${rows.map(({ n, j }, i) => { const fail = isFailing(j);
      return `<tr>${showNode ? `<td>${esc(n.id)}</td>` : ''}<td><b>${esc(j.name)}</b>${j.kind === 'schtask' && j.path !== '\\' ? `<div class="muted" style="font-size:11px">${esc(j.path)}</div>` : ''}${j.scope && j.scope !== 'user' ? ` <span class="pill tag">${esc(j.scope)}</span>` : ''}</td>
        <td class="muted">${esc(j.schedule)}</td><td>${esc(JOB_STATE[j.state] || j.state)}</td>
        <td>${j.last_result == null ? '<span class="muted">-</span>' : fail ? `<span class="pill critical">失敗 ${j.last_result}</span>` : '<span class="pill ok">成功</span>'}${j.last_run ? `<div class="muted" style="font-size:11px">${fmtTime(j.last_run)}</div>` : ''}</td>
        <td class="muted">${j.next_run ? fmtTime(j.next_run) : '-'}</td><td class="muted">${esc(j.program || '')}</td><td style="white-space:nowrap">${jobActions(n, j, i)}</td></tr>`; }).join('')}
  </table></div>`;
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
  const f = state.jobFilter;
  let rows = jobsOf(state.nodes.filter((n) => !f.node_id || n.id === f.node_id));
  const failing = rows.filter(({ j }) => isFailing(j)).length;
  if (f.failing) rows = rows.filter(({ j }) => isFailing(j));
  if (f.q) rows = rows.filter(({ j }) => `${j.name} ${j.program || ''} ${j.schedule}`.toLowerCase().includes(f.q.toLowerCase()));
  rows.sort((a, b) => (isFailing(b.j) - isFailing(a.j)) || a.n.id.localeCompare(b.n.id) || a.j.name.localeCompare(b.j.name));
  setToolbar('スケジュール', `launchd とタスクスケジューラ（Windows 標準・Apple 標準は除く）· 前回失敗 ${failing} 件`);
  $('#page').innerHTML = `
    <div class="logbar">
      <label class="search">${ICON.search}<input id="jobQ" placeholder="名前・実行ファイル・予定で絞り込む" value="${esc(f.q)}"></label>
      <select class="sel" id="jobNode"><option value="">全機体</option>${state.nodes.map((n) => `<option ${f.node_id === n.id ? 'selected' : ''}>${esc(n.id)}</option>`).join('')}</select>
      <div class="seg"><button data-jf="0" class="${!f.failing ? 'on' : ''}">すべて</button><button data-jf="1" class="${f.failing ? 'on' : ''}">前回失敗のみ</button></div>
    </div>
    ${jobsTable(rows)}`;
  let t;
  $('#jobQ').oninput = (e) => { clearTimeout(t); t = setTimeout(() => { state.jobFilter.q = e.target.value; renderJobs(); const i = $('#jobQ'); i.focus(); i.setSelectionRange(i.value.length, i.value.length); }, 250); };
  $('#jobNode').onchange = (e) => { state.jobFilter.node_id = e.target.value; renderJobs(); };
  $$('[data-jf]').forEach((b) => { b.onclick = () => { state.jobFilter.failing = b.dataset.jf === '1'; renderJobs(); }; });
  bindJobActions();
}

// ---- ログ ----
function logRow(l, showNode = true) {
  const span = l.repeat > 1 ? `<span class="pill tag">×${l.repeat}（${fmtTime(l.first_ts)} から）</span>` : '';
  return `<div class="log"><div class="when">${fmtTime(l.ts)}</div><div>
    <div class="meta"><span class="pill ${LEVEL_PILL[l.level]}">${esc(SEV_LABEL[l.level] || l.level)}</span>${showNode ? `<b>${esc(l.node_id)}</b>` : ''}<span>${esc(l.provider || '')}${l.event_id ? ` · ${esc(l.event_id)}` : ''}</span><span>· ${esc(l.source)}</span>${span}</div>
    <div class="msg">${esc(l.message)}</div></div></div>`;
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

async function nodeLogsHtml(id) {
  const since = Date.now() - 7 * 86400e3;
  const [sigs, rows] = await Promise.all([window.tune.logsSignatures({ since, node_id: id, limit: 15 }), window.tune.logsQuery({ node_id: id, limit: 80 })]);
  if (!rows.rows?.length) return '<div class="empty">まだログを取り込んでいません。ツールバーの「ログ」で取り込みます。</div>';
  return `<div class="logs-grid">
    <div><h2 class="sec" style="margin-top:0">同種ログ（7日）</h2><div class="panel">${sigs.map(sigHtml).join('')}</div></div>
    <div><h2 class="sec" style="margin-top:0">新しい順</h2><div class="panel">${collapse(rows.rows).map((l) => logRow(l, false)).join('')}</div></div></div>`;
}

const sigHtml = (s) => `<div class="sig"><div class="row"><span class="cnt">${s.n}</span><span class="pill ${LEVEL_PILL[s.level]}">${esc(SEV_LABEL[s.level] || s.level)}</span><span>${esc(s.provider || s.source)}</span>${s.nodes > 1 ? `<span class="pill info">${s.nodes} 台</span>` : ''}</div><div class="msg">${esc(s.sample)}</div><div class="compare" style="margin:3px 0 0">${esc(s.node_ids)} · 最終 ${ago(s.last_ts)}</div></div>`;

async function renderLogs() {
  const f = state.logFilter;
  setToolbar('ログ', '各機体のイベントログ・クラッシュ・NeonMonitor を、前回の続きから取り込みます');
  const since = Date.now() - f.since * 86400e3;
  const [sigs, res, cursors] = await Promise.all([
    window.tune.logsSignatures({ since, node_id: f.node_id || undefined, limit: 25 }),
    window.tune.logsQuery({ q: f.q || undefined, node_id: f.node_id || undefined, level: f.level || undefined, since, limit: 1000 }),
    window.tune.logsCursors(),
  ]);
  $('#page').innerHTML = `
    <div class="logbar">
      <label class="search">${ICON.search}<input id="logQ" placeholder="全文検索（例: WHEA、beszel、crash）" value="${esc(f.q)}"></label>
      <select class="sel" id="logNode"><option value="">全機体</option>${state.nodes.map((n) => `<option ${f.node_id === n.id ? 'selected' : ''}>${esc(n.id)}</option>`).join('')}</select>
      <select class="sel" id="logLevel"><option value="">すべてのレベル</option>${['critical', 'error', 'warn', 'info'].map((l) => `<option value="${l}" ${f.level === l ? 'selected' : ''}>${SEV_LABEL[l]}</option>`).join('')}</select>
      <select class="sel" id="logSince">${[1, 7, 30].map((d) => `<option value="${d}" ${f.since === d ? 'selected' : ''}>${d} 日</option>`).join('')}</select>
    </div>
    ${res.error ? `<div class="err">検索式を読めません: ${esc(res.error)}</div>` : ''}
    <div class="logs-grid">
      <div>
        <h2 class="sec" style="margin-top:0">同種ログ <span class="hint">数字・ID・パスを伏せて同じ形のもの</span></h2>
        <div class="panel">${sigs.length ? sigs.map(sigHtml).join('') : '<div class="empty">なし</div>'}</div>
        <h2 class="sec">取り込みの状態</h2>
        <div class="panel health"><table><tr><th>機体 · 取り込み元</th><th>最終成功</th><th class="n">前回</th><th class="n">捨てた数</th></tr>
        ${cursors.map((c) => `<tr><td>${esc(c.node_id)} · ${esc(c.source)}${c.last_error ? `<div class="err" style="margin:2px 0 0">${esc(c.last_error.slice(0, 140))}</div>` : ''}</td><td>${c.last_ok_at ? ago(c.last_ok_at) : '-'}</td><td class="n">${c.last_count ?? '-'}</td><td class="n">${c.dropped_total || ''}</td></tr>`).join('') || '<tr><td colspan="4" class="muted">まだ取り込んでいません</td></tr>'}
        </table></div>
      </div>
      <div>
        <h2 class="sec" style="margin-top:0">新しい順 <span class="hint">${(res.rows || []).length} 件</span></h2>
        <div class="panel">${(res.rows || []).length ? collapse(res.rows).map((l) => logRow(l)).join('') : '<div class="empty">該当なし</div>'}</div>
      </div>
    </div>`;
  let t;
  $('#logQ').oninput = (e) => { clearTimeout(t); t = setTimeout(() => { state.logFilter.q = e.target.value.trim(); renderLogs().then(() => { const i = $('#logQ'); i.focus(); i.setSelectionRange(i.value.length, i.value.length); }); }, 300); };
  $('#logNode').onchange = (e) => { state.logFilter.node_id = e.target.value; renderLogs(); };
  $('#logLevel').onchange = (e) => { state.logFilter.level = e.target.value; renderLogs(); };
  $('#logSince').onchange = (e) => { state.logFilter.since = +e.target.value; renderLogs(); };
}

// ---- 実行記録 ----
async function renderActions() {
  setToolbar('実行記録', 'このアプリから実行した操作と、元に戻す操作');
  const log = await window.tune.actionsLog();
  $('#page').innerHTML = log.length ? `<div class="panel"><table><tr><th>日時</th><th>機体</th><th>操作</th><th></th></tr>
    ${log.map((a) => `<tr><td class="muted">${fmtTime(a.at)}</td><td>${esc(a.node_id)}</td><td><div>${a.ok ? '✓' : '✗'} ${esc(a.label)}</div><div class="compare" style="margin:2px 0 0;white-space:pre-wrap">${esc(a.output || '')}</div></td>
    <td>${a.undo && !log.some((b) => b.undo_of === a.id && b.ok) ? `<button class="btn small" data-undo="${esc(a.id)}">元に戻す</button>` : a.undo_of ? '<span class="pill tag">戻し</span>' : ''}</td></tr>`).join('')}</table></div>`
    : '<div class="empty">まだ何も実行していません。</div>';
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

// 状態の更新通知は短時間にまとめて1回だけ反映する
let checksTimer;
window.tune.onChecksUpdated(() => {
  clearTimeout(checksTimer);
  checksTimer = setTimeout(async () => {
    if (state.view === 'status') return renderStatus();
    state.status = await window.tune.status();
    renderSidebar();
  }, 400);
});
window.tune.onNavigate((v) => go(v));

window.tune.onProbeResult((r) => {
  state.results[r.node_id] = r;
  state.busy.delete(r.node_id);
  render();
});

(async function init() {
  document.body.classList.add(`platform-${new URLSearchParams(location.search).get('platform') || 'darwin'}`);
  state.cfg = await window.tune.config();
  state.nodes = state.cfg.nodes;
  if (state.cfg.accent) document.documentElement.style.setProperty('--accent', state.cfg.accent);
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
