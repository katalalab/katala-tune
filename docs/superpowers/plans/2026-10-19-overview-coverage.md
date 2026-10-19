# 概要改善 Implementation Plan

**Goal:** 現在の分析結果と欠測を区別し、確認対象の機体を探せるようにする。
**Architecture:** DOMに依存しない集計をoverview.jsへ置く。app.jsが既存stateから渡し、既存UI部品で描画する。
**Tech Stack:** 素のJavaScript、node:test、既存Electronプレビュー。

- [x] `test/overview.test.js` に対象外・失敗・未分析・鮮度境界・不正時刻・検索・OS・要対応の動作テストを書き、未実装の失敗を確認する。
- [x] `renderer/overview.js` に分類と集計・絞り込みを実装してテストを通す。
- [x] `renderer/index.html` から読み込み、`renderer/app.js` の概要に母数・分類・検索・フィルター・解除を組み込む。カードに鮮度を表示し、古い結果を良好と表示しない。
- [x] `renderer/style.css` に既存トークンを使う分類ボタンを追加する。
- [x] `scripts/ui-preview.js` に検索・絞り込み・空結果・解除の操作検証を加える。
- [x] `npm run check`、`npm run oss-check -- --history`、架空データのUIプレビューを実行し、画像を確認する。

成功条件: 対象外を平均に入れず、分類の和が対象台数と一致し、検索後も全体の母数が分かる。専用worktreeで実機検証・独立レビューを行い、操作者が許可したcommit・PR・マージまで進める。戻す場合はこの差分のみをrevertする。既存の稼働設定は変更しない。

- [x] Electron/Tauriの起動時の最新失敗復元と、描画をまたぐ検索・鮮度更新を回帰検証する。
- [x] MacのTauri実機で分析・取得失敗・再起動・回復を確認し、Windows実機でprobe・ログ・Nodeテストを確認する。
- [ ] 最終差分をcommitしてPRを作成し、head固定の正典ゲートでCI・承認・会話を再確認してマージする。
