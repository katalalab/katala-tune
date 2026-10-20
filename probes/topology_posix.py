# Read-only topology probe. CONFIG is injected by the stdin transport.
import hashlib, json, math, os, platform, selectors, socket, stat, struct, subprocess, time, unicodedata

PARSER_VERSION = "katala-topology-parser.v1"
SKIP = {".git", ".branch-recovery", "target", "node_modules", ".cache", "cache", "__pycache__", ".venv", "venv", "vendor", "dist", "build"}


def canonical_json_typed_v1(value):
    if value is None:
        return b"z"
    if isinstance(value, bool):
        return b"b1" if value else b"b0"
    if isinstance(value, str):
        data = value.encode("utf-8", "strict")
        return b"s" + str(len(data)).encode() + b":" + data
    if isinstance(value, (int, float)):
        if isinstance(value, int) and abs(value) > 2 ** 53:
            raise ValueError("unsupported numeric range")
        number = float(value)
        if not math.isfinite(number) or (number.is_integer() and abs(number) > 2 ** 53):
            raise ValueError("unsupported numeric range")
        return b"n" + struct.pack(">d", number if number else 0.0).hex().encode()
    if isinstance(value, list):
        return b"a" + str(len(value)).encode() + b":[" + b"".join(canonical_json_typed_v1(x) for x in value) + b"]"
    if isinstance(value, dict):
        keys = sorted(value, key=lambda key: key.encode("utf-8", "strict"))
        return b"o" + str(len(keys)).encode() + b":{" + b"".join(canonical_json_typed_v1(key) + canonical_json_typed_v1(value[key]) for key in keys) + b"}"
    raise ValueError("unsupported json type")


def source_policy(config):
    policy = config.get("source_allowlist")
    if not isinstance(policy, list) or not policy or config.get("configs"):
        return None
    approved = {}
    total_files, total_bytes = 0, 0
    roots = config.get("roots", [])
    def relative(path, checkout=False):
        return isinstance(path, str) and ((checkout and path == ".") or (path and not path.startswith("/") and not any(c in path for c in "\\:") and not any(unicodedata.category(c) == "Cc" for c in path) and all(part not in ("", ".", "..") for part in path.split("/"))))
    def hexadecimal(value, lengths):
        return isinstance(value, str) and len(value) in lengths and all(c in "0123456789abcdef" for c in value)
    try:
        if len(policy) > config["limits"]["entries_per_environment"]:
            return None
        for root in roots:
            if not isinstance(root, str) or not root.startswith("/") or "\\" in root or (root != "/" and any(part in ("", ".", "..") for part in root[1:].split("/"))):
                return None
        for checkout in policy:
            if set(checkout) != {"root_index", "relative_path", "expected_head", "manifests"} or type(checkout["root_index"]) is not int or not 0 <= checkout["root_index"] < len(roots) or not relative(checkout["relative_path"], True) or not hexadecimal(checkout["expected_head"], (40, 64)) or not isinstance(checkout["manifests"], list):
                return None
            root = "/" + os.path.abspath(roots[checkout["root_index"]]).lstrip("/")
            key = (hashlib.sha256(root.encode()).hexdigest(), checkout["relative_path"])
            if key in approved:
                return None
            files = {}
            for item in checkout["manifests"]:
                if set(item) != {"relative_path", "bytes", "sha256"} or not relative(item["relative_path"]) or item["relative_path"] in files or type(item["bytes"]) is not int or not 0 <= item["bytes"] <= config["limits"]["file_bytes"] or not hexadecimal(item["sha256"], (64,)):
                    return None
                total_files += 1
                total_bytes += item["bytes"]
                path = item["relative_path"]
                workflow = path.startswith(".github/workflows/") and path.count("/") == 2 and path.endswith((".yml", ".yaml"))
                if (not workflow and path.split("/")[-1] not in ("Cargo.toml", "package.json", "pyproject.toml", "go.mod")) or (workflow and item["bytes"] > config["limits"]["workflow_bytes"]) or total_files > config["limits"]["manifests_per_environment"] or total_bytes > config["limits"]["total_bytes_per_environment"]:
                    return None
                files[path] = item
            approved[key] = (checkout, files)
    except (KeyError, TypeError, ValueError):
        return None
    return approved


def admitted_source(policy, root_hash, repo, path, data):
    approved = policy.get((root_hash, repo["relative_path"])) if policy is not None else None
    item = approved[1].get(path) if approved else None
    return bool(item is not None and repo["git"]["head"] == approved[0]["expected_head"] and len(data) == item["bytes"] and hashlib.sha256(data).hexdigest() == item["sha256"])


