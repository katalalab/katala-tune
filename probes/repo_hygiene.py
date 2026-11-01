# リポジトリの衛生監査（dirty・未push・stash・ghq のパスと origin の不一致）。読み取り専用・標準ライブラリのみ・macOS / Windows 共通。
# 走らせるのは git の読み取りコマンド（status / config --get / rev-list / stash list / log -1）だけ。fetch はしない（ネットワークに出ない）。
# 環境変数 GIT_OPTIONAL_LOCKS=0 で、status がインデックスを書き直さないようにする。ファイルの内容・差分・stderr は読まない・出さない。
# 出すのは、パス（ホームは ~ に置換）・ブランチ名・件数・経過秒・origin（認証部・クエリを除いたもの）・失敗の理由の種類だけ。
# 歩く範囲: ghq root（ghq → 環境変数 GHQ_ROOT → git config ghq.root → ~/ghq の順）と、KT_OPTS["roots"] の追加の置き場（KT_OPTS["ghq_root"] があれば ghq root の代わりにそれを使う）。
# 上限: リポジトリ数（max_repos）・全体の時間（budget_s）・1コマンドの時間（cmd_s）。超えた分は「未観測」として数と理由を残す。
# git が失敗・時間切れのリポジトリは state=unobserved（きれいとは扱わない）。
#
# 出力（1行の JSON）:
#   { probe, version, os, now, roots: [{ path, source }], limits, enumerated（見つけた数）, repos: [...], skipped（上限で見なかった数）,
#     truncated（歩く範囲を打ち切った）, elapsed_s, error? }
#   repos[]: { path, rel（ghq root からの相対。root の外は null）, linked（リンク worktree）, state: ok | unobserved, reason?,
#     branch, detached, upstream（bool）, ahead, behind（upstream が無い・消えたなら null）, local_only（そのときの、どの remote にも無い commit 数）,
#     tracked, untracked, conflicted, dirty_age_s（変更ファイルの最古の更新からの秒。測れなければ null）, head_age_s,
#     stash（数。読めなければ null）, origin, origin_host, origin_path（owner/repo。origin が無い・読めない・パスと見なせなければ null）, has_origin }
import json, os, re, shutil, subprocess, sys, threading, time
from concurrent.futures import ThreadPoolExecutor

KT_VERSION = "2026-11-01.5"   # 調査スクリプトの版（数え方を変えたら上げる）
KT_OPTS = {}                  # 呼び出し側が置き換える（数と文字列だけ）: { roots, ghq_root, max_repos, budget_s, cmd_s }
MAX_REPOS = int(KT_OPTS.get("max_repos", 1000))
BUDGET_S = float(KT_OPTS.get("budget_s", 60))
CMD_S = float(KT_OPTS.get("cmd_s", 12))
EXTRA_ROOTS = [str(r) for r in KT_OPTS.get("roots", [])][:8]
GHQ_ROOT_OPT = KT_OPTS.get("ghq_root")
WORKERS = 4
MAX_WALK_DIRS = 20000
GHQ_DEPTH = 5          # ghq root の下を歩く深さ（host/owner/repo と、GitLab のサブグループ）
EXTRA_DEPTH = 2
MAX_DIRTY_STAT = 200   # 更新時刻を測る変更ファイルの上限
MAX_OUT = 16 * 1024 * 1024  # 1 コマンドの標準出力の上限（超えたら止めて over_limit）
SKIP_DIRS = {"node_modules", "target", "venv", ".venv", "__pycache__", "Library", "AppData", "Applications", "Pictures", "Music", "Movies"}
KEEP_DOT = {".worktrees"}

HOME = os.path.expanduser("~")
T0 = time.time()
DEADLINE = T0 + BUDGET_S
ENV = {**os.environ, "GIT_OPTIONAL_LOCKS": "0", "GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C"}
NOWIN = getattr(subprocess, "CREATE_NO_WINDOW", 0)


def slash(p):
    return p.replace("\\", "/")


def normalize_root(p, pathmod=os.path):
    p = slash(p)
    if p and not p.strip("/"):
        return p if pathmod.sep == "\\" else ("//" if p == "//" else "/")
    drive, tail = pathmod.splitdrive(p)
    if drive and tail and not tail.strip("/"):
        return drive + "/"  # Windows drive root or UNC share root
    return p.rstrip("/")


