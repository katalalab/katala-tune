#!/usr/bin/env python3
"""katala-tune の macOS 調査。読み取り専用で、結果を JSON 1行で標準出力へ出す。

python3 標準ライブラリだけで動く。設定・プロセス・ファイルを変更しない。
秘密・環境変数・コマンドライン引数・ファイルの中身は集めない（プロセスは実行ファイル名だけ）。
使い方: python3 - [--skip-benchmark] [nonet]
  --skip-benchmark  ベンチマークを省く（台帳の "benchmark": false）
  nonet             ネットワークとセキュリティ（netsec）を集めない（台帳の "network": false）
"""
import json, os, plistlib, re, shutil, socket, statistics, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor, wait
from datetime import datetime

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
    # 失敗しているものだけ終了の理由を読む（多すぎるときは先頭 20 件）。1 件 4 秒の上限があるので、
    # 4 並列で読み、全体も 10 秒で打ち切る（遅い機体で調査全体の上限 90 秒に当たって結果をまるごと失わないため）
    uid = os.getuid()
    targets = bad[:20]
    if targets:
        ex = ThreadPoolExecutor(max_workers=4)
        futs = {ex.submit(exit_reason, f"gui/{uid}/{b['label']}"): b for b in targets}
        done, _ = wait(futs, timeout=10)
        for f in done:
            try:
                futs[f]["reason"] = f.result()
            except Exception:
                pass
        try:
            ex.shutdown(wait=False, cancel_futures=True)
        except TypeError:  # Python 3.8 以前
            ex.shutdown(wait=False)
    return bad


# ---- ネットワークとセキュリティ（docs/observability.md の 6）----
# パケットの中身は取らない。接続のメタデータ（プロセス・アドレス・ポート）と OS の防御の状態、自動起動の一覧（名前と実行ファイル名）だけ。
# 台帳で "network": false の機体には `nonet` を渡し、この部分を丸ごと飛ばす。
NETSEC_BUDGET_S = 15.0  # この部分の合計の上限。各取得はさらに短い上限を持ち、残り時間で打ち切る
LAUNCHD_ALL = [("~/Library/LaunchAgents", "user"), ("/Library/LaunchAgents", "agent"), ("/Library/LaunchDaemons", "daemon")]
NET_PROTO = re.compile(r"^(tcp|udp)(4|6|46) ")


class NetsecError(Exception):
    pass


def run_checked(cmd, timeout):
    """打ち切り・失敗を例外にする（取れなかったことを「空」と区別するため）"""
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise NetsecError(f"timeout {timeout:.0f}s")
    except OSError as e:
        raise NetsecError(f"{type(e).__name__}: {e}")
    if r.returncode != 0 and not r.stdout:
        raise NetsecError(f"exit {r.returncode}: {r.stderr.strip()[:120]}")
    return r.stdout


def _hostport(s, v4):
    """tcp4 は 192.0.2.1:443・*:5000、tcp6 は fe80::1%en0.49241・*.5000（ポートの区切りが違う）"""
    i = s.rfind(":" if v4 else ".")
    if i < 0:
        return s, None
    p = s[i + 1:]
    return s[:i], (int(p) if p.isdigit() else None)


def _loopback(a):
    return a.startswith("127.") or a in ("::1", "localhost") or a.startswith("::ffff:127.")


VERSION_DIR = re.compile(r"^v?\d+(\.\d+)+([-+_][0-9A-Za-z.]+)?$")


def stable_name(path):
    """実行ファイルの名前。版の番号そのものが名前のもの（…/claude/versions/2.1.0 など）は、版の上の名前にする
    （更新のたびに「外と初めて通信したプロセス」にならないように）"""
    parts = [p for p in path.split("/") if p]
    name = parts[-1] if parts else ""
    if VERSION_DIR.match(name):
        for p in reversed(parts[:-1]):
            if p != "versions" and not VERSION_DIR.match(p):
                return p
    return name or None