def enforce_sources(result, policy, denied):
    seen, complete = set(), True
    for repo in result["repositories"]:
        key = (repo["root_fingerprint"], repo["relative_path"])
        approved = policy.get(key) if policy is not None else None
        if not approved or key in seen or repo["git"]["head"] != approved[0]["expected_head"] or repo.get("config_file_read") or repo["configs"]:
            denied = True
            break
        seen.add(key)
        files = set()
        for manifest in repo["manifests"]:
            if manifest["relative_path"] in files or manifest["revision"] != approved[0]["expected_head"] or not admitted_source(policy, key[0], repo, manifest["relative_path"], manifest["content"].encode("utf-8")):
                denied = True
                break
            files.add(manifest["relative_path"])
        if len(files) != len(approved[1]):
            complete = False
            if len(repo["failures"]) < 64:
                repo["failures"].append({"code": "permission_denied", "message": "source allowlist incomplete"})
    if denied or policy is None:
        result["repositories"].clear()
        for counter in ("repositories", "manifests", "config_files", "config_keys"):
            result["counters"][counter] = 0
        result["progress"] = None
        result["failures"][:] = [{"code": "permission_denied", "message": "remote source allowlist mismatch"}]
    elif (not complete or len(seen) != len(policy)) and len(result["failures"]) < 64:
        result["failures"].append({"code": "permission_denied", "message": "source allowlist incomplete"})


