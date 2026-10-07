#!/usr/bin/env python3
"""katala-tune の macOS 調査。読み取り専用で、結果を JSON 1行で標準出力へ出す。

python3 標準ライブラリだけで動く。設定・プロセス・ファイルを変更しない。
秘密・環境変数・コマンドライン引数・ファイルの中身は集めない（プロセスは実行ファイル名だけ）。
"""
import json, os, plistlib, re, shutil, socket, statistics, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor

HOME = os.path.expanduser("~")
CACHE_DIRS = [
    "~/Library/Caches",
    "~/Library/Developer/Xcode/DerivedData",
    "~/Library/Developer/CoreSimulator/Devices",
    "~/.npm/_cacache",
    "~/.cache",
    "~/Library/Caches/Homebrew",
    "~/.colima",
    "~/Library/Containers/com.docker.docker/Data",
]


def run(cmd, timeout=8):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout).stdout
    except Exception:
        return ""


def sysctl(name):
    return run(["sysctl", "-n", name]).strip()


def num(s, default=None):
    try:
        return float(s)
    except (TypeError, ValueError):
        return default


JXA_AVAIL = (
    'ObjC.import("Foundation"); var k=$.NSURLVolumeAvailableCapacityForImportantUsageKey;'
    'var o=Ref(), e=Ref(); $.NSURL.fileURLWithPath(%s).getResourceValueForKeyError(o,k,e); String(o[0].longLongValue)'
)


def important_available(mount):
    """macOS が必要時に自動で空ける領域（Time Machine のローカルスナップショット・パージ可能なキャッシュ）を含めた空き。
    statvfs の空きはこれを含まず、実測で 19GB と 108GB のように大きく食い違うことがある。"""
    out = run(["osascript", "-l", "JavaScript", "-e", JXA_AVAIL % json.dumps(mount)], timeout=10).strip()
    return int(out) if out.isdigit() and int(out) > 0 else None


def bench():
    """同じ機体での前後比較用。1スレッドの固定量の計算を5回測る。"""
    runs = []
    for _ in range(5):
        t = time.perf_counter()
        sum(i * i for i in range(3_000_000))
        runs.append(round((time.perf_counter() - t) * 1000, 1))
    return {"runs_ms": runs, "median_ms": statistics.median(runs)}


def cpu_busy():
    out = run(["top", "-l", "2", "-n", "0", "-s", "1"], timeout=10)
    m = re.findall(r"CPU usage: ([\d.]+)% user, ([\d.]+)% sys, ([\d.]+)% idle", out)
    if not m:
        return None
    u, s, _ = map(float, m[-1])
    return round(u + s, 1)


def memory():
    page = int(sysctl("hw.pagesize") or 16384)
    vm = run(["vm_stat"])
    pages = {}
    for line in vm.splitlines():
        m = re.match(r'(?:"?)(.+?)(?:"?):\s+(\d+)\.', line)
        if m:
            pages[m.group(1)] = int(m.group(2))
    gb = lambda k: round(pages.get(k, 0) * page / 2**30, 2)
    swap = sysctl("vm.swapusage")  # total = 2048.00M  used = 1003.75M  free = ...
    sw = dict(re.findall(r"(total|used|free) = ([\d.]+)M", swap))
    level = {"1": "normal", "2": "warn", "4": "critical"}.get(sysctl("kern.memorystatus_vm_pressure_level"), "unknown")
    return {
        "total_gb": round(int(sysctl("hw.memsize") or 0) / 2**30, 1),
        "free_gb": gb("Pages free"),
        "wired_gb": gb("Pages wired down"),
        "compressed_gb": gb("Pages occupied by compressor"),
        "available_pct": num(sysctl("kern.memorystatus_level")),
        "pressure": level,
        "swap_used_gb": round(num(sw.get("used"), 0) / 1024, 2),
        "swap_total_gb": round(num(sw.get("total"), 0) / 1024, 2),
    }


def app_name(path):
    """/Applications/Google Chrome.app/.../Google Chrome Helper.app/... を Google Chrome にまとめる。"""
    i = path.find(".app/")
    if i > 0:
        return os.path.basename(path[:i])
    return os.path.basename(path) or path


