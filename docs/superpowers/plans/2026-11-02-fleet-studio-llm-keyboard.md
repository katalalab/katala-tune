# Fleet Studio LLM keyboard implementation plan

**Goal:** 非 loopback の表示中接続先を、明示確認と許可世代を保ちながらキーボードで選択・有効化・取消できるようにする。

**Design:** `Endpoint` 全体と選択時の `ModelGrant` を保持する独立した LLM 選択状態を追加し、現在の表示条件・`ModelConfirmation` nonce・grant を操作直前に照合する。検索・機体選択・R では画面と `ModelScope` の確認を破棄する。サンプルには実照会を dispatch できない専用 scope を置き、合成先のみを表示する。

**Ruling:** GPUI の macOS・X11 再送は `is_held` を保証せず、key-up に押下 ID もない。Windows で確認は Enter、有効化は異なる Y とし、既存機体の確認も同様にする。Y の事前押下・長押しは抑止し、focus 喪失では確認を破棄する。Windows 以外は LLM のキーボード選択と機体・LLM のキーボード最終許可を無効にしマウスへ誘導する。Windows の判定はイベント順・`is_held` に依存し、実 OS 配送は未検証。

**Tech Stack:** Rust、既存 GPUI 0.2.2、既存 `ModelScope`。依存は変更しない。

**Constraints:** 実台帳・実 DB・実通信・Job・子プロセス・cleanup を伴う試験は禁止。Cargo は offline、locked、単一 job、専用 target。対象テストだけ完全一致で実行する。

## Review focus

- フィルタと状態変更で行が隠れても他の接続先へ操作を移さず、Enter を消費する。
- 再許可後の古い grant・旧確認 nonce・古い mouse handler は新しい許可へ影響しない。
- R・検索・機体選択は旧確認を失効させ、更新待ちは取消だけ許す。
- IME・検索を優先し、修飾キー・長押しを LLM 以外の操作へ流さない。確認前の Y、Enter の繰返し、旧 token の Y は許可しない。
- サンプルの合成操作は HTTP、実 DB、子プロセスを開始できない。

## Steps

- [x] `crates/fleet-studio/tests/llm_keyboard.rs` に状態遷移と上記境界を検査する純粋テストを追加し、正確な完全一致名で実装前の失敗を確認する。
- [x] `src/llm_keyboard.rs` の選択・表示・世代確認と `src/llm.rs` のサンプル専用 scope を実装し、同じテストを成功させる。
- [x] `src/view.rs` に入力・表示・mouse の共通照合を接続し、固定説明と README を更新する。
- [x] 監査済みの対象純粋テストを完全一致で直列実行し、変更範囲と差分を確認して実装をローカル commit する。

## 実行済みの完全一致テスト

各行は `cargo test --test <対象> <名前> -- --exact` として個別に実行。すべて offline、locked、単一 job、専用 target。試験本体と cleanup の副作用を確認した。

`llm_keyboard`:

- `a_held_yes_after_a_delayed_release_requires_a_fresh_press`
- `a_known_release_recovers_a_confirmation_after_yes_was_held`
- `release_outside_confirmation_clears_unknown_focus_state`
- `focus_loss_modifiers_and_old_confirmation_generations_cannot_authorize`
- `focus_recovery_requires_a_new_confirmation_matched_key_cycle`
- `an_old_release_after_a_confirmation_change_does_not_arm_the_new_target`
- `navigation_repeat_without_held_flag_is_consumed_until_release`
- `keyboard_routing_gives_search_and_blocked_input_priority`
- `blocked_global_shortcuts_do_not_escape_into_normal_actions_or_override_ime`
- `machine_confirmation_uses_a_distinct_key_and_suppresses_modified_actions`
- `selecting_uses_endpoint_identity_and_hidden_targets_do_not_reappear`
- `stale_or_hidden_confirmations_and_grants_cannot_change_permissions`
- `switching_context_dismisses_both_copies_of_the_confirmation`
- `focus_loss_clears_repository_selection_and_invalidates_the_model_confirmation`
- `a_mouse_handler_from_before_a_hidden_endpoint_reappeared_is_stale`
- `a_mouse_revoke_from_an_old_view_cannot_revoke_a_still_current_grant`
- `selection_from_an_old_grant_does_not_attach_to_a_new_grant`
- `pending_refresh_blocks_confirmation_but_keeps_current_revocation`
- `state_filter_hiding_a_just_authorized_endpoint_never_selects_another`
- `selected_endpoint_remains_identifiable_when_a_notice_is_shown`
- `real_scope_still_dispatches_an_explicitly_authorized_synthetic_query`
- `sample_selection_and_confirmation_cannot_dispatch_a_real_query`

