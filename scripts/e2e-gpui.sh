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

# 任意: E2E_LIVE_NODE=<id> を付けると、その機体のライブ（1 秒ごとのサンプル）が 8 秒で数点届くことも確かめる。
# 台帳は実機のものを読むだけで、データは一時領域。
if [ -n "${E2E_LIVE_NODE:-}" ]; then
  live=$(./target/release/katala-tune --selftest --live "$E2E_LIVE_NODE" --data "$tmp/live-data")
  echo "$live"
  pts=$(echo "$live" | sed -n 's/^selftest live points=//p')
  [ "${pts:-0}" -ge 3 ] || { echo "E2E 失敗: ライブの点が $pts 点（3 点以上が必要）" >&2; exit 1; }
  echo "e2e-gpui live OK"
fi

# 任意: E2E_LOGS=1 で、保存済みログの読み出し（実機のデータを読むだけ）が画面モデルまで届くことを確かめる。
if [ -n "${E2E_LOGS:-}" ]; then
  out=$(./target/release/katala-tune --selftest --logs)
  echo "$out"
  echo "$out" | grep -q "^selftest logs rows=" || { echo "E2E 失敗: ログの読み出し" >&2; exit 1; }
  echo "e2e-gpui logs OK"
fi

# 任意: E2E_PAGES=1 で、各画面（電力・実行記録・状態・構成図）のデータが実機の保存済みから読めることを確かめる（読み取りだけ）。
if [ -n "${E2E_PAGES:-}" ]; then
  for pair in power:power actions:actions status:status topology:topology; do
    pg=${pair%%:*}; key=${pair##*:}
    n=$(./target/release/katala-tune --selftest --open "$pg" | sed -n "s/.*selftest page .*\b$key=\([0-9]*\).*/\1/p")
    [ "${n:-0}" -ge 1 ] || { echo "E2E 失敗: $pg のデータが読めない（$n）" >&2; exit 1; }
    echo "page $pg rows=$n"
  done
  echo "e2e-gpui pages OK"
fi
