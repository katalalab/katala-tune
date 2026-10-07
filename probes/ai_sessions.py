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
#     span: 読んだ区間の指紋（出どころの台帳）。{ start, end（バイト位置）, sha256（区間のバイト列）, lines（行数）, used（数えた行）,
#           skipped（形の違う行）, anchor: [長さ, sha256]（終わりの手前の数 KB。次回の続きの検算に使う） }。本文は出さない
#     rewound: 続きの位置から読めなかったので最初から読み直した理由（shrunk: 短くなった、anchor: 手前のハッシュが合わない）
#     responses（Claude Code だけ）: 応答ごとの [応答 ID の SHA-256 の先頭 16 桁, 時刻（epoch ms）, モデルの番号（resp_models の位置）,
#           入力, 出力, キャッシュ読み, キャッシュ書き, うち 1 時間のキャッシュ書き, 推論]。
#           ファイルをまたぐ重複（サブエージェントのファイルなど）を DB が応答ごとに除くため。応答 ID そのものは出さない
#   cursors: 読んだファイルの続きの位置。gone: KT_STATE にあるが、もう無いファイル
#   bytes_pending: 読む前に残っていた量。truncated: 時間で区切った（続きあり）
#
# KT_VERIFY に [[ファイルの鍵, 開始, 終了], ...] が入っているときは、取り込まずにその区間を読み直して指紋だけを返す（検算）:
#   { "version": 版, "verify": [{ file, start, end, state: ok | gone | short, sha256, lines }] }
import hashlib, json, os, re, time

KT_VERSION = "2026-10-07.1"   # 調査スクリプトの版（出どころの台帳に残る。数え方を変えたら上げる）
KT_STATE = {}       # 呼び出し側が置き換える: {"files": {key: offset}, "anchors": {key: [長さ, sha256]}}
KT_VERIFY = []      # 呼び出し側が置き換える（検算のときだけ）: [[key, start, end], ...]
KT_BUDGET_S = 25
ANCHOR = 4096       # 続きの検算に使う、位置の手前のバイト数

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


def resp_hash(i):
    return hashlib.sha256(("claude:" + str(i)).encode("utf-8")).hexdigest()[:16]


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


def price_model(m, u):
    # 単価が変わる使い方は別のモデルとして数える（単価表に無ければ「単価不明」になる）
    name = m.get("model")
    if not name or name == "<synthetic>":
        return None
    if u.get("speed") == "fast":
        name += "@fast"
    if u.get("inference_geo") == "us":
        name += "@us"
    return name


def claude_line(r, line, usage, tool_names):
    # 数えた行なら True（出どころの台帳の「取り込んだ行数」）
    if not (b'"type":"assistant"' in line or b'"type":"user"' in line or b'"pr-link"' in line or b'"hookErrors"' in line):
        return False
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
            u = m.get("usage") or {}
            # 同じ応答が複数行に分かれるので、ID ごとに最後の値を使う
            usage[m["id"]] = (u, h, ms(d.get("timestamp")), price_model(m, u))
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
    return True


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


def anchor_of(f, pos):
    # 位置の手前 ANCHOR バイトの指紋（次回、続きから読む前に同じかを確かめる）
    n = min(ANCHOR, pos)
    f.seek(pos - n)
    return [n, sha_bytes(f.read(n))]


def scan_file(tool, path, key, off, anchor, out):
    size = os.path.getsize(path)
    rewound = None
    with open(path, "rb") as f:
        if size < off:
            rewound = "shrunk"
        elif off > 0 and isinstance(anchor, list) and len(anchor) == 2:
            n = int(anchor[0])
            if n > off or n < 0:
                rewound = "anchor"
            else:
                f.seek(off - n)
                if sha_bytes(f.read(n)) != anchor[1]:
                    rewound = "anchor"  # 書き換え・ローテーション。最初から読み直す
        if rewound:
            off = 0
        r = new_rec(tool, key)
        r["mode"] = "replace" if off == 0 else "add"
        r["rewound"] = rewound
        if tool == "claude":
            rel = os.path.relpath(path, ROOTS["claude"]).split(os.sep)
            if "subagents" in rel:
                r["parent_id"] = rel[rel.index("subagents") - 1]
        usage, tool_names = {}, {}
        pos, lines, used, skipped = off, 0, 0, 0
        h = hashlib.sha256()
        f.seek(off)
        for line in f:
            if not line.endswith(b"\n"):
                break  # 書きかけの行は次回
            pos += len(line)
            lines += 1
            h.update(line)
            try:
                if claude_line(r, line, usage, tool_names) if tool == "claude" else codex_line(r, line):
                    used += 1
            except BAD_LINE:
                skipped += 1  # 形の違う行（古い版の書式など）は飛ばす。1行のせいでファイル全体を読めなくしない
            if time.time() - t0 > KT_BUDGET_S:
                out["truncated"] = True
                break
        r["span"] = {"start": off, "end": pos, "sha256": h.hexdigest(), "lines": lines, "used": used, "skipped": skipped,
                     "anchor": anchor_of(f, pos)}
    models, resp = [], []
    for i, (u, hr, ts, model) in usage.items():
        cw = u.get("cache_creation_input_tokens") or 0
        cw1h = min(cw, ((u.get("cache_creation") or {}).get("ephemeral_1h_input_tokens") or 0))
        add = (u.get("input_tokens") or 0, u.get("output_tokens") or 0, u.get("cache_read_input_tokens") or 0,
               cw, ((u.get("output_tokens_details") or {}).get("thinking_tokens") or 0))
        for j, k in enumerate(TOK):
            r["tokens"][k] += add[j]
            bump(r, hr, j, add[j])
        if model not in models:
            models.append(model)
        resp.append([resp_hash(i), ts, models.index(model), add[0], add[1], add[2], add[3], cw1h, add[4]])
    if tool == "claude":
        r["responses"] = resp
        r["resp_models"] = models
    r["assistant_msgs"] = len(usage)
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
        elif os.path.getsize(p) < end or start < 0 or end < start:
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
    print(json.dumps({"version": KT_VERSION, "verify": res}, ensure_ascii=False))


def main():
    if KT_VERIFY:
        verify(KT_VERIFY)
        return
    files = (KT_STATE or {}).get("files") or {}
    anchors = (KT_STATE or {}).get("anchors") or {}
    out = {"version": KT_VERSION, "sessions": [], "cursors": {}, "gone": [], "files_total": 0, "files_changed": 0, "bytes_pending": 0, "bytes_read": 0,
           "truncated": False, "errors": []}
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
            scan_file(tool, p, key, files.get(key, 0), anchors.get(key), out)
        except Exception as e:
            out["errors"].append(tilde_text(f"{key}: {e}")[:300])
    out["elapsed_s"] = round(time.time() - t0, 2)
    print(json.dumps(out, ensure_ascii=False))


main()
