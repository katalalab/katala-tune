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
#   platform: パッケージマネージャ・ランタイム管理そのもの（Homebrew・rustup/cargo・nvm・rbenv・pyenv・Nix・colima・Docker）
import json, os, plistlib, glob, time

HOME = os.path.expanduser("~")
t0 = time.time()
items, errors, failed = [], [], []


def add(source, name, version=None, explicit=True, **extra):
    it = {"source": source, "name": name, "version": version, "explicit": bool(explicit)}
    it.update({k: v for k, v in extra.items() if v is not None})
    items.append(it)


def listdir(p):
    # 無いのは「入っていない」。読めない（権限など）は失敗として上に投げる（空と見なすと、全部が削除に見える）
    try:
        return sorted(e for e in os.listdir(p) if not e.startswith("."))
    except (FileNotFoundError, NotADirectoryError):
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
            # macOS 付属のアプリ（Safari など）は /System 配下への参照なので数えない。Xcode・Final Cut など Apple 製でも自分で入れたものは数える
            if name in seen or os.path.realpath(app).startswith("/System/"):
                continue
            seen.add(name)
            info = {}
            try:
                with open(os.path.join(app, "Contents", "Info.plist"), "rb") as f:
                    info = plistlib.load(f)
            except Exception:
                pass
            bid = info.get("CFBundleIdentifier")
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


def platform():
    checks = {
        "homebrew": ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"],
        "rustup": [os.path.join(HOME, ".cargo", "bin", "rustup")],
        "cargo": [os.path.join(HOME, ".cargo", "bin", "cargo")],
        "nvm": [os.path.join(HOME, ".nvm", "nvm.sh")],
        "rbenv": [os.path.join(HOME, ".rbenv")],
        "pyenv": [os.path.join(HOME, ".pyenv")],
        "nix": ["/nix/store"],
    }
    for name, paths in checks.items():
        if any(os.path.exists(p) for p in paths):
            add("platform", name)


# 取り方ごとに、どの種類を返すか。失敗した取り方の種類は failed_sources に入れ、呼び出し側はその種類を「削除」と見なさない
COLLECTORS = ((brew, ("brew", "cask")), (apps, ("app",)), (mise, ("mise",)), (uv_cargo, ("uv", "cargo")), (npm_global, ("npm",)),
              (loose_bins, ("bin",)), (platform, ("platform",)))
for fn, sources in COLLECTORS:
    before = len(items)
    try:
        fn()
    except Exception as e:  # 1つの取り方が壊れても他は返す
        del items[before:]
        errors.append(f"{fn.__name__}: {e}")
        failed.extend(sources)

print(json.dumps({"os": "macos", "items": items, "errors": errors, "failed_sources": failed, "elapsed_s": round(time.time() - t0, 2)}, ensure_ascii=False))
