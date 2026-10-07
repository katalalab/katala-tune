#!/usr/bin/env python3
"""katala-tune のライブ表示用サンプラー（macOS）。読み取り専用。

標準入力でこのスクリプトを受け取り（`python3 - interval=1 procs=5 max=900`）、一定の間隔で 1 行 1 JSON を
標準出力へ出し続ける。python3 標準ライブラリだけで動き、OS の統計を ctypes で直接読む（子プロセスを作らない）。
設定・プロセス・ファイルを変更しない。集めるのは数値と、上位プロセスの PID・実行ファイル名・アプリ名だけ
（引数・環境変数・パス・ファイルの中身は読まない）。

止まり方:
- watch=1 なら、標準入力が閉じたとき（SSH の切断・アプリの終了）にすぐ終わる。アプリは
  `python3 -c 'import sys;exec(compile(sys.stdin.buffer.read(<バイト数>),"live_mac.py","exec"))' ... watch=1`
  でスクリプトを渡し、その後も標準入力を開いておく
- 読み手が居なくなると、次の書き込みが失敗して終わる
- max 秒を過ぎたら {"type":"end","reason":"max_age"} を出して終わる（呼び出し側が必要ならつなぎ直す）
- count=N を渡すと N 回出して {"type":"end","reason":"count"} で終わる（確認用）

出力（1 行目が hello、以後 s）:
  {"type":"hello","v":1,"os":"macos","cores":18,"interval":1.0,"procs_every":5,"mem_total_gb":64.0,
   "has":{"cpu":true,"mem":true,"disk":true,"net":true,"procs":true,"gpu":false},"errors":[]}
  {"type":"s","t":<epoch ms>,"seq":1,"cpu":12.3,"cores":[20,3,...],
   "mem":{"used_pct":41.0,"swap_used_gb":1.2,"swap_total_gb":2.0,"pressure":"normal"},
   "disk":{"read_bps":0,"write_bps":12345},"net":{"rx_bps":100,"tx_bps":50},
   "procs":{"count":812,"top_cpu":[{"pid":1,"name":"x","app":"X","cpu":12.5,"mem_mb":100}],"top_mem":[...]},
   "self":{"cpu_s":0.31,"rss_mb":23.0}}
  cpu・cores は機体全体に対する %。procs の cpu は 1 コア換算の %（前回の procs からの平均）。
  procs と self（このサンプラー自身の CPU 秒・RSS）は procs 秒ごとにだけ付く。
"""
import ctypes as C
import json
import os
import sys
import time

V = 1
TOP = 8


def arg(name, default, conv=float):
    for a in sys.argv[1:]:
        if a.startswith(name + "="):
            try:
                return conv(a.split("=", 1)[1])
            except ValueError:
                return default
    return default


INTERVAL = min(10.0, max(0.2, arg("interval", 1.0)))
PROCS_EVERY = min(60.0, max(1.0, arg("procs", 5.0)))
MAX_AGE = max(1.0, arg("max", 900.0))
COUNT = int(arg("count", 0, int))
# watch=1: 標準入力が閉じたら（呼び出し側が居なくなったら）すぐ終わる。呼び出し側はスクリプトを渡した後も標準入力を開いておく
WATCH = arg("watch", 0, int) == 1

LIBC = C.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
ERRORS = []


# ---- sysctl ----
LIBC.sysctlbyname.argtypes = [C.c_char_p, C.c_void_p, C.POINTER(C.c_size_t), C.c_void_p, C.c_size_t]
LIBC.sysctlbyname.restype = C.c_int


def sysctl(name, ctype):
    v = ctype()
    n = C.c_size_t(C.sizeof(v))
    if LIBC.sysctlbyname(name.encode(), C.byref(v), C.byref(n), None, 0) != 0:
        return None
    return v


class XswUsage(C.Structure):
    _fields_ = [("total", C.c_uint64), ("avail", C.c_uint64), ("used", C.c_uint64), ("pagesize", C.c_uint32), ("encrypted", C.c_int32)]


def memory():
    level = sysctl("kern.memorystatus_level", C.c_int)  # 使える割合（%）。mac_probe.py の available_pct と同じ値
    sw = sysctl("vm.swapusage", XswUsage)
    pressure = sysctl("kern.memorystatus_vm_pressure_level", C.c_int)
    return {
        "used_pct": round(100 - level.value, 1) if level is not None else None,
        "swap_used_gb": round(sw.used / 2**30, 2) if sw is not None else None,
        "swap_total_gb": round(sw.total / 2**30, 2) if sw is not None else None,
        "pressure": {1: "normal", 2: "warn", 4: "critical"}.get(pressure.value, "unknown") if pressure is not None else None,
    }


