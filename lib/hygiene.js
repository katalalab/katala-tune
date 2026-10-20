// リポジトリの衛生監査: 各機体の開発リポジトリの dirty・未 push・stash・ghq のパスと origin の不一致を、読み取り専用で集める。
// 調査は probes/repo_hygiene.py（git の読み取りコマンドだけ。fetch しない・内容や差分を読まない・stderr を捨てる）。
// 台帳の機体ごとの設定: "repo_hygiene": false で対象外（書いていなければ、共用機（shared）は対象外）、"repo_roots": ["~/work"] で ghq root のほかの置き場を足す。
// ここは調査の実行・結果の整形・所見・機体×リポジトリの表。tune-core の hygiene.rs と同じ結果になる（crates/tune-core/tests/parity_hygiene.rs）。
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { run, lastJsonLine, SSH_OPTS } = require('./collect');
const ledger = require('./ledger');

const PROBE = path.join(__dirname, '..', 'probes', 'repo_hygiene.py');
const NO_PYTHON = '@@KATALA_TUNE_NO_PYTHON@@';
const DEFAULTS = { max_repos: 1000, budget_s: 60, cmd_s: 12 };
const DAY = 86400;
// 変更が残ったままこの日数を超えたら注意（それまでは情報）
const STALE_DIRTY_DAYS = 7;
const LIST_MAX = 5;

const O = (v) => (v && typeof v === 'object' && !Array.isArray(v) ? v : {});
const A = (v) => (Array.isArray(v) ? v : []);
const N = (v) => (typeof v === 'number' && Number.isFinite(v) ? Math.trunc(v) : null);
const B = (v) => (typeof v === 'boolean' ? v : null);
const S = (v) => (typeof v === 'string' ? v : null);
const lowerAscii = (s) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());
const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

// 台帳の機体で集めるか。運用対象（online）でなければ集めない。書いていなければ共用機は集めない（他の人のリポジトリのパスを集めないため）
const enabled = (node) => {
  const n = O(node);
  if (!ledger.isActive(n)) return false;
  return n.repo_hygiene === true || (n.repo_hygiene !== false && n.shared !== true);
};

function clamp(v, lo, hi, d) {
  const n = N(v);
  return n == null ? d : Math.min(hi, Math.max(lo, n));
}

// 調査スクリプトに上限と置き場を埋め込む。数と文字列の配列だけなので、JSON がそのまま Python の辞書として読める
function script(opts = {}) {
  const o = O(opts);
  const lit = {
    max_repos: clamp(o.max_repos, 1, 5000, DEFAULTS.max_repos),
    budget_s: clamp(o.budget_s, 5, 300, DEFAULTS.budget_s),
    cmd_s: clamp(o.cmd_s, 1, 60, DEFAULTS.cmd_s),
    roots: A(o.roots).filter((r) => typeof r === 'string' && r.trim()).slice(0, 8),
  };
  if (typeof o.ghq_root === 'string' && o.ghq_root.trim()) lit.ghq_root = o.ghq_root;
  return fs.readFileSync(PROBE, 'utf8').replace(/^KT_OPTS = \{\}.*$/m, () => `KT_OPTS = ${JSON.stringify(lit)}`);
}

// Windows（ssh の既定シェルは Git Bash）で動く python（3.8 以上）を探す。無ければ印を出す
const WIN_REMOTE = `for c in python3 python py; do if command -v "$c" >/dev/null 2>&1 && "$c" -c "import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)" >/dev/null 2>&1 </dev/null; then exec "$c" -X utf8 -; fi; done; echo ${NO_PYTHON}`;
const MAC_REMOTE = 'command -v python3 >/dev/null && exec python3 -X utf8 - || exec /usr/bin/python3 -X utf8 -';

