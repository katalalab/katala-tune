'use strict';

(() => {
  const MAX_BYTES = 8 * 1024 * 1024;
  const MAX_REPORTS = 4;
  const labels = { original: '原本', artifact: '成果', intermediate: '中間ファイル', log: 'ログ' };
  const statuses = { completed: '正常終了', failed: '失敗', blocked: '開始保留' };
  const reasons = {
    insufficient_space: '必要量に対して空き不足', unavailable_space: '開始時の空きを取得できず',
    guest_capacity_only: 'WSL内の空きのみ取得。Windows側で仮想ディスクのあるドライブを指定し、WSL内の空きも確認してください',
    spawn_error: 'ジョブを起動できず', command_failed: 'コマンドが異常終了',
    incomplete: '終了未確認', wait_error: '終了状態を取得できず',
  };
  let reports = [];
  let messages = [];
  let reading = false;
  let request = 0;

  const natural = n => Number.isSafeInteger(n) && n >= 0;
  const nullable = (value, valid) => value === null || valid(value);
  const identifier = value => typeof value === 'string' && value.trim() && value.length <= 512;

  // 長いサンプル列や表示に使用しない拡張項目を画面の状態に保持しない。
  function summarize(r, name) {
    if (!r || r.schema_version !== 1 || r.kind !== 'job-storage') throw new Error('job-storage v1 のレポートを選んでください。');
    if (!identifier(r.job_id) || !identifier(r.volume) || typeof r.status !== 'string' || !Object.hasOwn(statuses, r.status)
      || !natural(r.interval_ms) || !r.interval_ms || !Array.isArray(r.samples)
      || !nullable(r.peak_bytes, natural) || !nullable(r.net_bytes, Number.isSafeInteger)
      || !nullable(r.residual_bytes, Number.isSafeInteger) || !nullable(r.unattributed_net_bytes ?? null, Number.isSafeInteger)
      || !Array.isArray(r.categories)
      || !r.preflight || !nullable(r.preflight.free_bytes ?? null, natural)
      || !['os_reserve_bytes', 'other_reserved_bytes', 'expected_peak_bytes'].every(k => natural(r.preflight[k]))) {
      throw new Error('レポートの項目または数値を確認してください。');
    }
    let missing = 0;
    let previous = -1;
    let minimum = Infinity;
    for (const sample of r.samples) {
      if (!sample || !natural(sample.elapsed_ms) || sample.elapsed_ms < previous || !nullable(sample.free_bytes, natural)) {
        throw new Error('空き容量の観測列を読み取れません。');
      }
      previous = sample.elapsed_ms;
      if (sample.free_bytes === null) missing++;
      else minimum = Math.min(minimum, sample.free_bytes);
    }
    const categories = new Map();
    for (const c of r.categories) {
      if (!c || typeof c.kind !== 'string' || !Object.hasOwn(labels, c.kind) || categories.has(c.kind) || !nullable(c.logical_delta_bytes, Number.isSafeInteger)) {
        throw new Error('ファイル分類の値または分類名を確認してください。');
      }
      categories.set(c.kind, c.logical_delta_bytes);
    }
    const total = r.sample_count ?? r.samples.length;
    if (!natural(total) || total < r.samples.length || (r.missing_sample_count != null && (
      !natural(r.missing_sample_count) || r.missing_sample_count < missing
      || r.missing_sample_count > missing + total - r.samples.length))) {
      throw new Error('観測件数が観測列と一致しません。');
    }

    const start = r.samples[0]?.free_bytes;
    const end = r.samples.at(-1)?.free_bytes;
    // 間引きのある報告では保存列より小さい空きを観測している場合がある。
    const observedPeak = start == null ? null : Math.max(0, start - minimum);
    const neverRan = r.status === 'blocked' || r.reason === 'incomplete' || r.reason === 'spawn_error';
    if ((neverRan && [r.net_bytes, r.peak_bytes, r.residual_bytes].some(n => n !== null))
      || (r.net_bytes !== null && (start == null || end == null || r.net_bytes !== start - end))
      || (r.peak_bytes !== null && (observedPeak === null || r.peak_bytes > start || r.peak_bytes < observedPeak
        || (total === r.samples.length && r.peak_bytes !== observedPeak)))) {
      throw new Error('集計値と空き容量の観測列が一致しません。');
    }

    return {
      name, id: r.job_id, volume: r.volume, status: statuses[r.status],
      reason: typeof r.reason === 'string' && Object.hasOwn(reasons, r.reason) ? reasons[r.reason] : '',
      started: natural(r.started_at_ms) && r.started_at_ms <= 8.64e15 ? new Date(r.started_at_ms).toLocaleString('ja-JP') : '未記録',
      interval: r.interval_ms, recorded: r.samples.length, total,
      gaps: r.missing_sample_count ?? (r.samples.length === total ? missing : null), retainedGaps: missing,
      start, end: !neverRan && r.samples.length >= 2 ? end : null,
      peak: r.peak_bytes, net: r.net_bytes, residual: r.residual_bytes, categories,
      unassigned: r.unattributed_net_bytes === r.net_bytes ? r.unattributed_net_bytes : null,
      available: natural(r.preflight.free_bytes) ? r.preflight.free_bytes : null,
      osReserve: r.preflight.os_reserve_bytes, otherReserved: r.preflight.other_reserved_bytes, expected: r.preflight.expected_peak_bytes,
    };
  }

  const fmt = new Intl.NumberFormat('ja-JP', { maximumFractionDigits: 2 });
  function bytes(value, unknown = '未取得') {
    if (value == null) return unknown;
    const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
    let unit = 0;
    let n = value;
    while (Math.abs(n) >= 1024 && unit < units.length - 1) { n /= 1024; unit++; }
    return fmt.format(n) + ' ' + units[unit];
  }

  function comparison(rows) {
    return UI.matrix({
      cls: 'scroll-x', corner: '測定項目', rows: rows.map(([label, read]) => ({ label, read })),
      cols: reports.map(report => ({ label: report.name, report })),
      cell: (row, col) => ({ html: UI.esc(row.read(col.report)) }),
    });
  }

  function observations(r) {
    const gaps = r.gaps === null ? '保存点の欠測 ' + r.retainedGaps : '欠測 ' + r.gaps;
    return r.total + ' 回（' + gaps + ' 回、保存 ' + r.recorded + ' 点）';
  }

  function draw() {
    const details = reports.length ? UI.section('ボリュームの変化', '観測時点の空き容量から算出')
      + comparison([
        ['ジョブID', r => r.id], ['結果', r => r.reason ? r.status + '（' + r.reason + '）' : r.status],
        ['開始時刻', r => r.started], ['対象ボリューム', r => r.volume], ['観測間隔', r => r.interval + ' ms'],
        ['観測回数', observations], ['開始時の空き', r => bytes(r.start)], ['終了時の空き', r => bytes(r.end)],
        ['観測した最大消費量', r => bytes(r.peak)], ['終了時の純増', r => bytes(r.net)],
        ['分類先が未確定の実容量', r => bytes(r.unassigned, '未判定')],
        ['物理量で照合した残差', r => bytes(r.residual, '未判定')],
      ])
      + UI.section('分類別のファイル増減', '論理サイズ。共有ブロックや圧縮があるため、実占有の内訳は別途測定が必要です。')
      + comparison(Object.entries(labels).map(([kind, label]) => [label, r =>
        r.categories.has(kind) ? bytes(r.categories.get(kind)) : '記録なし']))
      + UI.section('開始時の容量判定')
      + comparison([
        ['判定に使った空き', r => bytes(r.available)], ['OS用の余裕', r => bytes(r.osReserve)],
        ['他ジョブの予約', r => bytes(r.otherReserved)], ['予想最大消費量', r => bytes(r.expected)],
      ]) : '';

    page(UI.head({ icon: 'resources', title: 'ジョブ容量', desc: '初回と再実行のレポートを選ぶと、同じ項目を並べて比較できます。' })
      + '<div class="review-input"><label for="storageFile">計測レポートを追加（JSON・最大4件、各8 MiB）</label>'
      + '<input id="storageFile" type="file" accept=".json,application/json" multiple' + (reading ? ' disabled' : '') + '></div>'
      + (reports.length ? '<button class="btn" id="storageClear" type="button">表示をクリア</button>' : '')
      + '<div role="status" aria-live="polite">' + (reading ? '読み込み中…' : messages.map(m => '<p class="err">' + UI.esc(m) + '</p>').join('')) + '</div>'
      + details);
    document.querySelector('#storageFile').onchange = e => addFiles(e.target.files);
    if (reports.length) document.querySelector('#storageClear').onclick = () => { reset(); draw(); };
  }

  async function addFiles(files) {
    if (!files.length || reading) return;
    messages = [];
    if (files.length + reports.length > MAX_REPORTS) {
      messages.push('一度に表示できるのは4件です。表示をクリアするか、追加する件数を減らしてください。');
      draw();
      return;
    }
    const batch = [...files];
    const token = ++request;
    reading = true;
    draw();
    for (const file of batch) {
      if (token !== request) return;
      try {
        if (file.size > MAX_BYTES) throw new Error('8 MiBを超えています。');
        const source = await file.text();
        if (token !== request) return;
        let data;
        try { data = JSON.parse(source); } catch { throw new Error('JSONを解析できません。'); }
        reports.push(summarize(data, file.name));
      } catch (error) {
        if (token !== request) return;
        messages.push(file.name + ' — ' + error.message);
      }
    }
    reading = false;
    draw();
  }

  function reset() { request++; reports = []; messages = []; reading = false; }

  (window.KT_VIEWS ||= []).push({
    key: 'job-storage', label: 'ジョブ容量', icon: 'resources', leave: reset,
    render() { ticket(); setCrumbs([{ icon: 'resources', label: 'ジョブ容量' }]); draw(); },
  });
})();