def exe_name(pid, _lib=[]):
    """実行ファイルの名前（proc_pidpath の basename）。nettop の名前は 15 文字で切れ、ps の comm はプロセスが書き換えた題名
    （ssh の制御ソケットのハッシュ、node の next-server など）になるので、比べる鍵には実行ファイルの名前を使う"""
    try:
        if not _lib:
            import ctypes
            _lib.extend([ctypes.CDLL("/usr/lib/libproc.dylib"), ctypes.create_string_buffer(4096)])
        lib, buf = _lib
        if pid and lib.proc_pidpath(int(pid), buf, 4096) > 0:
            return stable_name(buf.value.decode("utf-8", "replace"))
    except Exception:  # noqa: BLE001 取れなければ nettop の名前を使う
        pass
    return None


def parse_nettop(text, name_of):
    """nettop -L 1 -n -x -J state の出力から、待ち受け（TCP の Listen と、相手の無い UDP）と、確立した TCP の (プロセス, 宛先, ポート) の数を作る。
    行の形: プロセスの行「名前.pid,,」のあとに、その接続の行「tcp4 192.0.2.1:50000<->198.51.100.7:443,Established,」が続く"""
    proc, pid = "?", None
    listen, conns, established = [], {}, []
    for line in text.splitlines():
        if not line or line.startswith(","):
            continue
        cols = line.rsplit(",", 2)
        head = cols[0]
        if not NET_PROTO.match(head):
            name, _, p = head.rpartition(".")
            pid = int(p) if p.isdigit() else None
            proc = name_of(pid) or name or head or "?"
            continue
        kind, _, rest = head.partition(" ")
        state = cols[1] if len(cols) > 1 else ""
        local, _, remote = rest.partition("<->")
        v4 = kind.endswith("4") and not kind.endswith("46")
        la, lp = _hostport(local, v4)
        ra, rp = _hostport(remote, v4)
        proto = kind[:3]
        if (proto == "tcp" and state == "Listen") or (proto == "udp" and remote in ("*:*", "*.*")):
            if lp:
                listen.append({"proto": proto, "addr": la, "port": lp, "pid": pid, "proc": proc})
        elif proto == "tcp" and state == "Established" and rp and not _loopback(ra):
            established.append((proc, ra, rp, lp))
    # 外向きだけ: 自分の待ち受けの番号で受けた接続は外から来たもの（相手の番号は毎回変わる）
    lports = {x["port"] for x in listen if x["proto"] == "tcp"}
    for proc_, ra, rp, lp in established:
        if lp not in lports:
            k = (proc_, ra, rp)
            conns[k] = conns.get(k, 0) + 1
    top = sorted(conns.items(), key=lambda x: (-x[1], x[0]))[:400]
    return listen[:600], [{"proc": k[0], "addr": k[1], "port": k[2], "n": n} for k, n in top]


