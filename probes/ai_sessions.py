# AI エージェント（Claude Code・Codex）のセッションの要約。読み取り専用・標準ライブラリのみ・macOS / Windows 共通。
# 行は読むが、会話の本文・ツールの入出力は取り出さない・出さない。出すのは数・名前・時刻・モデル・トークン・PR の URL だけ。
#   Claude Code: ~/.claude/projects/<プロジェクト>/<セッション>.jsonl（subagents/ 配下はサブエージェント）
#   Codex:       ~/.codex/sessions/<年>/<月>/<日>/rollout-*.jsonl
# 続きの位置（ファイルごとのバイト位置）は呼び出し側が KT_STATE に入れて渡す。この機体にはファイルを書かない。
# 1回の実行は KT_BUDGET_S 秒で区切り、残りは次回に回す（truncated=true）。
# ホームのパスは ~ に置き換えて出す（cwd・ファイルの鍵・エラー）。Claude Code のプロジェクト名に入っているホームも ~ にする。
#
# 出力（1行の JSON）:
#   sessions: ファイルごとの要約。mode が replace なら置き換え、add なら前回の値に足す（tokens_mode が max のトークンは累計なので最大値）
#     hours: { "<epoch の時（ms / 3600000）>": [入力, 出力, キャッシュ読み, キャッシュ書き, 推論, ツール呼び出し, 指示] }
#            トークンの5つは tokens_mode に従う（add はその時間の量、max はその時間の終わりまでの累計）
#   cursors: 読んだファイルの続きの位置。gone: KT_STATE にあるが、もう無いファイル
#   bytes_pending: 読む前に残っていた量。truncated: 時間で区切った（続きあり）
import json, os, re, time

KT_STATE = {}       # 呼び出し側が置き換える: {"files": {key: offset}}
KT_BUDGET_S = 25
MAX_LINE_BYTES = 1024 * 1024
DRAIN_BYTES = 64 * 1024

HOME = os.path.expanduser("~")
WIN = os.name == "nt"
# Claude Code のプロジェクト名は cwd の英数字以外を - にしたもの。その中のホームの部分
HOME_ENC = re.sub(r"[^A-Za-z0-9]", "-", HOME)
ROOTS = {"claude": os.path.join(HOME, ".claude", "projects"), "codex": os.path.join(HOME, ".codex", "sessions")}
TOK = ("in", "out", "cache_read", "cache_write", "reasoning")
BAD_LINE = (ValueError, AttributeError, TypeError, KeyError, IndexError)
t0 = time.time()


def ms(ts):
    # "2026-10-07T05:54:17.123Z" → epoch ms（標準ライブラリだけで）
    if not ts:
        return None
    try:
        from datetime import datetime
        return int(datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp() * 1000)
    except Exception:
        return None


def hour_of(ts):
    t = ms(ts)
    return None if t is None else t // 3600000


def fold(s):
    return s.lower() if WIN else s


def tilde(p):
    if not isinstance(p, str):
        return p
    a, h = fold(p), fold(HOME)
    if a == h or a.startswith(h + os.sep) or a.startswith(h + "/"):
        return "~" + p[len(HOME):]
    return p


def tilde_text(s):
    s, h = str(s), fold(HOME)
    if len(h) < 2:
        return s
    low, out, i = fold(s), [], 0
    while True:
        j = low.find(h, i)
        if j < 0:
            return "".join(out) + s[i:]
        out += [s[i:j], "~"]
        i = j + len(h)


def file_key(tool, root, path):
    rel = os.path.relpath(path, root).replace(os.sep, "/")
    if tool == "claude" and HOME_ENC.strip("-"):
        head = fold(rel[:len(HOME_ENC)])
        rest = rel[len(HOME_ENC):]
        if head == fold(HOME_ENC) and (rest == "" or rest[0] in "-/"):
            rel = "~" + rest
    return tool + ":" + rel


def new_rec(tool, key):
    return {"tool": tool, "file": key, "session_id": None, "parent_id": None, "cwd": None, "version": None, "origin": None, "model": None,
            "first_ts": None, "last_ts": None, "prompts": 0, "assistant_msgs": 0, "tool_calls": 0, "tool_errors": 0, "turn_errors": 0,
            "hook_errors": 0, "api_errors": 0, "tokens": {"in": 0, "out": 0, "cache_read": 0, "cache_write": 0, "reasoning": 0},
            "tokens_mode": "add", "tool_counts": {}, "tool_error_counts": {}, "prs": [], "hours": {}}


