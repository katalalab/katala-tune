#!/usr/bin/env bash
# tune-gpui の E2E: 単体テスト → 隔離したデータで画面を起動（selftest）→ 台帳の機体数と一致を確かめる。
# 実機の台帳・DB には触れない（--config / --data で一時領域を使う）。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test -p tune-gpui --release
cargo build -p tune-gpui --release
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
n=$(python3 -c 'import json,sys;print(len(json.load(open("config/nodes.example.json"))["nodes"]))')
out=$(./target/release/katala-tune --selftest --config config/nodes.example.json --data "$tmp/data")
echo "$out"
[ "$out" = "selftest ok nodes=$n" ] || { echo "E2E 失敗: 期待 nodes=$n" >&2; exit 1; }
echo "e2e-gpui OK"
