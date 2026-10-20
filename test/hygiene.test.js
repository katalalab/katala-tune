'use strict';
const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const hygiene = require('../lib/hygiene');

// Git for Windows は Node の os.devNull を設定ファイルとして開けない。
const gitConfigDir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-hyg-config-'));
const gitConfigFile = path.join(gitConfigDir, 'gitconfig');
fs.writeFileSync(gitConfigFile, '');

// 一時ディレクトリに作る架空のリポジトリだけを使う（実在の機体・リポジトリ名は使わない）
const GIT_ENV = {
  ...process.env, GIT_AUTHOR_NAME: 't', GIT_AUTHOR_EMAIL: 't@example.com', GIT_COMMITTER_NAME: 't', GIT_COMMITTER_EMAIL: 't@example.com',
  GIT_CONFIG_GLOBAL: gitConfigFile, GIT_CONFIG_SYSTEM: gitConfigFile,
};
for (const k of ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE']) delete GIT_ENV[k];
const git = (cwd, ...args) => {
  const r = spawnSync('git', ['-C', cwd, '-c', 'commit.gpgsign=false', ...args], { env: GIT_ENV, encoding: 'utf8' });
  assert.equal(r.status, 0, `git ${args.join(' ')}: ${r.stderr}`);
  return r.stdout;
};
const commit = (dir, name, body = 'x') => {
  fs.writeFileSync(path.join(dir, name), body);
  git(dir, 'add', '-A');
  git(dir, 'commit', '-q', '-m', name);
};

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'kt-hyg-'));
const CRED = 'https://fixtureuser:fixture-secret-token@example.com/acme/%s.git?token=query-secret#frag';

// 架空の ghq root を作る: 期待どおり置いたもの・dirty・ahead・behind・stash・名前が食い違うもの・origin 無し・壊れたもの・リンク worktree
function fixture() {
  const base = tmp();
  const ghq = path.join(base, 'ghq');
  const remotes = path.join(base, 'remotes');
  fs.mkdirSync(remotes);
  const make = (rel, repo, { origin = CRED.replace('%s', repo), setup } = {}) => {
    const bare = path.join(remotes, `${repo}.git`);
    git(base, 'init', '-q', '--bare', '-b', 'main', bare);
    const dir = path.join(ghq, 'example.com', rel);
    fs.mkdirSync(path.dirname(dir), { recursive: true });
    git(base, 'clone', '-q', bare, dir);
    commit(dir, 'a.txt');
    git(dir, 'push', '-q', '-u', 'origin', 'main');
    if (setup) setup(dir, bare);
    if (origin) git(dir, 'remote', 'set-url', 'origin', origin);
    else git(dir, 'remote', 'remove', 'origin');
    return dir;
  };
  make('acme/clean', 'clean');
  make('acme/CaseDiffers', 'casediffers'); // 大文字小文字だけの違いは一致と見なす
  make('acme/dirty', 'dirty', { setup: (d) => { fs.writeFileSync(path.join(d, 'a.txt'), 'SECRET-BODY-MARKER'); fs.writeFileSync(path.join(d, 'new.txt'), 'SECRET-BODY-MARKER'); } });
  make('acme/ahead', 'ahead', { setup: (d) => commit(d, 'b.txt') });
  make('acme/behind', 'behind', {
    setup: (d, bare) => {
      const other = path.join(base, 'other-clone');
      git(base, 'clone', '-q', bare, other);
      commit(other, 'c.txt');
      git(other, 'push', '-q', 'origin', 'main');
      git(d, 'fetch', '-q');
    },
  });
  make('acme/stashed', 'stashed', { setup: (d) => { fs.writeFileSync(path.join(d, 'a.txt'), 'changed'); git(d, 'stash', 'push', '-q'); } });
  make('acme/wrongplace', 'actualname'); // ディレクトリ名と origin が食い違う
  make('acme/noorigin', 'noorigin', { origin: null });
  const alone = path.join(ghq, 'example.com', 'acme', 'localonly');
  fs.mkdirSync(alone, { recursive: true });
  git(alone, 'init', '-q', '-b', 'main');
  commit(alone, 'a.txt'); // upstream の無いブランチのコミット
  const broken = path.join(ghq, 'example.com', 'acme', 'broken');
  fs.mkdirSync(path.join(broken, '.git'), { recursive: true });
  fs.writeFileSync(path.join(broken, '.git', 'HEAD'), 'not a ref');
  // リンク worktree は名前が食い違っていても照合しない
  git(path.join(ghq, 'example.com', 'acme', 'clean'), 'worktree', 'add', '-q', '-b', 'wt', path.join(ghq, 'example.com', 'acme', 'linked-wt'));
  return { base, ghq };
}

