# Codex の残り枠（使用率とリセット時刻）を、Codex 自身の app-server（JSON-RPC・標準入出力）から読む。標準ライブラリのみ・macOS / Windows 共通。
# 認証は Codex が持つ。このスクリプトは Cookie・トークン・auth.json を読まない。受け取った応答から、窓の長さ（分）・使用率・リセット時刻だけを出す
# （プラン・メール・クレジットなど、ほかの項目は出さない）。このスクリプト自身は機体に何も書かない
# （Codex が自分の認証を更新するなど、Codex 自身の動きはある）。
#
# 手順: codex app-server を起動 → initialize → initialized → account/rateLimits/read → 標準入力を閉じて終わらせる。
# 起動の直後は窓が空で返ることがあるので、空なら少し待って 1 回だけ読み直す。
#
# 出力（1行の JSON）:
#   { "version": 版, "codex": true/false（codex が見つかったか）, "windows": [{ "mins": 窓の長さ, "used_pct": 使用率, "resets_at": epoch 秒 }],
#     "empty": 窓が 1 つも報告されなかった, "error": 失敗の理由（無ければ null）, "elapsed_s": 秒 }
import json, os, queue, shutil, subprocess, sys, threading, time

KT_VERSION = "2026-10-07.1"
TIMEOUT_S = 25
HOME = os.path.expanduser("~")
WIN = os.name == "nt"
t0 = time.time()


def tilde_text(s):
    s = str(s)
    return s.replace(HOME, "~") if len(HOME) > 1 else s


def find_codex():
    found = shutil.which("codex")
    if found:
        return found
    # ssh の非対話のシェル・Finder から起動したアプリでは PATH が短い。よくある置き場所も見る
    cands = [os.path.join(HOME, ".local", "bin", "codex"), "/opt/homebrew/bin/codex", "/usr/local/bin/codex",
             os.path.join(HOME, ".npm-global", "bin", "codex"), os.path.join(HOME, ".bun", "bin", "codex")]
    if WIN:
        app = os.environ.get("APPDATA") or os.path.join(HOME, "AppData", "Roaming")
        cands = [os.path.join(app, "npm", "codex.cmd"), os.path.join(HOME, ".local", "bin", "codex.exe"), os.path.join(HOME, "scoop", "shims", "codex.exe")] + cands
    for c in cands:
        if os.path.isfile(c) and (WIN or os.access(c, os.X_OK)):
            return c
    return None


def window(w):
    # 窓 1 つから、長さ・使用率・リセット時刻だけを取り出す
    if not isinstance(w, dict):
        return None
    mins, used, reset = w.get("windowDurationMins"), w.get("usedPercent"), w.get("resetsAt")
    if not isinstance(used, (int, float)):
        return None
    return {"mins": mins if isinstance(mins, int) else None, "used_pct": used, "resets_at": reset if isinstance(reset, int) else None}


def windows_of(result):
    rl = (result or {}).get("rateLimits") or {}
    out = []
    for k in ("primary", "secondary"):
        w = window(rl.get(k))
        if w:
            out.append(w)
    return out


def main():
    out = {"version": KT_VERSION, "codex": False, "windows": [], "empty": False, "error": None}
    exe = find_codex()
    if not exe:
        out["elapsed_s"] = round(time.time() - t0, 2)
        print(json.dumps(out))
        return
    out["codex"] = True
    flags = 0x08000000 if WIN else 0  # CREATE_NO_WINDOW
    try:
        p = subprocess.Popen([exe, "app-server"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, creationflags=flags)
    except OSError as e:
        out["error"] = tilde_text(f"codex app-server を起動できない: {e}")[:300]
        out["elapsed_s"] = round(time.time() - t0, 2)
        print(json.dumps(out, ensure_ascii=False))
        return
    q = queue.Queue()

    def reader():
        for line in p.stdout:
            q.put(line)
        q.put(None)

    threading.Thread(target=reader, daemon=True).start()

    def send(msg):
        p.stdin.write((json.dumps(msg) + "\n").encode("utf-8"))
        p.stdin.flush()

    def wait_for(rid):
        # 応答（id が rid）を待つ。通知・サーバーからの要求は読み捨てる（中身は出さない）
        while True:
            left = TIMEOUT_S - (time.time() - t0)
            if left <= 0:
                raise TimeoutError("codex app-server が時間内に答えない")
            try:
                line = q.get(timeout=left)
            except queue.Empty:
                raise TimeoutError("codex app-server が時間内に答えない")
            if line is None:
                raise EOFError("codex app-server が途中で終わった")
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if isinstance(m, dict) and m.get("id") == rid and "method" not in m:
                return m

    try:
        send({"id": 0, "method": "initialize", "params": {"clientInfo": {"name": "katala_tune", "title": "Katala Tune", "version": KT_VERSION}}})
        r = wait_for(0)
        if r.get("error"):
            raise RuntimeError("initialize: " + str((r["error"] or {}).get("message"))[:200])
        send({"method": "initialized"})
        for attempt in (1, 2):
            send({"id": attempt, "method": "account/rateLimits/read"})
            r = wait_for(attempt)
            if r.get("error"):
                raise RuntimeError(str((r["error"] or {}).get("message"))[:200])
            out["windows"] = windows_of(r.get("result"))
            if out["windows"] or attempt == 2:
                break
            time.sleep(2)  # 起動の直後は空で返ることがある
        out["empty"] = not out["windows"]
    except Exception as e:
        out["error"] = tilde_text(str(e))[:300]
    finally:
        try:
            p.stdin.close()  # 標準入力を閉じると app-server は終わる
        except OSError:
            pass
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()
    out["elapsed_s"] = round(time.time() - t0, 2)
    print(json.dumps(out, ensure_ascii=False))


main()
sys.stdout.flush()
