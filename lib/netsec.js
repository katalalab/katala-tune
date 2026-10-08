// ネットワークとセキュリティ（docs/observability.md の 6）。副作用なし。
// 調査（probes/mac_probe.py・win_probe.ps1 の netsec）の出力を正規化し（normalize）、前回の分析と比べ（annotate）、
// 常駐の増減（diffPersist）と初めての接続先（classifyPeers）を決め、所見（findings）と状態（checks）を作る。
// パケットの中身は扱わない。宛先の IP は snapshot に残さず（strip）、tune-core の DB（net_peers、30 日）にだけ置く。
// 仕様はこのファイル。Rust 版は crates/tune-core/src/netsec.rs（tests/parity_netsec.rs で同じ入力に同じ出力を確かめる）。
'use strict';

const DAY = 86400e3;
const LEARN_DAYS = 7; // 外向きの接続は、機体ごとに最初の 7 日は覚えるだけ
const PEER_KEEP_DAYS = 30; // 宛先の保持
const STABLE_MAX = 10; // これ以下の宛先としか話さないプロセスだけ、新しい宛先を知らせる（ブラウザなど宛先の多いものは知らせない）
const HIGH_PORT = 10000; // これ以上の番号は起動ごとに変わることが多いので、プロセスごとに1つとして比べる
const XPROTECT_OLD_DAYS = 45;
const SIG_OLD_DAYS = 7;
const LOGIN_WARN = 10; // 24 時間のログイン失敗
const LOGIN_CRIT = 100;
const LIMITS = { listen: 500, outbound: 400, persist: 3000, addrs: 8, name: 120, key: 300, err: 200, new: 20 };
// errors のキー（調査の部分の名前）。この順に読む
const PARTS = ['listen', 'firewall', 'gatekeeper', 'xprotect', 'launchd', 'defender', 'av', 'detections', 'security_log', 'tasks', 'services', 'run', 'startup', 'store'];
const PERSIST_KINDS = { mac: ['launchd'], windows: ['schtask', 'service', 'run', 'startup'] };
const KIND_PART = { launchd: 'launchd', schtask: 'tasks', service: 'services', run: 'run', startup: 'startup' };
const KIND_LABEL = { launchd: 'launchd', schtask: 'タスク', service: 'サービス', run: 'Run キー', startup: 'スタートアップ' };
const EXPOSURE_RANK = { loopback: 0, lan: 1, any: 2 };
const WHY_RANK = { proc: 0, port: 1, dest: 2 };
const WHY_LABEL = { proc: '外と初めて通信したプロセス', port: '初めてのポート', dest: '決まった宛先としか話さないプロセスの新しい宛先' };
const DEFENSE_IDS = ['sec-gatekeeper-off', 'sec-firewall-off', 'sec-xprotect-old', 'sec-av-off', 'sec-av-old', 'sec-detections'];

// 型の決まった読み方（Rust 版と同じ結果にするため、暗黙の型変換を使わない）
const S = (v) => (typeof v === 'string' ? v : '');
const N = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const A = (v) => (Array.isArray(v) ? v : []);
const O = (v) => (v && typeof v === 'object' && !Array.isArray(v) ? v : {});
const B = (v) => (typeof v === 'boolean' ? v : null);
const has = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
const round = (v) => (N(v) == null ? null : Math.round(v));
const cut = (s, n) => (s.length > n ? s.slice(0, n) : s);
const lowerAscii = (s) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());
// UTF-16 の単位で比べる（Rust 版も同じ）
const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
const onoff = (b) => (b === true ? '有効' : b === false ? '無効' : '不明');

// 台帳の機体で集めるか（"network": false で止める）
const enabled = (node) => O(node).network !== false;
// ログインのアカウント名・送り元を残すか。共用機は明示 opt-in のときだけ集める。
const loginsEnabled = (node) => {
  const n = O(node);
  if (!enabled(n) || n.network_logins === false) return false;
  return n.network_logins === true || n.shared !== true;
};
// 外向きの接続先（宛先）を残すか。台帳の "network_peers" が true / false ならそれに従い、
// 書いていなければ共用機（shared）は残さない（他の人の通信の宛先を集めないため。AI セッションと同じ考え方）
const peersEnabled = (node) => {
  if (!enabled(node)) return false;
  const v = O(node).network_peers;
  return v === true || (v !== false && O(node).shared !== true);
};
// 調査の結果から宛先の一覧を落とす（DB にも画面にも入れない）
const dropPeers = (data) => {
  if (data && typeof data.netsec === 'object' && data.netsec) { data.netsec.outbound = []; data.netsec.outbound_skipped = 'shared'; }
  return data;
};