function probe(opts, env = {}) {
  const r = spawnSync('python3', ['-X', 'utf8', '-'], { input: hygiene.script(opts), env: { ...GIT_ENV, ...env }, encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  const line = r.stdout.trim().split('\n').pop();
  return { raw: r.stdout, data: JSON.parse(line) };
}

let FX;
before(() => { FX = fixture(); });
after(() => {
  if (FX) fs.rmSync(FX.base, { recursive: true, force: true });
  fs.rmSync(gitConfigDir, { recursive: true, force: true });
});

const byPath = (data) => Object.fromEntries(data.repos.map((r) => [r.path.replace(/^.*\/example\.com\//, ''), r]));

test('一時リポジトリで dirty・ahead・behind・stash・origin 不一致・origin 無し・リンク worktree を検出する', () => {
  const { ghq } = FX;
  const { raw, data } = probe({ ghq_root: ghq });
  assert.equal(data.error ?? null, null);
  assert.equal(data.roots[0].source, 'opt');
  const r = byPath(hygiene.normalize(data));
  assert.equal(r['acme/clean'].state, 'ok');
  assert.deepEqual([r['acme/clean'].tracked, r['acme/clean'].untracked, r['acme/clean'].ahead, r['acme/clean'].behind, r['acme/clean'].stash], [0, 0, 0, 0, 0]);
  assert.deepEqual([r['acme/dirty'].tracked, r['acme/dirty'].untracked], [1, 1]);
  assert.equal(typeof r['acme/dirty'].dirty_age_s, 'number');
  assert.equal(r['acme/ahead'].ahead, 1);
  assert.equal(r['acme/behind'].behind, 1);
  assert.equal(r['acme/stashed'].stash, 1);
  assert.equal(r['acme/stashed'].tracked, 0);
  assert.equal(r['acme/noorigin'].has_origin, false);
  assert.equal(r['acme/localonly'].upstream, false);
  assert.equal(r['acme/localonly'].local_only, 1);
  assert.equal(r['acme/localonly'].ahead, null);
  assert.equal(r['acme/linked-wt'].linked, true);
  assert.equal(r['acme/clean'].linked, false);

  assert.equal(hygiene.pathStatus(r['acme/clean']), 'ok');
  assert.equal(hygiene.pathStatus(r['acme/CaseDiffers']), 'ok');
  assert.equal(hygiene.pathStatus(r['acme/wrongplace']), 'mismatch');
  assert.equal(hygiene.expectedRel(r['acme/wrongplace']), 'example.com/acme/actualname');
  assert.equal(hygiene.pathStatus(r['acme/noorigin']), 'no_origin');
  assert.equal(hygiene.pathStatus(r['acme/linked-wt']), 'linked');

  const f = Object.fromEntries(hygiene.analyze(hygiene.normalize(data)).map((x) => [x.id, x]));
  assert.match(f['repo-path-mismatch'].detail, /acme\/wrongplace.*example\.com\/acme\/actualname/);
  assert.doesNotMatch(f['repo-path-mismatch'].detail, /linked-wt|CaseDiffers/);
  assert.match(f['repo-dirty'].detail, /acme\/dirty（変更 1・未追跡 1/);
  assert.match(f['repo-unpushed'].detail, /acme\/ahead（upstream より 1 件先）/);
  assert.match(f['repo-unpushed'].detail, /acme\/localonly（remote に無いコミット 1 件・upstream なし）/);
  assert.match(f['repo-behind'].detail, /acme\/behind（1 件遅れ）/);
  assert.match(f['repo-stash'].detail, /acme\/stashed（1 件）/);
  assert.match(f['repo-no-origin'].detail, /acme\/noorigin/);
  assert.ok(raw.length > 0);
});

test('読めないリポジトリは未観測として残し、きれいとは区別する', () => {
  const { ghq } = FX;
  const { data } = probe({ ghq_root: ghq });
  const r = byPath(hygiene.normalize(data));
  assert.equal(r['acme/broken'].state, 'unobserved');
  assert.equal(r['acme/broken'].reason, 'git_failed');
  assert.equal(r['acme/broken'].tracked, undefined);
  assert.equal(r['acme/clean'].state, 'ok');
  const f = hygiene.analyze(hygiene.normalize(data)).find((x) => x.id === 'repo-unobserved');
  assert.match(f.detail, /git を読めなかった 1 件（git_failed 1）/);
});

test('件数の上限を超えた分は skipped として数え、空とは区別する', () => {
  const { ghq } = FX;
  const { data } = probe({ ghq_root: ghq, max_repos: 2 });
  assert.equal(data.repos.length, 2);
  assert.ok(data.enumerated > 2);
  assert.equal(data.skipped, data.enumerated - 2);
  const f = hygiene.analyze(hygiene.normalize(data)).find((x) => x.id === 'repo-unobserved');
  assert.match(f.detail, new RegExp(`件数の上限で見ていない ${data.skipped} 件`));
  // 観測した2件だけの結果で、ほかのリポジトリが無いかのような所見は出さない
  assert.equal(hygiene.analyze(hygiene.normalize({ ...data, repos: [] })).some((x) => x.id === 'repo-unobserved'), true);
});

test('git が時間内に返らないリポジトリは timeout の未観測になる', { skip: process.platform === 'win32' }, () => {
  const { ghq } = FX;
  const bin = path.join(FX.base, 'bin');
  fs.mkdirSync(bin);
  const real = spawnSync('sh', ['-c', 'command -v git'], { encoding: 'utf8' }).stdout.trim();
  fs.writeFileSync(path.join(bin, 'git'), `#!/bin/sh\ncase "$*" in *"status --porcelain=v2"*) sleep 5;; esac\nexec "${real}" "$@"\n`, { mode: 0o755 });
  const { data } = probe({ ghq_root: ghq, cmd_s: 1, budget_s: 20, max_repos: 3 }, { PATH: `${bin}${path.delimiter}${process.env.PATH}` });
  assert.ok(data.repos.length > 0);
  for (const r of data.repos) { assert.equal(r.state, 'unobserved'); assert.equal(r.reason, 'timeout'); }
  assert.ok(data.elapsed_s < 30);
});

test('差分本文・ファイル内容・認証情報入りの URL を出力に含めない', () => {
  const { ghq } = FX;
  // scp 形式・ユーザー名とパスワードの両方・パスワードに @ を含む形も
  const dirty = path.join(ghq, 'example.com', 'acme', 'dirty');
  const forms = ['https://usr-one:pw-one@example.com/acme/dirty.git', 'ssh://git@example.com:22/acme/dirty.git', 'git@example.com:acme/dirty.git', 'https://usr-two:pw@two-word@example.com/acme/dirty.git', 'usr-three:pw-three@example.com:acme/dirty.git'];
  const outs = [];
  for (const form of forms) {
    git(dirty, 'remote', 'set-url', 'origin', form);
    const { raw, data } = probe({ ghq_root: ghq });
    outs.push([form, raw, byPath(data)['acme/dirty']]);
  }
  for (const [form, raw, r] of outs) {
    for (const secret of ['SECRET-BODY-MARKER', 'fixture-secret-token', 'fixtureuser', 'query-secret', 'usr-one', 'pw-one', 'usr-two', 'pw@two', 'two-word', 'usr-three', 'pw-three']) {
      assert.ok(!raw.includes(secret), `${form}: 出力に ${secret} がある`);
    }
    assert.ok(!/:\/\/[^/]*@/.test(r.origin ?? ''), `${form}: ${r.origin}`);
  }
  assert.equal(outs[1][2].origin_host, 'example.com');
  assert.equal(outs[1][2].origin_path, 'acme/dirty');
  assert.equal(outs[2][2].origin_path, 'acme/dirty');
  // 保存・表示の手前の整形でも落とす
  assert.equal(hygiene.stripUrl('https://user:tok@example.com/o/r.git?x=1#f'), 'https://example.com/o/r.git');
  assert.equal(hygiene.stripUrl('user:tok@example.com:o/r.git'), 'example.com:o/r.git');
  assert.equal(hygiene.stripUrl('git@example.com:o/r.git'), 'example.com:o/r.git');
  const n = hygiene.normalize({ probe: 'repo_hygiene', content: 'SECRET-BODY-MARKER', repos: [{ state: 'ok', path: '~/x', origin: 'https://a:b@example.com/o/r.git', diff: 'SECRET-BODY-MARKER' }] });
  assert.ok(!JSON.stringify(n).includes('SECRET-BODY-MARKER'));
  assert.ok(!JSON.stringify(n).includes('a:b'));
});

test('実行の前後でリポジトリのファイルとインデックスが変わらない（読み取り専用）', () => {
  const walk = (d) => fs.readdirSync(d, { withFileTypes: true }).flatMap((e) => {
    const p = path.join(d, e.name);
    return e.isDirectory() ? walk(p) : [`${p} ${fs.statSync(p).mtimeMs} ${fs.statSync(p).size}`];
  });
  const snap = () => walk(FX.ghq).sort().join('\n');
  const before = snap();
  probe({ ghq_root: FX.ghq });
  assert.equal(snap(), before);
});

test('git が無い機体は error として返す（空の結果にしない）', { skip: process.platform === 'win32' }, () => {
  const bin = tmp();
  try {
    const py = spawnSync('sh', ['-c', 'command -v python3'], { encoding: 'utf8' }).stdout.trim();
    const r = spawnSync(py, ['-X', 'utf8', '-'], { input: hygiene.script({}), env: { PATH: bin, HOME: bin }, encoding: 'utf8' });
    const data = JSON.parse(r.stdout.trim().split('\n').pop());
    assert.equal(data.error, 'git_missing');
    assert.deepEqual(data.repos, []);
    assert.match(hygiene.analyze(hygiene.normalize(data))[0].detail, /調査が失敗した（git_missing）/);
  } finally { fs.rmSync(bin, { recursive: true, force: true }); }
});

test('script: 上限と置き場を丸めて埋め込む（Python の辞書として読める）', () => {
  const s = hygiene.script({ max_repos: 99999, budget_s: 1, cmd_s: 'x', roots: ['~/a', 5, '', '日本語'], ghq_root: '/r' });
  const line = s.split('\n').find((l) => l.startsWith('KT_OPTS = '));
  assert.deepEqual(JSON.parse(line.replace(/^KT_OPTS = /, '').replace(/\s+#.*$/, '')), { max_repos: 5000, budget_s: 5, cmd_s: 12, roots: ['~/a', '日本語'], ghq_root: '/r' });
  assert.equal(s.split('\n').filter((l) => l.startsWith('KT_OPTS = ')).length, 1);
});

test('台帳: 共用機は既定で対象外、repo_hygiene で切り替える', () => {
  assert.equal(hygiene.enabled({ id: 'a' }), true);
  assert.equal(hygiene.enabled({ id: 'a', repo_hygiene: false }), false);
  assert.equal(hygiene.enabled({ id: 's', shared: true }), false);
  assert.equal(hygiene.enabled({ id: 's', shared: true, repo_hygiene: true }), true);
});

test('台帳: 運用対象でない機体は対象外で、接続せずに断る', async () => {
  for (const status of ['retired', 'no-ssh', 'paused', 'out-of-scope', 'bogus']) {
    const node = { id: 'x', alias: 'x', os: 'macos', status, repo_hygiene: true };
    assert.equal(hygiene.enabled(node), false, status);
    const r = await hygiene.auditNode(node, {});
    assert.deepEqual([r.ok, r.error], [false, 'excluded'], status);
  }
  assert.equal(hygiene.enabled({ id: 'a', status: 'online' }), true);
});

const row = (o = {}) => ({ path: '~/ghq/example.com/o/r', rel: 'example.com/o/r', linked: false, state: 'ok', reason: null, branch: 'main', detached: false, upstream: true, ahead: 0, behind: 0, local_only: null, tracked: 0, untracked: 0, conflicted: 0, dirty_age_s: null, head_age_s: 5, stash: 0, origin: 'https://example.com/o/r.git', origin_host: 'example.com', origin_path: 'o/r', has_origin: true, ...o });

test('所見: dirty は古さで重大度を分け、上位だけを並べる', () => {
  const young = hygiene.analyze({ repos: [row({ path: '~/a', tracked: 1, dirty_age_s: 3600 })] });
  assert.equal(young.length, 1);
  assert.deepEqual([young[0].id, young[0].severity, young[0].category], ['repo-dirty', 'info', 'repo']);
  assert.match(young[0].detail, /変更 1・未追跡 0・1 時間/);
  const old = hygiene.analyze({ repos: [row({ path: '~/a', untracked: 2, dirty_age_s: 9 * 86400 }), ...[1, 2, 3, 4, 5, 6].map((i) => row({ path: `~/b${i}`, tracked: 1, dirty_age_s: i * 60 }))] });
  assert.equal(old[0].severity, 'warn');
  assert.match(old[0].title, /7 件（7 日以上前からが 1 件）/);
  assert.match(old[0].detail, /^~\/a（変更 0・未追跡 2・9 日）/);
  assert.match(old[0].detail, /ほか 2 件$/);
  assert.deepEqual(hygiene.analyze({ repos: [row()] }), []);
  assert.deepEqual(hygiene.analyze({ repos: [] }), []);
});

test('所見: 観測できなかった分はきれいとして数えない', () => {
  const f = hygiene.analyze({ repos: [row(), { path: '~/x', rel: null, linked: false, state: 'unobserved', reason: 'timeout' }], skipped: 3, truncated: true });
  assert.equal(f.length, 1);
  assert.equal(f[0].id, 'repo-unobserved');
  assert.match(f[0].detail, /git を読めなかった 1 件（timeout 1）、件数の上限で見ていない 3 件、探索を時間または件数で打ち切った。確認できた 1 件だけで判定/);
});

test('ghq 照合: ghq root の外・.worktrees 配下・リンク worktree・origin を解釈できないものは不一致にしない', () => {
  assert.equal(hygiene.pathStatus(row({ rel: null })), 'outside');
  assert.equal(hygiene.pathStatus(row({ rel: '.worktrees/x' })), 'outside');
  assert.equal(hygiene.pathStatus(row({ linked: true, rel: 'elsewhere' })), 'linked');
  assert.equal(hygiene.pathStatus(row({ origin_host: null, origin_path: null, origin: '(local)' })), 'unknown');
  assert.equal(hygiene.pathStatus(row({ rel: 'example.com/O/R' })), 'ok');
  assert.equal(hygiene.pathStatus(row({ rel: 'gitlab.com/o/r' })), 'mismatch');
  assert.equal(hygiene.pathStatus(row({ rel: 'o/r' })), 'mismatch');
});

test('機体×リポジトリの表: origin で束ね、dirty の多い順に並べる', () => {
  const m = hygiene.matrix([
    { node_id: 'n1', repos: [row({ path: '~/z', tracked: 2 }), row({ path: '~/a' }), row({ origin_path: 'o/other', path: '~/o' }), row({ linked: true, path: '~/w' }), row({ has_origin: false, origin_host: null, origin_path: null, path: '~/n' })] },
    { node_id: 'n2', repos: [row({ untracked: 1, ahead: 2 })] },
  ]);
  assert.deepEqual(m.map((x) => [x.key, x.node_count, x.dirty_nodes]), [['example.com/o/r', 2, 1], ['example.com/o/other', 1, 0]]);
  assert.equal(m[0].nodes.n1.copies, 2);
  assert.equal(m[0].nodes.n1.tracked, 0); // パスの小さい方（~/a）が代表
  assert.equal(m[0].nodes.n2.ahead, 2);
});
