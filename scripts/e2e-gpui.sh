#!/usr/bin/env bash
# tune-gpui の起動smoke: 単体テスト → 隔離データで起動 → 台帳の機体数を確かめる。
# 描画完了、画面操作、収集から保存までのGUI受入は別に行う。
# 実機の台帳・DB には触れない（--config / --data で一時領域を使う）。
set -euo pipefail
repo=$(dirname "$(dirname "$(realpath "$0")")")
cargo test --manifest-path "$repo/Cargo.toml" -p tune-gpui --release --locked
cargo build --manifest-path "$repo/Cargo.toml" -p tune-gpui --release --locked
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
n=$(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1]))["nodes"]))' "$repo/config/nodes.example.json")
out=$("${CARGO_TARGET_DIR:-$repo/target}/release/katala-tune-gpui" --selftest --config "$repo/config/nodes.example.json" --data "$tmp/data")
echo "$out"
[ "$out" = "selftest ok nodes=$n" ] || { echo "E2E 失敗: 期待 nodes=$n" >&2; exit 1; }
echo "e2e-gpui OK"
