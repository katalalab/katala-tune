'use strict';
/* renderer/charts.js — 外部ライブラリなしのグラフ部品（SVG と CSS）
 *
 * どの関数も HTML 文字列を返すので、テンプレート文字列にそのまま埋め込める。値は中で必ずエスケープする。
 * 色は tone で指定し、style.css の .tone-* と CSS 変数で塗る。ライト／ダークやアクセントカラーが変わっても
 * CSS だけで追従するので、描き直しは要らない。
 *   tone: ok（正常）・warn（注意）・crit（異常）・unknown（不明）・info・accent・neutral
 *
 * 使い方
 *   Charts.meter(78, { label: 'メモリ', text: '78%', warn: 75, crit: 90 })
 *       横棒。値が warn 以上で注意、crit 以上で異常の色になり、しきい値の位置に細い目盛りを打つ。
 *       invert: true は「空き」のように小さいほど悪い値。max の既定は 100。値が null なら空の棒と「-」。
 *       label を省くと表のセルに入れる形（棒と数字だけ）になる。
 *   Charts.ring(86, { size: 44, label: true })
 *       スコアのリング。80 以上は正常、55 以上は注意、それ未満は異常の色。null は灰色で「–」。
 *       label: false で中の数字を描かない（数字を横に並べる小さいリング向け）。
 *   Charts.gauge(64, { label: '平均スコア' })
 *       半円のゲージ。しきい値の帯（0–55–80–100）の上に値の弧を重ねる。
 *   Charts.line(points, { unit: 'ms', tone: 'accent', min: 0, max: 100, marks: [{ v: 80, tone: 'ok' }], better: 'low' })
 *       折れ線。points = [{ t: ミリ秒, v: 数値 | null }]（null の点で線を切る）。
 *       縦軸の目盛り・横軸の日時・最新値・最小／最大の印と、下に「最新・最小・最大・回数」を書く。
 *       min / max を省くと値から切りの良い範囲を決める。better は 'low'（小さいほど良い）か 'high'。
 *   Charts.stacked([{ label: '異常', value: 3, tone: 'crit' }, ...], { legend: true })
 *       100% 積み上げの横棒と凡例（件数と割合）。内訳の比較に使う。
 *   Charts.bucketize(rows, { from, to, count })
 *       rows = [{ ts, series }] を期間 count 等分の buckets = [{ t0, t1, values: { [series]: 件数 } }] にする。
 *   Charts.columns(buckets, { series: [{ key: 'error', label: 'エラー', tone: 'crit' }, ...], label: (b, i) => '10/7', tip: (b) => '…' })
 *       時間の推移を積み上げの縦棒で描く。棒にカーソルを合わせると内訳（tip の文字＋系列ごとの件数）が出る。
 *       label を渡すと各棒の下の文字をそれで決める（空文字なら書かない）。省くと日時を 6 本ほど間引いて書く。
 *   Charts.heatmap({ rows: [{ key, label }], cols: [{ key, label }], value: (r, c) => 件数, tone: 'crit', title: (r, c, n) => '…' })
 *       行×列の濃淡（最大値に対する割合で 5 段階）。列ラベルが空文字なら表示しない。
 *   Charts.dataBar(value, max, { tone })
 *       表のセルに入れる相対値の細い棒（同じ列の中で大きさを比べる）。
 *   Charts.scoreTone(score) / Charts.level(value, { warn, crit, invert })
 *       色の判定だけを使いたいとき。
 */
