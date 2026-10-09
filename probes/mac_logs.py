#!/usr/bin/env python3
"""katala-tune の macOS ログ収集。読み取り専用。前回の続きから読み、JSON 1行で出す。

使い方: python3 - <diag_cursor_ms> <kernel_cursor_epoch> [<auth_cursor_epoch>] [nonet]
  diag    ~/Library/Logs/DiagnosticReports（と読めれば /Library/...）のクラッシュ・ハング・パニック・資源超過。
          ファイル名と先頭の見出し（app_name, bug_type）だけを読み、中身（スタック等）は送らない。
  kernel  統合ログのうちカーネルのエラー（Sandbox の deny を除く）。fault は1時間に数千件出るので取らない。
          同じ形（数字・16進・パス・引用符を伏せた鍵）の繰り返しは、代表1行と count（実際の件数）にまとめてから、300 行の上限を掛ける。
  auth    統合ログのうち sshd の Failed 認証試行。1試行を1件にし、送り元を provider に入れる。
          読むのは日時・アカウント名・送り元・認証の方式だけ。範囲は最大7日（初回は24時間）、件数は300件まで。
"""
import hashlib, json, os, re, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor

MAX_ROWS = 300
MAX_KEY = 400  # 同種の鍵に使う本文の長さ（lib/logs.js の fingerprint と同じ）
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


# 同種のログをまとめる鍵。lib/logs.js の fingerprint と同じ伏せ方（GUID・16進・パス・引用符の中身・数字）
_NORM = [(re.compile(r"\{?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\}?"), "<guid>"),
         (re.compile(r"0x[0-9a-f]+"), "<hex>"),
         (re.compile(r"[a-z]:\\[^\s\"']+|/(?:users|home|private|var|tmp|applications|library|system)/[^\s\"']*"), "<path>"),
         (re.compile(r"\"[^\"]*\"|'[^']*'"), "<q>"),
         (re.compile(r"[0-9]+(?:\.[0-9]+)?"), "<n>"),
         (re.compile(r"\s+"), " ")]


def norm_key(provider, event_id, message):
    s = str(message).lower()
    for rx, rep in _NORM:
        s = rx.sub(rep, s)
    return (provider or "", event_id or "", s.strip()[:MAX_KEY])


def aggregate(rows, limit=MAX_ROWS):
    """同じ鍵の行を、代表1行（最初の行の uid・本文）と count にまとめる。戻り値は (rows, dropped, events)。
    ts は最後に出た時刻。行は新しい順に limit 行だけ残し、切った行が持っていた件数を dropped に数える（実際の件数）。
    本文は代表1行のものだけを残す（繰り返しの本文を増やさない）"""
    groups = {}
    for r in rows:
        k = norm_key(r.get("provider"), r.get("event_id"), r.get("message"))
        g = groups.get(k)
        if g is None:
            groups[k] = dict(r, count=1)
        else:
            g["count"] += 1
            g["ts"] = max(g["ts"], r["ts"])
    out = sorted(groups.values(), key=lambda g: g["ts"])
    keep = out[-limit:] if limit else []
    cut = out[:len(out) - len(keep)]
    return keep, sum(g["count"] for g in cut), len(rows)


def parse_kernel(text, start):
    """log show --style ndjson の行から、カーネルのエラー1件ごとの行を作る。戻り値は (rows, 最新の時刻)"""
    rows, newest = [], start
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
        newest = max(newest, t)
        rows.append({"uid": f"{e.get('machTimestamp', '')}-{e.get('threadID', '')}", "ts": int(t * 1000), "level": "error",
                     "provider": e.get("senderImagePath", "").rsplit("/", 1)[-1] or e.get("processImagePath", "kernel").rsplit("/", 1)[-1],
                     "event_id": e.get("subsystem") or None, "message": (e.get("eventMessage") or "")[:1000]})
    return rows, newest


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
    rows, newest = parse_kernel(out, start)
    # 繰り返しを集約してから上限を掛ける（洪水の1種類で300行が埋まり、ほかの異常が捨てられるのを防ぐ）
    kept, dropped, events = aggregate(rows)
    return {"cursor": str(int(newest) + 1), "rows": kept, "dropped": dropped, "events": events}


# OpenSSH の Failed 認証試行（sshd・sshd-session・sshd-auth）。Invalid user・preauth close・maximum は同じ試行の補助行なので数えない。
SSH_FAIL = re.compile(r"^Failed (?P<m>\S+) for (?:invalid user )?(?P<u>.*?) from (?P<ip>\S+) port (?P<port>\d+)")


def parse_auth(text):
    """log show --style ndjson の行から、ssh の Failed 認証試行ごとの行を作る。戻り値は (rows, 最新の時刻)"""
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
        m = SSH_FAIL.match(msg)
        if not m:
            continue
        newest = max(newest, t)
        ip, port = m.group("ip"), m.group("port")
        stamp = int(t * 1_000_000)
        digest = hashlib.sha256(msg.encode("utf-8", "replace")).hexdigest()[:12]
        uid = f"ssh:{ip}:{port}:{stamp}:{digest}"
        attempts.setdefault(uid, {"uid": uid, "ts": int(t * 1000), "level": "warn", "provider": ip[:120], "event_id": "ssh-fail",
                                  "message": f"ssh login failed: account {(m.group('u') or '?')[:64]}, from {ip}, {m.group('m')[:40]}"})
    rows = list(attempts.values())
    rows.sort(key=lambda r: r["ts"])
    return rows, newest


def auth(cursor_epoch):
    started = time.time()
    # 初回は24時間。長く止まっていても7日より前は読まない（log show が重くなるため）
    start = max(cursor_epoch or started - 24 * 3600, started - 7 * 86400)
    since = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(start))
    try:
        p = subprocess.run(["log", "show", "--style", "ndjson", "--start", since, "--predicate",
                            'process BEGINSWITH "sshd" AND eventMessage BEGINSWITH "Failed "'],
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
