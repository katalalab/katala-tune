# AI エージェント（Claude Code・Codex）のセッションの要約。読み取り専用・標準ライブラリのみ・macOS / Windows 共通。
# 行は読むが、会話の本文・ツールの入出力は取り出さない・出さない。出すのは数・名前・時刻・モデル・トークン・PR の URL と重複排除用 ID。
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
#     span: 読んだ区間の指紋（出どころの台帳）。{ start, end（バイト位置）, sha256（区間のバイト列）, lines（行数）, used（数えた行。Claude Code は
#           読めた行、Codex は対象の種類の行）, skipped（読めなかった行・上限を超えた行）, anchor: [長さ, sha256]（終わりの手前の数 KB。次回の続きの検算に使う） }。本文は出さない
#     rewound: 続きの位置から読めなかったので最初から読み直した理由（shrunk: 短くなった、anchor: 手前のハッシュが合わない）
#   claude_usage: Claude Code の応答ごとの量。{ file, id（応答 ID）, usage: [入力, 出力, キャッシュ読み, キャッシュ書き, 推論], cw1h（キャッシュ書きのうち 1 時間の分）,
#           model（単価を引く名前。高速モード・US は @fast・@us を付ける）, hour }。ファイルをまたぐ重複は DB が応答 ID ごとに除く
#   cursors: 読んだファイルの続きの位置。gone: KT_STATE にあるが、もう無いファイル
#   bytes_pending: 読む前に残っていた量。truncated: 時間で区切った（続きあり）
#
# KT_VERIFY に [[ファイルの鍵, 開始, 終了], ...] が入っているときは、取り込まずにその区間を読み直して指紋だけを返す（検算）:
#   { "version": 版, "verify": [{ file, start, end, state: ok | gone | short, sha256, lines }] }
import hashlib, json, os, re, time

KT_VERSION = "2026-10-09.1"   # 調査スクリプトの版（出どころの台帳に残る。数え方を変えたら上げる）
KT_STATE = {}       # 呼び出し側が置き換える: {"files": {key: offset}, "anchors": {key: [長さ, sha256]}, "claude_replay": [key]}
KT_VERIFY = []      # 呼び出し側が置き換える（検算のときだけ）: [[key, start, end], ...]
KT_BUDGET_S = 25
ANCHOR = 4096       # 続きの検算に使う、位置の手前のバイト数
MAX_LINE_BYTES = 1024 * 1024
DRAIN_BYTES = 64 * 1024
MAX_EVENTS = 4096
MAX_METADATA_BYTES = 512 * 1024

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


def key_path(key):
    # file_key の逆（検算で元のファイルを開くため）。鍵の外へ出るもの（..）は扱わない
    tool, _, rel = key.partition(":")
    if tool not in ROOTS or not rel or ".." in rel.split("/"):
        return None
    if tool == "claude" and rel.startswith("~"):
        rel = HOME_ENC + rel[1:]
    return os.path.join(ROOTS[tool], *rel.split("/"))


def sha_bytes(b):
    return hashlib.sha256(b).hexdigest()


def price_model(m, u):
    # 単価が変わる使い方は別のモデルとして数える（単価表に無ければ「単価不明」になる）
    name = m.get("model")
    if not valid_metadata_text(name, 128) or name == "<synthetic>":
        return None
    if u.get("speed") == "fast":
        name += "@fast"
    if u.get("inference_geo") == "us":
        name += "@us"
    return name if len(name.encode("utf-8")) <= 128 else None


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


def usage_values(usage):
    # TOK の 5 つ＋キャッシュ書きのうち 1 時間の分（cw1h）
    return [usage.get("input_tokens") or 0, usage.get("output_tokens") or 0,
            usage.get("cache_read_input_tokens") or 0, usage.get("cache_creation_input_tokens") or 0,
            ((usage.get("output_tokens_details") or {}).get("thinking_tokens") or 0),
            ((usage.get("cache_creation") or {}).get("ephemeral_1h_input_tokens") or 0)]


def valid_metadata_text(v, limit):
    return isinstance(v, str) and 0 < len(v.encode("utf-8")) <= limit


