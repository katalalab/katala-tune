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
//! | [`inventory`] | 道具の棚卸し（調査・正規化・機体×道具の表） | lib/inventory.js |
//! | [`dogu`] | Do-gu との照合・デッキの下書き・登録の検証と送信 | lib/dogu.js |
//! | [`ai_sessions`] | AI エージェント（Claude Code・Codex）のセッションの取り込み | （tune-core だけ） |
//! | [`collate`] | `localeCompare` の近似（並び順を JS 版と揃える） | |
//! | [`engine_tools`] | 棚卸し・Do-gu・AI の流れ（[`engine::Engine`] のメソッド） | main.js の runInventory・inventoryView・dogu-* |
//! | [`netsec`] | ネットワークとセキュリティ（待ち受け・防御・常駐の増減・ログイン・初めての接続先。正規化・判定・状態と、DB を使う取り込み・画面） | lib/netsec.js（DB を使う部分は tune-core だけ） |
//! | [`live`] | ライブ表示（画面が見ているあいだだけ 1〜2 秒ごとに取る。経路・解析・保持・管理） | なし（Tauri 版だけ） |
//! | [`prices`] | 費用の推定（API 単価換算）。同梱の単価表 data/prices.json（取得日・出典・版つき） | （tune-core だけ） |
//! | [`codex_limits`] | Codex の残り枠（codex app-server の account/rateLimits/read。窓の長さ・使用率・リセット時刻だけ） | （tune-core だけ） |
//!
//! 表の定義は `db/` に機能ごとに置く（inventory は Electron 版と同じ表、ai_sessions は tune-core だけの表）。
//! 取り込んだ数字の出どころ（取り込みの回・読んだ区間・連鎖）は `db/provenance.rs`、重複を除いた量と費用は `db/ai_usage.rs`。

pub mod actions;
pub mod ai_sessions;
pub mod codex_limits;
pub mod collate;
pub mod collect;
pub mod db;
pub mod dogu;
pub mod engine;
pub mod engine_power;
pub mod engine_tools;
pub mod health;
pub mod inventory;
pub mod js;
pub mod live;
pub mod logs;
pub mod netsec;
pub mod nodes;
pub mod power;
pub mod power_observation;
pub mod prices;
pub mod rules;
