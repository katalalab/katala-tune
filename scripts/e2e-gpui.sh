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
bin="${CARGO_TARGET_DIR:-$repo/target}/release/katala-tune-gpui"
out=$("$bin" --selftest --config "$repo/config/nodes.example.json" --data "$tmp/data" | grep "^selftest ok")
echo "$out"
[ "$out" = "selftest ok nodes=$n" ] || { echo "E2E 失敗: 期待 nodes=$n" >&2; exit 1; }
echo "e2e-gpui OK"

# 任意: E2E_LIVE_NODE=<id> を付けると、その機体のライブ（1 秒ごとのサンプル）が 8 秒で数点届くことも確かめる。
# 台帳は実機のものを読むだけで、データは一時領域。
if [ -n "${E2E_LIVE_NODE:-}" ]; then
  live=$("${CARGO_TARGET_DIR:-$repo/target}/release/katala-tune-gpui" --selftest --live "$E2E_LIVE_NODE" --config "${E2E_CONFIG:?set E2E_CONFIG to an explicit ledger}" --data "$tmp/live-data")
  echo "$live"
  pts=$(echo "$live" | sed -n 's/^selftest live points=//p')
  [ "${pts:-0}" -ge 3 ] || { echo "E2E 失敗: ライブの点が $pts 点（3 点以上が必要）" >&2; exit 1; }
  echo "e2e-gpui live OK"
fi

# 任意: E2E_LOGS=1 で、明示したプレビューDBの保存済みログが画面モデルまで届くことを確かめる。
if [ -n "${E2E_LOGS:-}" ]; then
  out=$("${CARGO_TARGET_DIR:-$repo/target}/release/katala-tune-gpui" --selftest --logs --config "${E2E_CONFIG:?set E2E_CONFIG to an explicit ledger}" --data "${E2E_DATA:?set E2E_DATA to a preview database directory}")
  echo "$out"
  echo "$out" | grep -q "^selftest logs rows=" || { echo "E2E 失敗: ログの読み出し" >&2; exit 1; }
  echo "e2e-gpui logs OK"
fi

# 任意: E2E_PAGES=1 で、各画面（電力・実行記録・状態・構成図・道具・AI・スペック比較・セキュリティ・プロセス）のデータが、
# 明示したプレビューDBの保存済みから読めることを確かめる（読み取りだけ）。実機のDBを直接使わず、複製を E2E_DATA に指定する。
if [ -n "${E2E_PAGES:-}" ]; then
  for pair in power:power actions:actions status:status topology:topology tools:tools ai:ai specs:specs security:security processes:procs; do
    pg=${pair%%:*}; key=${pair##*:}
    n=$("$bin" --selftest --open "$pg" --config "${E2E_CONFIG:?set E2E_CONFIG to an explicit ledger}" --data "${E2E_DATA:?set E2E_DATA to a preview database directory}" | grep "^selftest page" | grep -o "$key=[0-9]*" | cut -d= -f2)
    [ "${n:-0}" -ge 1 ] || { echo "E2E 失敗: $pg のデータが読めない（$n）" >&2; exit 1; }
    echo "page $pg rows=$n"
  done
  echo "e2e-gpui pages OK"
fi

# 任意: E2E_NETWORK_NODE=<id> で、その機体への受動の到達性確認が結果を返すことを確かめる（読み取りだけ。ローカルの機体を指定する）。
if [ -n "${E2E_NETWORK_NODE:-}" ]; then
  out=$("$bin" --selftest --network "$E2E_NETWORK_NODE" --config "${E2E_CONFIG:?set E2E_CONFIG to an explicit ledger}" --data "$tmp/net-data" | grep "^selftest network")
  echo "$out"
  [ "$out" = "selftest network ok_rows=1" ] || { echo "E2E 失敗: ネットワーク確認 ($out)" >&2; exit 1; }
  echo "e2e-gpui network OK"
fi

# 常時: 変更操作の確認欄が描け、「やめる」で進まないこと（何も実行しない）。承認の判定そのものは単体テスト（gate.rs）で確かめる。
conf=$("$bin" --selftest --confirm-demo --config "$repo/config/nodes.example.json" --data "$tmp/confirm-data" | grep "^selftest confirm")
echo "$conf"
[ "$conf" = "selftest confirm shown=true approved=Some(false)" ] || { echo "E2E 失敗: 確認欄 ($conf)" >&2; exit 1; }
echo "e2e-gpui confirm OK"

# 常時: レビュー画面が見本の完了台帳（task-completion.v1）を読めること。壊れた入力は断る（単体テストで網羅）。
rv=$("$bin" --selftest --review "$repo/test/fixtures/review/task-completion.json" --config "$repo/config/nodes.example.json" --data "$tmp/review-data" | grep "^selftest page" | grep -o "review=[0-9]*")
echo "$rv"
[ "$rv" = "review=2" ] || { echo "E2E 失敗: レビュー ($rv)" >&2; exit 1; }
echo "e2e-gpui review OK"