`llm_consent`:

- `only_a_configured_remote_can_be_prepared_for_authorization`
- `an_old_confirmation_cannot_authorize_after_configuration_changes`
- `pending_refresh_suspends_old_and_new_confirmations_until_config_is_refreshed`
- `confirmation_discloses_the_exact_target_protocol_and_plain_http`

`scoped_observations`:

- `machine_keys_do_not_activate_actions_while_editing_or_modified`
- `machine_confirmation_discloses_refresh_and_revocation`

`full_refresh`:

- `sample_refresh_requests_no_real_sources`
- `machine_only_refresh_keeps_recent_and_model_confirmations_unchanged`
- `full_refresh_invalidates_both_busy_scopes_and_replays_only_current_work`

直近の結果: 対象 31 件成功。Esc 追補後も該当 9 件・GUI Clippy・GUI build が成功。先行の gate・focus 復帰テストは空 API／旧実装で失敗後に成功した。macOS を含む実 OS イベントと実データ画面は未検証。

## フォーカス復帰の追補

L・M の選択開始だけを復帰直後に許し、上下・slash・Esc と実行系キーは従来の gate を維持する。機体候補が空でも、旧 repo 選択・機体確認・確認キーを消す。L/M の実 DB・通信・監査・OS 起動の直接呼出しがないことは静的な呼出し経路と限定された純粋テストの範囲で確認する。`ModelScope::querying()` は未 dispatch の要求を含まないため、要求未生成の証明には使わない。GUI 経由の実イベント配送・focus 離脱の完全検出は未検証。

先行の `focus_recovery_allows_only_l_and_m_to_start_selection_once` と `empty_machine_selection_cancels_confirmation_and_confirmation_key` は、旧実装で assertion 失敗を確認した。追補の最終テスト結果は commit 後に記録する。

新 target で以下を個別に `--exact` 実行し、11 件成功した。

`llm_keyboard`:

- `focus_recovery_allows_only_l_and_m_to_start_selection_once`
- `empty_machine_selection_cancels_confirmation_and_confirmation_key`
- `machine_selection_preserves_cycling_order_after_confirmation_is_cleared`
- `selection_keeps_remote_unapproved_and_invalidates_old_confirmations`
- `navigation_repeat_without_held_flag_is_consumed_until_release`
- `blocked_global_shortcuts_do_not_escape_into_normal_actions_or_override_ime`
- `machine_confirmation_uses_a_distinct_key_and_suppresses_modified_actions`
- `focus_loss_clears_repository_selection_and_invalidates_the_model_confirmation`
- `sample_selection_and_confirmation_cannot_dispatch_a_real_query`

`full_refresh`: `full_refresh_invalidates_both_busy_scopes_and_replays_only_current_work`。

`scoped_observations`: `machine_keys_do_not_activate_actions_while_editing_or_modified`。

旧 target での build は既存 exe の削除失敗で終了 101。OS エラー分類と終了時刻は未取得で、既存 exe は元の hash のまま保持。未使用の新 target では Clippy と GUI build が成功し、source／manifest／lock の hash は build 中に変化なし。追補は `eb1f39186e530fb9bebfe4c802612bc22bda0ee6` に記録した。
