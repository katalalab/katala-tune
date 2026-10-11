# Safe Cache Accounting Implementation Plan

> このセッション内でexecuting-plansとTDDを使って実行する。追加agentは起動しない。

**Goal:** macOSのVM保存領域のキャッシュ誤分類と親子二重計上を既存snapshotでも防ぐ。

**Architecture:** JS/Rustのrulesで既知path・有効な容量を選別し、親子計測を除重する。保存領域は操作のないinfo findingへ分離する。

**Tech Stack:** Node.js標準test、Rust/serde_json、既存parity runner。依存追加なし。

## 手順とcheckpoint

- [x] baseline: `CARGO_TARGET_DIR="$PWD/target" npm run check`。専用worktree内のtargetのみを使い、既存失敗があれば原因を記録する。
- [x] RED: `test/rules.test.js` と `crates/tune-core/src/rules.rs` のtest moduleに次の合成入力・期待値を追加する。`crates/tune-core/tests/parity.rs` の既存fixturesにも同じ境界例を加える。
  - VM-only: 各保存領域が10 GiBならcache findingなし、合計30 GiBの `vm-storage` のみ、commands/actionなし。
  - 親子: Caches 12 GiB + Homebrew 4 GiBなら12 GiB。逆順でも同じ。親が0 GiBまたは1 GiBのときも子を重ねない。
  - 親欠測: Cachesのgbが未設定/null/文字列/負値ならHomebrew 12 GiBを採用。
  - 不明入力: 未知path 40 GiBや既知pathの非数値・負値はcache findingと削除コマンドに使わない。
  - mixed: Caches 12 GiB + Homebrew 4 GiB + npm 3 GiB + .cache 2 GiB + DerivedData 5 GiB + VM保存領域30 GiB + 未知40 GiBなら候補22 GiB・保存領域30 GiB。
  - 低空き: 保存領域/コンテナの存在だけではDocker prune/CoreSimulator削除を提案しない。削除前の確認を助言する。
- [x] RED確認コマンド: `node --test test/rules.test.js`、`CARGO_TARGET_DIR="$PWD/target" cargo test -p tune-core --lib rules::tests --locked`。失敗を期待値不一致として確認する（構文・環境エラーと区別）。
- [x] GREEN: `lib/rules.js` とRust rulesに完全一致allowlistと有限非負値の選別を実装し、親子排除・別finding・条件付き助言・コマンド制限を対応させる。`docs/safety.md` に分類根拠・削除保証ではないことを追記する。
- [x] targeted GREEN: 上記JS/Rustテストと `CARGO_TARGET_DIR="$PWD/target" cargo test -p tune-core --test parity --locked -- --nocapture` を実行。比較skipは設定しない。
- [x] 全体検証: `CARGO_TARGET_DIR="$PWD/target" npm run check`、`npm run oss-check -- --history`、`git diff --check`。
- 差分を自分でレビューし、許可範囲のみをローカルcommit。履歴OSS検査をcommit後にも行い、SHA・終了コード・未検証・リスクを作業報告へ記録する。push/merge/公開はしない。

## 実施証跡（合成入力のみ）

- baseline: `CARGO_TARGET_DIR="$PWD/target" npm run check` 終了コード0。JSは213件成功、既存のWindows専用12件skip。Rustは208件成功。
- RED: `node --test test/rules.test.js` 終了コード1、追加15件失敗（既存13件成功）。VM-only 30 GiBをキャッシュ扱い、親子12 + 4 GiBを16 GiBと合算、mixedを96 GiBと誤合算した。文字列容量の入力はJSの暗黙変換により `toFixed` のTypeErrorとなることも確認。
- RED: `CARGO_TARGET_DIR="$PWD/target" cargo test -p tune-core --lib rules::tests --locked` 終了コード101。追加2件失敗（既存2件成功）、VM-onlyの誤分類とGiB表記差を確認。
- GREEN: 同じJSコマンドは28件成功、同じRustコマンドは4件成功、いずれも終了コード0。共有fixture13例と不正親の型別ケース、JSのNaN/Infinityを検証。
- parity: `CARGO_TARGET_DIR="$PWD/target" cargo test -p tune-core --test parity --locked -- --nocapture` 終了コード0、2テスト成功。rules.analyzeは4,030入力でJS/Rust全文一致、JS例外による比較除外0件。
- 全体: `CARGO_TARGET_DIR="$PWD/target" npm run check` 終了コード0。JSは228件成功、既存のWindows専用12件skip、失敗0件。Rustは210件成功。fmt/clippy（警告をエラー扱い）も成功。
- OSS: `npm run oss-check -- --history` 終了コード0、既存履歴22コミットに個人・環境情報の検出なし。`git diff --cached --check` も終了コード0。修正commitを含めた履歴検査はcommit後の作業報告に残す。
- 自己レビュー: allowlist完全一致、型検証、親欠測・ゼロ・順序、保存領域のcommands/action欠如、過去snapshotのenrich再評価経路、変更範囲を確認。別agentによるレビューは実施しない。

## 仮定・未検証・rollback

設計文書の合成値は実環境と無関係。既存snapshotのpath表記を完全一致で扱うため、未知表記を安全なキャッシュに広げない。実機E2Eはこの作業で断定しない。戻す場合は修正commitのrevertのみ（本作業では実行しない）。
