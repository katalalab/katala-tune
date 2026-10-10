#!/usr/bin/env python3
"""On-demand connectivity only. Never emits addresses, resolver names or payloads."""
import json
import ipaddress
import re
import subprocess
import sys

def run(args):
    try:
        p = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=5)
        return p.returncode, p.stdout
    except FileNotFoundError:
        return -2, ""
    except subprocess.TimeoutExpired:
        return 28, ""
    except Exception:
        return None, ""

def usable_address(text):
    try:
        address = ipaddress.ip_address(text)
        if isinstance(address, ipaddress.IPv6Address) and address.ipv4_mapped:
            address = address.ipv4_mapped
        return not (address.is_loopback or address.is_unspecified or address.is_link_local or address.is_multicast)
    except ValueError:
        return False

def inspect(execute=run):
    out = dict(schema="katala_network_check.v1", platform="darwin", default_route_present=None,
               ip_address_present=None, dns_configured=None, interface_up=None, link_active=None)
    status, text = execute(["/sbin/route", "-n", "get", "default"])
    if status != 0:
        v6_status, v6_text = execute(["/sbin/route", "-n", "get", "-inet6", "default"])
        if v6_status == 0:
            status, text = v6_status, v6_text
        elif status == 1 and v6_status == 1:
            out["default_route_present"] = False
    match = re.search(r"interface:\s*([a-zA-Z0-9]+)\b", text) if status == 0 else None
    if match:
        out["default_route_present"] = True
        s, link = execute(["/sbin/ifconfig", match[1]])
        if s == 0 and re.search(r"^\w+: flags=", link, re.M):
            out["interface_up"] = bool(re.search(r"<[^>]*\bUP\b[^>]*>", link))
            out["link_active"] = True if re.search(r"status:\s*active\b", link) else False if re.search(r"status:\s*inactive\b", link) else None
            out["ip_address_present"] = any(usable_address(a) for a in re.findall(r"^\s*inet6?\s+(\S+)", link, re.M))
    status, text = execute(["/usr/sbin/scutil", "--dns"])
    if status == 0:
        if re.search(r"nameserver\[\d+\]", text): out["dns_configured"] = True
        elif text.strip() == "No DNS configuration available": out["dns_configured"] = False
    return out

def https(url, execute=run):
    status, text = execute(["/usr/bin/curl", "-q", "--proto", "=https", "--tlsv1.2", "--connect-timeout", "2", "--max-time", "4", "--silent", "--output", "/dev/null", "--write-out", "%{http_code} %{time_namelookup} %{time_connect} %{time_appconnect} %{time_starttransfer} %{time_total}", url])
    fields = text.strip().split()
    if status == 0 and fields and fields[0].isdigit() and 100 <= int(fields[0]) <= 599:
        out = dict(state="reachable", http_status=int(fields[0]), timings={})
        for key, value in zip(["dns_ms", "connect_ms", "tls_ms", "first_byte_ms", "total_ms"], fields[1:]):
            try:
                n = float(value) * 1000
                if 0 <= n <= 60000: out["timings"][key] = round(n, 3)
            except ValueError: pass
        return out
    return dict(state={-2:"tool_missing", 6:"dns_failed", 7:"connect_failed", 28:"timed_out", 35:"tls_failed", 60:"tls_certificate_failed"}.get(status,"unavailable"))

def main():
    if any(a != "--probe" for a in sys.argv[1:]): return 2
    out = inspect(); out["active_probes"] = "--probe" in sys.argv[1:]
    if out["active_probes"]: out["https"] = dict(named=https("https://www.apple.com/"), fixed_ip=https("https://1.1.1.1/cdn-cgi/trace"))
    print(json.dumps(out, separators=(",", ":")))
    return 0

if __name__ == "__main__": sys.exit(main())
