'use strict';
// probes/ai_sessions.py を架空のホームで動かす。本文が出ないこと・数え方・続きからの読み込みを確かめる
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const PROBE = fs.readFileSync(path.join(__dirname, '..', 'probes', 'ai_sessions.py'), 'utf8');
const PY = ['python3', 'python'].find((c) => spawnSync(c, ['--version']).status === 0);
const SECRET = ['TOP', 'SECRET', 'PROMPT'].join('-');
const MAX_LINE_BYTES = 1024 * 1024;

function run(home, state = {}) {
  const src = PROBE.replace('KT_STATE = {}', `KT_STATE = ${JSON.stringify(state)}`);
  const r = spawnSync(PY, ['-'], { input: src, env: { ...process.env, HOME: home, USERPROFILE: home }, encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  return { raw: r.stdout, out: JSON.parse(r.stdout) };
}

const jl = (rows) => rows.map((r) => JSON.stringify(r)).join('\n') + '\n';

function fixture() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-ai-'));
  const proj = path.join(home, '.claude', 'projects', '-work-demo');
  fs.mkdirSync(path.join(proj, 's1', 'subagents'), { recursive: true });
  const base = { sessionId: 's1', cwd: path.join(home, 'work', 'demo'), version: '9.9.9', entrypoint: 'cli' };
  const usage = { input_tokens: 10, output_tokens: 5, cache_read_input_tokens: 100, cache_creation_input_tokens: 7, output_tokens_details: { thinking_tokens: 2 } };
  fs.writeFileSync(path.join(proj, 's1.jsonl'), jl([
    { ...base, type: 'user', timestamp: '2026-10-01T00:00:00Z', message: { role: 'user', content: SECRET } },
    // 同じ応答が2行に分かれる（usage は ID ごとに1回だけ数える）
    { ...base, type: 'assistant', timestamp: '2026-10-01T00:00:01Z', message: { id: 'm1', model: 'model-a', usage, content: [{ type: 'text', text: SECRET }] } },
    { ...base, type: 'assistant', timestamp: '2026-10-01T00:00:02Z', message: { id: 'm1', model: 'model-a', usage, content: [{ type: 'tool_use', id: 't1', name: 'Bash', input: { command: SECRET } }] } },
    { ...base, type: 'user', timestamp: '2026-10-01T00:00:03Z', message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 't1', is_error: true, content: SECRET }] } },
    { type: 'pr-link', sessionId: 's1', prUrl: 'https://github.com/example/repo/pull/1', timestamp: '2026-10-01T00:00:04Z' },
  ]));
  fs.writeFileSync(path.join(proj, 's1', 'subagents', 'agent-x.jsonl'), jl([
    { ...base, sessionId: 'sub', type: 'user', timestamp: '2026-10-01T00:00:05Z', message: { role: 'user', content: SECRET } },
  ]));
  const cdx = path.join(home, '.codex', 'sessions', '2026', '10', '01');
  fs.mkdirSync(cdx, { recursive: true });
  const tok = (i, o) => ({ timestamp: '2026-10-01T01:00:02Z', type: 'event_msg', payload: { type: 'token_count', info: { total_token_usage: { input_tokens: i, cached_input_tokens: 50, output_tokens: o, reasoning_output_tokens: 3 } } } });
  fs.writeFileSync(path.join(cdx, 'rollout-a.jsonl'), jl([
    { timestamp: '2026-10-01T01:00:00Z', type: 'session_meta', payload: { id: 'c1', cwd: path.join(home, 'work'), cli_version: '1.2.3', originator: 'cli', source: { subagent: { thread_spawn: { parent_thread_id: 'c0' } } } } },
    { timestamp: '2026-10-01T01:00:01Z', type: 'turn_context', payload: { model: 'model-b' } },
    { timestamp: '2026-10-01T01:00:01Z', type: 'event_msg', payload: { type: 'task_started' } },
    { timestamp: '2026-10-01T01:00:01Z', type: 'response_item', payload: { type: 'function_call', name: 'shell', arguments: SECRET } },
    tok(100, 10), tok(200, 20),
    // 古い版の書式など、形の違う行は飛ばす（ファイル全体を読めなくしない）
    { timestamp: '2026-10-01T01:00:02Z', type: 'response_item', payload: 'oops', note: '"type":"function_call",' },
    { timestamp: '2026-10-01T01:00:02Z', type: 'event_msg', payload: { type: 'token_count', info: 'not-an-object' } },
    { timestamp: '2026-10-01T01:00:03Z', type: 'event_msg', payload: { type: 'task_complete', error: 'boom' } },
  ]));
  return { home, proj, cdx };
}

