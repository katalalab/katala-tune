// 変更操作の流れ（確認 → 台帳の読み直し → 実行記録 → 実行 → 結果の記録）。main.js の画面・ダイアログから切り離してあり、
// テストは確認・台帳・実行器・DB を偽物に差し替える（crates/tune-core/src/engine.rs の confirm_and_run と同じ流れ）。
//
// 守ること:
//   - 承認が無ければ実行器を呼ばない
//   - 承認のあとに台帳を読み直し、接続先（alias・OS・この機体か）が変わっていたり、共用機・保護対象になっていたら実行しない（理由を実行記録に残す）
//   - 実行の直前に「未完了」の記録を書く。書けなければ実行しない。結果を書く前にアプリが止まっても、成功としては残らない
'use strict';
const actions = require('./actions');

const PENDING_OUTPUT = '実行を開始した。結果は未確認（アプリが途中で止まった場合は、機体の状態を確かめてから必要なら手で戻す）';

// deps: { loadConfig(), confirm(title, detail) -> Promise<boolean>, exec(node, script, timeoutMs), db, now?, newId? }
function makeRunner(deps) {
  const now = deps.now || Date.now;
  const newId = deps.newId || (() => `${now()}-${Math.random().toString(36).slice(2, 7)}`);

  // 承認したあとで止めたときの記録（実行していない）。記録できなくても止めたことは変わらない
  function abort(node, action, title, undoOf, reason) {
    const refused = reason;
    try {
      deps.db.addAction({ id: newId(), at: now(), node_id: node.id, type: action.type, params: action.params, label: title, ok: false, output: `中止（実行していない）: ${reason}`, undo: null, undo_of: undoOf });
    } catch { /* 記録できなくても実行しない */ }
    return { ok: false, refused };
  }

  async function confirmAndRun(nodeId, action, title, undoOf = null) {
    let fresh;
    try { fresh = deps.loadConfig(); } catch (e) { return { ok: false, refused: `台帳を読めないので実行しない: ${e.message}` }; }
    const node = fresh.nodes.find((n) => n.id === nodeId);
    if (!node) return { ok: false, refused: '台帳に無い機体' };
    let p;
    try { p = actions.plan(node, action, { protect: fresh.protect }); } catch (e) { return { ok: false, refused: String(e.message || e) }; }
    const route = [node.alias, node.os, !!node.local].join('\u0000');
    const approved = await deps.confirm(title, `${p.describe}\n\n実行するコマンド:\n${p.script}`);
    if (!approved) return { ok: false, cancelled: true };

    // 承認のあいだに台帳が変わっていないか。変わっていたら、承認した内容とは別のものになりうるので実行しない
    let again;
    try { again = deps.loadConfig(); } catch (e) { return abort(node, action, title, undoOf, `台帳を読めないので実行しない: ${e.message}`); }
    const node2 = again.nodes.find((n) => n.id === nodeId);
    if (!node2) return abort(node, action, title, undoOf, '台帳に無い機体');
    if ([node2.alias, node2.os, !!node2.local].join('\u0000') !== route) {
      return abort(node, action, title, undoOf, '確認のあいだに台帳の接続先（alias・OS・この機体かどうか）が変わったので実行しない。もう一度確認してください');
    }
    // 共用機・保護リスト・引数は、読み直した台帳でもう一度確かめる（実行するコマンドは接続先と操作で決まるので、承認したものと同じ）
    try { actions.plan(node2, action, { protect: again.protect }); } catch (e) { return abort(node, action, title, undoOf, String(e.message || e)); }

    // 未完了の記録を先に書く。書けなければ実行しない
    const id = newId();
    const entry = { id, at: now(), node_id: node2.id, type: action.type, params: action.params, label: title, ok: null, output: PENDING_OUTPUT, undo: null, undo_of: undoOf };
    try { deps.db.addAction(entry); } catch (e) { return { ok: false, refused: `実行記録を書けないので実行しない: ${e.message}` }; }

    let r;
    try {
      r = await actions.execute(node2, action, { protect: again.protect }, deps.exec);
    } catch (e) {
      const output = `実行できなかった: ${String(e.message || e)}`;
      try { deps.db.finishAction(id, { ok: false, output, undo: null }); } catch { /* 未完了のまま残る（成功とは数えない） */ }
      return { ok: false, refused: output };
    }
    const output = `${r.outcome}\n${r.output}`.trim();
    // 書き込みの失敗は成功扱いにしない。記録は未完了のまま残る
    try { deps.db.finishAction(id, { ok: r.ok, output, undo: r.undo }); } catch (e) {
      return { ok: false, refused: `実行記録を書けなかった（操作は実行済み: ${r.outcome}）: ${e.message}` };
    }
    return { ...r, entry: { ...entry, ok: r.ok, output, undo: r.undo } };
  }

  async function undo(entryId) {
    const all = deps.db.actions(1000);
    const entry = all.find((a) => a.id === entryId);
    if (!entry?.undo) return { ok: false, refused: '元に戻せる記録が無い' };
    if (all.some((a) => a.undo_of === entryId && a.ok)) return { ok: false, refused: 'すでに元に戻した' };
    return confirmAndRun(entry.node_id, entry.undo, `${entry.node_id}: 元に戻す（${entry.label}）`, entryId);
  }

  return { confirmAndRun, undo };
}

module.exports = { makeRunner, PENDING_OUTPUT };