# ---- CPU（コアごとの tick。user・system・idle・nice） ----
LIBC.mach_host_self.restype = C.c_uint32
LIBC.host_processor_info.argtypes = [C.c_uint32, C.c_int, C.POINTER(C.c_uint32), C.POINTER(C.POINTER(C.c_uint32)), C.POINTER(C.c_uint32)]
LIBC.host_processor_info.restype = C.c_int
LIBC.vm_deallocate.argtypes = [C.c_uint32, C.c_size_t, C.c_size_t]
LIBC.vm_deallocate.restype = C.c_int
HOST = LIBC.mach_host_self()  # 1 回だけ取る（呼ぶたびに送信権が増える）
TASK = C.c_uint32.in_dll(LIBC, "mach_task_self_").value
PROCESSOR_CPU_LOAD_INFO = 2


def cpu_ticks():
    n, cnt = C.c_uint32(), C.c_uint32()
    info = C.POINTER(C.c_uint32)()
    if LIBC.host_processor_info(HOST, PROCESSOR_CPU_LOAD_INFO, C.byref(n), C.byref(info), C.byref(cnt)) != 0:
        return None
    try:
        vals = info[: cnt.value]
    finally:
        LIBC.vm_deallocate(TASK, C.cast(info, C.c_void_p).value, cnt.value * 4)
    return [vals[i * 4:(i + 1) * 4] for i in range(n.value)]


def cpu_pct(prev, cur):
    """(全体, コアごと)。tick は 32 ビットで一周するので差は 2^32 で割った余り"""
    if not prev or not cur or len(prev) != len(cur):
        return None, []
    cores, busy_all, total_all = [], 0, 0
    for a, b in zip(prev, cur):
        d = [(y - x) % 2**32 for x, y in zip(a, b)]
        busy, total = d[0] + d[1] + d[3], sum(d)
        busy_all += busy
        total_all += total
        cores.append(round(busy / total * 100) if total else 0)
    return (round(busy_all / total_all * 100, 1) if total_all else None), cores


# ---- ネットワーク（en* の送受信バイト。utun・bridge などは同じ通信を二重に数えるので除く） ----
class Sockaddr(C.Structure):
    _fields_ = [("sa_len", C.c_uint8), ("sa_family", C.c_uint8)]


class Ifaddrs(C.Structure):
    pass


Ifaddrs._fields_ = [
    ("ifa_next", C.POINTER(Ifaddrs)), ("ifa_name", C.c_char_p), ("ifa_flags", C.c_uint), ("ifa_addr", C.POINTER(Sockaddr)),
    ("ifa_netmask", C.c_void_p), ("ifa_dstaddr", C.c_void_p), ("ifa_data", C.c_void_p),
]


class IfData(C.Structure):
    _fields_ = [(k, C.c_uint8) for k in ("type", "typelen", "physical", "addrlen", "hdrlen", "recvquota", "xmitquota", "unused1")] + [
        (k, C.c_uint32) for k in ("mtu", "metric", "baudrate", "ipackets", "ierrors", "opackets", "oerrors", "collisions", "ibytes", "obytes")
    ]


LIBC.getifaddrs.argtypes = [C.POINTER(C.POINTER(Ifaddrs))]
LIBC.getifaddrs.restype = C.c_int
LIBC.freeifaddrs.argtypes = [C.POINTER(Ifaddrs)]
AF_LINK = 18


def net_bytes():
    head = C.POINTER(Ifaddrs)()
    if LIBC.getifaddrs(C.byref(head)) != 0:
        return None
    out = {}
    try:
        p = head
        while p:
            a = p.contents
            if a.ifa_addr and a.ifa_data and a.ifa_name and a.ifa_addr.contents.sa_family == AF_LINK:
                name = a.ifa_name.decode("ascii", "replace")
                if name.startswith("en"):
                    d = C.cast(a.ifa_data, C.POINTER(IfData)).contents
                    out[name] = (d.ibytes, d.obytes)
            p = a.ifa_next
    finally:
        LIBC.freeifaddrs(head)
    return out


