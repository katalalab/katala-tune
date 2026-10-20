'use strict';
// task-completion.v1 の一時表示。外部参照の取得、保存、承認操作は行わない。
(() => {
  UI.ICON.review = '<svg viewBox="0 0 20 20"><path d="M5 2.75h7l3 3v11.5H5z"/><path d="M12 2.75v3h3M7.5 10h5M7.5 13h5"/></svg>';

  const MODEL = window.KT_REVIEW_MODEL;
  const MAX_FILE = 2 * 1024 * 1024;
  let current = null;
  let sourceInfo = null;
  let readToken = 0;
  let taskPage = 0;
  const TASKS_PER_PAGE = 25;
  const GATE_LABEL = { implementation: '実装', verification: '検証', integration: '正式反映', deployment: '配布', acceptance: '受入' };
  const DELIVERY_LABEL = { local: 'この環境', repository: '正式ブランチ', environment: '対象環境' };

  const esc = (value) => UI.esc(value == null ? '' : value);
  const text = (value) => esc(value == null ? '未記載' : value);
  const evidenceRef = (item) => item.link
    ? `<a href="${esc(item.link)}" target="_blank" rel="noopener noreferrer">${text(item.ref)}</a>`
    : text(item.ref);

  const reviewRef = (review) => review.link
    ? `<a href="${esc(review.link)}" target="_blank" rel="noopener noreferrer">${text(review.ref)}</a>`
    : text(review.ref);

  function warnings(items) {
    return items.length ? `<div class="review-warnings">${items.map((item) => `<div>注意: ${text(item)}</div>`).join('')}</div>` : '';
  }

  function evidenceHtml(items) {
    if (!items.length) return '<p class="note-line">証跡: 未記載</p>';
    return `<details class="review-evidence"><summary>証跡 ${items.length}件</summary>${items.map((item) => `<div class="review-evidence-row"><div>${evidenceRef(item)}</div><div class="muted">結果（台帳の申告）: ${text(item.result)} · 日時: ${text(item.observedAt)} · revision: ${text(item.revision)} · 対象: ${text(item.environment)}</div></div>`).join('')}</details>`;
  }

  function coverageHtml(coverage) {
    if (!coverage.declared) return '<p class="note-line">対象範囲未記載</p>';
    const count = (values, declared) => declared ? String(values.length) : '未設定';
    const list = (label, values, declared) => `<div><b>${label}</b>: ${values.length ? values.map(text).join('、') : declared ? 'なし' : '未設定'}</div>`;
    const notes = [
      coverage.missing.length ? `未観測: ${coverage.missing.join('、')}` : '',
      coverage.extra.length ? `余分な観測: ${coverage.extra.join('、')}` : '',
      coverage.unobserved.length ? `申告された未観測: ${coverage.unobserved.join('、')}` : '',
      coverage.overlap.length ? `観測と未観測が重複: ${coverage.overlap.join('、')}` : '',
      coverage.unobservedOutsideExpected.length ? `想定外の未観測: ${coverage.unobservedOutsideExpected.join('、')}` : '',
      coverage.duplicates.length ? `重複: ${coverage.duplicates.join('、')}` : '',
      ...coverage.insufficient,
    ].filter(Boolean);
    return `<details class="review-coverage"><summary>対象範囲（想定 ${count(coverage.expected, coverage.expectedDeclared)}・観測 ${count(coverage.observed, coverage.observedDeclared)}・未観測 ${count(coverage.unobserved, coverage.unobservedDeclared)}）</summary><div class="review-coverage-values">${list('想定', coverage.expected, coverage.expectedDeclared)}${list('観測', coverage.observed, coverage.observedDeclared)}${list('未観測', coverage.unobserved, coverage.unobservedDeclared)}${warnings(notes)}</div></details>`;
  }

  function taskHtml(task) {
    const delivery = task.delivery === MODEL.missing ? task.delivery : (DELIVERY_LABEL[task.delivery] || task.delivery);
    const deploymentOptional = task.delivery === 'local' || task.delivery === 'repository';
    const gates = MODEL.GATES.map((key) => {
      const gate = task.gates[key];
      const gateStatus = key === 'deployment' && deploymentOptional && gate.statusLabel === MODEL.missing
        ? `必須ではありません（依頼の終点: ${text(delivery)}）`
        : text(gate.statusLabel);
      return `<section class="review-gate"><h3>${text(GATE_LABEL[key])}</h3><p>${gateStatus}</p>${evidenceHtml(gate.evidence)}${warnings(gate.warnings)}</section>`;
    }).join('');
    return `<section class="review-task"><h2>${text(task.id)}</h2><div class="props review-props"><div class="prop"><div class="prop-k">目的</div><div class="prop-v">${text(task.goal)}</div></div><div class="prop"><div class="prop-k">対象</div><div class="prop-v">${text(task.target)}</div></div><div class="prop"><div class="prop-k">対象版</div><div class="prop-v">${text(task.revision)}</div></div><div class="prop"><div class="prop-k">依頼の終点</div><div class="prop-v">${text(delivery)}</div></div><div class="prop"><div class="prop-k">レビュー</div><div class="prop-v">${text(task.review.acceptedLabel)}</div></div><div class="prop"><div class="prop-k">レビュー証跡</div><div class="prop-v">${reviewRef(task.review)} · ${text(task.review.observedAt)} · revision: ${text(task.review.revision)} · 対象: ${text(task.review.target)}</div></div></div>${warnings(task.review.warnings)}<h3 class="review-heading">5つの段階</h3><div class="review-gates">${gates}</div></section>`;
  }

  function sourceHtml() {
    if (!sourceInfo) return '';
    return `<section class="review-source"><h2 class="review-heading">選択した原本</h2><div class="props review-props"><div class="prop"><div class="prop-k">ファイル</div><div class="prop-v">${text(sourceInfo.name)}</div></div><div class="prop"><div class="prop-k">サイズ</div><div class="prop-v">${sourceInfo.size.toLocaleString('ja-JP')} bytes</div></div><div class="prop"><div class="prop-k">SHA-256</div><div class="prop-v"><input class="review-fingerprint" type="text" readonly value="${text(sourceInfo.fingerprint || '指紋未計算')}" aria-label="原本SHA-256"></div></div></div></section>`;
  }

  function baseHtml(body) {
    return `${UI.head({ title: 'レビュー', desc: 'task-completion.v1 の申告を一時表示します。ここでは検証・受入・完了を判定しません。' })}<section class="review-input"><label for="reviewFile">完了台帳 JSON</label><input id="reviewFile" type="file" accept=".json,application/json"><p class="note-line">2 MiBまで。画面を離れると読み込んだファイルと表示を破棄します。</p></section><div class="callout t-gray"><div class="co-body"><div class="co-title">未照合の表示です</div><div class="co-text">台帳の申告のみ。PRの現在のheadとは照合していません。生成元は未確認です。</div></div></div>${sourceHtml()}${body}`;
  }

  function draw(message = '', loading = false) {
    const taskCount = current?.tasks.length || 0;
    const pages = Math.max(1, Math.ceil(taskCount / TASKS_PER_PAGE));
    taskPage = Math.min(taskPage, pages - 1);
    const first = taskPage * TASKS_PER_PAGE;
    const last = Math.min(first + TASKS_PER_PAGE, taskCount);
    const pager = current ? `<div class="review-pager"><span>${first + 1}–${last} / ${taskCount}件</span><button id="reviewPrev" class="btn small"${taskPage === 0 ? ' disabled' : ''}>前へ</button><button id="reviewNext" class="btn small"${taskPage + 1 === pages ? ' disabled' : ''}>次へ</button></div>` : '';
    const body = current ? `<div class="callout t-gray"><div class="co-body"><div class="co-title">操作者の確認事項</div><div class="co-text">依頼内容への合致、結果の実演、利用者・費用・権限・データへの影響、未確認事項、戻し方、PR head SHAと台帳のrevisionの一致を確認してください。</div></div></div>${pager}${coverageHtml(current.coverage)}${current.tasks.slice(first, last).map(taskHtml).join('')}` : message ? `<div class="callout ${loading ? 't-gray' : 't-orange'}"><div class="co-body"><div class="co-title">${loading ? '読み込み中' : '読み込めませんでした'}</div><div class="co-text">${text(message)}</div></div></div>` : '<div class="empty"><p>task-completion.v1 のJSONを選択してください。</p></div>';
    page(baseHtml(body));
    const input = document.querySelector('#reviewFile');
    if (input) input.onchange = (event) => load(event.target?.files?.[0]);
    const prev = document.querySelector('#reviewPrev');
    if (prev) prev.onclick = () => { if (taskPage > 0) { taskPage--; draw(); } };
    const next = document.querySelector('#reviewNext');
    if (next) next.onclick = () => { if (current && (taskPage + 1) * TASKS_PER_PAGE < current.tasks.length) { taskPage++; draw(); } };
  }

  async function load(file) {
    const token = ++readToken;
    current = null;
    sourceInfo = null;
    if (!file || !Number.isFinite(file.size) || file.size < 0) return draw('ファイルサイズを確認できません。');
    if (file.size > MAX_FILE) return draw('ファイルは2 MiB以下にしてください。');
    draw('読み込み中です。', true);
    try {
      const bytes = await file.arrayBuffer();
      if (token !== readToken) return;
      if (!bytes || typeof bytes.byteLength !== 'number' || bytes.byteLength > MAX_FILE) throw Error('ファイルを安全に読み込めません。');
      const fingerprint = await sha256(bytes);
      if (token !== readToken) return;
      sourceInfo = { name: typeof file.name === 'string' ? file.name : '名前未取得', size: file.size, fingerprint };
      const source = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
      current = MODEL.parse(JSON.parse(source));
      taskPage = 0;
      draw();
    } catch (error) {
      if (token !== readToken) return;
      current = null;
      draw(error instanceof SyntaxError ? 'JSONの形式が不正です。' : `入力を受け付けられません: ${error?.message || '不明なエラー'}`);
    }
  }

  async function sha256(bytes) {
    const subtle = globalThis.crypto?.subtle;
    if (!subtle) return null;
    try {
      const digest = await subtle.digest('SHA-256', bytes);
      return [...new Uint8Array(digest)].map((value) => value.toString(16).padStart(2, '0')).join('');
    } catch { return null; }
  }

  function render() {
    ticket();
    setCrumbs([{ icon: 'review', label: 'レビュー' }]);
    draw();
  }

  function leave() {
    readToken++;
    current = null;
    sourceInfo = null;
    taskPage = 0;
  }

  (window.KT_VIEWS ||= []).push({ key: 'review', label: 'レビュー', icon: 'review', render, leave });
})();
