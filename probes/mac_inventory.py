# 道具の棚卸し（macOS）。読み取り専用・標準ライブラリのみ。結果を JSON 1行で出す。
# パッケージマネージャのコマンドは起動せず、インストール先を直接読む（速く、ネットワークに出ない）。
#   brew:  Cellar/<名前>/<版>/INSTALL_RECEIPT.json（installed_on_request で「自分で入れた」かを見る）
#   cask:  Caskroom/<名前>/<版>
#   app:   /Applications・~/Applications の .app（Info.plist の版と bundle id。App Store 由来か）
#   mise:  ~/.local/share/mise/installs/<道具>/<版>
#   uv:    ~/.local/share/uv/tools/<名前>
#   cargo: ~/.cargo/bin
#   npm:   グローバルの node_modules（mise の node も含む）
#   bin:   パッケージマネージャを通さずに置いた CLI（~/.local/bin・~/.bun/bin・~/go/bin・~/.deno/bin・/usr/local/bin）。
#          Homebrew へのリンク・退避ファイル（.bak など拡張子つき）は数えない。~/bin は個人のスクリプト置き場なので見ない
import json, os, plistlib, glob, time

HOME = os.path.expanduser("~")
t0 = time.time()
items, errors = [], []


def add(source, name, version=None, explicit=True, **extra):
    it = {"source": source, "name": name, "version": version, "explicit": bool(explicit)}
    it.update({k: v for k, v in extra.items() if v is not None})
    items.append(it)


def listdir(p):
    try:
        return sorted(e for e in os.listdir(p) if not e.startswith("."))
    except OSError:
        return []


def newest(dirpath):
    vs = [v for v in listdir(dirpath) if os.path.isdir(os.path.join(dirpath, v))]
    return max(vs, key=lambda v: os.path.getmtime(os.path.join(dirpath, v))) if vs else None


def brew():
    for prefix in ("/opt/homebrew", "/usr/local"):
        cellar = os.path.join(prefix, "Cellar")
        for name in listdir(cellar):
            ver = newest(os.path.join(cellar, name))
            explicit = True
            try:
                with open(os.path.join(cellar, name, ver or "", "INSTALL_RECEIPT.json")) as f:
                    explicit = bool(json.load(f).get("installed_on_request", True))
            except (OSError, ValueError):
                pass
            add("brew", name, ver, explicit)
        for name in listdir(os.path.join(prefix, "Caskroom")):
            add("cask", name, newest(os.path.join(prefix, "Caskroom", name)))


def apps():
    seen = set()
    for root in ("/Applications", os.path.join(HOME, "Applications"), "/Applications/Utilities"):
        for app in glob.glob(os.path.join(root, "*.app")):
            name = os.path.basename(app)[:-4]
            if name in seen:
                continue
            seen.add(name)
            info = {}
            try:
                with open(os.path.join(app, "Contents", "Info.plist"), "rb") as f:
                    info = plistlib.load(f)
            except Exception:
                pass
            bid = info.get("CFBundleIdentifier")
            # macOS 付属のアプリは数えない（/System/Applications にあり、ここに来るのは一部の例外だけ）
            if isinstance(bid, str) and bid.startswith("com.apple.") and root != os.path.join(HOME, "Applications"):
                continue
            mas = os.path.exists(os.path.join(app, "Contents", "_MASReceipt"))
            add("app", name, info.get("CFBundleShortVersionString") or info.get("CFBundleVersion"), True,
                id=bid if isinstance(bid, str) else None, store="mas" if mas else None)


def mise():
    root = os.path.join(os.environ.get("MISE_DATA_DIR", os.path.join(HOME, ".local", "share", "mise")), "installs")
    for tool in listdir(root):
        vers = [v for v in listdir(os.path.join(root, tool)) if v[:1].isdigit()]
        if vers:
            add("mise", tool, max(vers, key=lambda v: [int(x) if x.isdigit() else 0 for x in v.split(".")]), True)


def uv_cargo():
    for name in listdir(os.path.join(HOME, ".local", "share", "uv", "tools")):
        add("uv", name)
    for name in listdir(os.path.join(HOME, ".cargo", "bin")):
        if name not in ("cargo", "rustc", "rustup", "rustdoc", "rust-gdb", "rust-lldb", "rustfmt", "cargo-fmt", "cargo-clippy", "clippy-driver", "rls", "rust-analyzer"):
            add("cargo", name)


def npm_global():
    roots = ["/opt/homebrew/lib/node_modules", "/usr/local/lib/node_modules"]
    roots += glob.glob(os.path.join(HOME, ".local", "share", "mise", "installs", "node", "*", "lib", "node_modules"))
    seen = set()
    for root in roots:
        for name in listdir(root):
            pkgs = [os.path.join(name, s) for s in listdir(os.path.join(root, name))] if name.startswith("@") else [name]
            for pkg in pkgs:
                if pkg in seen or pkg in ("npm", "corepack"):
                    continue
                seen.add(pkg)
                ver = None
                try:
                    with open(os.path.join(root, pkg, "package.json")) as f:
                        ver = json.load(f).get("version")
                except (OSError, ValueError):
                    pass
                add("npm", pkg, ver)


def loose_bins():
    seen = set()
    for root in (os.path.join(HOME, ".local", "bin"), os.path.join(HOME, ".bun", "bin"), os.path.join(HOME, "go", "bin"),
                 os.path.join(HOME, ".deno", "bin"), "/usr/local/bin"):
        for name in listdir(root):
            p = os.path.join(root, name)
            if name in seen or "." in name or not os.access(p, os.X_OK) or os.path.isdir(p):
                continue
            real = os.path.realpath(p)
            if "/Cellar/" in real or "/Caskroom/" in real or "/node_modules/" in real or "/mise/installs/" in real or "/uv/tools/" in real:
                continue
            seen.add(name)
            add("bin", name)


for fn in (brew, apps, mise, uv_cargo, npm_global, loose_bins):
    try:
        fn()
    except Exception as e:  # 1つの取り方が壊れても他は返す
        errors.append(f"{fn.__name__}: {e}")

print(json.dumps({"os": "macos", "items": items, "errors": errors, "elapsed_s": round(time.time() - t0, 2)}, ensure_ascii=False))