def relative_to_root(path, root, pathmod=os.path):
    if not root:
        return None
    if path == root:
        return ""
    drive, tail = pathmod.splitdrive(root)
    drive_relative = bool(drive and not tail and not drive.startswith("//"))
    prefix = root if root.endswith("/") or drive_relative else root + "/"
    if not path.startswith(prefix):
        return None
    relative = path[len(prefix):]
    if drive_relative and (relative.startswith("/") or ".." in relative.split("/")):
        return None
    return relative


def same_root(a, b, pathmod=os.path):
    a, b = normalize_root(a, pathmod), normalize_root(b, pathmod)
    if a == b:
        return True
    a_drive, a_tail = pathmod.splitdrive(a)
    b_drive, b_tail = pathmod.splitdrive(b)
    return (a_drive.startswith("//") and b_drive.startswith("//") and not a_tail.strip("/") and not b_tail.strip("/")
            and pathmod.normcase(a_drive) == pathmod.normcase(b_drive))


def shorten(p):
    p = slash(p)
    h = slash(HOME).rstrip("/")
    if h and (p == h or p.startswith(h + "/")):
        return "~" + p[len(h):]
    return p


def run(cmd, timeout):
    # 戻り値: (終了コード, stdout) / 失敗は (None, "timeout" | "over_limit" | "oserror")。stderr は捨てる（URL などの秘密が入りうる）
    # 標準出力は MAX_OUT までしか持たない（巨大な untracked 一覧などでメモリを使い切らない）
    t = min(timeout, max(0.5, DEADLINE - time.time() + 5))
    try:
        p = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=ENV, creationflags=NOWIN)
    except OSError:
        return None, "oserror"
    buf, over, eof = bytearray(), [False], [False]

    def read():
        try:
            while True:
                c = p.stdout.read(65536)
                if not c:
                    eof[0] = True
                    return
                if len(buf) + len(c) > MAX_OUT:
                    over[0] = True
                    p.kill()
                    return
                buf.extend(c)
        except Exception:
            # 内容・パスを含みうる例外は出さず、EOF 未確認として扱う。
            return

    th = threading.Thread(target=read, daemon=True)
    th.start()
    try:
        p.wait(timeout=t)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait()
        th.join(1)
        return None, "timeout"
    # 未終了 reader と buffer は残り、本数の上限はない。全体の時間上限や子孫回収も保証しない。
    th.join(5)
    if over[0]:
        return None, "over_limit"
    if th.is_alive():
        return None, "timeout"
    if not eof[0]:
        return None, "oserror"
    return p.returncode, bytes(buf).decode("utf-8", "replace")


def git(path, *args):
    return run(["git", "-c", "core.fsmonitor=false", "-C", path, *args], CMD_S)


def ghq_root():
    if GHQ_ROOT_OPT:
        return normalize_root(os.path.expanduser(str(GHQ_ROOT_OPT))), "opt"
    if shutil.which("ghq"):
        rc, out = run(["ghq", "root"], 10)
        if rc == 0 and out.strip():
            return normalize_root(out.strip().splitlines()[0].strip()), "ghq"
    if os.environ.get("GHQ_ROOT"):
        return normalize_root(os.environ["GHQ_ROOT"].split(os.pathsep)[0]), "env"
    rc, out = run(["git", "config", "--global", "--get", "ghq.root"], 10)
    if rc == 0 and out.strip():
        return normalize_root(os.path.expanduser(out.strip().splitlines()[0])), "git-config"
    d = normalize_root(os.path.join(HOME, "ghq"))
    return (d, "default") if os.path.isdir(d) else (None, None)


def sanitize(url):
    # 認証部（user:pass@）・クエリ・フラグメントを除く。ホストとパスを取り出す（取り出せなければ host/path は None）
    u = url.strip()
    m = re.match(r"^[A-Za-z][A-Za-z0-9+.-]*://(?:[^/]*@)?([^/:@]+)(?::\d+)?/(.*)$", u)
    if m and not u.lower().startswith("file://"):
        host, rest = m.group(1), m.group(2)
        scheme = u.split("://", 1)[0]
        rest = re.split(r"[?#]", rest, maxsplit=1)[0]
        clean = f"{scheme}://{host}/{rest}"
    else:
        m = re.match(r"^(?:[^/@:]+@)?([^/:@]+):(?!//)(.+)$", u)
        if not m or re.match(r"^[A-Za-z]:[\\/]", u):
            return "(local)", None, None
        if "@" in m.group(2).split("/", 1)[0]:
            return "(unparsed)", None, None  # user:pass@host:path のような形。認証部を出さないために解釈しない
        host, rest = m.group(1), re.split(r"[?#]", m.group(2), maxsplit=1)[0]
        clean = f"{host}:{rest}"
    path = re.sub(r"\.git/?$", "", rest.strip("/"))
    return clean, host.lower(), (path or None)