def net_rate(prev, cur, dt):
    if prev is None or cur is None or dt <= 0:
        return None
    rx = tx = 0
    for k, (i, o) in cur.items():
        if k in prev:  # if_data のカウンタは 32 ビット
            rx += (i - prev[k][0]) % 2**32
            tx += (o - prev[k][1]) % 2**32
    return {"rx_bps": round(rx / dt), "tx_bps": round(tx / dt)}


# ---- ディスク（IOBlockStorageDriver の Statistics。Activity Monitor のディスクと同じ元） ----
class Disk:
    def __init__(self):
        self.io = C.CDLL("/System/Library/Frameworks/IOKit.framework/IOKit")
        self.cf = C.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
        io, cf = self.io, self.cf
        io.IOServiceMatching.argtypes = [C.c_char_p]
        io.IOServiceMatching.restype = C.c_void_p
        io.IOServiceGetMatchingServices.argtypes = [C.c_uint32, C.c_void_p, C.POINTER(C.c_uint32)]
        io.IOServiceGetMatchingServices.restype = C.c_int
        io.IOIteratorNext.argtypes = [C.c_uint32]
        io.IOIteratorNext.restype = C.c_uint32
        io.IOObjectRelease.argtypes = [C.c_uint32]
        io.IOObjectRelease.restype = C.c_int
        io.IORegistryEntryCreateCFProperty.argtypes = [C.c_uint32, C.c_void_p, C.c_void_p, C.c_uint32]
        io.IORegistryEntryCreateCFProperty.restype = C.c_void_p
        cf.CFStringCreateWithCString.argtypes = [C.c_void_p, C.c_char_p, C.c_uint32]
        cf.CFStringCreateWithCString.restype = C.c_void_p
        cf.CFDictionaryGetValue.argtypes = [C.c_void_p, C.c_void_p]
        cf.CFDictionaryGetValue.restype = C.c_void_p
        cf.CFNumberGetValue.argtypes = [C.c_void_p, C.c_int, C.c_void_p]
        cf.CFNumberGetValue.restype = C.c_bool
        cf.CFRelease.argtypes = [C.c_void_p]
        key = lambda s: cf.CFStringCreateWithCString(None, s, 0x08000100)  # UTF-8
        self.k_stats, self.k_read, self.k_write = key(b"Statistics"), key(b"Bytes (Read)"), key(b"Bytes (Write)")
        self.services, self.found_at = [], None

    def _enumerate(self):
        for s in self.services:
            self.io.IOObjectRelease(s)
        self.services = []
        it = C.c_uint32()
        if self.io.IOServiceGetMatchingServices(0, self.io.IOServiceMatching(b"IOBlockStorageDriver"), C.byref(it)) != 0:
            return
        while True:
            s = self.io.IOIteratorNext(it.value)
            if not s:
                break
            self.services.append(s)
        self.io.IOObjectRelease(it.value)

    def _num(self, d, k):
        v = C.c_int64(0)
        p = self.cf.CFDictionaryGetValue(d, k)
        return v.value if p and self.cf.CFNumberGetValue(p, 4, C.byref(v)) else 0  # kCFNumberSInt64Type

    def bytes(self, now):
        if self.found_at is None or now - self.found_at > 30:  # 外付けの抜き差しに追従する
            self._enumerate()
            self.found_at = now
        r = w = 0
        for s in self.services:
            st = self.io.IORegistryEntryCreateCFProperty(s, self.k_stats, None, 0)
            if not st:
                continue
            try:
                r += self._num(st, self.k_read)
                w += self._num(st, self.k_write)
            finally:
                self.cf.CFRelease(st)
        return r, w


# ---- プロセス ----
# 自分のプロセスは libproc で毎回（安い）。root や他のユーザーのプロセスは libproc では読めないので、
# setuid の ps で PS_EVERY 秒ごとに読む（ps は 1 回 30〜45ms の CPU を使うので、毎回は呼ばない）
class TaskInfo(C.Structure):
    _fields_ = [(k, C.c_uint64) for k in ("virtual_size", "resident_size", "total_user", "total_system", "threads_user", "threads_system")] + [
        (k, C.c_int32) for k in ("policy", "faults", "pageins", "cow_faults", "messages_sent", "messages_received",
                                 "syscalls_mach", "syscalls_unix", "csw", "threadnum", "numrunning", "priority")
    ]


class Timebase(C.Structure):
    _fields_ = [("numer", C.c_uint32), ("denom", C.c_uint32)]


