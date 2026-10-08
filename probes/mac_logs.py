#!/usr/bin/env python3
"""katala-tune の macOS ログ収集。読み取り専用。前回の続きから読み、JSON 1行で出す。

使い方: python3 - <diag_cursor_ms> <kernel_cursor_epoch> [<auth_cursor_epoch>] [nonet]
  diag    ~/Library/Logs/DiagnosticReports（と読めれば /Library/...）のクラッシュ・ハング・パニック・資源超過。
          ファイル名と先頭の見出し（app_name, bug_type）だけを読み、中身（スタック等）は送らない。
  kernel  統合ログのうちカーネルのエラー（Sandbox の deny を除く）。fault は1時間に数千件出るので取らない。
  auth    統合ログのうち sshd のログインの失敗。1回の接続（送り元のアドレスとポート）を1件にまとめ、送り元を provider に入れる。
          読むのは日時・アカウント名・送り元・認証の方式だけ。範囲は最大7日（初回は24時間）、件数は300件まで。
"""
import json, os, re, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor

MAX_ROWS = 300
DIAG_DIRS = [os.path.expanduser("~/Library/Logs/DiagnosticReports"), "/Library/Logs/DiagnosticReports"]
SKIP = re.compile(r"^SFA-")  # セキュリティ基盤の統計。障害ではない
BUG_TYPES = {"309": ("crash", "error"), "109": ("crash", "error"), "210": ("kernel panic", "critical"),
             "288": ("stackshot", "warn"), "298": ("jetsam (memory)", "warn"), "409": ("crash", "error"),
             "211": ("analytics", "info"), "313": ("hang", "warn")}
KINDS = [(".panic", "kernel panic", "critical"), (".crash", "crash", "error"), (".hang", "hang", "warn"),
         (".spin", "spin", "warn"), ("cpu_resource", "CPU resource limit", "info"), ("diskwrites_resource", "disk writes limit", "info"),
         ("wakeups_resource", "wakeups limit", "info"), ("ExcUserFault", "crash", "error"), (".diag", "diagnostic", "info")]


def diag(cursor_ms):
    rows, newest = [], cursor_ms
    floor = cursor_ms or (time.time() - 7 * 86400) * 1000  # 初回は7日前から
    for d in DIAG_DIRS:
        try:
            names = os.listdir(d)
        except OSError:
            continue
        for n in names:
            p = os.path.join(d, n)
            if SKIP.match(n) or not os.path.isfile(p):
                continue
            mt = os.path.getmtime(p) * 1000
            if mt < floor - 1000:
                continue
            newest = max(newest, mt)
            proc = re.split(r"[-_]\d{4}-\d{2}-\d{2}", n)[0]
            kind, level = "report", "info"
            for suffix, k, lv in KINDS:
                if suffix in n:
                    kind, level = k, lv
                    break
            if n.endswith(".ips"):
                try:
                    with open(p, "r", errors="replace") as f:
                        head = json.loads(f.readline())
                    proc = head.get("app_name") or head.get("name") or proc
                    kind, level = BUG_TYPES.get(str(head.get("bug_type")), ("report", "info"))
                except Exception:
                    pass
            rows.append({"uid": n, "ts": int(mt), "level": level, "provider": proc, "event_id": kind,
                         "message": f"{proc}: {kind}"})
    rows.sort(key=lambda r: r["ts"])
    dropped = max(0, len(rows) - MAX_ROWS)
    return {"cursor": str(int(newest)), "rows": rows[-MAX_ROWS:], "dropped": dropped}


def kernel(cursor_epoch):
    start = cursor_epoch or time.time() - 2 * 3600  # 初回は2時間（24時間だと M2 で25秒を超えた）
    since = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(start))
    try:
        p = subprocess.run(["log", "show", "--style", "ndjson", "--start", since, "--predicate",
                            # Sandbox の deny は作業中のツールが日常的に出すノイズなので除く
                            'messageType == error AND (process == "kernel" OR subsystem BEGINSWITH "com.apple.kernel") AND NOT (sender == "Sandbox")'],
                           capture_output=True, text=True, timeout=40)
        if p.returncode != 0:
            return {"error": f"log show exit {p.returncode}"}
        out = p.stdout
    except Exception as e:  # noqa: BLE001
        return {"error": str(e)}
    rows, newest = [], start
    for line in out.splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        ts = e.get("timestamp", "")
        try:
            t = time.mktime(time.strptime(ts[:19], "%Y-%m-%d %H:%M:%S")) + float("0" + ts[19:26])
        except ValueError:
            continue
        newest = max(newest, t)
        rows.append({"uid": f"{e.get('machTimestamp', '')}-{e.get('threadID', '')}", "ts": int(t * 1000), "level": "error",
                     "provider": e.get("senderImagePath", "").rsplit("/", 1)[-1] or e.get("processImagePath", "kernel").rsplit("/", 1)[-1],
                     "event_id": e.get("subsystem") or None, "message": (e.get("eventMessage") or "")[:1000]})
    dropped = max(0, len(rows) - MAX_ROWS)
    return {"cursor": str(int(newest) + 1), "rows": rows[-MAX_ROWS:], "dropped": dropped}


