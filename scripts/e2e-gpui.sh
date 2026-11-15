#!/usr/bin/env bash
# tune-gpui の保存データsmoke: 単体テスト → 隔離データで起動 → 機体数と全13画面の読込を確かめる。
# 画面操作・IME・収集から保存までのGUI受入は別に行う。
# 実機の台帳・DB には触れない（--config / --data で一時領域を使う）。
set -euo pipefail
repo=$(dirname "$(dirname "$(realpath "$0")")")
cargo test --manifest-path "$repo/Cargo.toml" -p tune-gpui --release --locked
cargo build --manifest-path "$repo/Cargo.toml" -p tune-gpui --release --locked
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
n=$(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1]))["nodes"]))' "$repo/config/nodes.example.json")
out=$("${CARGO_TARGET_DIR:-$repo/target}/release/katala-tune-gpui" --selftest --config "$repo/config/nodes.example.json" --data "$tmp/data")
echo "$out"
[ "$out" = "selftest ok nodes=$n views=13" ] || { echo "E2E 失敗: 期待 nodes=$n" >&2; exit 1; }
# 合成保存データがある状態でも全ルートを開き、観測値・状態が不変なことを検証。
cargo test --manifest-path "$repo/Cargo.toml" -p tune-gpui --release --locked --test saved_views -- --ignored
echo "e2e-gpui OK"
