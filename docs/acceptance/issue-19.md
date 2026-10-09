# Issue #19 受入試験の記録（mock 範囲）

**判定: 未完了。** mock SSH・一時ディレクトリの偽の対象での確認だけを行った。Issue #19 の検証方法にある「承認済み隔離 host で一つの可逆操作を実行し、対象 ID・前後状態・rollback 結果を照合する」は **未実施**。実 fleet への SSH、launchd / Scheduled Task の変更、startup の変更は一切していない。この記録を Issue を閉じる根拠にしない。

## 対象と環境

| 項目 | 内容 |
|---|---|
| 対象 | katala-tune 0.4.1（main 42504d5 + この PR） |
| 環境 | macOS 26.6 arm64、Node 24.21（Electron 内蔵 Node 24 でも同じ試験を実行）、Rust 1.99 |
| 実行日時 | 2026-10-09 |
| 偽物 | 偽の ssh（台本どおりの結果を返し、呼び出しを数える）、偽の実行器、偽の対象（一時ディレクトリの state ファイル）。実機の ssh は起動しない |

## 条件ごとの期待と実際

試験は Electron 版（`test/acceptance-issue19.test.js`）と Rust 版（`crates/tune-core/tests/acceptance_issue19.rs`）に同じ条件で置いた。

| 条件 | 期待 | 実際（証拠） |
|---|---|---|
| read-only probe が host 別の成功・timeout・unreachable を分ける | host ごとに ok / timeout / unreachable / auth / error が返り、画面用の文言が違う | **修正前は未達**（失敗は生の stderr 文だけで種類が無かった）。`reason`・`reason_text` を足して達成。6 host（成功・打ち切り・名前引き失敗・接続時間切れ・鍵拒否・不正出力）を同時に調べ、種類と文言が分かれ、元のエラー文は残る。`条件1: probe は host 別に…` / `probe_separates_success_timeout_unreachable_and_auth_per_host`、分類表 `失敗の分類表…` / `failure_classification_table_matches_electron` |
| 承認なしでは変更コマンドを実行しない | 承認されない・許可リスト外・共用機・保護対象・台帳なしで、実行器の呼び出しが 0 回 | 達成。呼び出し回数 0、DB に記録なし、対象の状態は変わらない。確認ダイアログ前の拒否では確認すら出ない。`条件2: …`（3 件）/ `nothing_runs_without_approval`・`refused_before_confirmation_never_reaches_the_runner`・`execute_called_directly_…` |
| 承認後も対象同一性不一致なら実行を止める | 承認後の読み直しで対象が変わっていたら実行器 0 回、理由を記録 | **Electron 版は修正前に未達**（承認後の読み直し自体が無かった。Rust 版は接続先の比較があった）。両方で、alias・OS・この機体か・機体の削除・共用機化・保護対象化・台帳破損を承認中に起こし、実行器 0 回・実行記録に「中止（実行していない）: 理由」が残ることを確認。変えていない台帳の書き直しでは実行される。`条件3: …`（3 件）/ `ledger_change_during_confirmation_…`・`protect_list_or_broken_ledger_…`・`unchanged_identity_still_runs` |
| 変更成功/失敗と rollback 結果を実行記録へ残す | 対象 ID・実行後の状態・戻し操作が残り、失敗・rollback 失敗も区別して残る | 達成。偽のタスク（一時ファイル）を止める（Ready → Disabled）→ 元に戻す（Disabled → Ready）を実際に動かし、記録の対象 ID（params）・実行後の状態（output）・戻し操作（undo）が実際の前後状態と一致。実行失敗は失敗として残り戻し操作なし。rollback 失敗は失敗として残り、元の記録は「戻し済み」にならずやり直せる。記録を書けないときは実行しない。`条件4: …`（6 件）/ `reversible_action_on_fake_target_…`・`failed_action_…`・`failed_rollback_…` |
| アプリ再起動後に未完了操作を成功扱いしない | 結果を書く前に止まった操作は、DB を開き直しても「未完了」 | **修正前は未達**（実行後にだけ記録を書くので、途中で止まると記録が無く、止まったことが分からなかった）。実行の直前に未完了の記録を書くようにし、実行器が返らないまま DB を開き直すと `state: incomplete`・`ok: false`、成功にも失敗にも数えず、戻しも出ない。`条件5: …` / `interrupted_action_is_incomplete_after_restart_not_success` |