# OpenSSH の失敗の書き方（sshd・sshd-session・sshd-auth）。どれも送り元のアドレスとポートが入る
SSH_FAIL = [
    re.compile(r"^Failed (?P<m>\S+) for (?:invalid user )?(?P<u>.*?) from (?P<ip>\S+) port (?P<port>\d+)"),
    re.compile(r"^Invalid user (?P<u>.*?) from (?P<ip>\S+) port (?P<port>\d+)"),
    re.compile(r"^(?:Connection closed by|Disconnected from) (?:authenticating|invalid) user (?P<u>.*?) (?P<ip>\S+) port (?P<port>\d+) \[preauth\]"),
    re.compile(r"^maximum authentication attempts exceeded for (?:invalid user )?(?P<u>.*?) from (?P<ip>\S+) port (?P<port>\d+)"),
]


def parse_auth(text):
    """log show --style ndjson の行から、ssh のログインの失敗を接続（送り元のアドレスとポート）ごとの行にする。戻り値は (rows, 最新の時刻)"""
    attempts, newest = {}, 0.0
    for line in text.splitlines():
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if not isinstance(e, dict):
            continue
        ts = e.get("timestamp", "")
        try:
            t = time.mktime(time.strptime(ts[:19], "%Y-%m-%d %H:%M:%S")) + float("0" + ts[19:26])
        except (ValueError, TypeError):
            continue
        msg = e.get("eventMessage") or ""
        m = next((x for x in (rx.match(msg) for rx in SSH_FAIL) if x), None)
        if not m:
            continue
        newest = max(newest, t)
        k = (m.group("ip"), m.group("port"))
        a = attempts.setdefault(k, {"ts": t, "user": (m.group("u") or "")[:64], "method": None})
        if m.groupdict().get("m"):
            a["method"] = m.group("m")[:40]
    rows = []
    for (ip, port), a in attempts.items():
        # 同じ接続の行が2回の読み取りにまたがっても1件になるよう、uid は送り元・ポート・日付
        rows.append({"uid": f"ssh:{ip}:{port}:{int(a['ts'] // 86400)}", "ts": int(a["ts"] * 1000), "level": "warn", "provider": ip[:120],
                     "event_id": "ssh-fail", "message": f"ssh login failed: account {a['user'] or '?'}, from {ip}" + (f", {a['method']}" if a["method"] else "")})
    rows.sort(key=lambda r: r["ts"])
    return rows, newest


def auth(cursor_epoch):
    started = time.time()
    # 初回は24時間。長く止まっていても7日より前は読まない（log show が重くなるため）
    start = max(cursor_epoch or started - 24 * 3600, started - 7 * 86400)
    since = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(start))
    try:
        p = subprocess.run(["log", "show", "--style", "ndjson", "--start", since, "--predicate",
                            'process BEGINSWITH "sshd" AND (eventMessage BEGINSWITH "Failed " OR eventMessage BEGINSWITH "Invalid user "'
                            ' OR eventMessage ENDSWITH "[preauth]" OR eventMessage BEGINSWITH "maximum authentication")'],
                           capture_output=True, text=True, timeout=30)
        if p.returncode != 0:
            return {"error": f"log show exit {p.returncode}"}
        out = p.stdout
    except Exception as e:  # noqa: BLE001
        return {"error": str(e)}
    rows, newest = parse_auth(out)
    dropped = max(0, len(rows) - MAX_ROWS)
    # 何も無かったときも続きの位置を進める（少し重ねて読み、uid で重複を除く）
    cursor = max(int(max(newest, start)) + 1, int(started) - 60)
    return {"cursor": str(cursor), "rows": rows[-MAX_ROWS:], "dropped": dropped}


def main():
    # nonet: 台帳の "network": false。ログインの記録（送り元のアドレス）は読まない
    network = "nonet" not in sys.argv[1:]
    a = [x for x in sys.argv[1:] if x != "nonet"] + ["0", "0", "0"]
    with ThreadPoolExecutor(max_workers=3) as ex:
        fd = ex.submit(diag, float(a[0] or 0))
        fk = ex.submit(kernel, float(a[1] or 0))
        fa = ex.submit(auth, float(a[2] or 0)) if network else None
        res = {"probe": "mac_logs", "sources": {"mac_diag": fd.result(), "mac_kernel": fk.result()}}
        if fa:
            res["sources"]["mac_auth"] = fa.result()
    json.dump(res, sys.stdout, ensure_ascii=False)
    print()


if __name__ == "__main__":
    main()
