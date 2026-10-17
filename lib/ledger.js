// 機体の運用状態（台帳の status）と、対象台帳（対象・除外・未観測とその理由）。副作用なし。Rust 版は crates/tune-core/src/nodes.rs。
// 到達できるか（1回の調査結果）と、運用対象か（台帳）は別の情報。到達結果から台帳を書き換えない（台帳は操作者の明示編集でだけ変わる）。
// 運用対象でない機体（online 以外）には SSH も probe も実行せず、設定も変えない。過去の履歴は閲覧できる。
'use strict';

const ONLINE = 'online';
// 台帳に書ける値。省略は online（この項目を持たない既存の台帳もそのまま動く）
const STATES = ['online', 'retired', 'no-ssh', 'paused', 'out-of-scope'];
const STATE_LABEL = { online: '運用中', retired: '退役', 'no-ssh': 'SSH 対象外', paused: '一時停止', 'out-of-scope': '対象外', invalid: '状態不明' };
const INVALID = 'invalid';

const str = (v) => (typeof v === 'string' ? v.trim() : '');

// 台帳の1台の運用状態。値が読めないとき（綴り違いなど）は安全側に倒して対象外（invalid）にする（退役機に誤って接続しない）
function opState(node) {
  const n = node && typeof node === 'object' ? node : {};
  const reason = str(n.status_reason);
  const date = str(n.status_date);
  const base = { reason, date };
  if (n.status === undefined || n.status === null) return { state: ONLINE, active: true, valid: true, ...base };
  const s = str(n.status).toLowerCase().replace(/_/g, '-');
  if (STATES.includes(s)) return { state: s, active: s === ONLINE, valid: true, ...base };
  const shown = typeof n.status === 'string' ? n.status.slice(0, 40) : typeof n.status;
  return { state: INVALID, active: false, valid: false, ...base, reason: `status の値が読めない（${shown}）ため対象外にしている。台帳を直す` };
}

const isActive = (node) => opState(node).active;

// 収集・自動スキャン・変更操作の対象（online だけ）と、除外（理由つき）に分ける
function partition(nodes = []) {
  const targets = [];
  const excluded = [];
  for (const node of nodes) {
    const st = opState(node);
    if (st.active) targets.push(node);
    else excluded.push({ node_id: node.id, state: st.state, reason: st.reason, date: st.date });
  }
  return { targets, excluded };
}

// 実行を断るときの理由（呼び出し側は ssh・probe を起動しない）
function refusal(node) {
  const st = opState(node);
  return st.active ? null : `${node?.id ?? '?'} は運用状態が ${st.state}（${STATE_LABEL[st.state]}）のため、接続も変更もしない`;
}

// 対象台帳。obs = { lastOkAt(id), lastError(id) }（無ければ未観測）
// target: online で最後の分析が成功。unobserved: online なのに観測できていない（到達不能・認証・未実施など。異常として扱う）
// excluded: 運用対象外（理由と判断日つき。履歴は残る）
function targetLedger(nodes = [], obs = {}) {
  const rows = nodes.map((node) => {
    const st = opState(node);
    const base = { node_id: node.id, os: node.os ?? null, state: st.state, state_label: STATE_LABEL[st.state] };
    if (!st.active) return { ...base, status: 'excluded', reason: st.reason || '理由の記載なし', date: st.date || null };
    const lastOkAt = obs.lastOkAt?.(node.id) ?? null;
    const lastError = obs.lastError?.(node.id) ?? null;
    if (lastError) return { ...base, status: 'unobserved', reason: `最後の分析が失敗: ${String(lastError).trim().split('\n').pop().slice(0, 120)}`, last_ok_at: lastOkAt };
    if (lastOkAt == null) return { ...base, status: 'unobserved', reason: 'まだ分析していない', last_ok_at: null };
    return { ...base, status: 'target', reason: null, last_ok_at: lastOkAt };
  });
  const count = (s) => rows.filter((r) => r.status === s).length;
  return { rows, counts: { target: count('target'), unobserved: count('unobserved'), excluded: count('excluded'), total: rows.length } };
}

module.exports = { STATES, STATE_LABEL, opState, isActive, partition, refusal, targetLedger };