条件 3 の機体側（プロセス終了スクリプトが、名前・起動時刻・負荷が合わなければ kill に進まない）は、試験が起こした子プロセス（sleep）にだけローカルの sh で実行し、exit 3（名前違い・起動時刻違い）と exit 4（負荷が回復）で止まり、対象が生きていることを確認した（kill に届く経路は動かしていない）。`条件3: プロセス終了スクリプトは…` / `kill_script_stops_before_kill_when_target_identity_or_load_differs`

## 見つけた不具合と修正

1. probe の失敗に種類が無い → `reason`（timeout・unreachable・auth・error）・`reason_text` を追加（`lib/collect.js`・`crates/tune-core/src/collect.rs`）。画面の「分析できませんでした」に文言を足した。
2. Electron 版は承認後に台帳を読み直さず、承認した内容と別の対象に実行しうる → 流れを `lib/runner.js` に切り出し、Rust 版と同じ読み直しを入れた。承認後に止めた理由は実行記録に残す（Rust 版は記録していなかった）。
3. 実行中に止まった操作が記録に残らない → 実行の直前に未完了の記録を書き、結果は同じ記録へ書く。`tune_actions.ok` が NULL の行を「未完了」として扱う（表の形は変えていない）。読み出しに `state`（ok / failed / incomplete）を足し、実行記録の画面に「未完了」を出す。

4. レビュー指摘の 4 件（先に再現する試験を書いてから修正）:
   - この機体（local）の調査・実行が、渡された実行口を迂回して本物のローカル実行に進んでいた → Rust は `Runner` に `local_script`・`local_powershell_file`・`local_python` を足し、`System` だけが本物のローカル実行をする。Electron は `probeNode` の `deps.localShell` を足した。
   - 結果の書き込みが「未更新（false）」を返しても成功として返していた（Rust・Electron 両方）→ 記録失敗として拒否する。
   - 内部エラー・例外で落ちた host の結果に `reason`・`reason_text` が無かった（Rust・Electron 両方）→ `error` を付ける。

## 実行した確認

| コマンド | 結果 |
|---|---|
| `npm test` | 163 件 pass（main 取り込み後。うちこの PR の試験 19） |
| `ELECTRON_RUN_AS_NODE=1 electron --test test/*.test.js`（Electron 内蔵 Node） | 158 件 pass・5 件 fail。fail は release・更新署名の試験で、origin/main 単体でも同じ 5 件が落ちる（この PR と無関係） |
| `cargo fmt --all --check` | 差分なし |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 警告なし |
| `cargo test --workspace --locked` | tune-core 96 件・acceptance_issue19 16 件・parity 系・safety を含め全件 pass |
| `npm run oss-check`（HEAD） | OK |
| `npm run oss-check -- --history` | main の履歴にある GitHub ボットのアドレスで既知 NG（PR #20 待ち）。この変更とは無関係 |

## できなかったこと・残り

- **承認済み隔離 host での実動作確認は未実施**（Issue #19 の完了条件のうち、実 ssh・実 host に触れる部分すべて）。実際の ssh が出す文言（OpenSSH の版・OS・言語による違い）での分類は、実機で確かめていない。分類表の文言は OpenSSH の一般的な出力による。
- 実行記録は、対象 ID（params）・実行後の状態（output）・戻し操作（undo）を残すが、**実行前の状態そのもの**は記録しない（戻し操作が前の状態を表す）。実機で実行前の状態を読む処理は入れていない。
- 「状態」画面の分析チェックは従来どおり生のエラー文の末尾 1 行を出す（`reason_text` は分析画面に出る）。
- Tauri の画面（WebView）での表示（未完了の印・失敗の理由の文言）は、画面の目視確認をしていない。コードの経路とデータの形だけ確認した。
- Windows の PowerShell で実行するスクリプト自体の動作（Disable-ScheduledTask など）は、この機体（macOS）では動かせず未確認。