LIBC.proc_listallpids.argtypes = [C.c_void_p, C.c_int]
LIBC.proc_listallpids.restype = C.c_int
LIBC.proc_pidinfo.argtypes = [C.c_int, C.c_int, C.c_uint64, C.c_void_p, C.c_int]
LIBC.proc_pidinfo.restype = C.c_int
LIBC.proc_pidpath.argtypes = [C.c_int, C.c_void_p, C.c_uint32]
LIBC.proc_pidpath.restype = C.c_int
LIBC.mach_timebase_info.argtypes = [C.POINTER(Timebase)]
PROC_PIDTASKINFO = 4
PS_EVERY = 3 * PROCS_EVERY


def cpu_seconds(s):
    """ps の time（[dd-][hh:]mm:ss.cc）を秒に"""
    days, _, rest = s.rpartition("-")
    sec = 0.0
    for part in rest.split(":"):
        sec = sec * 60 + float(part)
    return sec + (int(days) * 86400 if days else 0)


def app_name(path):
    """/Applications/Google Chrome.app/.../Google Chrome Helper.app/... を Google Chrome にまとめる（mac_probe.py と同じ）"""
    i = path.find(".app/")
    return os.path.basename(path[:i]) if i > 0 else (os.path.basename(path) or path)


def rates(cur, prev, dt):
    """{pid: (cpu 秒, rss バイト, comm)} の 2 回分から {pid: (1 コア換算の %, rss バイト, comm)}"""
    out = {}
    for pid, (cpu_s, rss, comm) in cur.items():
        p = prev.get(pid)
        out[pid] = (max(0.0, (cpu_s - p[0]) / dt * 100) if p and dt > 0 and p[2] == comm else 0.0, rss, comm)
    return out


class Procs:
    def __init__(self):
        import subprocess
        self.subprocess = subprocess
        tb = Timebase()
        LIBC.mach_timebase_info(C.byref(tb))
        self.ns = tb.numer / tb.denom if tb.denom else 1.0  # Apple シリコンでは CPU 時間が mach の時間単位
        self.buf = (C.c_int * 16384)()
        self.ti = TaskInfo()
        self.path = C.create_string_buffer(4096)
        self.own_prev, self.own_t = {}, None
        self.ps_prev, self.ps_t, self.next_ps = {}, None, 0.0
        self.others = {}

    def own(self):
        n = LIBC.proc_listallpids(self.buf, C.sizeof(self.buf))
        out, size = {}, C.sizeof(self.ti)
        for pid in self.buf[: max(0, n)]:
            if pid > 0 and LIBC.proc_pidinfo(pid, PROC_PIDTASKINFO, 0, C.byref(self.ti), size) == size:
                out[pid] = ((self.ti.total_user + self.ti.total_system) * self.ns / 1e9, self.ti.resident_size, None)
        return out

    def comm(self, pid, comm):
        """名前は上位に入ったものだけ引く（全プロセス分は引かない）"""
        if comm is None:
            comm = self.path.value.decode("utf-8", "replace") if LIBC.proc_pidpath(pid, self.path, 4096) > 0 else str(pid)
        return comm

    def ps(self):
        res = self.subprocess.run(["/bin/ps", "-axo", "pid=,time=,rss=,comm="], capture_output=True, text=True, timeout=10).stdout
        out = {}
        for line in res.splitlines():
            parts = line.split(None, 3)
            if len(parts) == 4:
                try:
                    out[int(parts[0])] = (cpu_seconds(parts[1]), int(parts[2]) * 1024, parts[3])
                except ValueError:
                    pass
        return out

    def sample(self, now):
        cur = self.own()
        mine = rates(cur, self.own_prev, now - self.own_t) if self.own_t else {}
        self.own_prev, self.own_t = cur, now
        if now >= self.next_ps:
            allp = self.ps()
            if self.ps_t:
                self.others = {pid: r for pid, r in rates(allp, self.ps_prev, now - self.ps_t).items() if pid not in cur}
            self.ps_prev, self.ps_t = allp, now
            self.next_ps = now + (PS_EVERY if self.ps_t and self.others else PROCS_EVERY)
        if not mine:
            return None
        rows = [(pid, cpu, rss, comm) for pid, (cpu, rss, comm) in {**self.others, **mine}.items()]

        def top(key):
            # 名前は実行ファイル名と .app の名前だけ（パスは出さない）
            out = []
            for pid, cpu, rss, comm in sorted(rows, key=key, reverse=True)[:TOP]:
                comm = self.comm(pid, comm)
                out.append({"pid": pid, "name": os.path.basename(comm), "app": app_name(comm), "cpu": round(cpu, 1), "mem_mb": round(rss / 2**20)})
            return out

        return {"count": len(rows), "top_cpu": top(lambda r: r[1]), "top_mem": top(lambda r: r[2])}