def netsec():
    t0 = time.time()
    c0 = time.thread_time()
    deadline = t0 + NETSEC_BUDGET_S
    res = {"v": 1, "listen": [], "outbound": [], "defense": {}, "persist": [], "errors": {}, "parts_ms": {}}

    def left(cap):
        return max(0.5, min(cap, deadline - time.time()))

    def part(name, fn):
        if time.time() > deadline:
            res["errors"][name] = "skipped: time budget"
            return
        s = time.time()
        try:
            fn()
        except Exception as e:  # noqa: BLE001 取れなかった理由を残し、ほかの部分は続ける
            res["errors"][name] = str(e)[:200] if isinstance(e, NetsecError) else f"{type(e).__name__}: {e}"[:200]
        res["parts_ms"][name] = int((time.time() - s) * 1000)

    def connections():
        # nettop は root のプロセス（sshd・tailscaled など）も含めて全部見える（lsof は自分のプロセスだけ）
        listen, outbound = parse_nettop(run_checked(["nettop", "-L", "1", "-n", "-x", "-J", "state"], left(8)), exe_name)
        res["listen"], res["outbound"] = listen, outbound

    def firewall():
        out = run_checked(["/usr/libexec/ApplicationFirewall/socketfilterfw", "--getglobalstate"], left(5))
        m = re.search(r"State = (\d)", out)
        res["defense"]["firewall"] = int(m.group(1)) if m else (0 if "disabled" in out else 1 if "enabled" in out else None)
        st = run_checked(["/usr/libexec/ApplicationFirewall/socketfilterfw", "--getstealthmode"], left(5))
        res["defense"]["stealth"] = True if " is on" in st else False if " is off" in st else None

    def gatekeeper():
        out = run_checked(["spctl", "--status"], left(5))
        res["defense"]["gatekeeper"] = True if "assessments enabled" in out else False if "assessments disabled" in out else None

    def xprotect():
        # macOS 15 以降は xprotect コマンドが版と入った日時を返す。無ければバンドルの版と更新日時
        ver, at = None, None
        if shutil.which("xprotect"):
            try:
                out = run_checked(["xprotect", "version"], left(5))
                m = re.search(r"Version:\s*(\S+)(?:\s+Installed:\s*(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [+-]\d{4}))?", out)
                if m:
                    ver = m.group(1)
                    if m.group(2):
                        at = int(datetime.strptime(m.group(2), "%Y-%m-%d %H:%M:%S %z").timestamp() * 1000)
            except NetsecError:
                pass
        if ver is None:
            for p in ("/var/protected/xprotect/XProtect.bundle/Contents/Info.plist",
                      "/Library/Apple/System/Library/CoreServices/XProtect.bundle/Contents/Info.plist"):
                try:
                    with open(p, "rb") as fh:
                        ver = str(plistlib.load(fh).get("CFBundleShortVersionString") or "") or None
                    at = int(os.path.getmtime(p) * 1000)
                    break
                except Exception:  # noqa: BLE001 次の場所を見る
                    continue
        if ver is None:
            raise NetsecError("XProtect の版が読めない")
        res["defense"]["xprotect_version"] = ver
        res["defense"]["xprotect_at"] = at

    def launchd():
        # com.apple.* も数える（本物の Apple のジョブは /System にあり、ここに置かれた com.apple.* は疑わしい）
        items = []
        for d, scope in LAUNCHD_ALL:
            dd = os.path.expanduser(d)
            try:
                names = sorted(os.listdir(dd))
            except FileNotFoundError:
                continue
            for f in names:
                if not f.endswith(".plist"):
                    continue
                label, prog = f[:-6], None
                try:
                    with open(os.path.join(dd, f), "rb") as fh:
                        pl = plistlib.load(fh)
                    label = str(pl.get("Label") or label)
                    prog = pl.get("Program") or (pl.get("ProgramArguments") or [None])[0]
                except Exception:  # noqa: BLE001 読めない plist も「ある」ことは残す
                    pass
                items.append({"kind": "launchd", "key": f"{scope}:{label}", "program": os.path.basename(str(prog)) if prog else None})
        res["persist"] = items[:3000]

    part("listen", connections)
    part("firewall", firewall)
    part("gatekeeper", gatekeeper)
    part("xprotect", xprotect)
    part("launchd", launchd)
    res["elapsed_ms"] = int((time.time() - t0) * 1000)
    res["cpu_ms"] = int((time.thread_time() - c0) * 1000)
    return res


def main():
    t0 = time.time()
    network = "nonet" not in sys.argv[1:]
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
        f_ns = ex.submit(netsec) if network else None
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
        if f_ns:
            result["netsec"] = f_ns.result()
    skip_benchmark = "--skip-benchmark" in sys.argv[1:]
    result["bench"] = None if skip_benchmark else bench()
    if skip_benchmark:
        result["benchmark_skipped"] = True
    result["elapsed_s"] = round(time.time() - t0, 1)
    json.dump(result, sys.stdout, ensure_ascii=False)
    print()


if __name__ == "__main__":
    main()