def claude_line(r, d, events):
    invalid = 0
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
        mid = m.get("id")
        if valid_metadata_text(mid, 256):
            u = m.get("usage") or {}
            values = usage_values(u)
            invalid += int(any(not isinstance(v, int) or isinstance(v, bool) or not 0 <= v <= 9223372036854775807 for v in values))
            current = [(v if isinstance(v, int) and not isinstance(v, bool) and 0 <= v <= 9223372036854775807 else 0) for v in values]
            old = events["usage"].get(mid)
            model = price_model(m, u) or (old[2] if old else None)
            events["usage"][mid] = ([max(current[i], old[0][i]) for i in range(len(current))] if old else current, old[1] if old else h, model)
        else:
            invalid += 1
        for b in m.get("content") or []:
            if isinstance(b, dict) and b.get("type") == "tool_use":
                tid, name = b.get("id"), b.get("name") or "?"
                if valid_metadata_text(tid, 256) and valid_metadata_text(name, 128):
                    events["tools"].setdefault(tid, (name, h))
                else:
                    invalid += 1
    elif t == "user":
        c = (d.get("message") or {}).get("content")
        if isinstance(c, str):
            if not d.get("isMeta"):
                r["prompts"] += 1
                bump(r, h, 6)
        elif isinstance(c, list):
            for b in c:
                if isinstance(b, dict) and b.get("type") == "tool_result" and b.get("is_error"):
                    tid = b.get("tool_use_id")
                    if valid_metadata_text(tid, 256):
                        events["errors"].setdefault(tid, h)
                    else:
                        invalid += 1
    elif t == "pr-link":
        if d.get("prUrl") and d["prUrl"] not in r["prs"]:
            r["prs"].append(d["prUrl"])
    elif d.get("hookErrors"):
        r["hook_errors"] += len(d["hookErrors"])
    return invalid

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
        return True
    if not (b'"token_count"' in line or b'"session_meta"' in line or b'"turn_context"' in line or b'"task_started"' in line or b'"task_complete"' in line):
        return False
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
    return True


def discard_large_line(f, first, digest):
    """Complete an over-limit line without retaining its body. (None, digest) means retry from its start.
    On success returns (size, a copy of digest that also covers the line); the digest passed in is left untouched."""
    size, tail = len(first), first
    d = digest.copy()
    d.update(first)
    while not tail.endswith(b"\n"):
        if time.time() - t0 > KT_BUDGET_S:
            return None, digest
        tail = f.readline(DRAIN_BYTES)
        if not tail:
            return None, digest
        size += len(tail)
        d.update(tail)
    return (size, d) if time.time() - t0 <= KT_BUDGET_S else (None, digest)


def anchor_of(f, pos):
    # 位置の手前 ANCHOR バイトの指紋（次回、続きから読む前に同じかを確かめる）
    n = min(ANCHOR, pos)
    f.seek(pos - n)
    return [n, sha_bytes(f.read(n))]


def event_size(file, kind, ident, value):
    if kind == "usage":
        item = {"file": file, "id": ident, "usage": value[0][:len(TOK)], "cw1h": value[0][len(TOK)], "model": value[2], "hour": value[1]}
    elif kind == "tools":
        item = {"file": file, "id": ident, "name": value[0], "hour": value[1]}
    else:
        item = {"file": file, "id": ident, "hour": value}
    # JSONの要素区切りも含める（最後の要素については1 byte安全側）。
    return len(json.dumps(item, ensure_ascii=False, separators=(",", ":")).encode("utf-8")) + 1


METADATA_OVERHEAD = len(json.dumps({"claude_usage": [], "claude_tools": [], "claude_errors": []}, separators=(",", ":")).encode("utf-8"))


