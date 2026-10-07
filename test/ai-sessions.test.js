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

// ---- 出どころの台帳（区間の指紋・続きの検算・検算モード）と、ファイルをまたぐ重複を除くための応答ごとの量 ----

const crypto = require('node:crypto');
const sha = (b) => crypto.createHash('sha256').update(b).digest('hex');

test('AI セッション: 区間の指紋・行数と、応答 ID のハッシュ（ID そのものは出さない）', { skip: !PY && 'python が無い' }, () => {
  const { home, proj } = fixture();
  const RESP = 'msg_RESPONSE_ID_SHOULD_NOT_LEAK';
  const base = { sessionId: 's1', cwd: path.join(home, 'work', 'demo'), version: '9.9.9' };
  const usage = { input_tokens: 3, output_tokens: 4, cache_read_input_tokens: 5, cache_creation_input_tokens: 9, cache_creation: { ephemeral_1h_input_tokens: 6, ephemeral_5m_input_tokens: 3 } };
  // 同じ応答がサブエージェントのファイルにも出る（DB が応答ごとに重複を除く）
  const rows = [{ ...base, type: 'assistant', timestamp: '2026-10-01T00:00:06Z', message: { id: RESP, model: 'model-a', usage, content: [{ type: 'text', text: SECRET }] } }];
  fs.writeFileSync(path.join(proj, 's1', 'subagents', 'agent-y.jsonl'), jl(rows) + 'not json\n');
  fs.appendFileSync(path.join(proj, 's1.jsonl'), jl(rows));
  const { raw, out } = run(home);
  assert.ok(!raw.includes(RESP), '応答 ID がそのまま出ている');
  assert.ok(!raw.includes(SECRET));
  const want = sha(`claude:${RESP}`).slice(0, 16);
  const main = out.sessions.find((s) => s.file === 'claude:-work-demo/s1.jsonl');
  const sub = out.sessions.find((s) => s.file.endsWith('agent-y.jsonl'));
  for (const s of [main, sub]) {
    const r = s.responses.find((x) => x[0] === want);
    // [応答, 時刻, モデルの番号, 入力, 出力, キャッシュ読み, キャッシュ書き, うち 1 時間, 推論]
    assert.deepEqual(r.slice(1), [Date.parse('2026-10-01T00:00:06Z'), s.resp_models.indexOf('model-a'), 3, 4, 5, 9, 6, 0]);
  }
  // 区間: 開始〜終了のバイト列の SHA-256・行数・数えた行・形の違う行（本文は出さない）
  const file = fs.readFileSync(path.join(proj, 's1', 'subagents', 'agent-y.jsonl'));
  assert.deepEqual({ ...sub.span, anchor: undefined }, { start: 0, end: file.length, sha256: sha(file), lines: 2, used: 1, skipped: 0, anchor: undefined });
  assert.deepEqual(sub.span.anchor, [file.length, sha(file)]);
  const m = fs.readFileSync(path.join(proj, 's1.jsonl'));
  assert.equal(main.span.sha256, sha(m));
  assert.equal(main.span.lines, 6);
  assert.equal(out.version, PROBE.match(/^KT_VERSION = "([^"]+)"/m)[1]);
});

test('AI セッション: 続きは位置の手前の指紋で検算し、合わなければ最初から読み直す', { skip: !PY && 'python が無い' }, () => {
  const { home, proj } = fixture();
  const p = path.join(proj, 's1.jsonl');
  const key = 'claude:-work-demo/s1.jsonl';
  const first = run(home).out;
  const s1 = first.sessions.find((s) => s.file === key);
  const anchors = { [key]: s1.span.anchor };
  const more = jl([{ type: 'user', sessionId: 's1', timestamp: '2026-10-02T00:00:00Z', message: { role: 'user', content: SECRET } }]);
  fs.appendFileSync(p, more);
  // 手前が同じ → 続き（add）。区間は前回の終了から
  const next = run(home, { files: first.cursors, anchors }).out.sessions.find((s) => s.file === key);
  assert.deepEqual([next.mode, next.rewound, next.span.start, next.span.lines, next.span.sha256], ['add', null, s1.span.end, 1, sha(Buffer.from(more))]);
  // 手前を書き換えた（同じ長さ）→ 最初から読み直す
  const buf = fs.readFileSync(p);
  buf[s1.span.end - 5] = buf[s1.span.end - 5] === 0x41 ? 0x42 : 0x41;
  fs.writeFileSync(p, buf);
  fs.appendFileSync(p, more);
  const again = run(home, { files: first.cursors, anchors }).out.sessions.find((s) => s.file === key);
  assert.deepEqual([again.mode, again.rewound, again.span.start], ['replace', 'anchor', 0]);
  // 短くなった → 最初から
  fs.writeFileSync(p, more);
  const shrunk = run(home, { files: first.cursors, anchors }).out.sessions.find((s) => s.file === key);
  assert.deepEqual([shrunk.mode, shrunk.rewound], ['replace', 'shrunk']);
});

