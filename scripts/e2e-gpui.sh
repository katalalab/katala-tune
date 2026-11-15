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

# 常時: 変更操作の確認欄が描け、「やめる」で進まないこと（何も実行しない）。
# 承認の判定そのもの（承認・取り消し・時間切れ・置き換え、Engine との結合）は単体テスト（gate.rs）で確かめる。
conf=$("${CARGO_TARGET_DIR:-$repo/target}/release/katala-tune-gpui" --selftest --confirm-demo --config "$repo/config/nodes.example.json" --data "$tmp/confirm-data" | grep "^selftest confirm")
echo "$conf"
[ "$conf" = "selftest confirm shown=true approved=false" ] || { echo "E2E 失敗: 確認欄 ($conf)" >&2; exit 1; }
echo "e2e-gpui confirm OK"
