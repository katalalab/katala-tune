'use strict';
/* renderer/ui.js — Notion 風の画面部品（HTML 文字列を返す）
 *
 * どの画面からでも使い回す。値は中でエスケープするが、引数名が html / body / summary のものは
 * 呼び出し側で組み立てた HTML をそのまま入れる（その中の値は呼び出し側で UI.esc を通すこと）。
 *
 *   UI.esc(v)                                   HTML に入れる値は必ず通す
 *   UI.icon(name)                               単色の線アイコン（UI.ICON のキー）
 *   UI.head({ icon, title, desc, props, actions })
 *       ページ見出し。アイコン・大きめのタイトル・説明・プロパティ（UI.props）・右上のボタン
 *   UI.props([[ラベル, html, アイコン], ...])     ページのプロパティ（ラベルと値の並び）。偽の要素は飛ばす
 *   UI.section(タイトル, ヒント, 右側の html)     見出し（h2）
 *   UI.tabs([[key, ラベル, 件数, アイコン], ...], 選択中の key)   ビューの切り替え。ボタンに data-tab が付く
 *   UI.chip(文字, tone, { dot, title })          チップ／タグ。tone: default・gray・brown・orange・yellow・green・blue・purple・pink・red
 *   UI.status('ok' | 'warn' | 'fail' | 'unknown', { title, label })   状態のチップ（点つき）
 *   UI.dot(status, title)                       状態の点だけ（狭い所に）
 *   UI.callout({ tone, icon, title, body })     コールアウト（注意・異常の囲み）
 *   UI.toggle({ summary, body, open, cls, attrs })   トグル（<details>）。summary と body は HTML
 *   UI.table({ cols, rows, row, rowAttrs, empty, cls })
 *       データベース風の表（固定ヘッダ・行ホバー・細い区切り）。cols = [{ label, cls: 'n'（右寄せ）| 'c'（中央）, icon, width }]
 *       row(r, i) は <td>…</td> の並びを返す。rowAttrs(r, i) は <tr> に足す属性（data-goto など）
 *   UI.matrix({ rows, cols, cell, corner })
 *       行×列のマトリクス（機体×項目、道具×機体など）。rows / cols = [{ key, label, html?, sub?, attrs? }]
 *       cell(r, c) = { html } か { text, tone, title, diff, cls }。diff: true のセルは強調する（版の違いなど）
 *   UI.search({ id, placeholder, value })       検索欄
 *   UI.select({ id, label, options: [[value, 表示], ...], value })   絞り込みのピル（<select>）
 *   UI.seg({ attr, options: [[value, 表示], ...], value })            セグメント。ボタンに data-<attr> が付く
 *   UI.empty(文字, action の html)               何もないときの表示
 */
