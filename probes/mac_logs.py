#!/usr/bin/env python3
"""katala-tune の macOS ログ収集。読み取り専用。前回の続きから読み、JSON 1行で出す。

使い方: python3 - <diag_cursor_ms> <kernel_cursor_epoch>
  diag    ~/Library/Logs/DiagnosticReports（と読めれば /Library/...）のクラッシュ・ハング・パニック・資源超過。
          ファイル名と先頭の見出し（app_name, bug_type）だけを読み、中身（スタック等）は送らない。
  kernel  統合ログのうちカーネルのエラー（Sandbox の deny を除く）。fault は1時間に数千件出るので取らない。
"""
import json, os, re, subprocess, sys, time

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
        out = subprocess.run(["log", "show", "--style", "ndjson", "--start", since, "--predicate",
                              # Sandbox の deny は作業中のツールが日常的に出すノイズなので除く
                              'messageType == error AND (process == "kernel" OR subsystem BEGINSWITH "com.apple.kernel") AND NOT (sender == "Sandbox")'],
                             capture_output=True, text=True, timeout=40).stdout
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


def main():
    a = sys.argv[1:] + ["0", "0"]
    res = {"probe": "mac_logs", "sources": {"mac_diag": diag(float(a[0] or 0)), "mac_kernel": kernel(float(a[1] or 0))}}
    json.dump(res, sys.stdout, ensure_ascii=False)
    print()


if __name__ == "__main__":
    main()
