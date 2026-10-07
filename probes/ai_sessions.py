# AI エージェント（Claude Code・Codex）のセッションの要約。読み取り専用・標準ライブラリのみ・macOS / Windows 共通。
# 行は読むが、会話の本文・ツールの入出力は取り出さない・出さない。出すのは数・名前・時刻・モデル・トークン・PR の URL だけ。
#   Claude Code: ~/.claude/projects/<プロジェクト>/<セッション>.jsonl（subagents/ 配下はサブエージェント）
#   Codex:       ~/.codex/sessions/<年>/<月>/<日>/rollout-*.jsonl
# 続きの位置（ファイルごとのバイト位置）は呼び出し側が KT_STATE に入れて渡す。この機体にはファイルを書かない。
# 1回の実行は KT_BUDGET_S 秒で区切り、残りは次回に回す（truncated=true）。
import base64, json, os, sys, time

KT_STATE = {}       # 呼び出し側が置き換える: {"files": {key: offset}}
KT_BUDGET_S = 25

HOME = os.path.expanduser("~")
ROOTS = {"claude": os.path.join(HOME, ".claude", "projects"), "codex": os.path.join(HOME, ".codex", "sessions")}
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


def tilde(p):
    if isinstance(p, str) and p.startswith(HOME):
        return "~" + p[len(HOME):]
    return p


def new_rec(tool, key):
    return {"tool": tool, "file": key, "session_id": None, "parent_id": None, "cwd": None, "version": None, "origin": None, "model": None,
            "first_ts": None, "last_ts": None, "prompts": 0, "assistant_msgs": 0, "tool_calls": 0, "tool_errors": 0, "turn_errors": 0,
            "hook_errors": 0, "api_errors": 0, "tokens": {"in": 0, "out": 0, "cache_read": 0, "cache_write": 0, "reasoning": 0},
            "tokens_mode": "add", "tool_counts": {}, "tool_error_counts": {}, "prs": []}


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
            usage[m["id"]] = m.get("usage") or {}  # 同じ応答が複数行に分かれるので、ID ごとに最後の値を使う
        for b in m.get("content") or []:
            if isinstance(b, dict) and b.get("type") == "tool_use":
                name = b.get("name") or "?"
                r["tool_calls"] += 1
                r["tool_counts"][name] = r["tool_counts"].get(name, 0) + 1
                if b.get("id"):
                    tool_names[b["id"]] = name
    elif t == "user":
        c = (d.get("message") or {}).get("content")
        if isinstance(c, str):
            if not d.get("isMeta"):
                r["prompts"] += 1
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
            name = json.loads(line).get("payload", {}).get("name") or "?"
        except ValueError:
            name = "?"
        r["tool_counts"][name] = r["tool_counts"].get(name, 0) + 1
        return
    if not (b'"token_count"' in line or b'"session_meta"' in line or b'"turn_context"' in line or b'"task_started"' in line or b'"task_complete"' in line):
        return
    d = json.loads(line)
    p = d.get("payload") or {}
    touch_ts(r, d.get("timestamp"))
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
    with open(path, "rb") as f:
        f.seek(off)
        for line in f:
            if not line.endswith(b"\n"):
                break  # 書きかけの行は次回
            pos += len(line)
            try:
                (claude_line(r, line, usage, tool_names) if tool == "claude" else codex_line(r, line))
            except ValueError:
                pass
            if time.time() - t0 > KT_BUDGET_S:
                out["truncated"] = True
                break
    for u in usage.values():
        r["tokens"]["in"] += u.get("input_tokens") or 0
        r["tokens"]["out"] += u.get("output_tokens") or 0
        r["tokens"]["cache_read"] += u.get("cache_read_input_tokens") or 0
        r["tokens"]["cache_write"] += u.get("cache_creation_input_tokens") or 0
        r["tokens"]["reasoning"] += ((u.get("output_tokens_details") or {}).get("thinking_tokens") or 0)
    r["assistant_msgs"] = len(usage)
    out["bytes_read"] += pos - off
    out["cursors"][key] = pos
    if pos > off or reset:
        out["sessions"].append(r)


def main():
    files = (KT_STATE or {}).get("files") or {}
    out = {"sessions": [], "cursors": {}, "files_total": 0, "files_changed": 0, "bytes_read": 0, "truncated": False, "errors": []}
    todo = []
    for tool, root in ROOTS.items():
        for dirpath, _dirs, names in os.walk(root):
            for n in names:
                if not n.endswith(".jsonl") or (tool == "codex" and not n.startswith("rollout-")):
                    continue
                p = os.path.join(dirpath, n)
                key = tool + ":" + os.path.relpath(p, root).replace(os.sep, "/")
                out["files_total"] += 1
                try:
                    st = os.stat(p)
                except OSError:
                    continue
                if st.st_size != files.get(key, 0):
                    todo.append((st.st_mtime, tool, p, key))
    out["files_changed"] = len(todo)
    for _m, tool, p, key in sorted(todo, reverse=True):  # 新しいものから
        if time.time() - t0 > KT_BUDGET_S:
            out["truncated"] = True
            break
        try:
            scan_file(tool, p, key, files.get(key, 0), out)
        except Exception as e:
            out["errors"].append(f"{key}: {e}"[:300])
    out["elapsed_s"] = round(time.time() - t0, 2)
    print(json.dumps(out, ensure_ascii=False))


main()
