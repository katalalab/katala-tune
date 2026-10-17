// WSL の中の観測（probes/win_probe.ps1 の snap.wsl.distros）から所見を作る。副作用なし。Rust 版は crates/tune-core/src/wsl.rs。
// distro の state: running（中を観測した。観測できなければ reason）/ stopped_unobserved（停止中。起動も実行もせず、観測していない）/
// unknown（走っている一覧を読めなかった。やはり何も実行しない）。停止中は「正常」でも「空」でもなく「観測していない」。
'use strict';

const gb = (mb) => (mb / 1024).toFixed(1);
const fin = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const MEM_WARN_PCT = 90;
const HELD_MIN_MB = 4096;
const REASON_TEXT = {
  'not-running': '停止中（起動しないので観測していない）',
  'running-list-failed': '動いているかを読めなかった（何も実行していない）',
  'internal-distro-skipped': 'Docker Desktop の内部 distro（観測の対象外）',
  'observation-limit': '観測する distro の数の上限',
  timeout: '時間切れ',
  failed: '実行できなかった',
  'no-data': '内側から値を読めなかった',
};
const reasonText = (r) => (typeof r === 'string' && Object.hasOwn(REASON_TEXT, r) ? REASON_TEXT[r] : typeof r === 'string' ? r.slice(0, 60) : '不明');

const distrosOf = (snap) => (Array.isArray(snap?.wsl?.distros) ? snap.wsl.distros.filter((d) => d && typeof d === 'object') : []);

// 内側で使われているメモリ（MB）。WSL2 の distro は1つの VM を共有するので、足さずに最大を取る
function usedMb(inside) {
  const total = fin(inside?.mem_total_mb);
  const avail = fin(inside?.mem_available_mb);
  if (total === null || avail === null || total <= 0) return null;
  return Math.max(0, total - avail);
}

// ctx = { vmMb: Windows 側の vmmem の実メモリ MB（無ければ null）, totalMb: Windows の物理メモリ MB }
function findings(snap, ctx = {}) {
  const out = [];
  const distros = distrosOf(snap);
  if (!distros.length) return out;
  const running = distros.filter((d) => d.state === 'running');
  const seen = running.filter((d) => d.observed === true && usedMb(d.inside) !== null);

  const hot = seen.filter((d) => Math.round((usedMb(d.inside) / d.inside.mem_total_mb) * 100) >= MEM_WARN_PCT);
  if (hot.length) {
    const d = hot[0];
    const pct = Math.round((usedMb(d.inside) / d.inside.mem_total_mb) * 100);
    const psi = fin(d.inside.psi_some_avg10);
    out.push({
      id: 'wsl-inside-memory', severity: 'warn', category: 'memory',
      title: `WSL の内側でメモリが ${pct}% 使われている（${hot.map((x) => String(x.name)).join('、')}）`,
      detail: `内側の使用 ${gb(usedMb(d.inside))} GB / WSL の VM の上限 ${gb(d.inside.mem_total_mb)} GB${psi !== null ? `、メモリ圧迫 PSI some ${psi}%（直近10秒）` : ''}`,
      advice: 'WSL の VM の中で足りなくなっている（Windows 側の空きとは別）。内側の上位プロセス（機体の詳細）で、開発環境・コンテナ・学習ジョブのどれが大きいかを確かめる。上限を上げるなら .wslconfig の memory（反映には `wsl --shutdown` が要る）。この画面からは変更しない。',
    });
  }

  // Windows 側（vmmem）が、内側の実使用より多く持ったままか
  const vmMb = fin(ctx.vmMb);
  const totalMb = fin(ctx.totalMb) || 0;
  if (vmMb !== null && seen.length && seen.length === running.length) {
    const inner = Math.max(...seen.map((d) => usedMb(d.inside)));
    const held = vmMb - inner;
    if (held >= HELD_MIN_MB && vmMb >= totalMb * 0.25) {
      out.push({
        id: 'wsl-reclaim', severity: 'info', category: 'memory',
        title: `WSL の VM が内側の実使用より約 ${gb(held)} GB 多く保持している`,
        detail: `Windows 側 vmmem ${gb(vmMb)} GB、内側の使用 ${gb(inner)} GB（${seen.length} distro は同じ VM を共有）`,
        advice: '内側で解放されたキャッシュが Windows に返っていない。.wslconfig の autoMemoryReclaim=gradual で返りやすくなる。すぐ返すなら、使い終わってから `wsl --shutdown`。',
      });
    }
  }

  // 観測していない distro（停止中・一覧を読めない・観測に失敗）。正常でも空でもない
  const stopped = distros.filter((d) => d.state === 'stopped_unobserved');
  const missed = distros.filter((d) => d.state !== 'stopped_unobserved' && !(d.observed === true && usedMb(d.inside) !== null) && d.reason !== 'internal-distro-skipped');
  if (stopped.length || missed.length) {
    const parts = [];
    if (stopped.length) parts.push(`停止中（起動しないので未観測）: ${stopped.map((d) => String(d.name)).join('、')}`);
    if (missed.length) parts.push(`観測できなかった: ${missed.map((d) => `${String(d.name)}（${reasonText(d.reason)}）`).join('、')}`);
    out.push({
      id: 'wsl-unobserved', severity: 'info', category: 'memory',
      title: `WSL の distro ${stopped.length + missed.length} 件は中を観測していない`,
      detail: parts.join(' / '),
      advice: '停止中の distro は「正常」でも「空」でもなく、観測していないだけ。調べるために起動はしない。中を見たいときは、その distro を自分で起動してから分析する。',
    });
  }
  return out;
}

module.exports = { findings, usedMb, distrosOf, REASON_TEXT, reasonText };