def processes():
    out = run(["ps", "-axo", "pid=,pcpu=,rss=,etime=,comm="], timeout=10)
    procs = []
    for line in out.splitlines():
        parts = line.split(None, 4)
        if len(parts) < 5:
            continue
        pid, cpu, rss, etime, comm = parts
        procs.append({"pid": int(pid), "cpu": float(cpu), "mem_mb": round(int(rss) / 1024), "etime": etime,
                      "name": os.path.basename(comm), "app": app_name(comm)})
    groups = {}
    for p in procs:
        g = groups.setdefault(p["app"], {"app": p["app"], "cpu": 0.0, "mem_mb": 0, "count": 0})
        g["cpu"] += p["cpu"]; g["mem_mb"] += p["mem_mb"]; g["count"] += 1
    for g in groups.values():
        g["cpu"] = round(g["cpu"], 1)
    by = lambda xs, k, n: sorted(xs, key=lambda x: -x[k])[:n]
    top_cpu = by(procs, "cpu", 15)
    # 起動時刻（lstart）。終了操作の直前に、同じ PID が同じプロセスのままかを確かめるのに使う
    if top_cpu:
        out = run(["ps", "-o", "pid=,lstart=", "-p", ",".join(str(p["pid"]) for p in top_cpu)])
        starts = {int(l.split(None, 1)[0]): l.split(None, 1)[1].strip() for l in out.splitlines() if len(l.split(None, 1)) == 2}
        for p in top_cpu:
            p["start"] = starts.get(p["pid"])
    agents = sum(1 for p in procs if re.match(r"^(claude|codex|opencode|cursor-agent|agy|gemini)\b", p["name"], re.I))
    return {
        "count": len(procs),
        "top_cpu": top_cpu,
        "top_mem": by(procs, "mem_mb", 15),
        "apps": by(list(groups.values()), "mem_mb", 25),
        "apps_cpu": by(list(groups.values()), "cpu", 10),
        "agent_processes": agents,
    }


def dir_size_gb(path):
    p = os.path.expanduser(path)
    if not os.path.isdir(p):
        return None
    out = run(["du", "-sk", p], timeout=6)
    m = re.match(r"(\d+)", out)
    return {"path": path, "gb": round(int(m.group(1)) / 2**20, 2)} if m else {"path": path, "gb": None, "timeout": True}


def containers():
    res = {}
    if shutil.which("colima") or os.path.exists("/opt/homebrew/bin/colima"):
        out = run([shutil.which("colima") or "/opt/homebrew/bin/colima", "list", "--json"])
        vms = []
        for line in out.splitlines():
            try:
                v = json.loads(line)
                vms.append({"name": v.get("name"), "status": v.get("status"), "cpus": v.get("cpus"),
                            "memory_gb": round((v.get("memory") or 0) / 2**30, 1), "disk_gb": round((v.get("disk") or 0) / 2**30, 1)})
            except ValueError:
                pass
        res["colima"] = vms
    f = os.path.join(HOME, "Library/Group Containers/group.com.docker/settings-store.json")
    if os.path.exists(f):
        try:
            s = json.load(open(f))
            res["docker_desktop"] = {"memory_gb": round(s.get("MemoryMiB", 0) / 1024, 1), "cpus": s.get("Cpus")}
        except Exception:
            pass
    return res


def power():
    therm = run(["pmset", "-g", "therm"])
    limit = re.search(r"CPU_Speed_Limit\s*=\s*(\d+)", therm)
    warn = re.search(r"thermal warning level", therm, re.I) and not re.search(r"No thermal warning level", therm, re.I)
    g = run(["pmset", "-g"])
    lpm = re.search(r"lowpowermode\s+(\d)", g)
    batt = run(["pmset", "-g", "batt"])
    return {
        "cpu_speed_limit": int(limit.group(1)) if limit else None,
        "thermal_warning": bool(warn),
        "low_power_mode": (lpm.group(1) == "1") if lpm else None,
        "on_battery": "Battery Power" in batt,
    }


LAUNCHD_DIRS = [("~/Library/LaunchAgents", "user"), ("/Library/LaunchAgents", "agent"), ("/Library/LaunchDaemons", "daemon")]


def launchd_jobs():
    """com.apple 以外の launchd ジョブ。plist から予定（間隔・日時・常駐）と実行ファイル名だけを読む（引数は読まない）。
    状態は自分のドメイン（gui/uid）の launchctl list で分かる分だけ。system ドメインの daemon は unknown。"""
    loaded = {}
    for line in run(["launchctl", "list"]).splitlines()[1:]:
        p = line.split("\t")
        if len(p) == 3:
            loaded[p[2]] = (p[0], p[1])
    jobs = []
    for d, scope in LAUNCHD_DIRS:
        dd = os.path.expanduser(d)
        try:
            names = sorted(os.listdir(dd))
        except OSError:
            continue
        for f in names:
            if not f.endswith(".plist"):
                continue
            path = os.path.join(dd, f)
            try:
                with open(path, "rb") as fh:
                    pl = plistlib.load(fh)
            except Exception:
                continue
            label = str(pl.get("Label") or f[:-6])
            if label.startswith("com.apple."):
                continue
            sched = []
            if "StartInterval" in pl:
                sched.append(f"{int(pl['StartInterval'])}秒ごと")
            if "StartCalendarInterval" in pl:
                ci = pl["StartCalendarInterval"]
                ci = ci if isinstance(ci, list) else [ci]
                sched.append("日時指定 " + "; ".join(",".join(f"{k}={v}" for k, v in sorted(x.items())) for x in ci[:3] if isinstance(x, dict)))
            if pl.get("RunAtLoad"):
                sched.append("起動時")
            if pl.get("KeepAlive"):
                sched.append("常駐")
            if pl.get("WatchPaths") or pl.get("QueueDirectories"):
                sched.append("ファイル監視")
            prog = pl.get("Program") or (pl.get("ProgramArguments") or [None])[0]
            pid, status = loaded.get(label, (None, None))
            if scope != "user" and label not in loaded:
                state = "unknown"
            elif pid not in (None, "-"):
                state = "running"
            elif label in loaded:
                state = "loaded"
            else:
                state = "disabled" if pl.get("Disabled") else "not-loaded"
            last_exit = None
            if status not in (None, "-"):
                try:
                    last_exit = int(status)
                except ValueError:
                    pass
            jobs.append({"kind": "launchd", "id": label, "name": label, "scope": scope, "state": state,
                         "pid": int(pid) if pid not in (None, "-") else None, "last_result": last_exit,
                         "schedule": "、".join(sched) or "手動", "program": os.path.basename(str(prog)) if prog else None,
                         "plist": path if scope == "user" else None})
    return jobs


