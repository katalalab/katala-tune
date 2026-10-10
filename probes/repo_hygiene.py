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
import json, os, re, shutil, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor

KT_VERSION = "2026-10-11.1"   # 調査スクリプトの版（数え方を変えたら上げる）
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
SKIP_DIRS = {"node_modules", "target", "venv", ".venv", "__pycache__", "Library", "AppData", "Applications", "Pictures", "Music", "Movies"}
KEEP_DOT = {".worktrees"}

HOME = os.path.expanduser("~")
T0 = time.time()
DEADLINE = T0 + BUDGET_S
ENV = {**os.environ, "GIT_OPTIONAL_LOCKS": "0", "GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C"}
NOWIN = getattr(subprocess, "CREATE_NO_WINDOW", 0)


def slash(p):
    return p.replace("\\", "/")


def shorten(p):
    p = slash(p)
    h = slash(HOME).rstrip("/")
    if h and (p == h or p.startswith(h + "/")):
        return "~" + p[len(h):]
    return p


def run(cmd, timeout):
    # 戻り値: (終了コード, stdout) / 失敗は (None, "timeout" | "oserror")。stderr は捨てる（URL などの秘密が入りうる）
    t = min(timeout, max(0.5, DEADLINE - time.time() + 5))
    try:
        r = subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=t, env=ENV, creationflags=NOWIN)
        return r.returncode, r.stdout.decode("utf-8", "replace")
    except subprocess.TimeoutExpired:
        return None, "timeout"
    except OSError:
        return None, "oserror"


def git(path, *args):
    return run(["git", "-c", "core.fsmonitor=false", "-C", path, *args], CMD_S)


def ghq_root():
    if GHQ_ROOT_OPT:
        return slash(os.path.expanduser(str(GHQ_ROOT_OPT))).rstrip("/"), "opt"
    if shutil.which("ghq"):
        rc, out = run(["ghq", "root"], 10)
        if rc == 0 and out.strip():
            return slash(out.strip().splitlines()[0].strip()), "ghq"
    if os.environ.get("GHQ_ROOT"):
        return slash(os.environ["GHQ_ROOT"].split(os.pathsep)[0]), "env"
    rc, out = run(["git", "config", "--global", "--get", "ghq.root"], 10)
    if rc == 0 and out.strip():
        return slash(os.path.expanduser(out.strip().splitlines()[0])), "git-config"
    d = slash(os.path.join(HOME, "ghq"))
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
    # porcelain v2 の -z 出力。ファイルの内容は含まれない。変更ファイルの最古の更新時刻だけを測る
    info = {"branch": None, "detached": False, "upstream": False, "ahead": None, "behind": None, "tracked": 0, "untracked": 0, "conflicted": 0, "stash": None}
    paths, oldest = [], None
    toks = out.split("\0")
    i = 0
    while i < len(toks):
        t = toks[i]
        i += 1
        if not t:
            continue
        if t.startswith("# branch.head "):
            h = t[len("# branch.head "):]
            info["detached"] = h == "(detached)"
            info["branch"] = None if info["detached"] else h
        elif t.startswith("# branch.upstream "):
            info["upstream"] = True
        elif t.startswith("# branch.ab "):
            m = re.match(r"# branch\.ab \+(\d+) -(\d+)", t)
            if m:
                info["ahead"], info["behind"] = int(m.group(1)), int(m.group(2))
        elif t.startswith("# stash "):
            m = re.match(r"# stash (\d+)", t)
            info["stash"] = int(m.group(1)) if m else None
        elif t.startswith("# "):
            pass
        elif t[0] == "?":
            info["untracked"] += 1
            paths.append(t[2:])
        elif t[0] == "1":
            info["tracked"] += 1
            paths.append(t.split(" ", 8)[-1])
        elif t[0] == "2":
            info["tracked"] += 1
            paths.append(t.split(" ", 9)[-1])
            i += 1  # 改名の元のパス
        elif t[0] == "u":
            info["tracked"] += 1
            info["conflicted"] += 1
            paths.append(t.split(" ", 10)[-1])
    return info, paths


def scan(path, root):
    d = {"path": shorten(path), "rel": None, "linked": False, "state": "unobserved"}
    if root and (path == root or path.startswith(root + "/")):
        d["rel"] = path[len(root) + 1:]
    dotgit = os.path.join(path, ".git")
    d["linked"] = os.path.isfile(dotgit)
    if time.time() > DEADLINE:
        d["reason"] = "over_limit"
        return d
    rc, out = git(path, "status", "--porcelain=v2", "--branch", "--show-stash", "-z", "--untracked-files=normal")
    if rc != 0:
        d["reason"] = "timeout" if out == "timeout" else "git_failed"
        return d
    now = time.time()
    info, files = parse_status(out, now)
    d.update(info)
    oldest = None
    for f in files[:MAX_DIRTY_STAT]:
        try:
            m = os.lstat(os.path.join(path, f)).st_mtime
        except OSError:
            continue
        oldest = m if oldest is None else min(oldest, m)
    d["dirty_age_s"] = int(now - oldest) if oldest is not None else None
    if d["stash"] is None:
        rc, out = git(path, "stash", "list", "--format=%gd")
        d["stash"] = len(out.splitlines()) if rc == 0 else None
    d["local_only"] = None
    if d["ahead"] is None and not d["detached"]:  # upstream が無い（または消えた）
        rc, out = git(path, "rev-list", "--count", "HEAD", "--not", "--remotes")
        d["local_only"] = int(out.strip()) if rc == 0 and out.strip().isdigit() else None
    rc, out = git(path, "log", "-1", "--format=%ct")
    d["head_age_s"] = int(now - int(out.strip())) if rc == 0 and out.strip().isdigit() else None
    rc, out = git(path, "config", "--get", "remote.origin.url")
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
    walk = {"dirs": 0, "truncated": False}
    root, src = ghq_root()
    targets = []
    if root and os.path.isdir(root):
        res["roots"].append({"path": shorten(root), "source": src})
        targets += [(p, root) for p in find_repos(root, GHQ_DEPTH, walk)]
    for r in EXTRA_ROOTS:
        r = slash(os.path.expanduser(r)).rstrip("/")
        if os.path.isdir(r) and not (root and r == root):
            res["roots"].append({"path": shorten(r), "source": "opt"})
            targets += [(p, root) for p in find_repos(r, EXTRA_DEPTH, walk)]
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
    take, rest = uniq[:MAX_REPOS], uniq[MAX_REPOS:]
    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        res["repos"] = list(pool.map(lambda a: scan(*a), take))
    res["skipped"] = len(rest)
    res["elapsed_s"] = round(time.time() - T0, 1)
    print(json.dumps(res, ensure_ascii=False))


if __name__ == "__main__":
    main()