def is_repo(p):
    return os.path.exists(os.path.join(p, ".git"))


def find_repos(root, depth, state):
    found = []
    stack = [(root, 0)]
    while stack:
        d, lvl = stack.pop()
        if state["dirs"] >= MAX_WALK_DIRS or time.time() > DEADLINE:
            state["truncated"] = True
            break
        state["dirs"] += 1
        try:
            names = sorted(os.listdir(d))
        except OSError:
            state["incomplete"] = True
            continue
        for n in names:
            if n.startswith(".") and n not in KEEP_DOT:
                continue
            q = os.path.join(d, n)
            if not os.path.isdir(q) or os.path.islink(q):
                continue
            if is_repo(q):
                found.append(slash(q))
            elif lvl + 1 < depth and n not in SKIP_DIRS:
                stack.append((q, lvl + 1))
    return found


def parse_status(out, now):
    # porcelain v2 の -z 出力。形式が欠けたら件数を確定しない。
    if not out or not out.endswith("\0"):
        return None
    info = {"branch": None, "detached": False, "upstream": False, "ahead": None, "behind": None, "tracked": 0, "untracked": 0, "conflicted": 0, "stash": None}
    paths = []
    toks = out.split("\0")
    toks.pop()  # 最後の NUL の後にできる空要素
    seen = set()
    unborn = False
    i = 0

    def count(value):
        # JS/Rust の正規化でも確実に扱える範囲。巨大な整数を int に渡さない。
        if len(value) > 16 or not re.fullmatch(r"[0-9]+", value):
            return None
        number = int(value)
        return number if number <= 9007199254740991 else None

    while i < len(toks):
        t = toks[i]
        i += 1
        if not t:
            return None
        if t.startswith("# "):
            key, sep, value = t[2:].partition(" ")
            if key not in ("branch.oid", "branch.head", "branch.upstream", "branch.ab", "stash"):
                continue  # 未知の header は porcelain v2 の拡張として許容する
            if not sep or not value or key in seen:
                return None
            seen.add(key)
            if key == "branch.oid":
                unborn = value == "(initial)"
                if not unborn and not re.fullmatch(r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})", value):
                    return None
            elif key == "branch.head":
                if value == "(unknown)":
                    return None
                info["detached"] = value == "(detached)"
                info["branch"] = None if info["detached"] else value
            elif key == "branch.upstream":
                info["upstream"] = True
            elif key == "branch.ab":
                if value == "+? -?":
                    continue  # Git の quick 比較: 件数は未取得
                match = re.fullmatch(r"\+([0-9]+) -([0-9]+)", value)
                if not match:
                    return None
                ahead, behind = count(match.group(1)), count(match.group(2))
                if ahead is None or behind is None:
                    return None
                info["ahead"], info["behind"] = ahead, behind
            else:
                stash = count(value)
                if stash is None:
                    return None
                info["stash"] = stash
        elif t.startswith("? ") or t.startswith("! "):
            if not t[2:]:
                return None
            if t[0] == "?":
                info["untracked"] += 1
                if len(paths) < MAX_DIRTY_STAT:
                    paths.append(t[2:])
        elif t.startswith(("1 ", "2 ", "u ")):
            kind = t[0]
            limit = {"1": 8, "2": 9, "u": 10}[kind]
            fields = t.split(" ", limit)
            if len(fields) != limit + 1 or any(not field for field in fields[1:]):
                return None
            if kind == "2":
                if i >= len(toks) or not toks[i]:
                    return None
                i += 1  # 改名元はレコード風のパスでも再解釈しない
            info["tracked"] += 1
            if kind == "u":
                info["conflicted"] += 1
            if len(paths) < MAX_DIRTY_STAT:
                paths.append(fields[-1])
        else:
            return None
    if not {"branch.oid", "branch.head"}.issubset(seen) or ("branch.ab" in seen and "branch.upstream" not in seen):
        return None
    return info, paths, unborn