async function auditNode(node, opts = {}) {
  // 運用対象でない機体（retired など）には接続しない
  if (ledger.refusal(node)) return { node_id: node.id, ok: false, error: 'excluded', wall_s: 0, at: Date.now() };
  const started = Date.now();
  const src = script({ ...opts, roots: [...A(opts.roots), ...A(O(node).repo_roots)] });
  const timeoutMs = (clamp(opts.budget_s, 5, 300, DEFAULTS.budget_s) + 40) * 1000;
  let res;
  if (node.os === 'macos') {
    res = node.local
      ? await run('/usr/bin/env', ['python3', '-X', 'utf8', '-'], { input: src, timeoutMs })
      : await run('ssh', [...SSH_OPTS, node.alias, MAC_REMOTE], { input: src, timeoutMs });
  } else if (node.local) {
    for (const py of ['python3', 'python', 'py']) {
      res = await run(py, ['-X', 'utf8', '-'], { input: src, timeoutMs });
      if (lastJsonLine(res.out)) break;
    }
  } else {
    res = await run('ssh', [...SSH_OPTS, node.alias, WIN_REMOTE], { input: src, timeoutMs });
  }
  const wall_s = (Date.now() - started) / 1000;
  const data = lastJsonLine(res.out);
  if (!data || data.probe !== 'repo_hygiene') {
    const noPython = res.out.includes(NO_PYTHON);
    // 標準エラーには何が混ざるか分からないので、理由の種類だけを残す
    return { node_id: node.id, ok: false, error: noPython ? 'no_python' : res.code == null ? 'timeout_or_unreachable' : 'probe_failed', wall_s, at: Date.now() };
  }
  return { node_id: node.id, ok: true, ...normalize(data), wall_s, at: Date.now() };
}