const Charts = (() => {
  const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
  const num = (v) => v != null && v !== '' && Number.isFinite(+v);
  const r2 = (v) => Math.round(v * 100) / 100;

  // 表示用の数値。大きい値は整数、小さい値は小数第1〜2位まで（末尾の 0 は落とす）
  function fmt(v, digits) {
    if (!num(v)) return '–';
    const a = Math.abs(v);
    const d = digits ?? (a >= 100 ? 0 : a >= 10 ? 1 : 2);
    return String(Number((+v).toFixed(d)));
  }

  function scoreTone(s) {
    if (!num(s)) return 'unknown';
    return s >= 80 ? 'ok' : s >= 55 ? 'warn' : 'crit';
  }

  // しきい値による色。invert は小さいほど悪い値（空きなど）
  function level(v, { warn, crit, invert = false } = {}) {
    if (!num(v)) return 'none';
    const over = (t) => t != null && (invert ? v <= t : v >= t);
    return over(crit) ? 'crit' : over(warn) ? 'warn' : 'neutral';
  }

  // 切りの良い目盛り（1・2・5 刻み）
  function niceTicks(lo, hi, n = 4) {
    if (!(hi > lo)) { lo -= 1; hi += 1; }
    const step0 = (hi - lo) / Math.max(1, n - 1);
    const mag = 10 ** Math.floor(Math.log10(step0));
    const step = [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= step0 - 1e-12);
    const a = Math.floor(lo / step + 1e-9) * step;
    const b = Math.ceil(hi / step - 1e-9) * step;
    const out = [];
    for (let v = a; v <= b + step * 1e-6; v += step) out.push(Number(v.toFixed(10)));
    return out;
  }

  function fmtT(t, span) {
    const d = new Date(t);
    const md = `${d.getMonth() + 1}/${d.getDate()}`;
    const hm = `${d.getHours()}:${String(d.getMinutes()).padStart(2, '0')}`;
    return span <= 2 * 86400e3 ? `${md} ${hm}` : md;
  }

  // ---- 横棒 ----
  function meter(value, o = {}) {
    const { label, text, warn, crit, invert = false, max = 100 } = o;
    const has = num(value);
    const pct = has ? clamp((value / max) * 100, 0, 100) : 0;
    const lv = level(value, { warn, crit, invert });
    const ticks = [warn, crit].filter((x) => x != null).map((x) => `<b style="left:${r2(clamp((x / max) * 100, 0, 100))}%"></b>`).join('');
    const shown = text ?? (has ? `${Math.round(value)}%` : '-');
    return `<div class="meter${label == null ? ' bare' : ''} lv-${lv}" role="meter" aria-label="${esc(label ?? '')} ${esc(shown)}" aria-valuemin="0" aria-valuemax="${max}"${has ? ` aria-valuenow="${r2(value)}"` : ''}>`
      + `${label != null ? `<span class="m-label">${esc(label)}</span>` : ''}`
      + `<span class="m-track">${has ? `<i style="width:${r2(Math.max(pct, 1.5))}%"></i>` : ''}${ticks}</span>`
      + `<span class="m-text">${esc(shown)}</span></div>`;
  }

  // ---- リング ----
  function ring(score, { size = 44, stroke, label = true } = {}) {
    const sw = stroke ?? Math.max(3, Math.round(size / 10));
    const r = (size - sw) / 2;
    const c = 2 * Math.PI * r;
    const has = num(score);
    const off = has ? c * (1 - clamp(score, 0, 100) / 100) : c;
    const h = size / 2;
    return `<div class="ring tone-${scoreTone(score)}" style="width:${size}px;height:${size}px" role="img" aria-label="スコア ${has ? score : 'なし'}">`
      + `<svg viewBox="0 0 ${size} ${size}" width="${size}" height="${size}"><circle class="ring-track" cx="${h}" cy="${h}" r="${r2(r)}" stroke-width="${sw}"/>`
      + `${has ? `<circle class="ring-val" cx="${h}" cy="${h}" r="${r2(r)}" stroke-width="${sw}" stroke-dasharray="${r2(c)}" stroke-dashoffset="${r2(off)}" transform="rotate(-90 ${h} ${h})"/>` : ''}</svg>`
      + `${label ? `<span style="font-size:${Math.max(10, Math.round(size * 0.31))}px">${has ? Math.round(score) : '–'}</span>` : ''}</div>`;
  }

  // ---- 半円のゲージ ----
  function gauge(value, { label = '', sub = '' } = {}) {
    const CX = 60, CY = 58, R = 46, SW = 9;
    const pt = (f) => { const a = Math.PI * (1 - f); return [r2(CX + R * Math.cos(a)), r2(CY - R * Math.sin(a))]; };
    const arc = (f0, f1) => { const [x0, y0] = pt(f0); const [x1, y1] = pt(f1); return `M${x0} ${y0} A${R} ${R} 0 0 1 ${x1} ${y1}`; };
    const has = num(value);
    const v = has ? clamp(value, 0, 100) / 100 : 0;
    const bands = [[0, 0.55, 'crit'], [0.55, 0.8, 'warn'], [0.8, 1, 'ok']]
      .map(([a, b, t]) => `<path class="g-band tone-${t}" d="${arc(a + (a ? 0.006 : 0), b - (b < 1 ? 0.006 : 0))}" stroke-width="${SW}"/>`).join('');
    const val = has && v > 0 ? `<path class="g-val tone-${scoreTone(value)}" d="${arc(0, Math.max(v, 0.012))}" stroke-width="${SW}"/>` : '';
    const [nx, ny] = pt(v);
    const needle = has ? `<circle class="g-knob tone-${scoreTone(value)}" cx="${nx}" cy="${ny}" r="${SW / 2 + 1.5}"/>` : '';
    return `<div class="gauge" role="img" aria-label="${esc(label)} ${has ? Math.round(value) : 'なし'}">`
      + `<svg viewBox="0 0 120 72">${bands}${val}${needle}`
      + `<text class="g-num${has ? '' : ' none'}" x="${CX}" y="${CY - 2}" text-anchor="middle">${has ? Math.round(value) : '–'}</text>`
      + `<text class="g-end" x="${CX - R}" y="${CY + 15}" text-anchor="middle">0</text><text class="g-end" x="${CX + R}" y="${CY + 15}" text-anchor="middle">100</text></svg>`
      + `${label ? `<div class="g-label">${esc(label)}</div>` : ''}${sub ? `<div class="g-sub">${esc(sub)}</div>` : ''}</div>`;
  }

  // ---- 折れ線 ----
  function line(points, o = {}) {
    const W = o.width || 560, H = o.height || 176;
    const pad = { l: 42, r: 62, t: 18, b: 26 };
    const unit = o.unit ? `${o.unit === '%' ? '' : ' '}${o.unit}` : '';
    const all = (points || []).filter((p) => p && num(p.t));
    const pts = all.filter((p) => num(p.v));
    if (pts.length < 2) return `<div class="chart-empty">${esc(o.empty || '2回以上分析すると推移を表示します')}</div>`;
    const vals = pts.map((p) => +p.v);
    const t0 = Math.min(...all.map((p) => p.t)), t1 = Math.max(...all.map((p) => p.t));
    const span = Math.max(1, t1 - t0);
    let ticks;
    if (o.min != null && o.max != null) ticks = niceTicks(o.min, o.max, o.ticks || 5);
    else {
      const lo = o.min ?? Math.min(...vals), hi = o.max ?? Math.max(...vals);
      ticks = niceTicks(lo, hi === lo ? lo + 1 : hi, o.ticks || 4);
    }
    const lo = ticks[0], hi = ticks.at(-1);
    const x = (t) => r2(pad.l + ((t - t0) / span) * (W - pad.l - pad.r));
    const y = (v) => r2(pad.t + (1 - (v - lo) / (hi - lo)) * (H - pad.t - pad.b));
    const base = H - pad.b;

    const grid = ticks.map((v) => `<line class="c-grid" x1="${pad.l}" x2="${W - pad.r}" y1="${y(v)}" y2="${y(v)}"/><text class="c-axis" x="${pad.l - 7}" y="${y(v) + 3.5}" text-anchor="end">${esc(fmt(v))}</text>`).join('');
    const marks = (o.marks || []).filter((m) => m.v > lo && m.v < hi)
      .map((m) => `<line class="c-mark tone-${m.tone || 'neutral'}" x1="${pad.l}" x2="${W - pad.r}" y1="${y(m.v)}" y2="${y(m.v)}"/>`).join('');
    const mid = t0 + span / 2;
    const xl = `<text class="c-axis" x="${pad.l}" y="${H - 8}" text-anchor="start">${esc(fmtT(t0, span))}</text>`
      + `<text class="c-axis" x="${x(mid)}" y="${H - 8}" text-anchor="middle">${esc(fmtT(mid, span))}</text>`
      + `<text class="c-axis" x="${W - pad.r}" y="${H - 8}" text-anchor="end">${esc(fmtT(t1, span))}</text>`;

    // null で線を切る
    const segs = [];
    let cur = [];
    for (const p of all) {
      if (num(p.v)) cur.push([x(p.t), y(+p.v)]);
      else if (cur.length) { segs.push(cur); cur = []; }
    }
    if (cur.length) segs.push(cur);
    const path = segs.map((s) => `M${s.map((q) => q.join(' ')).join(' L')}`).join(' ');
    const area = segs.filter((s) => s.length > 1).map((s) => `M${s[0][0]} ${base} L${s.map((q) => q.join(' ')).join(' L')} L${s.at(-1)[0]} ${base} Z`).join(' ');

    const last = pts.at(-1);
    let iMin = 0, iMax = 0;
    pts.forEach((p, i) => { if (+p.v < +pts[iMin].v) iMin = i; if (+p.v >= +pts[iMax].v) iMax = i; });
    const lx = (px) => clamp(px, pad.l + 18, W - pad.r - 18);
    const ext = [];
    if (vals.some((v) => v !== vals[0])) {
      if (iMax !== pts.length - 1) ext.push(`<circle class="c-ext" cx="${x(pts[iMax].t)}" cy="${y(pts[iMax].v)}" r="3"/><text class="c-extl" x="${lx(x(pts[iMax].t))}" y="${Math.max(pad.t - 5, y(pts[iMax].v) - 8)}" text-anchor="middle">最大 ${esc(fmt(pts[iMax].v))}</text>`);
      if (iMin !== pts.length - 1) ext.push(`<circle class="c-ext" cx="${x(pts[iMin].t)}" cy="${y(pts[iMin].v)}" r="3"/><text class="c-extl" x="${lx(x(pts[iMin].t))}" y="${Math.min(base - 4, y(pts[iMin].v) + 15)}" text-anchor="middle">最小 ${esc(fmt(pts[iMin].v))}</text>`);
    }
    const lxv = x(last.t), lyv = y(last.v);
    const dir = o.better === 'low' ? '小さいほど良い' : o.better === 'high' ? '大きいほど良い' : '';
    const label = `${o.label || ''} 最新 ${fmt(last.v)}${unit}、最小 ${fmt(Math.min(...vals))}、最大 ${fmt(Math.max(...vals))}`;
    return `<figure class="chart-line tone-${o.tone || 'accent'}">`
      + `<svg viewBox="0 0 ${W} ${H}" role="img" aria-label="${esc(label)}">${grid}${marks}`
      + `<path class="c-area" d="${area}"/><path class="c-line" d="${path}"/>${ext.join('')}`
      + `<circle class="c-dot" cx="${lxv}" cy="${lyv}" r="4"/><text class="c-last" x="${r2(lxv + 9)}" y="${r2(clamp(lyv + 4, pad.t + 4, base))}">${esc(fmt(last.v))}${esc(unit)}</text>${xl}</svg>`
      + `<figcaption><span>最新 <b>${esc(fmt(last.v))}${esc(unit)}</b></span><span>最小 ${esc(fmt(Math.min(...vals)))}</span><span>最大 ${esc(fmt(Math.max(...vals)))}</span><span>${pts.length} 回</span>${dir ? `<span class="c-dir">${esc(dir)}</span>` : ''}</figcaption></figure>`;
  }

  // ---- 100% 積み上げの横棒 ----
  function stacked(parts, { legend = true, label = '' } = {}) {
    const total = parts.reduce((s, p) => s + (+p.value || 0), 0);
    const seg = parts.filter((p) => p.value > 0)
      .map((p) => `<i class="tone-${p.tone || 'neutral'}" style="flex:${p.value} 1 0" title="${esc(`${p.label} ${p.value}`)}"></i>`).join('');
    const aria = parts.map((p) => `${p.label} ${p.value}`).join('、');
    return `<div class="stacked"><div class="stack${total ? '' : ' empty'}" role="img" aria-label="${esc(label)} ${esc(aria)}">${seg}</div>`
      + `${legend ? `<div class="legend">${parts.map((p) => `<span class="lg"><i class="sw tone-${p.tone || 'neutral'}"></i>${esc(p.label)}<b>${esc(p.value)}</b>${total ? `<small>${Math.round((p.value / total) * 100)}%</small>` : ''}</span>`).join('')}</div>` : ''}</div>`;
  }

  // ---- 時間の区切り ----
  function bucketize(rows, { from, to, count = 24 } = {}) {
    const span = Math.max(1, to - from);
    const w = span / count;
    const out = Array.from({ length: count }, (_, i) => ({ t0: from + i * w, t1: from + (i + 1) * w, values: {} }));
    for (const r of rows) {
      if (!(r.ts >= from && r.ts <= to)) continue;
      const i = Math.min(count - 1, Math.floor((r.ts - from) / w));
      out[i].values[r.series] = (out[i].values[r.series] || 0) + 1;
    }
    return out;
  }

  // ---- 積み上げの縦棒（時間の推移） ----
  function columns(buckets, o = {}) {
    const series = o.series || [];
    const W = o.width || 560, H = o.height || 140;
    const pad = { l: 30, r: 6, t: 10, b: 24 };
    if (!buckets.length) return `<div class="chart-empty">${esc(o.empty || 'データがありません')}</div>`;
    const totals = buckets.map((b) => series.reduce((s, x) => s + (b.values[x.key] || 0), 0));
    const sum = totals.reduce((a, b) => a + b, 0);
    const ticks = niceTicks(0, Math.max(1, ...totals), 3);
    const top = ticks.at(-1);
    const ih = H - pad.t - pad.b;
    const bw = (W - pad.l - pad.r) / buckets.length;
    const gap = Math.min(4, bw * 0.28);
    const span = buckets.at(-1).t1 - buckets[0].t0;
    const lab = o.label || ((b) => fmtT(b.t0, span));
    const tipOf = o.tip || ((b) => `${fmtT(b.t0, 0)}〜${fmtT(b.t1, 0)}`);
    const every = o.label ? 1 : Math.max(1, Math.ceil(buckets.length / (o.labels || 6)));
    const grid = ticks.map((v) => { const yy = r2(pad.t + ih - (v / top) * ih); return `<line class="c-grid" x1="${pad.l}" x2="${W - pad.r}" y1="${yy}" y2="${yy}"/><text class="c-axis" x="${pad.l - 6}" y="${yy + 3.5}" text-anchor="end">${esc(fmt(v))}</text>`; }).join('');
    const bars = buckets.map((b, i) => {
      const x0 = r2(pad.l + i * bw + gap / 2), w = r2(Math.max(1, bw - gap));
      let yb = pad.t + ih;
      const rects = series.map((s) => {
        const v = b.values[s.key] || 0;
        if (!v) return '';
        const h = (v / top) * ih;
        yb -= h;
        return `<rect class="tone-${s.tone || 'neutral'}" x="${x0}" y="${r2(yb)}" width="${w}" height="${r2(Math.max(h, 1))}" rx="1.5"/>`;
      }).join('');
      const tip = `${tipOf(b)}: ${series.map((s) => `${s.label} ${b.values[s.key] || 0}`).join('・')}（計 ${totals[i]}）`;
      const text = i % every === 0 ? lab(b, i) : '';
      const xl = text ? `<text class="c-axis" x="${r2(x0 + w / 2)}" y="${H - 8}" text-anchor="middle">${esc(text)}</text>` : '';
      return `<g class="col"><title>${esc(tip)}</title><rect class="col-hit" x="${r2(pad.l + i * bw)}" y="${pad.t}" width="${r2(bw)}" height="${ih}"/>${rects}</g>${xl}`;
    }).join('');
    const legend = series.map((s) => `<span class="lg"><i class="sw tone-${s.tone || 'neutral'}"></i>${esc(s.label)}<b>${buckets.reduce((a, b) => a + (b.values[s.key] || 0), 0)}</b></span>`).join('');
    return `<figure class="chart-cols"><svg viewBox="0 0 ${W} ${H}" role="img" aria-label="${esc(o.title || '件数の推移')} 計 ${sum}">${grid}${bars}<line class="c-base" x1="${pad.l}" x2="${W - pad.r}" y1="${pad.t + ih}" y2="${pad.t + ih}"/></svg>`
      + `<figcaption class="legend">${legend}</figcaption></figure>`;
  }

  // ---- ヒートマップ ----
  function heatmap({ rows, cols, value, tone = 'crit', title }) {
    const vals = rows.map((r) => cols.map((c) => +value(r, c) || 0));
    const max = Math.max(0, ...vals.flat());
    const lv = (n) => (n <= 0 || max <= 0 ? 0 : Math.max(1, Math.min(4, Math.ceil((n / max) * 4))));
    const head = `<div class="hm-corner"></div>${cols.map((c) => `<div class="hm-col">${esc(c.label)}</div>`).join('')}`;
    const body = rows.map((r, i) => `<div class="hm-row" title="${esc(r.label)}">${esc(r.label)}</div>${cols.map((c, j) => {
      const n = vals[i][j];
      return `<div class="hm-cell l${lv(n)}" title="${esc(title ? title(r, c, n) : `${r.label} ${c.label}: ${n}`)}"></div>`;
    }).join('')}`).join('');
    return `<div class="heatmap tone-${tone}" style="--hm-cols:${cols.length}" role="img" aria-label="件数の濃淡（最大 ${max}）">${head}${body}</div>`
      + `<div class="hm-legend"><span>少</span>${[0, 1, 2, 3, 4].map((k) => `<i class="hm-cell l${k} tone-${tone}"></i>`).join('')}<span>多（最大 ${max}）</span></div>`;
  }

  // ---- 表のセルの相対値の棒 ----
  function dataBar(value, max, { tone = 'neutral' } = {}) {
    const pct = num(value) && max > 0 ? clamp((value / max) * 100, 0, 100) : 0;
    return `<span class="databar tone-${tone}"><i style="width:${r2(Math.max(pct, pct ? 2 : 0))}%"></i></span>`;
  }

  return { meter, ring, gauge, line, stacked, bucketize, columns, heatmap, dataBar, scoreTone, level, niceTicks, fmt };
})();

if (typeof module !== 'undefined' && module.exports) module.exports = Charts;