def exit_reason(target):
    """launchctl print の last exit reason（起動制約で止められた OS_REASON_CODESIGNING など）。読めなければ None"""
    for line in run(["launchctl", "print", target], timeout=4).splitlines():
        m = re.match(r"\s*last exit reason\s*=\s*(.+)$", line, re.I)
        if m:
            return m.group(1).strip()[:300]
    return None


def launchd_failing():
    out = run(["launchctl", "list"])
    bad = []
    for line in out.splitlines()[1:]:
        parts = line.split("\t")
        if len(parts) == 3 and not parts[2].startswith("com.apple.") and parts[1] not in ("0", "-") and parts[0] == "-":
            bad.append({"label": parts[2], "exit": parts[1], "reason": None})
    # 失敗しているものだけ終了の理由を読む（数は失敗分だけなので軽い。多すぎるときは先頭 20 件）
    uid = os.getuid()
    for b in bad[:20]:
        b["reason"] = exit_reason(f"gui/{uid}/{b['label']}")
    return bad


def main():
    t0 = time.time()
    with ThreadPoolExecutor(max_workers=12) as ex:
        f_cpu = ex.submit(cpu_busy)
        f_mem = ex.submit(memory)
        f_ps = ex.submit(processes)
        f_ct = ex.submit(containers)
        f_pw = ex.submit(power)
        f_ld = ex.submit(launchd_failing)
        f_jobs = ex.submit(launchd_jobs)
        f_tm = ex.submit(lambda: "Running = 1" in run(["tmutil", "status"]))
        f_sp = ex.submit(lambda: run(["mdutil", "-s", "/"]).strip().splitlines()[-1:] or [""])
        f_dirs = [ex.submit(dir_size_gb, d) for d in CACHE_DIRS]
        # 他の調査と並べると計測が乱れるので、ベンチは最後に単独で回す
        boot = re.search(r"sec = (\d+)", sysctl("kern.boottime"))
        data_vol = "/System/Volumes/Data" if os.path.exists("/System/Volumes/Data") else "/"
        f_avail = ex.submit(important_available, data_vol)
        du = shutil.disk_usage(data_vol)
        result = {
            "probe": "mac", "probe_version": 2,
            "host": {
                "hostname": socket.gethostname(),
                "os": "macOS " + run(["sw_vers", "-productVersion"]).strip(),
                "model": sysctl("hw.model"),
                "cpu": sysctl("machdep.cpu.brand_string"),
                "cores": int(sysctl("hw.ncpu") or 0),
                "p_cores": int(sysctl("hw.perflevel0.logicalcpu") or 0),
                "e_cores": int(sysctl("hw.perflevel1.logicalcpu") or 0),
                "uptime_h": round((time.time() - int(boot.group(1))) / 3600, 1) if boot else None,
            },
            "load": [round(x, 2) for x in os.getloadavg()],
        }
        eff = f_avail.result() or du.free
        # free_* は実効の空き（自動で空く分を含む）。raw_free_gb は statvfs の値
        result["disk"] = [{"mount": "/", "total_gb": round(du.total / 2**30), "free_gb": round(eff / 2**30),
                           "free_pct": round(eff / du.total * 100, 1), "raw_free_gb": round(du.free / 2**30)}]
        result["cpu_busy"] = f_cpu.result()
        result["memory"] = f_mem.result()
        result["processes"] = f_ps.result()
        result["containers"] = f_ct.result()
        result["power"] = f_pw.result()
        result["launchd_failing"] = f_ld.result()
        result["jobs"] = f_jobs.result()
        result["time_machine_running"] = f_tm.result()
        result["spotlight"] = f_sp.result()[0].strip()
        result["caches"] = [r for r in (f.result() for f in f_dirs) if r]
    result["bench"] = bench()
    result["elapsed_s"] = round(time.time() - t0, 1)
    json.dump(result, sys.stdout, ensure_ascii=False)
    print()


if __name__ == "__main__":
    main()
