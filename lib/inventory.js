// 道具の棚卸し: 各機体に入っているソフトウェア（パッケージ・アプリ・開発ツール）を集め、機体をまたいだ一覧にする。
// 読み取り専用。調査は probes/mac_inventory.py・probes/win_inventory.ps1（インストール先を直接読むだけ）。
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { run, lastJsonLine, localShell, SSH_OPTS } = require('./collect');

const PROBES = path.join(__dirname, '..', 'probes');
// 種類ごとの表示名。種類は「どこから入ったか」で、同じ道具が複数の種類に出ることがある（brew と app など）
const SOURCES = {
  brew: 'Homebrew', cask: 'Homebrew Cask', app: 'アプリ', mise: 'mise', uv: 'uv tool', cargo: 'cargo', npm: 'npm -g',
  winreg: 'インストール済み', scoop: 'Scoop', choco: 'Chocolatey', bin: '単体の CLI', platform: '基盤',
};

async function inventoryNode(node) {
  const started = Date.now();
  let res;
  if (node.os === 'macos') {
    const script = fs.readFileSync(path.join(PROBES, 'mac_inventory.py'), 'utf8');
    res = node.local
      ? await run('/usr/bin/env', ['python3', '-'], { input: script, timeoutMs: 60000 })
      : await run('ssh', [...SSH_OPTS, node.alias, 'command -v python3 >/dev/null && exec python3 - || exec /usr/bin/python3 -'], { input: script, timeoutMs: 60000 });
  } else if (node.local) {
    res = await localShell.powershellFile(fs.readFileSync(path.join(PROBES, 'win_inventory.ps1')), '', 90000);
  } else {
    res = await run('ssh', [...SSH_OPTS, node.alias,
      'mkdir -p ~/.katala-tune && cat > ~/.katala-tune/inventory.ps1 && powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(cygpath -w ~/.katala-tune/inventory.ps1)"'],
    { input: fs.readFileSync(path.join(PROBES, 'win_inventory.ps1')), timeoutMs: 90000 });
  }
  const data = lastJsonLine(res.out);
  const wall_s = (Date.now() - started) / 1000;
  if (!data || !Array.isArray(data.items)) {
    return { node_id: node.id, ok: false, error: (res.err || res.out || `exit ${res.code}`).trim().slice(-800), wall_s, at: Date.now() };
  }
  // failed_sources: 取り方が失敗した種類。保存のとき、その種類は前回の一覧を残す（空と見なして削除にしない）
  return { node_id: node.id, ok: true, items: normalizeItems(data.items), errors: data.errors || [], failedSources: data.failed_sources || [], wall_s, at: Date.now() };
}

// 種類と名前で一意にし、空の名前を捨てる。版は文字列に揃える
function normalizeItems(items) {
  const seen = new Map();
  for (const it of items || []) {
    const name = String(it?.name ?? '').trim();
    const source = String(it?.source ?? '').trim();
    if (!name || !source) continue;
    const key = `${source}\u0000${name}`;
    if (seen.has(key)) continue;
    const { source: _s, name: _n, version, explicit, ...extra } = it;
    seen.set(key, { source, name, version: version == null || version === '' ? null : String(version), explicit: explicit !== false, extra: Object.keys(extra).length ? extra : null });
  }
  return [...seen.values()];
}

// 機体をまたいで同じ道具をまとめるための鍵。Do-gu の slug に寄せられればそれを使い、無ければ名前を正規化する
const normName = (s) => String(s).toLowerCase()
  .replace(/\.app$/, '')
  .replace(/\s*\((?:x64|x86|arm64|64-bit|32-bit|user|machine|system)\)\s*/g, ' ')
  .replace(/\s+v?\d+(?:\.\d+)+\S*$/, '')
  .replace(/[^\p{L}\p{N}]+/gu, '');

function groupKey(item, matchSlug) {
  const slug = matchSlug?.(item);
  return slug ? `dogu:${slug}` : `name:${normName(item.name)}`;
}

// 機体×道具の表。drift は「共通の版を1つも持たない機体の組がある」とき（同じ機体の中の書き方の違いは数えない）
function matrix(rows, { matchSlug, explicitOnly = true } = {}) {
  const groups = new Map();
  for (const r of rows) {
    if (r.removed_at) continue;
    if (explicitOnly && !r.explicit) continue;
    const key = groupKey(r, matchSlug);
    let g = groups.get(key);
    if (!g) groups.set(key, (g = { key, name: r.name, slug: key.startsWith('dogu:') ? key.slice(5) : null, sources: new Set(), nodes: {} }));
    g.sources.add(r.source);
    const cell = (g.nodes[r.node_id] ||= { versions: [], sources: [] });
    if (r.version && !cell.versions.includes(r.version)) cell.versions.push(r.version);
    if (!cell.sources.includes(r.source)) cell.sources.push(r.source);
  }
  return [...groups.values()].map((g) => {
    const sets = Object.values(g.nodes).map((c) => new Set(c.versions)).filter((x) => x.size);
    const drift = sets.some((a, i) => sets.slice(i + 1).some((b) => ![...a].some((v) => b.has(v))));
    return { ...g, sources: [...g.sources], node_count: Object.keys(g.nodes).length, drift };
  }).sort((a, b) => b.node_count - a.node_count || a.name.localeCompare(b.name));
}

module.exports = { inventoryNode, normalizeItems, normName, matrix, SOURCES };