def scan(path, root):
    d = {"path": shorten(path), "rel": None, "linked": False, "state": "unobserved"}
    d["rel"] = relative_to_root(path, root)
    dotgit = os.path.join(path, ".git")
    d["linked"] = os.path.isfile(dotgit)
    if time.time() > DEADLINE:
        d["reason"] = "over_limit"
        return d
    rc, out = git(path, "status", "--porcelain=v2", "--branch", "--show-stash", "-z", "--untracked-files=normal")
    if rc != 0:
        d["reason"] = out if rc is None and out in ("timeout", "over_limit") else "git_failed"
        return d
    now = time.time()
    parsed = parse_status(out, now)
    if parsed is None:
        d["reason"] = "git_failed"
        return d
    info, files, unborn = parsed
    d.update(info)
    oldest = None
    for f in files[:MAX_DIRTY_STAT]:
        try:
            m = os.lstat(os.path.join(path, f)).st_mtime
        except OSError:
            continue
        oldest = m if oldest is None else min(oldest, m)
    d["dirty_age_s"] = int(now - oldest) if oldest is not None else None
    def failed(rc, out):
        # 続きの git が時間切れ・失敗したら、きれいとは数えず未観測にする
        d["state"] = "unobserved"
        d["reason"] = out if rc is None and out in ("timeout", "over_limit") else "git_failed"
        return d

    if d["stash"] is None:
        rc, out = git(path, "stash", "list", "--format=%gd")
        if rc != 0:
            return failed(rc, out)
        d["stash"] = len(out.splitlines())
    d["local_only"] = None
    if d["ahead"] is None and not unborn:  # upstream が無い（または消えた）・detached HEAD
        rc, out = git(path, "rev-list", "--count", "HEAD", "--not", "--remotes")
        if rc != 0 or not out.strip().isdigit():
            return failed(rc, out)
        d["local_only"] = int(out.strip())
    d["head_age_s"] = None
    if not unborn:
        rc, out = git(path, "log", "-1", "--format=%ct")
        if rc != 0:
            return failed(rc, out)
        d["head_age_s"] = int(now - int(out.strip())) if out.strip().isdigit() else None
    rc, out = git(path, "config", "--get", "remote.origin.url")
    if rc is None:
        return failed(rc, out)
    d["origin"], d["origin_host"], d["origin_path"] = None, None, None
    if rc == 0 and out.strip():
        d["origin"], d["origin_host"], d["origin_path"] = sanitize(out.strip().splitlines()[0])
        d["has_origin"] = True
    elif rc == 1:
        d["has_origin"] = False
    else:
        d["has_origin"] = None  # 読めなかった（無いとは言わない）
    d["state"] = "ok"
    return d


def main():
    res = {"probe": "repo_hygiene", "version": KT_VERSION, "os": sys.platform, "now": int(T0), "roots": [], "limits": {"max_repos": MAX_REPOS, "budget_s": BUDGET_S, "cmd_s": CMD_S},
           "enumerated": 0, "repos": [], "skipped": 0, "truncated": False}
    if not shutil.which("git"):
        res.update(error="git_missing", elapsed_s=round(time.time() - T0, 1))
        print(json.dumps(res, ensure_ascii=False))
        return
    walk = {"dirs": 0, "truncated": False, "incomplete": False}
    root, src = ghq_root()
    targets = []
    selected_roots = [root] if root else []
    if root:
        if os.path.isdir(root):
            res["roots"].append({"path": shorten(root), "source": src})
            targets += [(p, root) for p in find_repos(root, GHQ_DEPTH, walk)]
        else:
            walk["incomplete"] = True
    for r in EXTRA_ROOTS:
        r = normalize_root(os.path.expanduser(r))
        if not r:
            continue
        if any(same_root(r, selected) for selected in selected_roots):
            continue
        selected_roots.append(r)
        if os.path.isdir(r):
            res["roots"].append({"path": shorten(r), "source": "opt"})
            targets += [(p, root) for p in find_repos(r, EXTRA_DEPTH, walk)]
        else:
            walk["incomplete"] = True
    seen, uniq = set(), []
    for p, rt in targets:
        k = p.lower() if os.name == "nt" else p
        if k not in seen:
            seen.add(k)
            uniq.append((p, rt))
    # 上限で切るときは、リンク worktree と .worktrees 配下を後に回す（本体のクローンを先に見る）
    uniq.sort(key=lambda a: (os.path.isfile(os.path.join(a[0], ".git")) or "/.worktrees/" in a[0], a[0]))
    res["enumerated"] = len(uniq)
    res["truncated"] = walk["truncated"]
    if walk["incomplete"]:
        res["error"] = "walk_incomplete"
    take, rest = uniq[:MAX_REPOS], uniq[MAX_REPOS:]
    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        res["repos"] = list(pool.map(lambda a: scan(*a), take))
    res["skipped"] = len(rest)
    res["elapsed_s"] = round(time.time() - T0, 1)
    print(json.dumps(res, ensure_ascii=False))


if __name__ == "__main__":
    main()