def scan_file(tool, path, key, off, anchor, out, migrate_files):
    size = os.path.getsize(path)
    rewound = None
    if size < off:
        rewound = "shrunk"
    elif off > 0 and isinstance(anchor, list) and len(anchor) == 2:
        # 続きから読む前に、位置の手前が前回と同じかを確かめる（書き換え・ローテーション）
        n = anchor[0]
        if not isinstance(n, int) or isinstance(n, bool) or not 0 <= n <= off:
            rewound = "anchor"
        else:
            with open(path, "rb") as f:
                f.seek(off - n)
                if sha_bytes(f.read(n)) != anchor[1]:
                    rewound = "anchor"
    # 旧cursorはバイト位置だけでClaude応答IDを持たない。次の差分pollで同じ応答を足さないよう、一度だけ全量をreplaceする。
    migrate = tool == "claude" and off > 0 and key in migrate_files
    if rewound or migrate:
        off = 0
    r = new_rec(tool, key)
    r["mode"] = "replace" if off == 0 else "add"
    r["rewound"] = rewound
    if tool == "claude":
        rel = os.path.relpath(path, ROOTS["claude"]).split(os.sep)
        if "subagents" in rel:
            r["parent_id"] = rel[rel.index("subagents") - 1]
    events = {"usage": {}, "tools": {}, "errors": {}}
    pos = off
    skipped_large = 0
    invalid_metadata = 0
    lines, used, skipped = 0, 0, 0
    digest = hashlib.sha256()
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
                consumed, digest_next = discard_large_line(f, line, digest)
                if consumed is None:
                    out["truncated"] = True
                    break
                pos += consumed
                digest = digest_next
                lines += 1
                skipped += 1
                skipped_large += 1
                continue
            if not line.endswith(b"\n"):
                out["truncated"] = True
                break  # 書きかけの行は次回
            start = pos
            pos += len(line)
            try:
                if tool == "claude":
                    # その行だけを先に評価する。過去全イベントのコピー・再シリアライズをしない。
                    d = json.loads(line)
                    pending = {"usage": {}, "tools": {}, "errors": {}}
                    line_invalid = claude_line(new_rec(tool, key), d, pending)
                    changes, delta_count, delta_bytes = [], 0, 0
                    standalone_count, standalone_bytes = 0, 0
                    for kind, entries in pending.items():
                        for ident, value in entries.items():
                            standalone_count += 1
                            standalone_bytes += event_size(key, kind, ident, value)
                            old = events[kind].get(ident)
                            if old is not None:
                                if kind == "usage":
                                    value = ([max(value[0][i], old[0][i]) for i in range(len(value[0]))], old[1], value[2] or old[2])
                                else:
                                    value = old
                            if ident not in events[kind] or old != value:
                                delta_count += int(ident not in events[kind])
                                delta_bytes += event_size(key, kind, ident, value) - (event_size(key, kind, ident, old) if ident in events[kind] else 0)
                                changes.append((kind, ident, value))
                    used_count, used_bytes = out["_event_count"], out["_event_bytes"]
                    if used_count + delta_count > MAX_EVENTS or used_bytes + delta_bytes > MAX_METADATA_BYTES:
                        if standalone_count > MAX_EVENTS or standalone_bytes + METADATA_OVERHEAD > MAX_METADATA_BYTES:
                            out["errors"].append(f"{key}: metadata event budget exceeded; metadata unavailable")
                            # この行自身が入らない時だけ欠測にする。通常の行は次のpollへ。
                            invalid_metadata += line_invalid
                            claude_line(r, d, {"usage": {}, "tools": {}, "errors": {}})
                        else:
                            pos = start
                            out["truncated"] = True
                            out["_metadata_full"] = True
                            break
                    else:
                        invalid_metadata += line_invalid
                        claude_line(r, d, {"usage": {}, "tools": {}, "errors": {}})
                        for kind, ident, value in changes:
                            events[kind][ident] = value
                        out["_event_count"] += delta_count
                        out["_event_bytes"] += delta_bytes
                else:
                    relevant = codex_line(r, line)
                used += 1 if tool == "claude" else int(bool(relevant))
            except BAD_LINE:
                skipped += 1
            lines += 1
            digest.update(line)
            if time.time() - t0 > KT_BUDGET_S:
                out["truncated"] = True
                break
        r["span"] = {"start": off, "end": pos, "sha256": digest.hexdigest(), "lines": lines, "used": used, "skipped": skipped, "anchor": anchor_of(f, pos)}
    if invalid_metadata:
        out["errors"].append(f"{key}: {invalid_metadata} invalid metadata field(s); metadata unavailable")
    if skipped_large:
        out["errors"].append(f"{key}: {skipped_large} line(s) exceeds {MAX_LINE_BYTES} bytes; metadata unavailable")
    if tool == "claude":
        for mid, (usage, h, model) in events["usage"].items():
            out["claude_usage"].append({"file": key, "id": mid, "usage": usage[:len(TOK)], "cw1h": usage[len(TOK)], "hour": h, "model": model})
        for tid, (name, h) in events["tools"].items():
            out["claude_tools"].append({"file": key, "id": tid, "name": name, "hour": h})
        for tid, h in events["errors"].items():
            out["claude_errors"].append({"file": key, "id": tid, "hour": h})
    out["bytes_read"] += pos - off
    out["cursors"][key] = pos
    if pos > off or rewound:
        out["sessions"].append(r)