def bucket(r, h):
    return r["hours"].setdefault(str(h), [0, 0, 0, 0, 0, 0, 0])


def bump(r, h, i, n=1):
    if h is not None and n:
        bucket(r, h)[i] += n


def touch_ts(r, ts):
    t = ms(ts)
    if t:
        r["first_ts"] = t if r["first_ts"] is None else min(r["first_ts"], t)
        r["last_ts"] = t if r["last_ts"] is None else max(r["last_ts"], t)


def claude_line(r, line, usage, tool_names):
    if not (b'"type":"assistant"' in line or b'"type":"user"' in line or b'"pr-link"' in line or b'"hookErrors"' in line):
        return
    d = json.loads(line)
    t = d.get("type")
    touch_ts(r, d.get("timestamp"))
    h = hour_of(d.get("timestamp"))
    r["session_id"] = r["session_id"] or d.get("sessionId")
    r["cwd"] = r["cwd"] or tilde(d.get("cwd"))
    r["version"] = d.get("version") or r["version"]
    r["origin"] = r["origin"] or d.get("entrypoint")
    if t == "assistant":
        m = d.get("message") or {}
        if d.get("isApiErrorMessage"):
            r["api_errors"] += 1
        if m.get("model") and m["model"] != "<synthetic>":
            r["model"] = m["model"]
        if m.get("id"):
            usage[m["id"]] = (m.get("usage") or {}, h)  # 同じ応答が複数行に分かれるので、ID ごとに最後の値を使う
        for b in m.get("content") or []:
            if isinstance(b, dict) and b.get("type") == "tool_use":
                name = b.get("name") or "?"
                r["tool_calls"] += 1
                r["tool_counts"][name] = r["tool_counts"].get(name, 0) + 1
                bump(r, h, 5)
                if b.get("id"):
                    tool_names[b["id"]] = name
    elif t == "user":
        c = (d.get("message") or {}).get("content")
        if isinstance(c, str):
            if not d.get("isMeta"):
                r["prompts"] += 1
                bump(r, h, 6)
        elif isinstance(c, list):
            for b in c:
                if isinstance(b, dict) and b.get("type") == "tool_result" and b.get("is_error"):
                    name = tool_names.get(b.get("tool_use_id"), "?")
                    r["tool_errors"] += 1
                    r["tool_error_counts"][name] = r["tool_error_counts"].get(name, 0) + 1
    elif t == "pr-link":
        if d.get("prUrl") and d["prUrl"] not in r["prs"]:
            r["prs"].append(d["prUrl"])
    elif d.get("hookErrors"):
        r["hook_errors"] += len(d["hookErrors"])


def codex_line(r, line):
    if b'"type":"function_call",' in line or b'"type":"custom_tool_call",' in line:
        r["tool_calls"] += 1
        try:
            d = json.loads(line)
            name = (d.get("payload") or {}).get("name") or "?"
            h = hour_of(d.get("timestamp"))
        except BAD_LINE:
            name, h = "?", None
        r["tool_counts"][name] = r["tool_counts"].get(name, 0) + 1
        bump(r, h, 5)
        return
    if not (b'"token_count"' in line or b'"session_meta"' in line or b'"turn_context"' in line or b'"task_started"' in line or b'"task_complete"' in line):
        return
    d = json.loads(line)
    p = d.get("payload") or {}
    touch_ts(r, d.get("timestamp"))
    h = hour_of(d.get("timestamp"))
    kind = d.get("type")
    if kind == "session_meta":
        r["session_id"] = p.get("id") or p.get("session_id")
        r["cwd"] = tilde(p.get("cwd"))
        r["version"] = p.get("cli_version")
        r["origin"] = p.get("originator")
        spawn = ((p.get("source") or {}).get("subagent") or {}).get("thread_spawn") if isinstance(p.get("source"), dict) else None
        if spawn:
            r["parent_id"] = spawn.get("parent_thread_id")
    elif kind == "turn_context":
        r["model"] = p.get("model") or r["model"]
    elif p.get("type") == "task_started":
        r["prompts"] += 1
        bump(r, h, 6)
    elif p.get("type") == "task_complete":
        if p.get("error"):
            r["turn_errors"] += 1
    elif p.get("type") == "token_count":
        tot = (p.get("info") or {}).get("total_token_usage")
        if tot:  # スレッドの累計なので、足さずに最後の値を使う
            r["tokens_mode"] = "max"
            r["tokens"] = {"in": max(0, (tot.get("input_tokens") or 0) - (tot.get("cached_input_tokens") or 0)), "out": tot.get("output_tokens") or 0,
                           "cache_read": tot.get("cached_input_tokens") or 0, "cache_write": tot.get("cache_write_input_tokens") or 0,
                           "reasoning": tot.get("reasoning_output_tokens") or 0}
            if h is not None:
                b = bucket(r, h)
                for i, k in enumerate(TOK):
                    b[i] = max(b[i], r["tokens"][k])