test('AI セッション: 本文を出さずに数・トークン・モデル・PR を出す', { skip: !PY && 'python が無い' }, () => {
  const { home } = fixture();
  const { raw, out } = run(home);
  assert.ok(!raw.includes(SECRET), '本文が出力に含まれている');
  assert.equal(out.truncated, false);
  assert.deepEqual(out.errors, [], '形の違う行でファイル全体を読めなくしない');
  const c = out.sessions.find((s) => s.file === 'claude:-work-demo/s1.jsonl');
  assert.equal(c.cwd, path.join('~', 'work', 'demo'));
  assert.deepEqual([c.prompts, c.assistant_msgs, c.tool_calls, c.tool_errors, c.model, c.version], [1, 1, 1, 1, 'model-a', '9.9.9']);
  assert.deepEqual(c.tokens, { in: 10, out: 5, cache_read: 100, cache_write: 7, reasoning: 2 });
  assert.deepEqual(c.tool_error_counts, { Bash: 1 });
  assert.deepEqual(c.prs, ['https://github.com/example/repo/pull/1']);
  assert.equal(out.sessions.find((s) => s.file.endsWith('agent-x.jsonl')).parent_id, 's1');
  const x = out.sessions.find((s) => s.tool === 'codex');
  assert.deepEqual([x.session_id, x.parent_id, x.model, x.prompts, x.tool_calls, x.turn_errors, x.tokens_mode], ['c1', 'c0', 'model-b', 1, 1, 1, 'max']);
  assert.deepEqual(x.tokens, { in: 150, out: 20, cache_read: 50, cache_write: 0, reasoning: 3 });
});

test('AI セッション: 続きの位置から差分だけ読み、変わっていないファイルは読まない', { skip: !PY && 'python が無い' }, () => {
  const { home, proj } = fixture();
  const first = run(home).out;
  const again = run(home, { files: first.cursors }).out;
  assert.equal(again.files_changed, 0);
  assert.equal(again.sessions.length, 0);
  fs.appendFileSync(path.join(proj, 's1.jsonl'), jl([{ type: 'user', sessionId: 's1', timestamp: '2026-10-02T00:00:00Z', message: { role: 'user', content: SECRET } }]) + '{"type":"user","partial');
  const next = run(home, { files: { ...first.cursors } }).out;
  const s = next.sessions.find((r) => r.file === 'claude:-work-demo/s1.jsonl');
  assert.deepEqual([s.mode, s.prompts], ['add', 1]);
  // 書きかけの行の手前までしか進めない
  assert.equal(next.cursors['claude:-work-demo/s1.jsonl'], fs.statSync(path.join(proj, 's1.jsonl')).size - '{"type":"user","partial'.length);
});