const UI = (() => {
  const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

  // 20×20 の線アイコン。色は currentColor
  const ICON = {
    overview: '<svg viewBox="0 0 20 20"><rect x="3" y="3" width="6" height="6" rx="1.5"/><rect x="11" y="3" width="6" height="6" rx="1.5"/><rect x="3" y="11" width="6" height="6" rx="1.5"/><rect x="11" y="11" width="6" height="6" rx="1.5"/></svg>',
    status: '<svg viewBox="0 0 20 20"><path d="M2.5 10.5h3.2l2-5.2 4 10 2.2-4.8h3.6"/></svg>',
    resources: '<svg viewBox="0 0 20 20"><rect x="3" y="11" width="3" height="6" rx="1"/><rect x="8.5" y="7" width="3" height="10" rx="1"/><rect x="14" y="3" width="3" height="14" rx="1"/></svg>',
    procs: '<svg viewBox="0 0 20 20"><rect x="3" y="3" width="14" height="14" rx="2"/><path d="M7 7h6M7 10h6M7 13h4"/></svg>',
    jobs: '<svg viewBox="0 0 20 20"><rect x="3" y="4" width="14" height="13" rx="2"/><path d="M3 8h14M7 2.5v3M13 2.5v3"/></svg>',
    logs: '<svg viewBox="0 0 20 20"><path d="M4 5h12M4 10h12M4 15h8"/></svg>',
    actions: '<svg viewBox="0 0 20 20"><circle cx="10" cy="10" r="7"/><path d="M10 6v4l2.5 2"/></svg>',
    mac: '<svg viewBox="0 0 20 20"><rect x="3" y="4" width="14" height="9.5" rx="1.5"/><path d="M7.5 16.5h5M10 13.5v3"/></svg>',
    win: '<svg viewBox="0 0 20 20"><rect x="3" y="3.5" width="14" height="13" rx="1.5"/><path d="M10 3.5v13M3 10h14"/></svg>',
    search: '<svg viewBox="0 0 20 20"><circle cx="9" cy="9" r="5"/><path d="M13 13l4 4"/></svg>',
    config: '<svg viewBox="0 0 20 20"><path d="M5.5 2.75h6l3.75 3.75v10.75H5.5z"/><path d="M11.5 2.75V6.5h3.75M8 10.5h5M8 13.5h5"/></svg>',
    data: '<svg viewBox="0 0 20 20"><path d="M2.75 5.5A1.5 1.5 0 0 1 4.25 4h3.2l1.6 1.75h6.7a1.5 1.5 0 0 1 1.5 1.5v7.25a1.5 1.5 0 0 1-1.5 1.5H4.25a1.5 1.5 0 0 1-1.5-1.5z"/></svg>',
    sync: '<svg viewBox="0 0 20 20"><path d="M4 10a6 6 0 0 1 10.2-4.3M16 10a6 6 0 0 1-10.2 4.3M14.5 2.5v3.5H11M5.5 17.5V14H9"/></svg>',
    fleet: '<svg viewBox="0 0 20 20"><path d="M3 15l4-5 3 3 4-6 3 4"/></svg>',
    play: '<svg viewBox="0 0 20 20"><path d="M6.5 4.5l9 5.5-9 5.5z" fill="currentColor"/></svg>',
    info: '<svg viewBox="0 0 20 20"><circle cx="10" cy="10" r="7.25"/><path d="M10 9v4.6M10 6.5v.1"/></svg>',
    alert: '<svg viewBox="0 0 20 20"><path d="M10 3.2l7.4 12.8H2.6z"/><path d="M10 8.2v3.6M10 13.7v.1"/></svg>',
    check: '<svg viewBox="0 0 20 20"><circle cx="10" cy="10" r="7.25"/><path d="M6.8 10.2l2.2 2.2 4.2-4.6"/></svg>',
    cross: '<svg viewBox="0 0 20 20"><circle cx="10" cy="10" r="7.25"/><path d="M7.5 7.5l5 5M12.5 7.5l-5 5"/></svg>',
    chevron: '<svg viewBox="0 0 20 20"><path d="M8 5l5 5-5 5"/></svg>',
    copy: '<svg viewBox="0 0 20 20"><rect x="7" y="7" width="9" height="9" rx="1.5"/><path d="M13 7V5.5A1.5 1.5 0 0 0 11.5 4h-6A1.5 1.5 0 0 0 4 5.5v6A1.5 1.5 0 0 0 5.5 13H7"/></svg>',
    bulb: '<svg viewBox="0 0 20 20"><path d="M7.5 14.5h5M8.2 17h3.6M10 2.8a5 5 0 0 0-3 9c.6.5 1 1.2 1 2v.7h4v-.7c0-.8.4-1.5 1-2a5 5 0 0 0-3-9z"/></svg>',
    history: '<svg viewBox="0 0 20 20"><path d="M3 16.5h14M4.5 13l3.5-4 3 2.5 4.5-6"/></svg>',
    machine: '<svg viewBox="0 0 20 20"><rect x="5" y="5" width="10" height="10" rx="1.5"/><path d="M8 2.5V5M12 2.5V5M8 15v2.5M12 15v2.5M2.5 8H5M2.5 12H5M15 8h2.5M15 12h2.5"/></svg>',
    gauge: '<svg viewBox="0 0 20 20"><path d="M3.5 14.5a6.5 6.5 0 1 1 13 0"/><path d="M10 14.5l3.2-4.6"/></svg>',
    link: '<svg viewBox="0 0 20 20"><path d="M8.5 11.5l3-3M7 9l-1.8 1.8a2.8 2.8 0 0 0 4 4L11 13M9 7l1.8-1.8a2.8 2.8 0 0 1 4 4L13 11"/></svg>',
    tag: '<svg viewBox="0 0 20 20"><path d="M3 3.5h6.2l7.3 7.3-5.7 5.7-7.3-7.3z"/><circle cx="6.8" cy="7.2" r="1"/></svg>',
    tool: '<svg viewBox="0 0 20 20"><path d="M12.6 3.2a4 4 0 0 0-4.9 5.3l-4.6 4.6a1.6 1.6 0 0 0 2.3 2.3l4.6-4.6a4 4 0 0 0 5.3-4.9l-2.4 2.4-2.1-.6-.6-2.1z"/></svg>',
    filter: '<svg viewBox="0 0 20 20"><path d="M3.5 5h13M6 10h8M8.5 15h3"/></svg>',
  };
  const icon = (name, cls = '') => (ICON[name] || '').replace('<svg ', `<svg class="i${cls ? ' ' + cls : ''}" aria-hidden="true" `);

  const STATUS = { ok: ['正常', 'green'], warn: ['注意', 'orange'], fail: ['異常', 'red'], unknown: ['不明', 'gray'] };
  const attr = (k, v) => (v ? ` ${k}="${esc(v)}"` : '');

  const chip = (text, tone = 'default', { dot = false, title = '', cls = '' } = {}) =>
    `<span class="chip t-${tone}${dot ? ' chip-dot' : ''}${cls ? ' ' + cls : ''}"${attr('title', title)}>${esc(text)}</span>`;

  function status(st, { title = '', label } = {}) {
    const [l, tone] = STATUS[st] || STATUS.unknown;
    return chip(label ?? l, tone, { dot: true, title });
  }
  const dot = (st, title = '') => `<span class="sdot s-${esc(st)}"${attr('title', title)}></span>`;

  function head({ icon: ic, title, desc = '', props: p = '', actions = '' }) {
    return `<header class="ph">${ic ? `<div class="ph-icon">${icon(ic)}</div>` : ''}`
      + `<div class="ph-row"><h1 class="ph-title">${esc(title)}</h1>${actions ? `<div class="ph-actions">${actions}</div>` : ''}</div>`
      + `${desc ? `<p class="ph-desc">${esc(desc)}</p>` : ''}${p}</header>`;
  }

  const props = (rows) => `<div class="props">${rows.filter(Boolean).map(([label, html, ic]) =>
    `<div class="prop"><div class="prop-k">${ic ? icon(ic) : ''}<span>${esc(label)}</span></div><div class="prop-v">${html}</div></div>`).join('')}</div>`;

  const section = (title, hint = '', extra = '') =>
    `<h2 class="sec"><span>${esc(title)}</span>${hint ? `<span class="hint">${esc(hint)}</span>` : ''}${extra ? `<span class="sec-extra">${extra}</span>` : ''}</h2>`;

  const tabs = (list, active) => `<div class="tabs" role="tablist">${list.map(([k, label, count, ic]) =>
    `<button class="tab${k === active ? ' on' : ''}" role="tab" aria-selected="${k === active}" data-tab="${esc(k)}">${ic ? icon(ic) : ''}<span>${esc(label)}</span>${count != null && count !== '' ? `<span class="tab-count">${esc(count)}</span>` : ''}</button>`).join('')}</div>`;

  const callout = ({ tone = 'gray', icon: ic = 'info', title = '', body = '', cls = '' }) =>
    `<div class="callout t-${tone}${cls ? ' ' + cls : ''}"><div class="co-icon">${icon(ic)}</div><div class="co-body">${title ? `<div class="co-title">${esc(title)}</div>` : ''}${body ? `<div class="co-text">${body}</div>` : ''}</div></div>`;

  const toggle = ({ summary, body, open = false, cls = '', attrs = '' }) =>
    `<details class="toggle${cls ? ' ' + cls : ''}"${open ? ' open' : ''}${attrs ? ' ' + attrs : ''}><summary><span class="caret" aria-hidden="true"></span><span class="tg-sum">${summary}</span></summary><div class="tg-body">${body}</div></details>`;

  function table({ cols, rows, row, rowAttrs = () => '', empty: emptyText = 'なし', cls = '' }) {
    const th = cols.map((c) => `<th${attr('class', c.cls)}${c.width ? ` style="width:${esc(c.width)}"` : ''}>${c.icon ? icon(c.icon) : ''}${esc(c.label ?? '')}</th>`).join('');
    const body = rows.length
      ? rows.map((r, i) => `<tr ${rowAttrs(r, i)}>${row(r, i)}</tr>`).join('')
      : `<tr class="empty-row"><td colspan="${cols.length}">${esc(emptyText)}</td></tr>`;
    return `<div class="db${cls ? ' ' + cls : ''}"><table class="dbt"><thead><tr>${th}</tr></thead><tbody>${body}</tbody></table></div>`;
  }

  function matrix({ rows, cols, cell, corner = '', cls = '' }) {
    const th = `<th class="mx-corner">${esc(corner)}</th>${cols.map((c) => `<th class="c mx-col"${c.attrs ? ' ' + c.attrs : ''}>${c.html ?? esc(c.label)}</th>`).join('')}`;
    const body = rows.map((r) => `<tr><th class="mx-row" scope="row"${r.attrs ? ' ' + r.attrs : ''}>${r.html ?? esc(r.label)}${r.sub ? `<small>${esc(r.sub)}</small>` : ''}</th>${cols.map((c) => {
      const x = cell(r, c) || {};
      const inner = x.html ?? (x.text != null ? chip(x.text, x.tone || 'default') : '<span class="muted">–</span>');
      return `<td class="c mx${x.diff ? ' diff' : ''}${x.cls ? ' ' + x.cls : ''}"${attr('title', x.title)}>${inner}</td>`;
    }).join('')}</tr>`).join('');
    return `<div class="db matrix${cls ? ' ' + cls : ''}"><table class="dbt"><thead><tr>${th}</tr></thead><tbody>${body}</tbody></table></div>`;
  }

  const search = ({ id, placeholder = '', value = '' }) =>
    `<label class="search">${icon('search')}<input id="${esc(id)}" type="search" placeholder="${esc(placeholder)}" value="${esc(value)}" autocomplete="off" spellcheck="false"></label>`;

  // 先頭の選択肢（「すべて」など）以外を選んでいるときは色を付けて、絞り込み中だと分かるようにする。
  // active: false を渡すと色を付けない（設定の値など）
  const select = ({ id, label = '', options, value, active }) => {
    const on = active ?? (value !== '' && value != null && options[0] && String(options[0][0]) !== String(value));
    return `<span class="pill-select${on ? ' active' : ''}">${label ? `<span class="ps-label">${esc(label)}</span>` : ''}<select id="${esc(id)}"${label ? ` aria-label="${esc(label)}"` : ''}>${options.map(([v, t]) => `<option value="${esc(v)}"${String(v) === String(value) ? ' selected' : ''}>${esc(t)}</option>`).join('')}</select></span>`;
  };

  const seg = ({ attr: a, options, value }) =>
    `<div class="seg">${options.map(([v, t]) => `<button data-${a}="${esc(v)}" class="${String(v) === String(value) ? 'on' : ''}">${esc(t)}</button>`).join('')}</div>`;

  const empty = (text, action = '') => `<div class="empty"><p>${esc(text)}</p>${action}</div>`;

  return { esc, ICON, icon, chip, status, dot, head, props, section, tabs, callout, toggle, table, matrix, search, select, seg, empty, STATUS };
})();

if (typeof module !== 'undefined' && module.exports) module.exports = UI;