// 認証部（user:pass@）・クエリ・フラグメントを URL から落とす。調査でも落としているが、保存や表示の手前でもう一度落とす
function stripUrl(s) {
  const v = S(s);
  if (v == null) return null;
  return v.replace(/^([A-Za-z][A-Za-z0-9+.-]*:\/\/)[^/]*@/, '$1').replace(/^[^/@:\s]+(?::[^@/\s]*)?@([^/@\s]+:)/, '$1').replace(/[?#].*$/, '').slice(0, 300);
}

const REASONS = new Set(['timeout', 'git_failed', 'over_limit']);

function normalizeRepo(r) {
  const x = O(r);
  const ok = x.state === 'ok';
  const row = {
    path: S(x.path) ?? '', rel: S(x.rel), linked: x.linked === true, state: ok ? 'ok' : 'unobserved',
    reason: ok ? null : (REASONS.has(x.reason) ? x.reason : 'git_failed'),
  };
  if (!ok) return row;
  return {
    ...row,
    branch: S(x.branch), detached: x.detached === true, upstream: x.upstream === true, ahead: N(x.ahead), behind: N(x.behind), local_only: N(x.local_only),
    tracked: N(x.tracked) ?? 0, untracked: N(x.untracked) ?? 0, conflicted: N(x.conflicted) ?? 0, dirty_age_s: N(x.dirty_age_s), head_age_s: N(x.head_age_s),
    stash: N(x.stash), origin: stripUrl(x.origin), origin_host: S(x.origin_host), origin_path: S(x.origin_path), has_origin: B(x.has_origin),
  };
}

// 調査の出力を、知っている項目だけの形に揃える（余分な項目は捨てる）
function normalize(data) {
  const d = O(data);
  return {
    version: S(d.version), os: S(d.os), roots: A(d.roots).map((r) => ({ path: S(O(r).path) ?? '', source: S(O(r).source) ?? '' })),
    enumerated: N(d.enumerated) ?? 0, skipped: N(d.skipped) ?? 0, truncated: d.truncated === true, elapsed_s: typeof d.elapsed_s === 'number' ? d.elapsed_s : null,
    error: S(d.error), repos: A(d.repos).map(normalizeRepo),
  };
}

// ghq の期待パス（<host>/<owner>/<repo>、小文字）。origin から導けなければ null
const expectedRel = (r) => (r.origin_host && r.origin_path ? lowerAscii(`${r.origin_host}/${r.origin_path}`) : null);

// 'linked' | 'outside' | 'unknown' | 'no_origin' | 'ok' | 'mismatch'。リンク worktree と ghq root の外・.worktrees 配下は照合しない
function pathStatus(r) {
  if (r.linked) return 'linked';
  if (r.rel == null || r.rel.startsWith('.worktrees/')) return 'outside';
  if (r.state !== 'ok') return 'unknown';
  if (r.has_origin === false) return 'no_origin';
  const exp = expectedRel(r);
  if (exp == null) return 'unknown';
  return exp === lowerAscii(r.rel) ? 'ok' : 'mismatch';
}

const ageText = (s) => (s == null ? '経過不明' : s >= DAY ? `${Math.floor(s / DAY)} 日` : s >= 3600 ? `${Math.floor(s / 3600)} 時間` : '1 時間未満');
const more = (n) => (n > LIST_MAX ? ` ほか ${n - LIST_MAX} 件` : '');

// 所見（rules.js の finding と同じ形）。audit は normalize の出力（観測できなかった機体は呼ばない）
function analyze(audit) {
  const repos = A(audit?.repos);
  const out = [];
  const add = (f) => out.push({ category: 'repo', ...f });
  const seen = repos.filter((r) => r.state === 'ok');
  const gone = repos.filter((r) => r.state !== 'ok');

  const miss = [];
  if (audit?.error) miss.push(`調査が失敗した（${audit.error}）`);
  if (gone.length) {
    const by = {};
    for (const r of gone) by[r.reason] = (by[r.reason] || 0) + 1;
    miss.push(`git を読めなかった ${gone.length} 件（${Object.keys(by).sort().map((k) => `${k} ${by[k]}`).join('・')}）`);
  }
  if (audit?.skipped > 0) miss.push(`件数の上限で見ていない ${audit.skipped} 件`);
  if (audit?.truncated) miss.push('探索を時間または件数で打ち切った');
  if (miss.length) {
    add({
      id: 'repo-unobserved', severity: 'info', title: 'リポジトリの一部を確認できていない',
      detail: `${miss.join('、')}。確認できた ${seen.length} 件だけで判定している（読めなかった分は「きれい」とは数えない）`,
      advice: '読めない理由は、git の所有者違い（safe.directory）・壊れたリポジトリ・巨大で時間切れのどれかが多い。上限は repo_roots の絞り込みか調査の max_repos / budget_s で調整する。',
    });
  }

  const mism = repos.filter((r) => pathStatus(r) === 'mismatch').sort((a, b) => cmp(a.path, b.path));
  if (mism.length) {
    add({
      id: 'repo-path-mismatch', severity: 'warn', title: `ghq のパスと origin が食い違うリポジトリが ${mism.length} 件`,
      detail: mism.slice(0, LIST_MAX).map((r) => `${r.path}（origin は ${expectedRel(r)}）`).join('、') + more(mism.length),
      advice: '別のリポジトリの中身を置いていると、取り違えて別の origin へ変更を入れる事故になる。内容を確かめてから、置き場所を origin に合わせて移す（このアプリは移さない）。',
    });
  }
  const noOrigin = repos.filter((r) => pathStatus(r) === 'no_origin').sort((a, b) => cmp(a.path, b.path));
  if (noOrigin.length) {
    add({
      id: 'repo-no-origin', severity: 'info', title: `origin の無い ghq 配下のリポジトリが ${noOrigin.length} 件`,
      detail: noOrigin.slice(0, LIST_MAX).map((r) => r.path).join('、') + more(noOrigin.length),
      advice: 'push 先が無いので、機体が壊れると失われる。残すなら remote を付け、要らなければ消す。',
    });
  }

  const unpushed = seen.filter((r) => (r.ahead ?? 0) > 0 || (r.local_only ?? 0) > 0)
    .sort((a, b) => ((b.ahead ?? 0) + (b.local_only ?? 0)) - ((a.ahead ?? 0) + (a.local_only ?? 0)) || cmp(a.path, b.path));
  if (unpushed.length) {
    add({
      id: 'repo-unpushed', severity: 'warn', title: `push していないコミットのあるリポジトリが ${unpushed.length} 件`,
      detail: unpushed.slice(0, LIST_MAX).map((r) => (r.ahead != null ? `${r.path}（upstream より ${r.ahead} 件先）` : `${r.path}（remote に無いコミット ${r.local_only} 件・upstream なし）`)).join('、') + more(unpushed.length),
      advice: 'その機体だけにある作業。必要なものは push するか別の機体へ退避する。upstream の無いブランチは、先に push 先を決める。',
    });
  }

  const dirty = seen.filter((r) => r.tracked > 0 || r.untracked > 0)
    .sort((a, b) => (b.dirty_age_s ?? -1) - (a.dirty_age_s ?? -1) || cmp(a.path, b.path));
  if (dirty.length) {
    const stale = dirty.filter((r) => (r.dirty_age_s ?? 0) >= STALE_DIRTY_DAYS * DAY).length;
    add({
      id: 'repo-dirty', severity: stale ? 'warn' : 'info',
      title: `未コミットの変更が残るリポジトリが ${dirty.length} 件${stale ? `（${STALE_DIRTY_DAYS} 日以上前からが ${stale} 件）` : ''}`,
      detail: dirty.slice(0, LIST_MAX).map((r) => `${r.path}（変更 ${r.tracked}・未追跡 ${r.untracked}・${ageText(r.dirty_age_s)}${r.conflicted ? `・衝突 ${r.conflicted}` : ''}）`).join('、') + more(dirty.length),
      advice: '長く残った変更は、作業の持ち主が分からなくなり、別の作業と混ざる。コミットして push するか、不要なら破棄する前に内容を確かめる（このアプリは変更を戻さない）。',
    });
  }

  const stashed = seen.filter((r) => (r.stash || 0) > 0).sort((a, b) => b.stash - a.stash || cmp(a.path, b.path));
  if (stashed.length) {
    add({
      id: 'repo-stash', severity: 'info', title: `stash が残るリポジトリが ${stashed.length} 件（合計 ${stashed.reduce((s, r) => s + r.stash, 0)} 件）`,
      detail: stashed.slice(0, LIST_MAX).map((r) => `${r.path}（${r.stash} 件）`).join('、') + more(stashed.length),
      advice: 'stash は push されず、リポジトリを消すと一緒に失われる。必要なら branch にしてコミットする。',
    });
  }

  const behind = seen.filter((r) => (r.behind || 0) > 0).sort((a, b) => b.behind - a.behind || cmp(a.path, b.path));
  if (behind.length) {
    add({
      id: 'repo-behind', severity: 'info', title: `upstream より遅れているリポジトリが ${behind.length} 件`,
      detail: behind.slice(0, LIST_MAX).map((r) => `${r.path}（${r.behind} 件遅れ）`).join('、') + more(behind.length) + '。遅れは最後に fetch した時点のもの（このアプリは fetch しない）',
      advice: '作業の前に pull（または fetch して確認）する。dirty のまま遅れていると、あとで衝突しやすい。',
    });
  }
  return out;
}

// 機体×リポジトリの表。キーは origin から導いた <host>/<owner>/<repo>（小文字）。リンク worktree と origin の無いものは含めない。
// 同じ機体に同じ origin のクローンが複数あれば、パスの小さい方を代表にして copies に数を残す。観測できなかったものは state=unobserved で残す
function matrix(audits) {
  const rows = new Map();
  for (const a of audits) {
    const mine = new Map();
    for (const r of A(a.repos)) {
      const key = expectedRel(r);
      if (!key || r.linked) continue;
      const cur = mine.get(key);
      if (!cur) mine.set(key, { r, copies: 1 });
      else { cur.copies++; if (cmp(r.path, cur.r.path) < 0) cur.r = r; }
    }
    for (const [key, { r, copies }] of mine) {
      let row = rows.get(key);
      if (!row) rows.set(key, (row = { key, nodes: {} }));
      row.nodes[a.node_id] = { state: r.state, tracked: r.tracked, untracked: r.untracked, ahead: r.ahead, behind: r.behind, stash: r.stash, branch: r.branch, copies };
    }
  }
  const isDirty = (c) => c.tracked > 0 || c.untracked > 0;
  return [...rows.values()].map((row) => {
    const cells = Object.values(row.nodes);
    return { ...row, node_count: cells.length, dirty_nodes: cells.filter(isDirty).length };
  }).sort((a, b) => b.dirty_nodes - a.dirty_nodes || b.node_count - a.node_count || cmp(a.key, b.key));
}

module.exports = { enabled, script, auditNode, normalize, stripUrl, expectedRel, pathStatus, analyze, matrix, DEFAULTS, NO_PYTHON, STALE_DIRTY_DAYS };
