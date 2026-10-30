'use strict';
// Pure comparison model is shared with the tests. The view uses only window.tune.
const SpecsModel = (() => {
  const categories = {system:'本体',cpu:'CPU',memory:'メモリ',gpu:'GPU',storage:'ストレージ',board:'マザーボード',firmware:'ファームウェア',os:'OS',display:'ディスプレイ',network:'ネットワーク',usb:'USB',thunderbolt:'Thunderbolt / USB4',pci:'PCI',audio:'オーディオ',settings:'機能・設定'};
  const labels = {model:'型番・モデル',manufacturer:'メーカー',part_number:'部品番号',slot:'スロット',cores:'物理コア',threads:'論理スレッド',p_cores:'Pコア',e_cores:'Eコア',architecture:'アーキテクチャ',max_clock_mhz:'OS報告の最大クロック (MHz)',current_clock_mhz:'取得時クロック (MHz)',l2_cache_kib:'L2キャッシュ (KiB)',l3_cache_kib:'L3キャッシュ (KiB)',capacity_bytes:'容量',total_capacity_bytes:'合計容量',capacity:'OS表記の容量',rated_speed_mts:'公称転送速度 (MT/s)',configured_speed_mts:'設定転送速度 (MT/s)',memory_type:'メモリ種別',form_factor:'フォームファクター',data_width_bits:'データ幅 (bit)',voltage_mv:'設定電圧 (mV)',memory_kind:'メモリ方式',dedicated_vram_bytes:'専用VRAM容量',vram:'OS表記のVRAM',driver_version:'ドライバー版',driver_date:'ドライバー日付',firmware_version:'ファームウェア版',bios_date:'BIOS日付',interface:'インターフェース',media_type:'媒体',link_speed:'リンク速度',status:'デバイス状態',resolution:'解像度',refresh_hz:'リフレッシュレート (Hz)',vendor_id:'ベンダーID',product_id:'製品ID',device_id:'デバイスID',class:'種別',version:'バージョン',build:'ビルド',kernel:'カーネル',metal:'Metal',cuda_version:'CUDA対応版',vbios_version:'VBIOS版',power_limit_w:'電力上限 (W)',pcie_generation:'PCIe世代',pcie_width:'PCIeレーン幅',secure_boot:'セキュアブート',virtualization_firmware:'BIOS仮想化',hypervisor_present:'ハイパーバイザー',slat:'SLAT',power_plan:'電源プラン',low_power_mode:'低電力モード',sleep_minutes:'スリープ (分)',display_sleep_minutes:'画面スリープ (分)',disk_sleep_minutes:'ディスクスリープ (分)',hibernate_mode:'休止モード',battery_cycles:'充放電回数',battery_condition:'バッテリー状態',battery_max_capacity_pct:'バッテリー最大容量 (%)',xmp_expo:'XMP / EXPO',boot_mode:'起動モード',duplex:'全二重',speed_bps:'リンク速度 (bit/s)',enabled:'有効',dhcp_enabled:'DHCP',mtu_bytes:'MTU (byte)',_coverage:'取得状況',_count:'デバイス数'};
  Object.assign(labels,{cpu_core_options:'CPUコア構成の選択肢',memory_bandwidth_gbs:'メモリ帯域 (GB/s)',socket:'ソケット',chipset:'チップセット',memory_slots:'メモリスロット数',supported_xmp:'対応XMP版',cluster0_name:'CPUクラスタ0 種別',cluster0_cores:'CPUクラスタ0 コア数',cluster1_name:'CPUクラスタ1 種別',cluster1_cores:'CPUクラスタ1 コア数',cuda_cores:'CUDAコア数',dedicated_vram_gib:'公称専用VRAM (GiB)',gpu_core_options:'GPUコア構成の選択肢',memory_bandwidth_options_gbs:'メモリ帯域の選択肢 (GB/s)',neural_engine_cores:'Neural Engineコア数',display_inches:'画面サイズ (インチ)',native_resolution:'パネル解像度',thunderbolt_ports:'Thunderboltポート数',thunderbolt_generation:'Thunderbolt世代',vram_options_gib:'VRAM構成の選択肢 (GiB)',memory_bus_bits:'メモリバス (bit)',max_turbo_ghz:'公称最大ターボ (GHz)',cache_mb:'公称キャッシュ (MB)'});
  const states = {ok:'取得済み',partial:'一部取得',empty:'OSからの報告なし',not_collected:'未取得',permission_denied:'権限不足',unsupported:'未対応',error:'取得失敗',timeout:'時間切れ',tool_missing:'取得ツールなし',output_limit:'取得量の上限'};
  const terms = {system_total:'システム合計メモリ',unified:'共有メモリ（CPU / GPU）',dedicated:'専用メモリ',unknown:'不明',enabled:'有効',disabled:'無効',balanced:'バランス',high_performance:'高パフォーマンス',power_saver:'省電力',custom_or_unknown:'カスタム／不明',not_exposed_by_standard_provider:'標準APIでは取得不可',not_collected_requires_recovery_policy_inspection:'未取得（Recoveryポリシーの確認が必要）',not_applicable:'対象外'};
  const esc = s => String(s ?? '').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const label = k => labels[k] || k;
  function format(k,v) {
    if(v == null || v === '') return '未取得';
    if(typeof v === 'boolean') return v ? '有効' : '無効';
    if(k==='memory_type' && typeof v==='number') return ({26:'DDR4',30:'LPDDR4',34:'DDR5',35:'LPDDR5'}[v] || `SMBIOS ${v}`);
    if(k==='form_factor' && typeof v==='number') return ({8:'DIMM',12:'SO-DIMM'}[v] || `SMBIOS ${v}`);
    if(k.endsWith('_date') && typeof v==='string' && /^\/Date\(-?\d+/.test(v)) return new Date(Number(/-?\d+/.exec(v)[0])).toLocaleDateString('ja-JP');
    if(k.endsWith('capacity_bytes') || k === 'dedicated_vram_bytes') return `${Number((Number(v)/2**30).toFixed(2))} GiB`;
    return terms[v] || (k === '_coverage' ? states[v] : null) || String(v);
  }
  function rows(nodes) {
    const rows = [];
    for(const [id,category] of Object.entries(categories)) {
      const sections = nodes.map(n=>n.report?.sections?.[id]);
      if(!sections.some(Boolean)) continue;
      const add = (field,cells) => {
        const multiple = cells.some(cell=>cell.length>1);
        const signatures = cells.map(cell=>JSON.stringify(cell.map(c=>multiple?[c.name,c.value]:c.value).sort((a,b)=>JSON.stringify(a).localeCompare(JSON.stringify(b)))));
        rows.push({key:`${id}.${field}`,id,category,field,label:label(field),cells,different:new Set(signatures).size>1});
      };
      add('_coverage',sections.map(s=>[{name:'',value:s?.status || 'not_collected',source:s?.source}]));
      add('_count',sections.map(s=>s && ['ok','partial','empty'].includes(s.status)?[{name:'',value:s.components?.length ?? 0}]:[]));
      const fields = new Set(sections.flatMap(s=>(s?.components || []).flatMap(c=>Object.keys(c.fields || {}))));
      for(const field of fields) add(field,sections.map(s=>(s?.components || []).filter(c=>c.fields?.[field] != null).map(c=>({name:[c.name,c.fields.slot].filter(Boolean).join(' · '),value:c.fields[field],source:c.source}))));
      if(id==='memory' && sections.some(s=>s?.components?.some(c=>typeof c.fields?.capacity_bytes === 'number'))) {
        add('total_capacity_bytes',sections.map(s=>{
          const all = s?.components || [];
          const unified = all.filter(c=>['unified','system_total'].includes(c.fields?.memory_kind) && typeof c.fields?.capacity_bytes==='number');
          const items = unified.length ? unified : all;
          const capacities = items.map(c=>c.fields?.capacity_bytes);
          return capacities.length && capacities.every(v=>typeof v==='number') ? [{name:'',value:capacities.reduce((a,b)=>a+b,0)}] : [];
        }));
      }
    }
    return rows;
  }
  const filter = (rows,q,diff) => rows.filter(r=>(!diff || r.different) && `${r.category} ${r.field} ${r.label} ${JSON.stringify(r.cells)}`.toLowerCase().includes(q.toLowerCase()));
  function table(nodes,rows) {
    if(!nodes.length) return '<div class="empty">比較する機体を選択してください。</div>';
    if(!rows.length) return '<div class="empty">該当する情報がありません。機体を選んで「仕様を収集」を実行してください。</div>';
    return `<div class="specs-scroll" role="region" aria-label="機体仕様の比較" tabindex="0"><table class="specs-table"><thead><tr><th scope="col">比較項目</th>${nodes.map(n=>`<th scope="col">${esc(n.node_id)}<small>${n.os==='macos'?'macOS':n.os==='windows'?'Windows':''}</small></th>`).join('')}</tr></thead><tbody>${rows.map(r=>`<tr class="${r.different?'specs-diff':''}"><th scope="row"><small>${esc(r.category)}</small>${esc(r.label)}</th>${r.cells.map(cell=>`<td>${cell.length?cell.map(c=>`<div class="specs-value" title="${esc(c.source || '')}">${cell.length>1?`<small>${esc(c.name)}</small>`:''}${esc(format(r.field,c.value))}</div>`).join(''):'<span class="muted">未取得</span>'}</td>`).join('')}</tr>`).join('')}</tbody></table></div>`;
  }
  function officialDetails(nodes) {
    return nodes.map(n=>`<section class="specs-sources"><h3>${esc(n.node_id)} の公式情報・取得元</h3>${Object.entries(n.report?.sections || {}).map(([id,s])=>(s.components?.length || s.providers?.length)?`<details><summary>${esc(categories[id] || id)} · ${s.components.length} 件</summary><p class="muted">${(s.providers || []).map(p=>`${esc(p.source)}: ${esc(states[p.status] || p.status)}`).join('<br>')}</p>${s.components.map(c=>{
      const official = c.official || {};
      const safe = typeof official.url === 'string' && /^https:\/\/(?:[a-z0-9-]+\.)*(?:nvidia\.com|intel\.com|apple\.com|asus\.com|asrock\.com|amd\.com)\//i.test(official.url);
      return `<div class="specs-component"><strong>${esc(c.name)}</strong><small>取得元: ${esc(c.source || s.source || '未取得')}</small>${official.status==='matched' && safe?`<p>公式モデル照合済み · 確認日 ${esc(official.verified_at)}<br><a href="${esc(official.url)}" target="_blank" rel="noopener noreferrer">${esc(official.name || 'メーカー公式仕様')}</a> <button class="btn ghost" data-spec-url="${esc(official.url)}">URLをコピー</button></p><dl>${Object.entries(official.specifications || {}).map(([k,v])=>`<dt>${esc(label(k))}</dt><dd>${esc(v)}</dd>`).join('')}</dl><small>${esc(official.scope || '')}</small>`:'<p class="muted">公式仕様: 未照合（完全一致する型番の登録なし）</p>'}<small>未取得の識別・版情報: ${esc((c.missing_fields || []).map(label).join('、') || 'なし')}</small></div>`;
    }).join('')}</details>`:'').join('')}</section>`).join('');
  }
  return {rows,filter,table,officialDetails,format,label,esc};
})();
if(typeof module !== 'undefined' && module.exports) module.exports = SpecsModel;
if(typeof window !== 'undefined') (()=>{
  let data = null, selected = null, query = '', differences = false, running = false, error = '', seq = 0;
  const $ = s=>document.querySelector(s);
  const eligible = n=>n.eligible !== false;
  async function render() {
    const stamp = ++seq;
    setCrumbs([{icon:'resources',label:'スペック比較'}]);
    const supported = typeof window.tune.specsView==='function';
    if(supported && !data) {
      try {data = await window.tune.specsView();} catch(e) {error=String(e.message || e);}
      if(stamp!==seq || state.view!=='specs') return;
    }
    const nodes = data?.nodes || state.nodes.map(n=>({node_id:n.id,os:n.os,eligible:!n.shared && (!n.status || n.status==='online')}));
    if(selected===null) selected=new Set(nodes.filter(eligible).map(n=>n.node_id));
    const chosen = nodes.filter(n=>selected.has(n.node_id));
    const allRows = SpecsModel.rows(chosen);
    const rows = SpecsModel.filter(allRows,query,differences);
    const busy = running || data?.running;
    const available = chosen.filter(eligible).map(n=>n.node_id);
    const head = UI.head({icon:'resources',title:'スペック比較',desc:'Mac / Windowsの構成・バージョン・設定を、取得元と公式仕様とともに比較します。',actions:`<button class="btn" id="specsReload">保存済みを再読込</button><button class="btn" id="specsCopy" ${!chosen.some(n=>n.report)?'disabled':''}>JSONをコピー</button><button class="btn primary" id="specsRun" ${!supported || busy || !available.length?'disabled':''}>${busy?'収集中…':'仕様を収集'}</button>`});
    const notices = !supported ? UI.callout({tone:'orange',title:'Rust版で利用できます',body:'Tauri版のKatala Tuneに切り替えてください。'}) : '';
    page('<div class="specs-view">'+head+notices+`<div role="status" aria-live="polite" class="specs-notice">${error?SpecsModel.esc(error):busy?'選択した機体から読み取り中です。':''}</div><fieldset class="specs-selection"><legend>比較する機体</legend>${nodes.map(n=>`<label><input type="checkbox" data-spec-node="${SpecsModel.esc(n.node_id)}" ${selected.has(n.node_id)?'checked':''}>${SpecsModel.esc(n.node_id)} <small>${n.os==='macos'?'Mac':'Win'}${n.eligible===false?' · 収集対象外':''}</small></label>`).join('')}</fieldset><div class="specs-freshness">${chosen.map(n=>`<div><strong>${SpecsModel.esc(n.node_id)}</strong> ${n.report?`${n.stale?'前回値・要更新':'取得済み'} · ${SpecsModel.esc(new Date(n.report.at).toLocaleString('ja-JP'))}`:'未収集'}${n.last_attempt?.ok===false?`<span class="specs-error">最終試行失敗: ${SpecsModel.esc(n.last_attempt.error)}</span>`:''}</div>`).join('')}</div><div class="specs-toolbar">${UI.search({id:'specsSearch',value:query,label:'型番・項目を検索',placeholder:'型番・項目を検索'})}<label><input id="specsDiff" type="checkbox" ${differences?'checked':''}> 差分のみ</label><span class="muted" id="specsCount" role="status">${rows.length} 項目</span></div><p class="muted">未取得は非搭載を意味しません。CPU/GPUの最大値は実効性能の順位ではありません。公式仕様は登録済み型番のみ照合し、設定値と区別します。</p>`+'<div id="specsTable">'+SpecsModel.table(chosen,rows)+'</div>'+SpecsModel.officialDetails(chosen)+'</div>');
    document.querySelectorAll('[data-spec-node]').forEach(el=>el.onchange=()=>{if(el.checked)selected.add(el.dataset.specNode);else selected.delete(el.dataset.specNode);render();});
    $('#specsDiff').onchange=e=>{differences=e.target.checked;render();};
    $('#specsSearch').oninput=e=>{
      query=e.target.value;
      const filtered = SpecsModel.filter(allRows,query,differences);
      $('#specsTable').innerHTML = SpecsModel.table(chosen,filtered);
      $('#specsCount').textContent = `${filtered.length} 項目`;
    };
    $('#specsReload').onclick=async()=>{data=null;await render();};
    $('#specsCopy').onclick=()=>window.tune.copy(JSON.stringify({schema:'katala_specs_comparison.v1',nodes:chosen},null,2));
    document.querySelectorAll('[data-spec-url]').forEach(el=>el.onclick=()=>window.tune.copy(el.dataset.specUrl));
    $('#specsRun').onclick=async()=>{
      if(running)return;running=true;error='';render();
      try {data=await window.tune.specsRefresh(available);}catch(e){error=String(e.message || e);}
      finally {running=false;if(state.view==='specs')render();}
    };
  }
  (window.KT_VIEWS=window.KT_VIEWS || []).push({key:'specs',label:'スペック比較',icon:'resources',render,leave:()=>{seq++;}});
})();