test('AI セッション: 時間ごとの使用量・残りの量・消えたファイル・ホームを ~ にした鍵', { skip: !PY && 'python が無い' }, () => {
  const { home } = fixture();
  // Claude Code のプロジェクト名に入ったホーム（英数字以外を - にしたもの）は ~ にする
  const enc = home.replace(/[^A-Za-z0-9]/g, '-');
  const own = path.join(home, '.claude', 'projects', `${enc}-work-app`);
  fs.mkdirSync(own, { recursive: true });
  fs.writeFileSync(path.join(own, 's2.jsonl'), jl([{ type: 'user', sessionId: 's2', cwd: path.join(home, 'work', 'app'), timestamp: '2026-10-03T00:00:00Z', message: { role: 'user', content: SECRET } }]));
  const { raw, out } = run(home, { files: { 'claude:gone/old.jsonl': 10 } });
  assert.ok(!raw.includes(SECRET));
  assert.ok(!raw.includes(enc), 'ホームのパスが鍵に残っている');
  assert.ok(out.sessions.some((s) => s.file === 'claude:~-work-app/s2.jsonl'));
  assert.deepEqual(out.gone, ['claude:gone/old.jsonl']);
  assert.equal(out.bytes_pending, out.bytes_read);
  const h = (iso) => String(Math.floor(Date.parse(iso) / 3600000));
  const c = out.sessions.find((s) => s.file === 'claude:-work-demo/s1.jsonl');
  // [入力, 出力, キャッシュ読み, キャッシュ書き, 推論, ツール呼び出し, 指示]
  assert.deepEqual(c.hours[h('2026-10-01T00:00:00Z')], [10, 5, 100, 7, 2, 1, 1]);
  const x = out.sessions.find((s) => s.tool === 'codex');
  // Codex は累計（その時間の終わりまで）。ツール呼び出しと指示は数
  assert.deepEqual(x.hours[h('2026-10-01T01:00:00Z')], [150, 20, 50, 0, 3, 1, 1]);
});

test('AI セッション: 1MiBを超える完結行を捨てて直後の正常行を読む', { skip: !PY && 'python が無い' }, () => {
  const { home, proj } = fixture();
  const file = path.join(proj, 'huge.jsonl');
  const normal = JSON.stringify({ type: 'user', sessionId: 'huge', timestamp: '2026-10-04T00:00:00Z', message: { role: 'user', content: SECRET } });
  fs.writeFileSync(file, Buffer.concat([Buffer.alloc(MAX_LINE_BYTES + 1, 0x78), Buffer.from('\n' + normal + '\n')]));

  const { out } = run(home);
  const key = 'claude:-work-demo/huge.jsonl';
  const session = out.sessions.find((s) => s.file === key);
  assert.equal(session.prompts, 1, '巨大行の直後の完結した正常行を数える');
  assert.equal(out.cursors[key], fs.statSync(file).size, '完結した巨大行は安全に読み捨てて進める');
  assert.ok(out.errors.some((e) => e.includes('1048576 bytes; metadata unavailable')), '巨大行で欠けたメタデータを出す');
});

test('AI セッション: partial の巨大行は cursor を進めず、完結後に同じ行境界から再読する', { skip: !PY && 'python が無い' }, () => {
  const { home, proj } = fixture();
  const file = path.join(proj, 'partial-huge.jsonl');
  const key = 'claude:-work-demo/partial-huge.jsonl';
  fs.writeFileSync(file, Buffer.alloc(MAX_LINE_BYTES + 1, 0x79));

  const first = run(home).out;
  assert.equal(first.cursors[key], 0, '改行の無い巨大行の途中へ cursor を置かない');
  assert.equal(first.truncated, true, '完結していない巨大行は次回に持ち越す');
  fs.appendFileSync(file, '\n' + JSON.stringify({ type: 'user', sessionId: 'partial', timestamp: '2026-10-05T00:00:00Z', message: { role: 'user', content: SECRET } }) + '\n');

  const next = run(home, { files: first.cursors }).out;
  const session = next.sessions.find((s) => s.file === key);
  assert.equal(session.prompts, 1, '完結後は巨大行を先頭から捨て、直後の正常行を数える');
  assert.equal(next.cursors[key], fs.statSync(file).size, '再読後は完結行の境界へ進める');
  assert.ok(next.errors.some((e) => e.includes('metadata unavailable')));
});
