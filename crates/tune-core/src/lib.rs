//! Katala Tune の分析エンジン。Tauri に依存しない（docs/architecture.md）。
//!
//! | モジュール | 役割 | JS 版（仕様） |
//! |---|---|---|
//! | [`nodes`] | 台帳（`~/.config/katala-tune/nodes.json`）と保存先の場所 | lib/nodes.js |
//! | [`db`] | 保存（SQLite。Electron 版と同じファイル・同じ表） | lib/db.js |
//! | [`collect`] | 調査の実行（SSH・ローカル）。probes/ を埋め込んで渡す | lib/collect.js |
//! | [`rules`] | 判定（所見と提案・スコア・前回比） | lib/rules.js |
//! | [`health`] | 状態（正常／注意／異常／不明） | lib/health.js |
//! | [`logs`] | ログの取り込み・伏せ字・指紋・ログ由来の所見 | lib/logs.js |
//! | [`actions`] | 変更操作の計画と検証（許可リスト）と実行 | lib/actions.js |
//! | [`engine`] | 上をつなぐ流れ（分析・取り込み・状態・自動スキャンの判断・確認つきの実行） | main.js の画面以外 |
//!
//! 道具の台帳（inventory・Do-gu）と AI エージェントのセッション（ai_sessions）は、同じ形で
//! `inventory.rs` / `ai_sessions.rs` と `db/inventory.rs` / `db/ai_sessions.rs` を足し、`engine` から呼ぶ。
//! 表の定義は `db/` に機能ごとに置く（inventory の表はすでに揃えてある）。

pub mod actions;
pub mod collect;
pub mod db;
pub mod engine;
pub mod health;
pub mod js;
pub mod logs;
pub mod nodes;
pub mod rules;