def discard_large_line(f, first):
    """Complete an over-limit line without retaining its body. None means retry from its start."""
    size, tail = len(first), first
    while not tail.endswith(b"\n"):
        if time.time() - t0 > KT_BUDGET_S:
            return None
        tail = f.readline(DRAIN_BYTES)
        if not tail:
            return None
        size += len(tail)
    return size if time.time() - t0 <= KT_BUDGET_S else None


def scan_file(tool, path, key, off, out):
    size = os.path.getsize(path)
    reset = size < off
    if reset:
        off = 0
    r = new_rec(tool, key)
    r["mode"] = "replace" if off == 0 else "add"
    if tool == "claude":
        rel = os.path.relpath(path, ROOTS["claude"]).split(os.sep)
        if "subagents" in rel:
            r["parent_id"] = rel[rel.index("subagents") - 1]
    usage, tool_names = {}, {}
    pos = off
    skipped_large = 0
    with open(path, "rb") as f:
        f.seek(off)
        while True:
            if time.time() - t0 > KT_BUDGET_S:
                out["truncated"] = True
                break
            line = f.readline(MAX_LINE_BYTES + 1)
            if not line:
                break
            if len(line) > MAX_LINE_BYTES:
                consumed = discard_large_line(f, line)
                if consumed is None:
                    out["truncated"] = True
                    break
                pos += consumed
                skipped_large += 1
                continue
            if not line.endswith(b"\n"):
                out["truncated"] = True
                break  # 書きかけの行は次回
            pos += len(line)
            try:
                (claude_line(r, line, usage, tool_names) if tool == "claude" else codex_line(r, line))
            except BAD_LINE:
                pass  # 形の違う行（古い版の書式など）は飛ばす。1行のせいでファイル全体を読めなくしない
            if time.time() - t0 > KT_BUDGET_S:
                out["truncated"] = True
                break
    if skipped_large:
        out["errors"].append(f"{key}: {skipped_large} line(s) exceeds {MAX_LINE_BYTES} bytes; metadata unavailable")
    for u, h in usage.values():
        add = (u.get("input_tokens") or 0, u.get("output_tokens") or 0, u.get("cache_read_input_tokens") or 0,
               u.get("cache_creation_input_tokens") or 0, ((u.get("output_tokens_details") or {}).get("thinking_tokens") or 0))
        for i, k in enumerate(TOK):
            r["tokens"][k] += add[i]
            bump(r, h, i, add[i])
    r["assistant_msgs"] = len(usage)
    out["bytes_read"] += pos - off
    out["cursors"][key] = pos
    if pos > off or reset:
        out["sessions"].append(r)


def main():
    files = (KT_STATE or {}).get("files") or {}
    out = {"sessions": [], "cursors": {}, "gone": [], "files_total": 0, "files_changed": 0, "bytes_pending": 0, "bytes_read": 0, "truncated": False, "errors": []}
    todo, seen = [], set()
    for tool, root in ROOTS.items():
        for dirpath, _dirs, names in os.walk(root):
            for n in names:
                if not n.endswith(".jsonl") or (tool == "codex" and not n.startswith("rollout-")):
                    continue
                p = os.path.join(dirpath, n)
                key = file_key(tool, root, p)
                seen.add(key)
                out["files_total"] += 1
                try:
                    st = os.stat(p)
                except OSError:
                    continue
                off = files.get(key, 0)
                if st.st_size != off:
                    todo.append((st.st_mtime, tool, p, key))
                    out["bytes_pending"] += st.st_size - off if st.st_size > off else st.st_size
    out["gone"] = sorted(k for k in files if k not in seen)
    out["files_changed"] = len(todo)
    for _m, tool, p, key in sorted(todo, reverse=True):  # 新しいものから
        if time.time() - t0 > KT_BUDGET_S:
            out["truncated"] = True
            break
        try:
            scan_file(tool, p, key, files.get(key, 0), out)
        except Exception as e:
            out["errors"].append(tilde_text(f"{key}: {e}")[:300])
    out["elapsed_s"] = round(time.time() - t0, 2)
    print(json.dumps(out, ensure_ascii=False))


main()