// アドレスの範囲。any（全部の口）・loopback・link（リンクローカル）・private（LAN・CGNAT/tailnet・ULA・マルチキャスト・名前）・public
function addrInfo(raw) {
  let a = lowerAscii(S(raw).trim());
  if (a.startsWith('[') && a.endsWith(']')) a = a.slice(1, -1);
  const pct = a.indexOf('%');
  if (pct >= 0) a = a.slice(0, pct);
  if (!a || a.length > 64) return null;
  if (a === '*' || a === '0.0.0.0' || a === '::' || a === '0:0:0:0:0:0:0:0') return { addr: a, scope: 'any' };
  const v4 = a.startsWith('::ffff:') && a.length > 7 ? a.slice(7) : a;
  const q = v4.split('.');
  if (q.length === 4 && q.every((x) => x.length >= 1 && x.length <= 3 && [...x].every((c) => c >= '0' && c <= '9') && Number(x) <= 255)) {
    const [o1, o2] = q.map(Number);
    const scope = o1 === 127 ? 'loopback'
      : o1 === 169 && o2 === 254 ? 'link'
        : o1 === 0 || o1 === 10 || (o1 === 172 && o2 >= 16 && o2 <= 31) || (o1 === 192 && o2 === 168) || (o1 === 100 && o2 >= 64 && o2 <= 127) || o1 >= 224 ? 'private'
          : 'public';
    return { addr: q.map(Number).join('.'), scope };
  }
  if (a.includes(':') && [...a].every((c) => (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || c === ':' || c === '.')) {
    const scope = a === '::1' ? 'loopback'
      : a.startsWith('fe8') || a.startsWith('fe9') || a.startsWith('fea') || a.startsWith('feb') ? 'link'
        : a.startsWith('fc') || a.startsWith('fd') || a.startsWith('ff') ? 'private'
          : 'public';
    return { addr: a, scope };
  }
  if (a === 'localhost') return { addr: a, scope: 'loopback' };
  return { addr: a, scope: 'private' };
}

const isPublic = (raw) => addrInfo(raw)?.scope === 'public';
const exposureOf = (scope) => (scope === 'any' ? 'any' : scope === 'loopback' ? 'loopback' : 'lan');
const isExt = (l) => { const e = O(l).exposure; return e === 'any' || e === 'lan'; };
const portStr = (p) => (N(p) == null ? '?' : String(p));
// 待ち受けを比べる鍵。番号の大きいもの（HIGH_PORT 以上）はプロセスごとに1つにまとめる
function listenKey(l) {
  const o = O(l);
  const p = N(o.port);
  return `${S(o.proto)}|${S(o.proc)}|${p == null ? '' : p < HIGH_PORT ? String(p) : 'high'}`;
}
const listenLabel = (l) => { const o = O(l); return `${S(o.proc) || '?'} ${o.proto === 'udp' ? 'UDP' : 'TCP'} ${portStr(o.port)}`; };

// 調査の出力（netsec）を正規化する。probe = snapshot の probe（'mac' | 'windows'）
function normalize(raw, probe) {
  const r = O(raw);
  const os = probe === 'windows' ? 'windows' : 'mac';
  const out = { v: 1, os, listen: [], outbound: [], defense: null, persist: [], errors: {}, parts_ms: {}, elapsed_ms: round(r.elapsed_ms), cpu_ms: round(r.cpu_ms) };

  // 待ち受け: (proto, port, proc) でまとめ、アドレスを並べる。公開の範囲は広い方を採る
  const L = new Map();
  for (const x of A(r.listen)) {
    const o = O(x);
    const proto = o.proto === 'tcp' || o.proto === 'udp' ? o.proto : null;
    const port = N(o.port);
    const ai = addrInfo(o.addr);
    if (!proto || port == null || !Number.isInteger(port) || port <= 0 || port > 65535 || !ai) continue;
    const proc = cut(S(o.proc) || '?', LIMITS.name);
    const key = `${proto}|${port}|${proc}`;
    let g = L.get(key);
    if (!g) {
      if (L.size >= LIMITS.listen) continue;
      L.set(key, (g = { proto, port, proc, exposure: exposureOf(ai.scope), addrs: [] }));
    }
    const ex = exposureOf(ai.scope);
    if (EXPOSURE_RANK[ex] > EXPOSURE_RANK[g.exposure]) g.exposure = ex;
    if (!g.addrs.includes(ai.addr) && g.addrs.length < LIMITS.addrs) g.addrs.push(ai.addr);
  }
  out.listen = [...L.values()].map((g) => ({ ...g, addrs: g.addrs.slice().sort(cmp) }))
    .sort((a, b) => cmp(a.proto, b.proto) || a.port - b.port || cmp(a.proc, b.proc));

  // 外向きの接続（標本）: (proc, 宛先, ポート) ごとの数。この機体の中（loopback）と「全部の口」は除く
  const P = new Map();
  for (const x of A(r.outbound)) {
    const o = O(x);
    const port = N(o.port);
    const ai = addrInfo(o.addr);
    if (port == null || !Number.isInteger(port) || port <= 0 || port > 65535 || !ai || ai.scope === 'any' || ai.scope === 'loopback') continue;
    const proc = cut(S(o.proc) || '?', LIMITS.name);
    const n0 = N(o.n);
    const n = n0 == null ? 1 : Math.min(100000, Math.max(1, Math.round(n0)));
    const key = `${proc}|${ai.addr}|${port}`;
    const g = P.get(key);
    if (g) g.n = Math.min(1000000, g.n + n);
    else if (P.size < LIMITS.outbound) P.set(key, { proc, addr: ai.addr, port, n, public: ai.scope === 'public' });
  }
  out.outbound = [...P.values()].sort((a, b) => cmp(a.proc, b.proc) || a.port - b.port || cmp(a.addr, b.addr));

  // 防御の状態
  const d = O(r.defense);
  if (os === 'mac') {
    out.defense = {
      firewall: [0, 1, 2].includes(d.firewall) ? d.firewall : null,
      stealth: B(d.stealth),
      gatekeeper: B(d.gatekeeper),
      xprotect_version: S(d.xprotect_version) ? cut(S(d.xprotect_version), 40) : null,
      xprotect_at: round(d.xprotect_at),
    };
  } else {
    const hasDef = d.defender != null && typeof d.defender === 'object' && !Array.isArray(d.defender);
    const df = O(d.defender);
    out.defense = {
      defender: hasDef ? { realtime: B(df.realtime), antivirus: B(df.antivirus), service: B(df.service), mode: cut(S(df.mode), 40), sig_age_days: round(df.sig_age_days), sig_at: round(df.sig_at), tamper: B(df.tamper) } : null,
      av: A(d.av).slice(0, 10).map((x) => { const o = O(x); return { name: cut(S(o.name), 80), enabled: B(o.enabled), uptodate: B(o.uptodate) }; }),
      firewall: A(d.firewall).slice(0, 5).map((x) => { const o = O(x); return { name: cut(S(o.name), 20), enabled: B(o.enabled) }; }),
      active: A(d.active).filter((x) => typeof x === 'string').slice(0, 5).map((x) => cut(x, 20)),
      detections_30d: round(d.detections_30d),
      detections_open_30d: round(d.detections_open_30d),
      security_log: d.security_log === 'ok' || d.security_log === 'no-permission' ? d.security_log : null,
    };
  }

  // 自動起動（常駐）の一覧。(種類, 鍵) で一意
  const K = new Map();
  const kinds = PERSIST_KINDS[os];
  for (const x of A(r.persist)) {
    const o = O(x);
    const kind = S(o.kind);
    const key = cut(S(o.key), LIMITS.key);
    if (!kinds.includes(kind) || !key) continue;
    const k = `${kind}|${key}`;
    if (K.has(k) || K.size >= LIMITS.persist) continue;
    K.set(k, { kind, key, program: cut(S(o.program), LIMITS.name) || null });
  }
  out.persist = [...K.values()].sort((a, b) => cmp(a.kind, b.kind) || cmp(a.key, b.key));

  const e = O(r.errors);
  const pm = O(r.parts_ms);
  for (const p of PARTS) {
    if (has(e, p) && typeof e[p] === 'string') out.errors[p] = cut(e[p], LIMITS.err);
    if (has(pm, p) && N(pm[p]) != null) out.parts_ms[p] = Math.round(pm[p]);
  }
  // 共用機で宛先を落としたしるし（dropPeers）。あれば初めての接続先を覚えない・点検に出さない
  if (r.outbound_skipped === 'shared') out.outbound_skipped = 'shared';
  return out;
}

// 前回の分析と比べる。at = この分析の時刻（ms）。prev = 前回の snapshot の netsec（無ければ null）
function annotate(ns, prev, at) {
  const cur = O(ns);
  const p = O(prev);
  const usable = (x) => Array.isArray(x.listen) && typeof O(x.errors).listen !== 'string';
  const out = { ...cur, at: round(at), listen_new: [], listen_base: true };
  if (usable(cur) && usable(p)) {
    const before = new Set(p.listen.filter(isExt).map(listenKey));
    out.listen_new = cur.listen.filter((l) => isExt(l) && !before.has(listenKey(l))).slice(0, LIMITS.new);
    out.listen_base = false;
  }
  return out;
}

function sanitizePeerSummary(value) {
  if (value == null || typeof value !== 'object' || Array.isArray(value)) return value;
  const peers = { ...value };
  for (const k of Object.keys(peers)) if (!['learning', 'since', 'until', 'sampled', 'known', 'new'].includes(k)) delete peers[k];
  if (Array.isArray(peers.new)) peers.new = peers.new.map((value) => {
    if (value == null || typeof value !== 'object' || Array.isArray(value)) return value;
    const row = { ...value };
    for (const k of Object.keys(row)) if (!['proc', 'port', 'why', 'dests', 'public'].includes(k)) delete row[k];
    return row;
  });
  return peers;
}

// snapshot に残す形にする。宛先（outbound）と自動起動の一覧（persist）は件数だけ残す（一覧は tune-core の DB にある）
function strip(ns) {
  const o = { ...O(ns) };
  const outbound = A(o.outbound).length;
  const persist = A(o.persist).length;
  delete o.outbound;
  delete o.persist;
  if (has(o, 'peers')) o.peers = sanitizePeerSummary(o.peers);
  return { ...o, outbound_count: outbound, persist_count: persist };
}

// Engine の外へ返す snapshot。保存済みの旧値にも現在の台帳の privacy 設定を適用する。
function snapshotForNode(data, node) {
  if (!data || typeof data !== 'object' || Array.isArray(data)) return data;
  const out = { ...O(data) };
  if (!enabled(node)) { delete out.netsec; return out; }
  if (!out.netsec || typeof out.netsec !== 'object' || Array.isArray(out.netsec)) return out;
  const ns = { ...out.netsec };
  delete ns.outbound;
  if (has(ns, 'peers')) ns.peers = sanitizePeerSummary(ns.peers);
  if (!peersEnabled(node)) { delete ns.peers; delete ns.outbound_count; }
  out.netsec = ns;
  return out;
}

// ログイン収集を止めた後も DB に残る旧所見を公開結果へ混ぜない。
const filterLogFindings = (findings, node) => loginsEnabled(node)
  ? A(findings)
  : A(findings).filter((f) => !S(O(f).id).startsWith('log-login-'));

const LOGIN_SOURCES = new Set(['mac_auth', 'win_security']);
const loginNodeIds = (nodes) => new Set(A(nodes).filter(loginsEnabled).map((n) => S(O(n).id)).filter(Boolean));
const filterLogRows = (rows, nodes) => {
  const allowed = loginNodeIds(nodes);
  return A(rows).filter((row) => !LOGIN_SOURCES.has(S(O(row).source)) || allowed.has(S(O(row).node_id)));
};
const filterLogSignatures = (rows, nodes) => {
  const allowed = loginNodeIds(nodes);
  return A(rows).filter((row) => {
    const r = O(row);
    if (!LOGIN_SOURCES.has(S(r.source))) return true;
    const ids = S(r.node_ids).split(',').map((x) => x.trim()).filter(Boolean);
    return ids.length > 0 && ids.every((id) => allowed.has(id));
  });
};
const filterCheckRows = (rows, nodes) => {
  const byId = new Map(A(nodes).map((n) => [S(O(n).id), n]));
  return A(rows).filter((row) => {
    const r = O(row);
    const node = byId.get(S(r.scope));
    const id = S(typeof r.id === 'string' ? r.id : r.check_id);
    if (!id.startsWith('sec-')) return true;
    if (!node) return false;
    if (!enabled(node)) return false;
    if (id === 'sec-login' && !loginsEnabled(node)) return false;
    if (id === 'sec-peers' && !peersEnabled(node)) return false;
    return true;
  });
};

// Electron 版の分析の後処理（DB を使う増減と接続先は tune-core だけ）
const prepare = (raw, probe, prev, at) => strip(annotate(normalize(raw, probe), prev, at));

// 今回取った自動起動の種類と、取れなかった種類（取れなかった種類は前回の一覧を残し、削除と見なさない）
function persistKinds(ns) {
  const o = O(ns);
  const kinds = PERSIST_KINDS[o.os === 'windows' ? 'windows' : 'mac'];
  const e = O(o.errors);
  const failed = kinds.filter((k) => has(e, KIND_PART[k]) && typeof e[KIND_PART[k]] === 'string');
  return { kinds, failed };
}

// 自動起動の増減。prev = これまでの一覧 [{ kind, key, program, removed_at }]、items = 今回の一覧（normalize 済み）、
// baselined = 初回を済ませた種類。初回の種類は記録だけして増減を出さない（全部が「増えた」になるため）
function diffPersist(prev, items, { kinds = [], failed = [], baselined = [] } = {}) {
  const changes = [];
  const baseline = [];
  for (const kind of A(kinds)) {
    if (A(failed).includes(kind)) continue;
    if (!A(baselined).includes(kind)) { baseline.push(kind); continue; }
    const before = new Map();
    for (const x of A(prev)) {
      const o = O(x);
      if (S(o.kind) === kind && N(o.removed_at) == null) before.set(S(o.key), o);
    }
    for (const x of A(items)) {
      const it = O(x);
      if (S(it.kind) !== kind) continue;
      const key = S(it.key);
      const program = S(it.program) || null;
      const p = before.get(key);
      if (!p) changes.push({ kind, key, program, change: 'added' });
      else {
        before.delete(key);
        const was = S(p.program) || null;
        if (was !== program) changes.push({ kind, key, program, change: 'changed', from: was });
      }
    }
    for (const [key, p] of before) changes.push({ kind, key, program: S(p.program) || null, change: 'removed' });
  }
  return { changes, baseline };
}

// 初めての接続先。known = 30 日以内に見た [{ proc, addr, port }]、sample = 今回の outbound（normalize 済み）。
// 覚えている途中（learning）は何も知らせない。(proc, port) ごとにまとめ、理由の強い順に最大 20 件
function classifyPeers(known, sample, { learning = false } = {}) {
  const procs = new Set();
  const ports = new Set();
  const pairs = new Set();
  const addrs = new Map();
  for (const x of A(known)) {
    const k = O(x);
    const proc = S(k.proc);
    const addr = S(k.addr);
    const port = N(k.port);
    procs.add(proc);
    ports.add(`${proc}|${port}`);
    pairs.add(`${proc}|${addr}|${port}`);
    if (!addrs.has(proc)) addrs.set(proc, new Set());
    addrs.get(proc).add(addr);
  }
  const groups = new Map();
  for (const x of A(sample)) {
    const s = O(x);
    const proc = S(s.proc);
    const addr = S(s.addr);
    const port = N(s.port);
    if (pairs.has(`${proc}|${addr}|${port}`)) continue;
    const why = !procs.has(proc) ? 'proc' : !ports.has(`${proc}|${port}`) ? 'port' : (addrs.get(proc)?.size ?? 0) <= STABLE_MAX ? 'dest' : null;
    if (!why) continue;
    const key = `${proc}|${port}`;
    let g = groups.get(key);
    if (!g) groups.set(key, (g = { proc, port, why, dests: 0, public: false }));
    g.dests += 1;
    if (s.public === true) g.public = true;
    if (WHY_RANK[why] < WHY_RANK[g.why]) g.why = why;
  }
  if (learning) return [];
  return [...groups.values()]
    .sort((a, b) => WHY_RANK[a.why] - WHY_RANK[b.why] || cmp(a.proc, b.proc) || (a.port ?? -1) - (b.port ?? -1))
    .slice(0, LIMITS.new);
}

// 注意にする初めての接続先: 外と初めて通信したプロセスが、外部のアドレスへ 80・443 以外の番号で話している。
// 新しく入れたソフトや更新は 443 で話すことが多く、実機（この機体の標本）では 443 だけで知らせると注意が出続けたため
const oddPeer = (p) => O(p).why === 'proc' && O(p).public === true && N(O(p).port) != null && p.port !== 443 && p.port !== 80;

// macOS のファイアウォールが「すべての受信を遮断」（State = 2）なら、待ち受けは外から届かない
const blocksAll = (ns) => O(ns).os !== 'windows' && O(O(ns).defense).firewall === 2;

function daysSince(at, t) {
  return N(at) != null && N(t) != null ? Math.floor((at - t) / DAY) : null;
}

// 所見。node = 台帳の1台（shared を見る）
function findings(ns, node) {
  if (!enabled(node)) return [];
  if (!ns || typeof ns !== 'object' || Array.isArray(ns)) return [];
  const out = [];
  const add = (id, severity, title, detail, advice) => out.push({ id, severity, category: 'security', title, detail, advice });
  const shared = O(node).shared === true;
  const d = O(ns.defense);
  const e = O(ns.errors);
  const ext = A(ns.listen).filter(isExt);
  const extTcp = ext.filter((l) => O(l).proto === 'tcp');

  if (ns.os === 'windows' && ns.defense) {
    const df = d.defender != null && typeof d.defender === 'object' && !Array.isArray(d.defender) ? d.defender : null;
    const av = A(d.av).map(O);
    const states = [...(df ? [df.realtime] : []), ...av.map((a) => a.enabled)];
    const explicitlyOff = states.length > 0 && states.every((v) => v === false);
    const defenderRead = df != null && typeof e.defender !== 'string';
    const avRead = typeof e.av !== 'string';
    if (defenderRead && avRead && explicitlyOff) {
      add('sec-av-off', 'critical', 'リアルタイムのウイルス対策が動いていない',
        `Defender のリアルタイム保護 ${df ? onoff(df.realtime) : '不明'}${av.length ? `、登録: ${av.map((a) => `${S(a.name) || '?'}（${onoff(a.enabled)}）`).join('、')}` : ''}`,
        'Windows セキュリティ > ウイルスと脅威の防止 で、リアルタイム保護をオンにする。別のウイルス対策を入れているなら、それが動いているかを確かめる。この画面からは変更しない。');
    } else if (df && df.realtime === true && N(df.sig_age_days) != null && df.sig_age_days >= SIG_OLD_DAYS) {
      add('sec-av-old', 'warn', df.sig_age_days < 10000 ? `Defender の定義が ${df.sig_age_days} 日前のまま` : 'Defender の定義の更新日が分からない',
        `定義の更新 ${df.sig_age_days < 10000 ? `${df.sig_age_days} 日前` : '記録なし'}`,
        'Windows Update か「ウイルスと脅威の防止の更新」で定義を更新する。更新が止まり続けるなら、ネットワークと Windows Update のエラーを確かめる。');
    } else {
      const tp = av.find((a) => a.enabled === true && a.uptodate === false);
      if (tp && !(df && df.realtime === true)) add('sec-av-old', 'warn', `${S(tp.name) || 'ウイルス対策'} の定義が古い`, 'Windows セキュリティに登録された状態', 'そのソフトの画面で定義を更新する。');
    }
    const fw = A(d.firewall).map(O);
    const off = fw.filter((p) => p.enabled === false);
    if (off.length) {
      const active = A(d.active).filter((x) => typeof x === 'string');
      const hot = off.filter((p) => active.includes(S(p.name)));
      add('sec-firewall-off', hot.length ? 'critical' : 'warn', `ファイアウォールが無効（${off.map((p) => S(p.name) || '?').join('、')}）`,
        active.length ? `いまつながっているネットワークの種類: ${active.join('、')}` : 'いまつながっているネットワークの種類は不明',
        'Windows セキュリティ > ファイアウォールとネットワーク保護 で有効にする。特定のアプリだけ通したいなら、全体を切らずに規則を足す。この画面からは変更しない。');
    }
    // 検出は、隔離・削除・駆除・遮断が済んでいれば提案、未解決（または状態が分からない）なら注意
    const det = N(d.detections_30d);
    if (det != null && det > 0) {
      const open = N(d.detections_open_30d);
      const advice = 'Windows セキュリティ > ウイルスと脅威の防止 > 保護の履歴 で、何がどこで見つかり、どう処理されたかを確かめる。';
      if (open == null || open > 0) add('sec-detections', 'warn', open == null ? `Defender が直近30日に ${det} 件を検出した` : `Defender が直近30日に ${det} 件を検出し、${open} 件が未解決`, 'Defender の検出の記録（MSFT_MpThreatDetection）', advice);
      else add('sec-detections', 'info', `Defender が直近30日に ${det} 件を検出した（すべて隔離・削除済み）`, 'Defender の検出の記録（MSFT_MpThreatDetection）', advice);
    }
  } else if (ns.os !== 'windows' && ns.defense) {
    if (d.gatekeeper === false) {
      add('sec-gatekeeper-off', 'critical', 'Gatekeeper が無効', 'spctl --status: assessments disabled',
        '署名や公証を確かめずにアプリが開く状態。`sudo spctl --global-enable` で戻せる（管理者権限が要る。この画面からは変更しない）。');
    }
    if (d.firewall === 0) {
      add('sec-firewall-off', extTcp.length ? 'warn' : 'info', 'ファイアウォールが無効',
        extTcp.length ? `外から届く TCP の待ち受けが ${extTcp.length} 件: ${extTcp.slice(0, 4).map(listenLabel).join('、')}${extTcp.length > 4 ? ' ほか' : ''}` : '外から届く TCP の待ち受けは無い',
        'システム設定 > ネットワーク > ファイアウォール で有効にする。外から使うもの（画面共有・ファイル共有など）は、有効にした後に許可される。この画面からは変更しない。');
    }
    const xd = daysSince(ns.at, d.xprotect_at);
    if (xd != null && xd >= XPROTECT_OLD_DAYS) {
      add('sec-xprotect-old', 'warn', `XProtect の定義が ${xd} 日更新されていない`, `版 ${S(d.xprotect_version) || '不明'}`,
        'macOS の自動アップデート（「セキュリティ対応とシステムファイルをインストール」）がオンかを確かめる。');
    }
  }

  const blocked = blocksAll(ns);
  for (const x of A(ns.listen_new).slice(0, 5)) {
    const l = O(x);
    const tcp = l.proto !== 'udp';
    add(`net-listen-${tcp ? 'tcp' : 'udp'}-${portStr(l.port)}-${S(l.proc)}`, tcp && !blocked ? 'warn' : 'info',
      `${S(l.proc) || '?'} が ${tcp ? 'TCP' : 'UDP'} ${portStr(l.port)} で外から届く待ち受けを始めた`,
      `${A(l.addrs).filter((a) => typeof a === 'string').join(', ') || '-'}（前回の分析には無かった${blocked ? '。ファイアウォールがすべての受信を遮断しているので、いまは外から届かない' : ''}）`,
      '心当たりが無ければ、そのアプリの共有・リモート操作・開発サーバ（--host など）の設定を確かめる。この機体の中だけで使うなら 127.0.0.1 で待つ設定にする。');
  }

  const added = A(ns.persist_changes).map(O).filter((c) => c.change === 'added');
  if (added.length) {
    add('net-persist-added', 'warn', `自動起動が ${added.length} 件増えた`,
      added.slice(0, 5).map((c) => `${has(KIND_LABEL, S(c.kind)) ? KIND_LABEL[S(c.kind)] : S(c.kind)} ${S(c.key)}${S(c.program) ? `（${S(c.program)}）` : ''}`).join('、') + (added.length > 5 ? ' ほか' : ''),
      '入れた覚えのあるソフト（更新を含む）なら問題ない。覚えが無いものは、実行ファイルの場所と署名を確かめる。増減の記録は「セキュリティ」の画面にある。');
  }

  const pe = ns.peers != null && typeof ns.peers === 'object' && !Array.isArray(ns.peers) ? ns.peers : null;
  if (pe && pe.learning !== true) {
    const nw = A(pe.new).map(O);
    if (nw.length) {
      add('net-peers-new', nw.some(oddPeer) && !shared ? 'warn' : 'info', `初めての接続先が ${nw.length} 件`,
        nw.slice(0, 5).map((p) => `${S(p.proc) || '?'} → ${portStr(p.port)} 番（${has(WHY_LABEL, S(p.why)) ? WHY_LABEL[S(p.why)] : S(p.why)}${N(p.dests) != null && p.dests > 1 ? `・宛先 ${p.dests}` : ''}）`).join('、') + (nw.length > 5 ? ' ほか' : ''),
        `新しく入れたソフトや更新なら問題ない。心当たりの無いプロセスなら、宛先（「セキュリティ」の画面）と実行ファイルを確かめる。${shared ? '共用機なので提案だけにしている。' : ''}`);
    }
  }
  return out;
}

function defenseSummary(ns) {
  const d = O(ns.defense);
  if (ns.os === 'windows') {
    const df = d.defender != null && typeof d.defender === 'object' && !Array.isArray(d.defender) ? d.defender : null;
    const others = A(d.av).map(O).filter((a) => a.enabled === true && !lowerAscii(S(a.name)).includes('defender')).map((a) => S(a.name));
    const age = df ? N(df.sig_age_days) : null;
    const fw = A(d.firewall).map(O);
    return [
      `リアルタイム保護 ${df ? onoff(df.realtime) : '不明'}${others.length ? `（${others.join('、')}）` : ''}`,
      age != null && age < 10000 ? `定義 ${age} 日前` : null,
      fw.length ? `ファイアウォール ${fw.map((p) => `${S(p.name) || '?'} ${onoff(p.enabled)}`).join('・')}` : null,
      N(d.detections_30d) != null && d.detections_30d > 0 ? `検出 30 日で ${d.detections_30d} 件（未解決 ${N(d.detections_open_30d) == null ? '不明' : d.detections_open_30d}）` : null,
    ].filter(Boolean).join('、');
  }
  const fw = d.firewall;
  const xd = daysSince(ns.at, d.xprotect_at);
  return [
    `ファイアウォール ${fw === 2 ? 'すべて遮断' : fw === 1 ? '有効' : fw === 0 ? '無効' : '不明'}`,
    `Gatekeeper ${onoff(d.gatekeeper)}`,
    `XProtect ${S(d.xprotect_version) || '不明'}${xd != null ? `（${xd} 日前）` : ''}`,
  ].join('、');
}

// 状態（health.js の nodeChecks から呼ぶ）。findings = その機体の所見（ログ由来を含む）、ctx = nodeChecks の ctx（cursors）
function checks(ns, findings, ctx, node) {
  if (!enabled(node)) return [];
  if (!ns || typeof ns !== 'object' || Array.isArray(ns)) return [];
  const out = [];
  const add = (id, name, status, detail) => out.push({ id, name, status, detail });
  const fs = A(findings).map(O);
  const e = O(ns.errors);
  const err = (p) => (has(e, p) && typeof e[p] === 'string' ? e[p] : null);
  const worst = (xs) => (xs.some((f) => f.severity === 'critical') ? 'fail' : xs.some((f) => f.severity === 'warn') ? 'warn' : 'ok');
  const shared = O(node).shared === true;

  // 防御
  const defParts = ns.os === 'windows' ? ['defender', 'av', 'firewall', 'detections'] : ['firewall', 'gatekeeper', 'xprotect'];
  const defErrs = defParts.filter((p) => err(p) != null);
  const defF = fs.filter((f) => DEFENSE_IDS.includes(S(f.id)) && (f.severity === 'critical' || f.severity === 'warn'));
  const df = O(O(ns.defense).defender);
  const avStates = A(O(ns.defense).av).map((a) => O(a).enabled);
  const protection = [...(O(ns.defense).defender ? [df.realtime] : []), ...avStates];
  const protectionKnown = protection.some((v) => v === true) || (protection.length > 0 && protection.every((v) => v === false));
  const protectionUnknown = ns.os === 'windows' && !protectionKnown;
  if (!ns.defense || defErrs.length === defParts.length || (defErrs.length > 0 && defF.length === 0) || protectionUnknown) {
    const why = defErrs.map((p) => `${p} ${err(p)}`).join(' / ') || (protectionUnknown ? 'リアルタイム保護の状態が不明' : '調査の結果が無い');
    add('sec-defense', '防御', 'unknown', `取れない: ${why}`);
  } else {
    const st = worst(defF);
    const tail = defErrs.length ? `（取れない: ${defErrs.join(', ')}）` : '';
    add('sec-defense', '防御', st, st === 'ok' ? `${defenseSummary(ns)}${tail}` : `${defF.map((f) => S(f.title)).join(' / ')}（${defenseSummary(ns)}）${tail}`);
  }

  // 待ち受け
  const listen = A(ns.listen);
  const ext = listen.filter(isExt);
  const neu = A(ns.listen_new).map(O);
  if (err('listen') != null) add('sec-listen', '待ち受け', 'unknown', `取れない: ${err('listen')}`);
  else {
    const tcpNew = neu.filter((l) => l.proto !== 'udp');
    const extTcp = ext.filter((l) => O(l).proto === 'tcp').length;
    add('sec-listen', '待ち受け', tcpNew.length && !blocksAll(ns) ? 'warn' : 'ok', neu.length
      ? `新しく外から届く: ${neu.slice(0, 4).map(listenLabel).join('、')}${neu.length > 4 ? ' ほか' : ''}`
      : `外から届く ${ext.length} 件（TCP ${extTcp}・UDP ${ext.length - extTcp}）、この機体の中だけ ${listen.length - ext.length} 件${ns.listen_base === true ? '（比べる前回が無い）' : ''}`);
  }

  // 常駐の増減（tune-core が DB と比べた結果があるときだけ）
  if (Array.isArray(ns.persist_changes) || Array.isArray(ns.persist_baseline)) {
    const { kinds, failed } = persistKinds(ns);
    const ch = A(ns.persist_changes).map(O);
    const added = ch.filter((c) => c.change === 'added');
    const removed = ch.filter((c) => c.change === 'removed');
    const base = A(ns.persist_baseline).filter((k) => typeof k === 'string');
    const count = N(ns.persist_count);
    if (failed.length === kinds.length) add('sec-persist', '常駐の増減', 'unknown', `取れない: ${failed.map((k) => `${k} ${err(KIND_PART[k])}`).join(' / ')}`);
    else if (added.length) add('sec-persist', '常駐の増減', 'warn', `${added.length} 件増えた: ${added.slice(0, 4).map((c) => S(c.key)).join('、')}${added.length > 4 ? ' ほか' : ''}`);
    else {
      add('sec-persist', '常駐の増減', 'ok', `${count == null ? '-' : count} 件${removed.length ? `、${removed.length} 件減った` : ''}${base.length ? `（初回の記録: ${base.map((k) => (has(KIND_LABEL, k) ? KIND_LABEL[k] : k)).join('・')}。増減は次回から）` : '、前回から増えていない'}${failed.length ? `（取れない: ${failed.join(', ')}）` : ''}`);
    }
  }

  // ログイン（ログ由来の所見と、記録を読めるか）
  if (loginsEnabled(node)) {
    const lg = fs.filter((f) => S(f.id).startsWith('log-login-'));
    const lgBad = lg.filter((f) => f.severity === 'critical' || f.severity === 'warn');
    const src = ns.os === 'windows' ? 'win_security' : 'mac_auth';
    const cur = A(O(ctx).cursors).map(O).find((c) => c.node_id === O(node).id && c.source === src);
    if (lgBad.length) add('sec-login', 'ログイン', worst(lgBad), lgBad.map((f) => S(f.title)).join(' / '));
    else if (ns.os === 'windows' && O(ns.defense).security_log === 'no-permission') add('sec-login', 'ログイン', 'unknown', 'セキュリティログを読む権限が無い（管理者か Event Log Readers のグループが要る）');
    else if (!cur) add('sec-login', 'ログイン', 'unknown', 'まだ取り込んでいない');
    else if (S(cur.last_error)) add('sec-login', 'ログイン', 'unknown', `取り込めない: ${cut(S(cur.last_error), 80)}`);
    else add('sec-login', 'ログイン', 'ok', lg.length ? lg.map((f) => S(f.title)).join(' / ') : `24時間のログイン失敗は ${LOGIN_WARN} 件未満、外部のアドレスからの成功なし`);
  }

  // 初めての接続先（tune-core が覚えた結果があるときだけ）
  const pe = ns.peers != null && typeof ns.peers === 'object' && !Array.isArray(ns.peers) ? ns.peers : null;
  if (pe && peersEnabled(node)) {
    const known = N(pe.known);
    if (err('listen') != null) add('sec-peers', '初めての接続先', 'unknown', `取れない: ${err('listen')}`);
    else if (pe.learning === true) {
      const left = N(pe.until) != null && N(ns.at) != null ? Math.max(0, Math.ceil((pe.until - ns.at) / DAY)) : null;
      add('sec-peers', '初めての接続先', 'ok', `覚えている途中（${left == null ? '' : `あと ${left} 日、`}${known == null ? '-' : known} 件）`);
    } else {
      const nw = A(pe.new).map(O);
      const procNew = nw.filter((p) => p.why === 'proc');
      const odd = nw.filter(oddPeer);
      add('sec-peers', '初めての接続先', odd.length && !shared ? 'warn' : 'ok', nw.length
        ? `初めて ${nw.length} 件${procNew.length ? `（外と初めて通信したプロセス: ${procNew.slice(0, 4).map((p) => S(p.proc) || '?').join('、')}）` : ''}${odd.length ? `（外部へ 80・443 以外の番号: ${odd.slice(0, 4).map((p) => `${S(p.proc) || '?'} ${portStr(p.port)}`).join('、')}）` : ''}${shared ? '（共用機のため提案だけ）' : ''}`
        : `新しい接続先なし（覚えている ${known == null ? '-' : known} 件）`);
    }
  }
  return out;
}

module.exports = {
  enabled, loginsEnabled, peersEnabled, dropPeers, addrInfo, isPublic, normalize, annotate, strip, snapshotForNode, filterLogFindings, filterLogRows, filterLogSignatures, filterCheckRows, prepare, persistKinds, diffPersist, classifyPeers, findings, checks, listenKey,
  LEARN_DAYS, PEER_KEEP_DAYS, STABLE_MAX, HIGH_PORT, LOGIN_WARN, LOGIN_CRIT, PERSIST_KINDS, KIND_LABEL, WHY_LABEL, DAY,
};