def self_usage():
    """このサンプラー自身の負荷（呼んだ ps を含む CPU 秒と最大 RSS）。呼び出し側が差分から CPU% を出す"""
    import resource
    me, kids = resource.getrusage(resource.RUSAGE_SELF), resource.getrusage(resource.RUSAGE_CHILDREN)
    return {"cpu_s": round(me.ru_utime + me.ru_stime + kids.ru_utime + kids.ru_stime, 3), "rss_mb": round(me.ru_maxrss / 2**20, 1)}


def emit(obj):
    try:
        sys.stdout.write(json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + "\n")
        sys.stdout.flush()
        return True
    except (BrokenPipeError, OSError, ValueError):
        return False


def finish(code=0):
    # 読み手が居ないときに終わり際の flush で例外を出さないよう、標準出力を捨て先に向けてから終わる
    try:
        os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
    except OSError:
        pass
    os._exit(code)


def init(name, f):
    try:
        return f()
    except Exception as e:  # 取れないものは無いまま続ける（hello の errors で知らせる）
        ERRORS.append(f"{name}: {type(e).__name__}: {e}"[:200])
        return None


def main():
    ncpu = sysctl("hw.ncpu", C.c_int)
    mem_total = sysctl("hw.memsize", C.c_uint64)
    disk = init("disk", Disk)
    procs = init("procs", Procs)
    t0 = time.monotonic()
    cpu_prev = init("cpu", cpu_ticks)
    net_prev = init("net", net_bytes)
    disk_prev = init("disk", lambda: disk.bytes(t0)) if disk else None
    if procs:
        init("procs", lambda: procs.sample(t0))
    has = {"cpu": cpu_prev is not None, "mem": True, "disk": disk_prev is not None, "net": net_prev is not None, "procs": procs is not None, "gpu": False}
    hello = {"type": "hello", "v": V, "os": "macos", "cores": ncpu.value if ncpu else None, "interval": INTERVAL, "procs_every": PROCS_EVERY,
             "mem_total_gb": round(mem_total.value / 2**30, 1) if mem_total else None, "has": has, "errors": ERRORS}
    if WATCH:
        import threading

        def watch_stdin():
            try:
                while sys.stdin.buffer.read1(4096):
                    pass  # スクリプトの後に来るものは読み捨てる（来ない）
            except Exception:
                pass
            finish()

        threading.Thread(target=watch_stdin, daemon=True).start()
    if not emit(hello):
        finish()
    seq, last, next_t = 0, t0, t0
    next_procs = t0 + INTERVAL  # 最初の上位プロセスはすぐ出し、以後は PROCS_EVERY ごと
    while True:
        next_t += INTERVAL
        wait = next_t - time.monotonic()
        if wait > 0:
            time.sleep(wait)
        elif wait < -2 * INTERVAL:  # スリープ明けなどで遅れたら、まとめて出さずに今から数え直す
            next_t = time.monotonic()
        now = time.monotonic()
        if now - t0 >= MAX_AGE:
            emit({"type": "end", "reason": "max_age"})
            finish()
        dt, last = now - last, now
        seq += 1
        s = {"type": "s", "t": int(time.time() * 1000), "seq": seq}
        cur = cpu_ticks() if has["cpu"] else None
        s["cpu"], s["cores"] = cpu_pct(cpu_prev, cur)
        cpu_prev = cur or cpu_prev
        try:
            s["mem"] = memory()
        except Exception:
            s["mem"] = None
        if has["disk"]:
            try:
                d = disk.bytes(now)
                s["disk"] = {"read_bps": round(max(0, d[0] - disk_prev[0]) / dt), "write_bps": round(max(0, d[1] - disk_prev[1]) / dt)} if dt > 0 else None
                disk_prev = d
            except Exception:
                s["disk"] = None
        if has["net"]:
            cur_net = net_bytes()
            s["net"] = net_rate(net_prev, cur_net, dt)
            net_prev = cur_net if cur_net is not None else net_prev
        if procs and now >= next_procs:
            next_procs = now + PROCS_EVERY
            try:
                s["procs"] = procs.sample(now)
            except Exception:
                pass
            s["self"] = self_usage()
        if not emit(s):
            finish()
        if COUNT and seq >= COUNT:
            emit({"type": "end", "reason": "count"})
            finish()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        finish()
