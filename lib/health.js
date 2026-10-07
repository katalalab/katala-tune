// 「状態」の判定。アプリ自身の機能と、各機体の機能を ok / warn / fail / unknown で返す。副作用なし。
// 操作者が一目で判定できるよう、どの項目も「何を見て」「なぜその判定か」を detail に書く。
'use strict';
const netsec = require('./netsec');

// タスクスケジューラの「失敗ではない」結果コード（SCHED_S_*）。0x41300〜0x4130F
const SCHED_OK = new Set([0, ...Array.from({ length: 16 }, (_, i) => 0x41300 + i)]);
const ORDER = { ok: 0, unknown: 1, warn: 2, fail: 3 };
const worst = (xs) => xs.reduce((w, s) => (ORDER[s] > ORDER[w] ? s : w), 'ok');

function failingJobs(jobs = []) {
  return jobs.filter((j) => {
    if (j.state === 'disabled' || j.state === 'unknown' || j.state === 'running') return false;
    if (j.kind === 'schtask') return j.last_result != null && !SCHED_OK.has(Number(j.last_result));
    return j.scope === 'user' && j.last_result != null && j.last_result !== 0;
  });
}

const ageMin = (ms, now) => (ms ? Math.round((now - ms) / 60000) : null);

// 1台分の機能チェック。snap = 最新の分析結果（無ければ null）、findings = その所見、ctx = { cursors, expect, schedule, now, lastError }
function nodeChecks(node, snap, findings = [], ctx = {}) {
  const now = ctx.now ?? Date.now();
  const probeEvery = (ctx.schedule?.probe_minutes ?? 60) * 60000;
  const logsEvery = (ctx.schedule?.logs_minutes ?? 15) * 60000;
  const d = snap?.data;
  const out = [];
  const add = (id, name, status, detail) => out.push({ id, name, status, detail });
  const has = (prefix) => findings.find((f) => f.id === prefix || f.id.startsWith(prefix));

  // 調査が通るか（SSH・スクリプト）
  if (ctx.lastError) add('probe', '分析', 'fail', `前回の分析が失敗: ${ctx.lastError.split('\n').pop().slice(0, 120)}`);
  else if (!snap) add('probe', '分析', 'unknown', 'まだ分析していない');
  else if (now - snap.at > probeEvery * 3) add('probe', '分析', 'warn', `最後の成功が ${ageMin(snap.at, now)} 分前（間隔 ${probeEvery / 60000} 分の3倍を超えた）`);
  else add('probe', '分析', 'ok', `${ageMin(snap.at, now)} 分前に成功（${snap.wall_s?.toFixed?.(1) ?? '-'} 秒）`);

  // ログの取り込み
  const cur = (ctx.cursors || []).filter((c) => c.node_id === node.id);
  if (!cur.length) add('logs', 'ログ取り込み', 'unknown', 'まだ取り込んでいない');
  else {
    const errs = cur.filter((c) => c.last_error);
    const stale = cur.filter((c) => !c.last_error && c.last_ok_at && now - c.last_ok_at > logsEvery * 3);
    if (errs.length) add('logs', 'ログ取り込み', 'fail', errs.map((c) => `${c.source}: ${c.last_error.slice(0, 80)}`).join(' / '));
    else if (stale.length) add('logs', 'ログ取り込み', 'warn', `${stale.map((c) => c.source).join(', ')} の最後の成功が ${ageMin(Math.min(...stale.map((c) => c.last_ok_at)), now)} 分前`);
    else add('logs', 'ログ取り込み', 'ok', `${cur.length} か所、最新 ${ageMin(Math.max(...cur.map((c) => c.last_ok_at || 0)), now)} 分前`);
  }
  if (!d) return out;

  // 定期処理（launchd / タスクスケジューラ）
  const jobs = d.jobs || [];
  const bad = failingJobs(jobs);
  add('jobs', '定期処理', jobs.length ? (bad.length ? 'warn' : 'ok') : 'unknown',
    jobs.length ? (bad.length ? `${bad.length} 件が前回失敗: ${bad.slice(0, 4).map((j) => j.name).join(', ')}${bad.length > 4 ? ' ほか' : ''}` : `${jobs.length} 件、前回失敗なし`) : '取得できない（古い調査スクリプト）');

  // 期待する常駐（台帳の expect）
  const exp = ctx.expect || {};
  for (const name of exp.services || []) {
    const s = (d.third_party_services || []).find((x) => x.name.toLowerCase() === name.toLowerCase());
    add(`svc:${name}`, `サービス ${name}`, !s ? 'fail' : s.state === 'running' ? 'ok' : 'fail', !s ? '見つからない' : `${s.state}（起動 ${s.start}）`);
  }
  for (const label of exp.jobs || []) {
    const j = jobs.find((x) => x.id === label || x.name === label);
    const st = !j ? 'fail' : j.state === 'disabled' || j.state === 'not-loaded' ? 'fail' : failingJobs([j]).length ? 'warn' : 'ok';
    add(`job:${label}`, `定期処理 ${label}`, st, !j ? '見つからない' : `${j.state}${j.last_result != null ? `、前回の結果 ${j.last_result}` : ''}`);
  }
  for (const p of exp.processes || []) {
    const hit = [...(d.processes?.apps || []), ...(d.processes?.top_cpu || [])].some((x) => (x.app || x.name || '').toLowerCase() === p.toLowerCase());
    // apps は上位25件だけなので、見えないことがある → unknown
    add(`proc:${p}`, `プロセス ${p}`, hit ? 'ok' : 'unknown', hit ? '動いている' : 'メモリ上位に見えない（動いていないか、小さい）');
  }

  // リソース
  const sys = (d.disk || []).find((x) => x.mount === '/' || /^C:/i.test(x.mount));
  if (sys) add('disk', 'ディスク', sys.free_pct < 5 ? 'fail' : sys.free_pct < 10 ? 'warn' : 'ok', `${sys.mount} 実効の空き ${sys.free_gb} GB（${sys.free_pct}%）`);
  const m = d.memory || {};
  const memBad = m.pressure === 'critical' || m.commit_pct >= 90 || (m.available_pct != null && d.probe === 'windows' && m.available_pct < 10);
  const memWarn = m.pressure === 'warn' || m.commit_pct >= 80 || (d.probe === 'windows' && m.available_pct < 20);
  add('memory', 'メモリ', memBad ? 'fail' : memWarn ? 'warn' : 'ok', d.probe === 'windows' ? `空き ${m.available_pct}%・コミット ${m.commit_pct}%` : `圧迫 ${m.pressure}・swap ${m.swap_used_gb} GB`);
  add('cpu', 'CPU', d.cpu_busy >= 85 ? 'warn' : 'ok', `${d.cpu_busy}%${has('runaway-') ? '、1コアを使い切るプロセスあり' : ''}`);

  // 安定性・保護
  if (d.stability_7d) {
    const c = Math.max(d.stability_7d.bugcheck_1001 || 0, d.stability_7d.kernel_power_41 || 0);
    add('stability', '安定性', c >= 3 ? 'fail' : c > 0 ? 'warn' : 'ok', `7日で予期しない停止 ${c} 回`);
  } else if (has('log-panic')) add('stability', '安定性', 'fail', 'カーネルパニックの記録あり');
  else add('stability', '安定性', 'ok', 'パニックの記録なし');
  // netsec（lib/netsec.js）があれば、防御・待ち受け・常駐の増減・ログイン・初めての接続先をそちらで判定する（Defender もそこに含む）
  if (d.defender && !d.netsec?.defense) add('defender', 'Defender', d.defender.realtime ? 'ok' : 'warn', d.defender.realtime ? 'リアルタイム保護 有効' : 'リアルタイム保護 無効');
  out.push(...netsec.checks(d.netsec, findings, ctx, node));
  const flood = findings.filter((f) => f.id.startsWith('log-flood-'));
  add('flood', 'エラーの繰り返し', flood.length ? 'warn' : 'ok', flood.length ? flood.map((f) => f.title).join(' / ') : '24時間で200件を超える同じエラーなし');
  const hw = has('log-whea') || has('log-gpu-reset') || has('log-disk');
  if (hw) add('hardware', 'ハードウェア', hw.severity === 'critical' ? 'fail' : 'warn', hw.title);
  return out;
}

