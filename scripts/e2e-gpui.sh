#!/usr/bin/env bash
# tune-gpui の E2E: 単体テスト → 隔離したデータで画面を起動（selftest）→ 台帳の機体数と一致を確かめる。
# 実機の台帳・DB には触れない（--config / --data で一時領域を使う）。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test -p tune-gpui --release
cargo build -p tune-gpui --release
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
n=$(python3 -c 'import json,sys;print(len(json.load(open("config/nodes.example.json"))["nodes"]))')
out=$(./target/release/katala-tune --selftest --config config/nodes.example.json --data "$tmp/data" | grep "^selftest ok")
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

# 任意: E2E_PAGES=1 で、各画面（電力・実行記録・状態・構成図・道具・AI）のデータが実機の保存済みから読めることを確かめる（読み取りだけ）。
if [ -n "${E2E_PAGES:-}" ]; then
  for pair in power:power actions:actions status:status topology:topology tools:tools ai:ai specs:specs security:security processes:procs; do
    pg=${pair%%:*}; key=${pair##*:}
    n=$(./target/release/katala-tune --selftest --open "$pg" | grep "^selftest page" | grep -o "$key=[0-9]*" | cut -d= -f2)
    [ "${n:-0}" -ge 1 ] || { echo "E2E 失敗: $pg のデータが読めない（$n）" >&2; exit 1; }
    echo "page $pg rows=$n"
  done
  echo "e2e-gpui pages OK"
fi

# 任意: E2E_NETWORK_NODE=<id> で、その機体への受動の到達性確認が結果を返すことを確かめる（読み取りだけ。ローカルの機体を指定する）。
if [ -n "${E2E_NETWORK_NODE:-}" ]; then
  out=$(./target/release/katala-tune --selftest --network "$E2E_NETWORK_NODE" --data "$tmp/net-data" | grep "^selftest network")
  echo "$out"
  [ "$out" = "selftest network ok_rows=1" ] || { echo "E2E 失敗: ネットワーク確認 ($out)" >&2; exit 1; }
  echo "e2e-gpui network OK"
fi

# 常時: 変更操作の確認欄が描け、「やめる」で進まないこと（何も実行しない）。承認の判定そのものは単体テスト（gate.rs）で確かめる。
conf=$(./target/release/katala-tune --selftest --config config/nodes.example.json --data "$tmp/confirm-data" --confirm-demo | grep "^selftest confirm")
echo "$conf"
[ "$conf" = "selftest confirm shown=true approved=Some(false)" ] || { echo "E2E 失敗: 確認欄 ($conf)" >&2; exit 1; }
echo "e2e-gpui confirm OK"

# 常時: レビュー画面が見本の完了台帳（task-completion.v1）を読めること。壊れた入力は断る（単体テストで網羅）。
rv=$(./target/release/katala-tune --selftest --config config/nodes.example.json --data "$tmp/review-data" --review test/fixtures/review/task-completion.json | grep "^selftest page" | grep -o "review=[0-9]*")
echo "$rv"
[ "$rv" = "review=2" ] || { echo "E2E 失敗: レビュー ($rv)" >&2; exit 1; }
echo "e2e-gpui review OK"
