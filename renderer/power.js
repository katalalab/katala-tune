'use strict';
// 電力と Clock。計算条件は私有 DB、機体の変更は既存の確認付き action 経路を使う。
(() => {
  const value = (v, unit = '', digits = 1) => typeof v === 'number' && Number.isFinite(v) ? `${UI.esc(v.toFixed(digits))}${unit}` : '未取得';
  const kinds = { measured: '実測', estimated: '推定', missing: '未取得' };
  const missing = { gpu_w: 'GPU電力', cpu_w: 'CPU電力', base_w: 'その他の電力', psu_efficiency: '電源効率' };
  let data;
  const unknownAfterAction = new Map();
  let refreshAt = 0;
  const sessionHtml = (o) => !o ? '<p class="note-line">ライブ観測の電力量はまだありません。</p>' : `<p class="note-line">直近の観測窓（最大300点、機体全体は推定）: ${[['gpu','GPU'],['cpu','CPU'],['soc','SoC'],['whole','入力側']].map(([k,label]) => `${label} ${o[k]?.covered_seconds > 0 ? `${value(o[k].kwh,' kWh',6)} · ${value(o[k].covered_seconds,'秒',1)}観測 / カバー率 ${value(o[k].coverage * 100,'%',1)}` : '未観測'}`).join(' ／ ')}${o.last_at ? ` · ${UI.esc(fmtTime(o.last_at))}` : ''}</p>`;
  function fields(r) {
    return `<form data-power-settings="${UI.esc(r.node_id)}" class="power-form">${[['base_w','その他の電力 (W)',0,10000,'0.1'],['psu_efficiency','電源効率 (0–1)',0.01,1,'0.01'],['hours','試算時間 (h)',0,8784,'0.1'],['rate_per_kwh','単価 / kWh',0,1000000,'0.01']].map(([k,label,min,max,step]) => `<label>${label}<input name="${k}" type="number" min="${min}" max="${max}" step="${step}" value="${UI.esc(r.settings[k] ?? '')}" placeholder="未設定"></label>`).join('')}<label>通貨<input name="currency" pattern="[A-Z]{3}" maxlength="3" value="${UI.esc(r.settings.currency ?? '')}" placeholder="JPY"></label><button class="btn small" type="submit">計算条件を保存</button></form>`;
  }
  function controls(n, r) {
    if (n.shared) return '<p class="note-line">共用機の設定変更は無効です。</p>';
    const gpus = (r.data.gpus || []);
    const gpuControls = gpus.map((g,i) => {
      const supported = g.uuid && [g.power_limit_w,g.power_min_w,g.power_max_w].every((v) => typeof v === 'number' && Number.isFinite(v));
      return `<div class="power-control"><b>${UI.esc(g.name)}</b> ${value(g.power_limit_w,' W')}<input aria-label="${UI.esc(g.name)} の電力上限 W" data-limit="${i}" type="number" step="1" min="${UI.esc(g.power_min_w ?? '')}" max="${UI.esc(g.power_max_w ?? '')}" value="${UI.esc(g.power_limit_w ?? '')}" ${supported ? '' : 'disabled'}><button class="btn small" data-power-action="${UI.esc(n.id)}" data-gpu="${i}" ${supported ? '' : 'disabled'}>電力上限を設定</button><span class="muted">${supported ? `${value(g.power_min_w,'')}–${value(g.power_max_w,' W')} · 管理権限が必要` : '上限・対応範囲が未取得'}</span></div>`;
    }).join('');
    const p = r.data.power || {};
    const plan = n.os === 'windows' && p.plan_guid ? `<div class="power-control"><label>電源プラン <select data-plan="${UI.esc(n.id)}">${(p.plans || []).map((x) => `<option value="${UI.esc(x.guid)}" ${x.guid === p.plan_guid ? 'selected' : ''}>${UI.esc(x.name)}</option>`).join('')}</select></label><button class="btn small" data-plan-action="${UI.esc(n.id)}">電源プランを設定</button></div>` : '';
    const mac = n.os === 'macos' && typeof p.low_power_mode === 'boolean' && typeof p.on_battery === 'boolean' ? `<div class="power-control"><label>低電力モード <select data-mac="${UI.esc(n.id)}"><option value="false" ${!p.low_power_mode ? 'selected' : ''}>オフ</option><option value="true" ${p.low_power_mode ? 'selected' : ''}>オン</option></select></label><button class="btn small" data-mac-action="${UI.esc(n.id)}">電源モードを設定</button><span class="muted">${p.on_battery ? 'バッテリー' : 'AC電源'} · 管理権限が必要</span></div>` : '';
    return `${gpuControls}${plan}${mac}<p class="note-line">Clockは読み取り専用です。この版では固定範囲の取得・復元と変更に未対応です。電源設定は実行前に値を確認し、変更後に読み返します。</p>`;
  }
  function card(r) {
    const n = state.nodes.find((n) => n.id === r.node_id) || { id: r.node_id };
    if (unknownAfterAction.has(r.node_id)) return UI.toggle({open:true,summary:`<b>${UI.esc(n.id)}</b>${UI.chip('状態未確認','orange')}`,body:`<p class="note-line">設定操作後の分析結果を取得できていません。この機体を再分析して現在値を確認してください。電源設定の再実行は現在値の取得後に有効になります。</p><button class="btn small" data-power-refresh="${UI.esc(r.node_id)}">この機体を再分析</button>${fields(r)}${controls(n,{data:{}})}`});
    const s = r.summary, p = r.data.power || {}, c = r.data.cpu_clock || {};
    return UI.toggle({ open: true, summary: `<b>${UI.esc(n.id)}</b>${UI.chip(kinds[s.kind] || '未取得',s.kind === 'measured' ? 'green' : s.kind === 'estimated' ? 'blue' : 'gray')}<span>${value(s.total_w,' W')}</span><span class="muted">${r.at ? UI.esc(ago(r.at)) : '未分析'}${r.fresh ? '' : ' · 合計対象外'}</span>`, body: `${UI.props([['GPU電力',value(s.gpu_w ?? ((r.data.gpus?.length && r.data.gpus.every(g => typeof g.power_w === 'number' && Number.isFinite(g.power_w) && g.power_w >= 0)) ? r.data.gpus.reduce((a,g) => a + g.power_w,0) : null),' W')],['CPU電力',`${value(p.package_w,' W')} · ${UI.esc(p.package_source || 'センサー未取得')}`],['SoC電力 (CPU+GPU+ANE)',`${value(s.soc_w,' W')} · ${UI.esc(p.soc_source || 'センサー未取得')}`],['システムセンサーの推定',`${value(p.platform_w,' W')} · ${UI.esc(p.platform_source || '未取得')}`],['入力側電力',`${value(s.total_w,' W')} · ${UI.esc(s.source || '計算不可')}`],['CPU Clock',`${value(c.effective_mhz,' MHz')} · ${UI.esc(c.source || '実効値未取得')}（基準 ${value(c.nominal_mhz,' MHz')}）`],['CPU E / P Clock',`${value(c.clusters_mhz?.e,' MHz')} / ${value(c.clusters_mhz?.p,' MHz')}`],['電力量の試算',value(s.projected_kwh,' kWh',4)],['費用の試算',`${value(s.projected_cost,'',2)} ${UI.esc(s.currency || '通貨未設定')}`]])}${s.missing?.length ? `<p class="note-line">不足: ${UI.esc(s.missing.map((k) => missing[k] || k).join('、'))}</p>` : ''}${UI.table({cols:[{label:'GPU'},{label:'電力 / 上限'},{label:'Graphics / SM / Memory (MHz)'},{label:'温度 / P-state'},{label:'取得元'}],rows:r.data.gpus || [],empty:'GPUセンサー未取得',row:g => `<td>${UI.esc(g.name)}<small class="muted">${UI.esc(g.uuid || '')}</small></td><td>${value(g.power_w,' W')} / ${value(g.power_limit_w,' W')}</td><td>${value(g.clocks_graphics_mhz,'',0)} / ${value(g.clocks_sm_mhz,'',0)} / ${value(g.clocks_memory_mhz,'',0)}</td><td>${value(g.temp_c,' °C')} / ${UI.esc(g.pstate || '未取得')}</td><td>${UI.esc(g.source || 'nvidia-smi')}</td>`})}${fields(r)}${controls(n,r)}` });
  }
  async function runAction(id, action, label) {
    let res;
    try { res = await window.tune.action(id,action,label); }
    catch (e) { res = {uncertain:true,output:String(e.message || e)}; }
    toast(res.cancelled ? '取り消しました' : res.ok ? '設定を読み返して確認しました' : res.refused || res.output || '設定に失敗しました',6000);
    if (res.refresh_required || res.entry || res.ok || res.uncertain) {
      unknownAfterAction.set(id, Math.max(Date.now(), data.nodes.find(r => r.node_id === id)?.at || 0));
      await refreshNode(id);
    }
    await render();
  }
  async function refreshNode(id) {
    try {
      const out = await window.tune.probe([id]);
      const result = out?.results?.find(r => r.node_id === id);
      if (out?.busy || !result?.ok || !(result.at > unknownAfterAction.get(id))) throw new Error('対象の新しい分析を保存できていません');
      const report = await window.tune.powerReport();
      const row = report.nodes.find(r => r.node_id === id);
      if (!row?.fresh || !(row.at >= result.at)) throw new Error('対象の現在値を読み返せません');
      unknownAfterAction.delete(id);
    } catch (e) { toast(`操作後の分析結果は未取得です: ${String(e.message || e)}`,6000); }
  }
  async function render() {
    const t = ticket();
    setCrumbs([{icon:'resources',label:'電力・Clock'}]);
    if (typeof window.tune.powerReport !== 'function') return page(UI.empty('この版は電力の計算に未対応です。'));
    data = await window.tune.powerReport().catch((e) => ({error:String(e.message || e)}));
    if (stale(t)) return;
    if (data.error) return page(UI.empty(data.error));
    const f = unknownAfterAction.size ? {...data.fleet,total_w:null,available_w:null,available:0} : data.fleet;
    page(`${UI.head({icon:'resources',title:'電力・Clock',desc:'全機のセンサー値と計算条件。GPU電力と、機体全体の入力側電力を分けて表示します。',actions:Live.toggle('resources')})}${Live.panel()}<div class="stats">${stat('全機の入力側電力',value(f.total_w,' W'),{sub:`取得 ${f.available} / ${f.expected} 台 · 5分以内の値`})}${stat('取得できた機体の小計',value(f.available_w,' W'),{sub:'未取得の機体は加算していません'})}</div><p class="note-line">試算は表示時の電力が続く場合の予測です。「その他の電力」はCPU・GPUと重ならない実測校正値、効率は電源の実効値を入力してください。観測窓同士は重なるため合算しません。</p>${data.nodes.map((r) => `${card(r)}<div data-power-session="${UI.esc(r.node_id)}">${sessionHtml(r.observation)}</div>`).join('')}`);
    Live.mount('resources',state.nodes.filter((n) => !n.shared).map((n) => n.id),state.nodes);
    document.querySelectorAll('[data-power-refresh]').forEach(b => { b.onclick = async () => { b.disabled = true; await refreshNode(b.dataset.powerRefresh); await render(); }; });
    document.querySelectorAll('[data-power-settings]').forEach((form) => { form.onsubmit = async (e) => { e.preventDefault(); const patch = {}; for (const [k,v] of new FormData(form)) patch[k] = v === '' ? null : k === 'currency' ? v : Number(v); try { await window.tune.powerSettings(form.dataset.powerSettings,patch); toast('計算条件を保存しました'); render(); } catch(e) { toast(String(e.message || e),6000); } }; });
    document.querySelectorAll('[data-power-action]').forEach((b) => { b.onclick = () => { const r = data.nodes.find((r) => r.node_id === b.dataset.powerAction), g = r.data.gpus[+b.dataset.gpu]; const watts = Number(b.parentElement.querySelector('input').value); runAction(r.node_id,{type:'set-gpu-power-limit',params:{uuid:g.uuid,watts,min:g.power_min_w,max:g.power_max_w,prev_w:g.power_limit_w}},`${g.name} の電力上限を ${watts} Wに設定`); }; });
    document.querySelectorAll('[data-plan-action]').forEach((b) => { b.onclick = () => { const r = data.nodes.find((r) => r.node_id === b.dataset.planAction); runAction(r.node_id,{type:'set-power-plan',params:{guid:b.parentElement.querySelector('select').value,prev_guid:r.data.power.plan_guid}},'電源プランを設定'); }; });
    document.querySelectorAll('[data-mac-action]').forEach((b) => { b.onclick = () => { const r = data.nodes.find((r) => r.node_id === b.dataset.macAction),p=r.data.power; runAction(r.node_id,{type:'set-low-power-mode',params:{source:p.on_battery ? 'battery' : 'ac',enabled:b.parentElement.querySelector('select').value === 'true',prev:p.low_power_mode}},'低電力モードを設定'); }; });
  }
  window.tune.onLive?.((event) => {
    if (state.view !== 'power' || typeof window.tune.powerSession !== 'function') return;
    const stopped = Object.values(event.nodes || {}).some((n) => n.state === 'stopped');
    if (!stopped && Date.now() - refreshAt < 5000) return;
    refreshAt = Date.now();
    for (const id of Object.keys(event.nodes || {})) window.tune.powerSession(id,true).then((o) => {
      const el = document.querySelector(`[data-power-session="${CSS.escape(id)}"]`);
      if (el) el.innerHTML = sessionHtml(o);
    }).catch(() => {});
  });
  (window.KT_VIEWS = window.KT_VIEWS || []).push({key:'power',label:'電力・Clock',icon:'resources',render});
})();