// アプリ自身の機能
function appChecks(ctx) {
  const now = ctx.now ?? Date.now();
  const out = [];
  const add = (id, name, status, detail) => out.push({ id, name, status, detail });
  add('config', '機体台帳', ctx.configError ? 'fail' : ctx.example ? 'warn' : 'ok', ctx.configError || (ctx.example ? '見本のまま' : `${ctx.nodeCount} 台、保護 ${ctx.protectCount} 件`));
  add('db', 'ローカル DB', ctx.dbCheck === 'ok' ? 'ok' : 'fail', ctx.dbCheck === 'ok' ? `整合性 ok、${(ctx.dbBytes / 1048576).toFixed(1)} MB` : `整合性チェック: ${ctx.dbCheck}`);
  const s = ctx.scheduler || {};
  if (!s.enabled) add('scheduler', '自動スキャン', 'warn', '止まっている');
  else {
    const late = s.lastProbeAt && now - s.lastProbeAt > s.probe_minutes * 60000 * 2;
    add('scheduler', '自動スキャン', late ? 'warn' : 'ok', `分析 ${s.probe_minutes} 分ごと（前回 ${s.lastProbeAt ? ageMin(s.lastProbeAt, now) + ' 分前' : 'まだ'}）、ログ ${s.logs_minutes} 分ごと（前回 ${s.lastLogsAt ? ageMin(s.lastLogsAt, now) + ' 分前' : 'まだ'}）`);
  }
  if (ctx.fleet !== undefined) add('fleet', 'katala-fleet 連携', ctx.fleet?.error ? 'warn' : ctx.fleet ? 'ok' : 'unknown', ctx.fleet?.error || (ctx.fleet ? `取得 ${ageMin(ctx.fleetAt, now)} 分前` : 'まだ取得していない'));
  add('login', 'ログイン時に起動', ctx.openAtLogin ? 'ok' : 'unknown', ctx.openAtLogin ? '有効' : '無効（自動スキャンはアプリを開いている間だけ動く）');
  return out;
}

module.exports = { nodeChecks, appChecks, failingJobs, worst, ORDER, SCHED_OK };