test('AI セッション: 検算モードは区間を読み直して指紋だけを返す（取り込まない）', { skip: !PY && 'python が無い' }, () => {
  const { home, proj } = fixture();
  const p = path.join(proj, 's1.jsonl');
  const size = fs.statSync(p).size;
  const src = PROBE.replace('KT_VERIFY = []', `KT_VERIFY = ${JSON.stringify([['claude:-work-demo/s1.jsonl', 0, size], ['claude:-work-demo/s1.jsonl', 10, size + 99], ['claude:gone/x.jsonl', 0, 5], ['claude:../escape', 0, 1]])}`);
  const r = spawnSync(PY, ['-'], { input: src, env: { ...process.env, HOME: home, USERPROFILE: home }, encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  assert.ok(!r.stdout.includes(SECRET));
  const v = JSON.parse(r.stdout);
  assert.equal(v.sessions, undefined, '取り込みはしない');
  assert.deepEqual(v.verify.map((x) => x.state), ['ok', 'short', 'gone', 'gone']);
  assert.equal(v.verify[0].sha256, sha(fs.readFileSync(p)));
  assert.equal(v.verify[0].lines, 5);
});

// ---- Codex の残り枠（probes/codex_limits.py）。偽の codex で app-server の受け答えを確かめる ----

const LIMITS = fs.readFileSync(path.join(__dirname, '..', 'probes', 'codex_limits.py'), 'utf8');

function fakeCodex(mode) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-codex-'));
  const bin = path.join(dir, 'codex');
  fs.writeFileSync(bin, `#!/usr/bin/env python3
import json, sys
MODE = ${JSON.stringify(mode)}
n = 0
assert sys.argv[1:] == ["app-server"], sys.argv
for line in sys.stdin:
    m = json.loads(line)
    if m.get("method") == "initialize":
        print(json.dumps({"id": m["id"], "result": {"userAgent": "fake", "codexHome": "/SECRET-HOME"}}), flush=True)
    elif m.get("method") == "account/rateLimits/read":
        n += 1
        print(json.dumps({"method": "account/rateLimits/updated", "params": {"rateLimits": {}}}), flush=True)
        if MODE == "error":
            print(json.dumps({"id": m["id"], "error": {"code": -32600, "message": "chatgpt authentication required to read rate limits"}}), flush=True)
            continue
        if MODE == "empty" or (MODE == "empty-then-full" and n == 1):
            res = {"rateLimits": {"primary": None, "secondary": None}}
        else:
            res = {"rateLimits": {"limitId": "codex", "planType": "SECRET-PLAN", "credits": {"balance": "SECRET-BALANCE"},
                                  "primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 1900000000},
                                  "secondary": {"usedPercent": 61, "windowDurationMins": 10080, "resetsAt": 1900500000}},
                   "rateLimitResetCredits": {"availableCount": 2}}
        print(json.dumps({"id": m["id"], "result": res}), flush=True)
`);
  fs.chmodSync(bin, 0o755);
  return dir;
}

function limits(dir, src = LIMITS) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-home-'));
  const r = spawnSync(PY, ['-'], { input: src, env: { ...process.env, HOME: home, PATH: `${dir}${path.delimiter}${process.env.PATH}` }, encoding: 'utf8', timeout: 40000 });
  assert.equal(r.status, 0, r.stderr);
  return { raw: r.stdout, out: JSON.parse(r.stdout) };
}

const NO_FAKE = (!PY || process.platform === 'win32') && '偽の codex は macOS / Linux だけ';

test('Codex の残り枠: 窓の長さ・使用率・リセット時刻だけを出す（起動直後の空は 1 回だけ読み直す）', { skip: NO_FAKE }, () => {
  const { raw, out } = limits(fakeCodex('empty-then-full'));
  for (const s of ['SECRET-HOME', 'SECRET-PLAN', 'SECRET-BALANCE', 'availableCount', 'fake']) assert.ok(!raw.includes(s), `${s} が出ている`);
  assert.deepEqual([out.codex, out.empty, out.error], [true, false, null]);
  assert.deepEqual(out.windows, [{ mins: 300, used_pct: 25, resets_at: 1900000000 }, { mins: 10080, used_pct: 61, resets_at: 1900500000 }]);
});

test('Codex の残り枠: 空のまま・失敗・codex が無い', { skip: NO_FAKE }, () => {
  const empty = limits(fakeCodex('empty')).out;
  assert.deepEqual([empty.empty, empty.windows, empty.error], [true, [], null]);
  const err = limits(fakeCodex('error')).out;
  assert.match(err.error, /authentication required/);
  // よくある置き場所も見ないようにして、PATH に codex が無い状態を作る
  const none = LIMITS.replace(/cands = \[[\s\S]*?\]\n/, 'cands = []\n');
  assert.notEqual(none, LIMITS);
  // PATH には python だけを置いた空のディレクトリ（本物の codex を見つけて呼ばない）
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kt-nocodex-'));
  const exe = spawnSync(PY, ['-c', 'import sys; print(sys.executable)'], { encoding: 'utf8' }).stdout.trim();
  fs.symlinkSync(exe, path.join(dir, PY));
  const r = spawnSync(path.join(dir, PY), ['-'], { input: none, env: { ...process.env, PATH: dir, HOME: dir }, encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(JSON.parse(r.stdout).codex, false);
});