def collect_topology(config):
    policy = source_policy(config)
    source_denied = False
    limits = config["limits"]
    started = time.monotonic()
    deadline = started + max(0.1, config.get("work_timeout_ms", max(100, int(limits["remote_timeout_seconds"] * 800))) / 1000)
    counters = {"entries": 0, "repositories": 0, "manifests": 0, "bytes": 0, "files": 0, "duration_ms": 0, "children": 0, "config_files": 0, "config_keys": 0}
    failures, repositories = [], []
    opaque = lambda value: hashlib.sha256(value.encode("utf-8")).hexdigest() if value is not None else None
    kernel_wsl = platform.system() == "Linux" and "microsoft" in platform.release().lower()
    distro = None
    if kernel_wsl:
        try:
            with open("/etc/os-release", encoding="utf-8") as source:
                for line in source:
                    if line.startswith("ID="):
                        candidate = line.split("=", 1)[1].strip().strip("\"'")
                        if candidate and all(c.isascii() and (c.isalnum() or c in "._-") for c in candidate):
                            distro = candidate
                        break
        except OSError:
            pass
    runtime = {"kind": "wsl" if kernel_wsl else "native", "platform": "posix", "kernel_wsl": kernel_wsl, "distro": distro}
    result = {"schema": "katala_topology_capture.v1", "scope_id": config["scope_id"], "adapter": "remote", "observed_at_ms": config["observed_at_ms"], "limits": limits,
              "target_identity": opaque(socket.gethostname().lower().removesuffix(".local")), "runtime": runtime,
              "repositories": repositories, "failures": failures, "counters": counters, "progress": None}

    def failure(code, message, repo=None):
        item = {"code": code, "message": message}
        collection = failures if repo is None else repo["failures"]
        if item not in collection and len(collection) < 64:
            collection.append(item)

    def check():
        if time.monotonic() >= deadline:
            failure("timeout", "probe time budget exceeded")
            return False
        return True

    def finish():
        enforce_sources(result, policy, source_denied)
        counters["duration_ms"] = int((time.monotonic() - started) * 1000)
        return result

    if policy is None:
        return finish()

    expected = config.get("expected_runtime", "native")
    if (config.get("expected_identity_hash") and config["expected_identity_hash"] != result["target_identity"]) or expected != runtime["kind"] or (expected == "wsl" and (not distro or distro != config.get("expected_distro"))):
        failure("permission_denied", "runtime identity mismatch")
        return finish()

    def git(path, arguments):
        if not check():
            return None
        env = {key: value for key, value in os.environ.items() if not key.upper().startswith("GIT_")}
        env.update(GIT_OPTIONAL_LOCKS="0", GIT_NO_LAZY_FETCH="1", GIT_TERMINAL_PROMPT="0", GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
        counters["children"] += 1
        child = None
        try:
            child = subprocess.Popen([config.get("git_program") or "/usr/bin/git", "--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-C", path] + arguments,
                                     stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env)
            output, until = bytearray(), min(deadline, time.monotonic() + 3)
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    remaining = until - time.monotonic()
                    if remaining <= 0:
                        child.kill(); child.wait(); failure("timeout", "git read timed out"); return None
                    for key, _ in selector.select(min(remaining, 0.1)):
                        chunk = os.read(key.fileobj.fileno(), min(8193 - len(output), 4096))
                        if not chunk:
                            selector.unregister(key.fileobj)
                        else:
                            output.extend(chunk)
                            if len(output) > 8192:
                                child.kill(); child.wait(); failure("limit_reached", "git metadata output exceeded"); return None
            code = child.wait(timeout=max(0.01, until - time.monotonic()))
            return output.decode("utf-8").strip() if code == 0 else None
        except FileNotFoundError:
            failure("tool_missing", "git executable unavailable")
        except (OSError, UnicodeError, subprocess.TimeoutExpired):
            failure("parse_error", "git metadata unavailable")
        finally:
            if child is not None:
                if child.poll() is None:
                    child.kill(); child.wait()
                child.stdout.close()
        return None

    def origin(value):
        if not value:
            return None
        value = value.split("#", 1)[0].split("?", 1)[0]
        if "://" in value:
            value = value.split("://", 1)[1].rsplit("@", 1)[-1]
        elif "@" in value and ":" in value:
            host, path = value.rsplit("@", 1)[-1].split(":", 1); value = host + "/" + path
        if "/" not in value or value.startswith(("/", ".")) or "\\" in value:
            return None
        host, path = value.split("/", 1); path = path.rstrip("/").removesuffix(".git")
        return host.lower() + "/" + path if host and path and "@" not in host and ":" not in host else None

    def safe_relative(value, entry=False):
        if not isinstance(value, str) or any(unicodedata.category(c) == "Cc" for c in value) or "\\" in value or ":" in value:
            return False
        if entry:
            return value == "" or (value not in {".", ".."} and "/" not in value)
        return value == "." or (bool(value) and not value.startswith("/") and all(p not in {"", ".", ".."} for p in value.split("/")))

    def cursor_valid(cursor, roots_count):
        if not isinstance(cursor, dict) or set(cursor) != {"version", "fingerprint", "next_root", "frames"}:
            return False
        if type(cursor["version"]) is not int or cursor["version"] != 1 or type(cursor["next_root"]) is not int or not 0 <= cursor["next_root"] <= roots_count:
            return False
        if not isinstance(cursor["fingerprint"], str) or len(cursor["fingerprint"]) != 64 or any(c not in "0123456789abcdef" for c in cursor["fingerprint"]):
            return False
        frames = cursor["frames"]
        if not isinstance(frames, list) or len(frames) > min(64, limits["depth"] + 1) or (cursor["next_root"] == roots_count and frames):
            return False
        if len(json.dumps(cursor, separators=(",", ":"), ensure_ascii=True).encode()) > 8192:
            return False
        for i, frame in enumerate(frames):
            if not isinstance(frame, dict) or set(frame) != {"dir", "after"} or not safe_relative(frame["dir"]) or not safe_relative(frame["after"], True):
                return False
            if frame["dir"].count("/") + (frame["dir"] != ".") > limits["depth"]:
                return False
            if i == 0 and frame["dir"] != ".":
                return False
            if i and frame["dir"] != (frames[i-1]["dir"] + "/" if frames[i-1]["dir"] != "." else "") + frames[i-1]["after"]:
                return False
        return True

    roots = ["/" + os.path.abspath(root).lstrip("/") for root in config.get("roots", [])]
    if not roots or len(roots) > limits["roots_per_environment"]:
        failure("unsupported", "invalid configured root count"); return finish()
    root_stamps = []
    for root in roots:
        try:
            info = os.lstat(root)
            root_stamps.append([root, info.st_dev, info.st_ino, info.st_mtime_ns])
        except OSError:
            root_stamps.append([root, None])
    fingerprint = opaque(json.dumps({"version": 1, "roots": root_stamps, "skip": sorted(SKIP), "parser": PARSER_VERSION,
                                    "limits": limits, "configs": config.get("configs", []), "source_allowlist": config.get("source_allowlist"), "hash_mode": "hash_json_typed_v1"}, sort_keys=True, separators=(",", ":")))
    cursor = config.get("cursor")
    if cursor is not None and not cursor_valid(cursor, len(roots)):
        failure("permission_denied", "invalid continuation cursor"); return finish()
    if cursor is not None and cursor["fingerprint"] != fingerprint:
        failure("unsupported", "continuation cursor invalidated")
        cursor = None
    root_index = cursor["next_root"] if cursor else 0
    frames = [dict(frame) for frame in cursor["frames"]] if cursor else []
    initial_frames = [dict(frame) for frame in frames]
    initial_root = root_index
    repo_cache, listing_cache, active = {}, {}, set()
    stop, stalled = False, False

    def progress():
        return {"version": 1, "fingerprint": fingerprint, "next_root": root_index, "frames": [dict(frame) for frame in frames]} if root_index < len(roots) else None

    def fits():
        result["progress"] = progress()
        # Reserve one static failure and final timing digits before accepting data.
        return len(json.dumps(result, separators=(",", ":"), ensure_ascii=True).encode()) + 192 <= limits["stdout_bytes"]

    def under_root(root, relative):
        path = root
        if os.path.islink(root) or os.path.realpath(root) != root:
            return None
        for part in ([] if relative == "." else relative.split("/")):
            path = os.path.join(path, part)
            if os.path.islink(path):
                return None
        return path if os.path.commonpath([root, os.path.abspath(path)]) == root else None

    def repo_for(root, relative):
        nonlocal stop
        pieces = [] if relative == "." else relative.split("/")
        for count in range(len(pieces), -1, -1):
            base_relative = "/".join(pieces[:count]) or "."
            path = under_root(root, base_relative)
            if path is None:
                continue
            marker = os.path.join(path, ".git")
            if not os.path.lexists(marker) or os.path.islink(marker):
                continue
            key = (root_index, base_relative)
            if key in repo_cache:
                return repo_cache[key]
            if counters["repositories"] >= limits["repositories_per_environment"]:
                failure("limit_reached", "repository budget exceeded"); stop = True; return None
            private = git(path, ["rev-parse", "--absolute-git-dir"])
            common = git(path, ["rev-parse", "--path-format=absolute", "--git-common-dir"])
            repo = {"root_fingerprint": opaque(root), "relative_path": base_relative,
                    "git": {"origin": origin(git(path, ["config", "--local", "--get", "remote.origin.url"])), "head": git(path, ["rev-parse", "--verify", "HEAD"]),
                            "git_dir": opaque(private), "common_dir": opaque(common), "is_worktree": bool(private and common and private != common)},
                    "manifests": [], "configs": [], "failures": []}
            if not private or not common:
                failure("parse_error", "repository metadata unavailable", repo)
            if root_index == initial_root and any(frame["dir"] == base_relative and (frame["after"] or i < len(initial_frames) - 1) for i, frame in enumerate(initial_frames)):
                failure("limit_reached", "continued-fragment", repo)
            repositories.append(repo); counters["repositories"] += 1
            if not fits():
                repositories.pop(); counters["repositories"] -= 1
                failure("limit_reached", "stdout metadata budget exceeded"); stop = True; return None
            repo_cache[key] = repo; active.add(key)
            return repo
        return None

    def read_file(path, maximum, repo=None):
        nonlocal stop
        if not check():
            stop = True; return None
        budget = max(0, limits["total_bytes_per_environment"] - counters["bytes"])
        try:
            info = os.lstat(path)
            if not stat.S_ISREG(info.st_mode):
                failure("unsupported", "non regular input rejected", repo); return None
            if info.st_size > maximum:
                failure("limit_reached", "file budget exceeded; narrow root or raise limit", repo); return None
            if info.st_size > budget:
                if counters["bytes"]:
                    failure("limit_reached", "total read budget exceeded"); stop = True
                else:
                    failure("limit_reached", "stalled_directory: file exceeds total budget; narrow root or raise limit", repo)
                return None
            with os.fdopen(os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)), "rb") as source:
                if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                    failure("unsupported", "non regular input rejected", repo); return None
                data = source.read(min(maximum, budget))
                counters["files"] += 1; counters["bytes"] += len(data)
                after = os.fstat(source.fileno())
                if after.st_size != info.st_size or after.st_mtime_ns != info.st_mtime_ns:
                    failure("parse_error", "input changed during read", repo); return None
            if len(data) > min(maximum, budget):
                failure("limit_reached", "file changed beyond read budget", repo); return None
            return data
        except OSError:
            failure("permission_denied", "input unreadable", repo); return None

    while root_index < len(roots) and not stop:
        root = roots[root_index]
        if not os.path.isdir(root) or os.path.islink(root) or os.path.realpath(root) != root:
            failure("permission_denied", "root missing unreadable or symlink"); root_index += 1; frames = []; continue
        if not frames:
            frames = [{"dir": ".", "after": ""}]
        while frames and not stop:
            if not check():
                stop = True; break
            current = frames[-1]; relative = current["dir"]; directory = under_root(root, relative)
            if directory is None or not os.path.isdir(directory):
                failure("permission_denied", "continuation directory missing or symlink"); stop = True; break
            repo = repo_for(root, relative)
            if stop:
                break
            cache_key = (root_index, relative)
            if cache_key in listing_cache:
                names = listing_cache[cache_key]
            else:
                names = []
                try:
                    with os.scandir(directory) as iterator:
                        for entry in iterator:
                            if not check():
                                stop = True; break
                            if counters["entries"] >= limits["entries_per_environment"]:
                                failure("limit_reached", "entry budget exceeded"); stop = True
                                if len(names) >= limits["entries_per_environment"]:
                                    failure("limit_reached", "stalled_directory: narrow root or raise entry limit"); stalled = True
                                break
                            counters["entries"] += 1; names.append(entry.name)
                except OSError:
                    failure("permission_denied", "directory unreadable", repo); frames.pop(); continue
                if stop:
                    break
                names.sort()
                listing_cache[cache_key] = names
            pending = [name for name in names if name > current["after"]]
            for name in pending:
                if not safe_relative(name, True):
                    failure("unsupported", "unsupported directory entry name", repo); continue
                path = os.path.join(directory, name)
                try:
                    info = os.lstat(path)
                except OSError:
                    failure("permission_denied", "entry unreadable", repo); current["after"] = name; continue
                if stat.S_ISLNK(info.st_mode):
                    current["after"] = name; continue
                if stat.S_ISDIR(info.st_mode):
                    if name in SKIP:
                        current["after"] = name; continue
                    if len(frames) - 1 >= limits["depth"] or len(frames) >= 64:
                        failure("limit_reached", "depth budget exceeded", repo); current["after"] = name; continue
                    previous = current["after"]; current["after"] = name
                    child = (relative + "/" if relative != "." else "") + name
                    frames.append({"dir": child, "after": ""})
                    if len(json.dumps(progress(), separators=(",", ":"), ensure_ascii=True).encode()) > 8192 or not fits():
                        frames.pop(); current["after"] = previous; failure("limit_reached", "stalled_directory: cursor metadata budget; narrow root"); stop = True; stalled = True
                    break
                if repo is not None and stat.S_ISREG(info.st_mode):
                    base = os.path.join(root, repo["relative_path"])
                    repo_relative = os.path.relpath(path, base).replace(os.sep, "/")
                    kind = {"Cargo.toml": "cargo", "package.json": "node", "pyproject.toml": "python", "go.mod": "go"}.get(name)
                    if repo_relative.startswith(".github/workflows/") and repo_relative.count("/") == 2 and name.endswith((".yml", ".yaml")):
                        kind = "workflow"
                    if kind:
                        if counters["manifests"] >= limits["manifests_per_environment"]:
                            failure("limit_reached", "manifest budget exceeded"); stop = True; break
                        data = read_file(path, min(limits["file_bytes"], limits["workflow_bytes"]) if kind == "workflow" else limits["file_bytes"], repo)
                        if stop:
                            break
                        if data is not None:
                            if not admitted_source(policy, repo["root_fingerprint"], repo, repo_relative, data):
                                source_denied = True; current["after"] = name; continue
                            try:
                                content = data.decode("utf-8", "strict")
                            except UnicodeError:
                                failure("parse_error", "manifest is not utf-8", repo); current["after"] = name; continue
                            item = {"relative_path": repo_relative, "kind": kind, "content": content, "content_sha256": hashlib.sha256(data).hexdigest(), "revision": repo["git"]["head"]}
                            repo["manifests"].append(item); counters["manifests"] += 1
                            if not fits():
                                repo["manifests"].pop(); counters["manifests"] -= 1
                                if counters["manifests"]:
                                    failure("limit_reached", "stdout byte budget exceeded"); stop = True; break
                                failure("limit_reached", "stalled_directory: manifest stdout budget; narrow root or raise limit", repo)
                current["after"] = name
            else:
                key = (root_index, relative)
                active.discard(key)
                frames.pop()
                continue
            # A child frame was pushed or a budget stopped at the unprocessed entry.
        if not frames:
            root_index += 1
    for key in active:
        failure("limit_reached", "continued-fragment", repo_cache[key])
    if stop and not stalled and not any(x["code"] in {"limit_reached", "timeout"} for x in failures):
        failure("limit_reached", "collection incomplete")
    result["progress"] = progress()
    # Each configuration source has its own opaque checkout scope; no literal value leaves the probe.
    config_key_attempts = 0
    for selection_index, selection in enumerate(config.get("configs", [])):
        if selection_index >= limits["manifests_per_environment"]:
            failure("limit_reached", "configuration file budget exceeded")
            break
        if not check():
            break
        path = selection.get("path", "")
        if not os.path.isabs(path) or selection.get("format", "json") != "json":
            failure("unsupported", "configuration format or path unsupported"); continue
        if counters["repositories"] >= limits["repositories_per_environment"]:
            failure("limit_reached", "configuration repository budget exceeded")
            break
        path_id = opaque(os.path.abspath(path))
        repo = {"root_fingerprint": opaque(config["scope_id"] + "|configuration|" + path_id), "relative_path": "configuration-" + path_id,
                "git": {"origin": None, "head": None, "git_dir": None, "common_dir": None, "is_worktree": False}, "manifests": [], "configs": [], "config_file_read": False, "failures": []}
        data = read_file(path, limits["file_bytes"], repo)
        if data is None:
            failure("parse_error", "selected configuration unavailable", repo)
        else:
            repo["config_file_read"] = True
            counters["config_files"] += 1
            try:
                value = json.loads(data)
                for key in selection.get("keys", []):
                    if config_key_attempts >= limits["manifests_per_environment"]:
                        failure("limit_reached", "configuration key budget exceeded")
                        failure("limit_reached", "configuration key budget exceeded", repo)
                        break
                    config_key_attempts += 1
                    pointer = key.get("pointer", "")
                    if key.get("mode", "hash") not in {"hash", "hash_json_typed_v1"} or not isinstance(pointer, str) or (pointer and not pointer.startswith("/")):
                        failure("permission_denied", "literal configuration rejected", repo); continue
                    current = value
                    for component in pointer.split("/")[1:] if pointer else []:
                        component = component.replace("~1", "/").replace("~0", "~")
                        if isinstance(current, list) and (not component.isdigit() or (len(component) > 1 and component.startswith("0"))):
                            raise ValueError("invalid pointer")
                        current = current[int(component)] if isinstance(current, list) else current[component]
                    item = {"relative_path": path_id, "key": pointer, "value_hash": hashlib.sha256(canonical_json_typed_v1(current)).hexdigest(), "mode": "hash_json_typed_v1"}
                    repo["configs"].append(item)
                    counters["config_keys"] += 1
            except (ValueError, KeyError, IndexError, TypeError, UnicodeError, OverflowError):
                failure("parse_error", "selected configuration unavailable", repo)
        repositories.append(repo)
        counters["repositories"] += 1
        if not fits():
            removed = repositories.pop()
            counters["repositories"] -= 1
            counters["config_files"] -= int(removed["config_file_read"])
            counters["config_keys"] -= len(removed["configs"])
            failure("limit_reached", "configuration stdout budget exceeded"); break
    # Recheck HEAD after all reads. Exact approved byte buffers are the only text admitted.
    for repo in repositories:
        approved = policy.get((repo["root_fingerprint"], repo["relative_path"]))
        if approved is None:
            source_denied = True
            break
        path = under_root(roots[approved[0]["root_index"]], repo["relative_path"])
        if path is None or git(path, ["rev-parse", "--verify", "HEAD"]) != approved[0]["expected_head"]:
            source_denied = True
            break
    finish()
    # Empty metadata itself must fit; requests below this envelope are rejected by Rust.
    if len(json.dumps(result, separators=(",", ":"), ensure_ascii=True).encode()) > limits["stdout_bytes"]:
        repositories.clear(); counters["repositories"] = 0; counters["manifests"] = 0; counters["config_files"] = 0; counters["config_keys"] = 0; result["progress"] = None
        failures[:] = [{"code": "limit_reached", "message": "stalled_directory: stdout metadata budget; raise limit"}]
    return result


print(json.dumps(collect_topology(CONFIG), separators=(",", ":"), ensure_ascii=True))