def verify(items):
    # 検算: 区間を読み直して指紋を返す（本文は出さない）
    res = []
    for it in items[:50]:
        try:
            key, start, end = str(it[0]), int(it[1]), int(it[2])
        except BAD_LINE:
            continue
        o = {"file": key, "start": start, "end": end}
        p = key_path(key)
        if p is None or not os.path.isfile(p):
            o["state"] = "gone"
        elif start < 0 or end < start or os.path.getsize(p) < end:
            o["state"] = "short"
        else:
            h, n, left = hashlib.sha256(), 0, end - start
            with open(p, "rb") as f:
                f.seek(start)
                while left > 0:
                    b = f.read(min(left, 1 << 20))
                    if not b:
                        break
                    h.update(b)
                    n += b.count(b"\n")
                    left -= len(b)
            o.update({"state": "ok" if left == 0 else "short", "sha256": h.hexdigest(), "lines": n})
        res.append(o)
    print(json.dumps({"version": KT_VERSION, "verify": res}, ensure_ascii=False, separators=(",", ":")))


def main():
    if KT_VERIFY:
        verify(KT_VERIFY)
        return
    state = KT_STATE if isinstance(KT_STATE, dict) else {}
    files = state.get("files") if isinstance(state.get("files"), dict) else state
    files = {k: v for k, v in files.items() if isinstance(k, str) and isinstance(v, int) and not isinstance(v, bool) and v >= 0}
    migrate_files = set(state.get("claude_replay") or []) if isinstance(state.get("claude_replay"), list) else set()
    anchors = state.get("anchors") if isinstance(state.get("anchors"), dict) else {}
    out = {"version": KT_VERSION, "sessions": [], "cursors": {}, "gone": [], "claude_usage": [], "claude_tools": [], "claude_errors": [], "files_total": 0, "files_changed": 0, "bytes_pending": 0, "bytes_read": 0, "truncated": False, "errors": [], "_event_count": 0, "_event_bytes": METADATA_OVERHEAD, "_metadata_full": False}
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
                if st.st_size != off or (tool == "claude" and key in migrate_files):
                    todo.append((st.st_mtime, tool, p, key))
                    out["bytes_pending"] += st.st_size - off if st.st_size > off else st.st_size
    out["gone"] = sorted(k for k in files if k not in seen)

    out["files_changed"] = len(todo)
    for _m, tool, p, key in sorted(todo, reverse=True):  # 新しいものから
        if time.time() - t0 > KT_BUDGET_S or out["_metadata_full"]:
            out["truncated"] = True
            break
        try:
            scan_file(tool, p, key, files.get(key, 0), anchors.get(key), out, migrate_files)
        except Exception as e:
            out["errors"].append(tilde_text(f"{key}: {e}")[:300])
    out["elapsed_s"] = round(time.time() - t0, 2)
    out.pop("_event_count", None); out.pop("_event_bytes", None); out.pop("_metadata_full", None)
    print(json.dumps(out, ensure_ascii=False, separators=(",", ":")))


main()
