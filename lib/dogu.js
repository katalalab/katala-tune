// Do-gu（https://do-gu.niwa.dev、仕事道具のデッキを見せ合うサービス）との連携。
// - 道具の一覧（共通マスター）は認証なしの GET /api/tools。照合のときだけ取りにいき、1日は使い回す
// - デッキへの登録は本人の API キーで POST /api/decks（差分マージ）。登録したデッキは公開ページになるので、
//   必ず画面で全件を見せて承認を取ってから送る。新しい道具の作成（name・category・website_url は後から直せない）はしない
'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { normName } = require('./inventory');

const BASE = 'https://do-gu.niwa.dev';
const TOOLS_TTL_MS = 24 * 3600e3;

// 名前の別表記。インストール先の名前と Do-gu の slug が素直に一致しないものだけ
const ALIASES = {
  code: 'visual-studio-code', microsoftvisualstudiocode: 'visual-studio-code', githubcli: 'gh', node: 'node-js', nodejs: 'node-js',
  dockerdesktop: 'docker-desktop', anthropicclaudecode: 'claude-code', openaicodex: 'codex',
};

// マスターから「正規化した名前 → slug」の索引を作る。同じ名前の道具が複数あるときは、公式サイトとアイコンがそろったほうを選ぶ
function buildIndex(tools) {
  const idx = new Map();
  const rank = (t) => (t.website_url ? 2 : 0) + (t.icon_url ? 1 : 0);
  for (const t of tools || []) {
    if (!t?.slug) continue;
    for (const k of [t.slug, t.name]) {
      const n = normName(k || '');
      if (!n) continue;
      const cur = idx.get(n);
      if (!cur || (cur.slug !== n && rank(t) > rank(cur))) idx.set(n, t);
    }
  }
  return new Map([...idx].map(([k, t]) => [k, t.slug]));
}

// 種類ごとの別表記（単体の CLI の claude は Claude Code。アプリの Claude はチャットのほう）
const SOURCE_ALIASES = { bin: { claude: 'claude-code' }, platform: { chocolatey: 'chocolatey' } };

function makeMatcher(tools) {
  const idx = buildIndex(tools);
  const bySlug = new Set((tools || []).map((t) => t.slug));
  return (item) => {
    const n = normName(item.name);
    // npm のスコープ付き（@scope/pkg）は pkg 側でも探す
    const cands = [n, item.name.includes('/') ? normName(item.name.split('/').pop()) : null].filter(Boolean);
    for (const c of cands) {
      const a = SOURCE_ALIASES[item.source]?.[c] || ALIASES[c];
      if (a && bySlug.has(a)) return a;
      if (idx.has(c)) return idx.get(c);
    }
    return null;
  };
}

// デッキの下書き: 自分で入れた道具のうち、Do-gu に既にあるもの。除外リストに入れたものは出さない
function deckDraft(matrixRows, tools, exclude = []) {
  const meta = new Map((tools || []).map((t) => [t.slug, t]));
  const ex = new Set(exclude);
  return matrixRows.filter((g) => g.slug && meta.has(g.slug) && !ex.has(g.slug)).map((g) => ({
    slug: g.slug, name: meta.get(g.slug).name, category: meta.get(g.slug).category, nodes: Object.keys(g.nodes), sources: g.sources,
  })).sort((a, b) => a.category.localeCompare(b.category) || a.name.localeCompare(b.name));
}

// POST /api/decks の本文。slug だけを送る（既存の道具に紐づけるだけ。新規作成はしない）
const deckPayload = (slugs) => ({ items: [...new Set(slugs)].map((slug) => ({ tool: { slug } })) });

// API キー: 環境変数 → Do-gu の案内にある保存場所の順。見つけても移動・書き換えはしない
function apiKey(env = process.env, home = os.homedir(), platform = process.platform) {
  if (env.DO_GU_API_KEY?.trim()) return env.DO_GU_API_KEY.trim();
  const dirs = platform === 'win32'
    ? [env.LOCALAPPDATA, env.APPDATA].filter(Boolean)
    : [path.join(home, '.local', 'share'), path.join(home, '.config')];
  for (const d of dirs) for (const sub of ['do-gu', 'do_gu']) for (const f of ['api_key', 'api_key.txt']) {
    try { const v = fs.readFileSync(path.join(d, sub, f), 'utf8').trim(); if (v) return v; } catch { /* 次へ */ }
  }
  return null;
}

async function fetchJson(url, opts = {}) {
  const res = await fetch(url, { ...opts, signal: AbortSignal.timeout(20000) });
  const text = await res.text();
  let body = null;
  try { body = JSON.parse(text); } catch { /* 本文をそのまま返す */ }
  if (!res.ok) throw new Error(`Do-gu ${res.status}: ${(body && JSON.stringify(body)) || text.slice(0, 300)}`);
  return body;
}

// 共通マスター（db の meta に1日キャッシュ）
async function tools(db, { force = false } = {}) {
  const cached = db.getMeta('dogu_tools');
  if (!force && cached && Date.now() - cached.at < TOOLS_TTL_MS) return cached;
  const body = await fetchJson(`${BASE}/api/tools`);
  const list = (Array.isArray(body) ? body : body?.tools || []).map(({ slug, name, category, website_url, icon_url }) => ({ slug, name, category, website_url, icon_url: icon_url || null }));
  const v = { at: Date.now(), tools: list };
  db.setMeta('dogu_tools', v);
  return v;
}

async function me(key) {
  return fetchJson(`${BASE}/api/me`, { headers: { Authorization: `Bearer ${key}` } });
}

// 送るのは承認済みの slug だけ。リトライしない（エラーは本文ごと画面に出す）
async function publish(key, slugs) {
  return fetchJson(`${BASE}/api/decks`, {
    method: 'POST', headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' }, body: JSON.stringify(deckPayload(slugs)),
  });
}

module.exports = { BASE, makeMatcher, buildIndex, deckDraft, deckPayload, apiKey, tools, me, publish };
