# 開発レビュー画面 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 利用者が選んだ完了台帳を、未照合の申告であることを保ったまま日本語で確認できる画面を追加する。

**Architecture:** file inputから一時的に読み、純粋な表示モデルを作る。既存のKT_VIEWSへ登録し、画面遷移で破棄する。IPC、DB、認証、ネットワーク、自動承認を追加しない。

**Tech Stack:** 素のJavaScript、Node標準test、既存WebView。

## 1. 入力モデルと境界

Files: `renderer/review-model.js`, `test/review.test.js`。

- [x] 合成台帳でschema、missing coverage、重複id、不正型、上限、日時、revision、URLのテストを書く。`assert.throws(() => Review.parse('{'))`、`assert.equal(Review.githubLink('javascript:alert(1)'), null)`、成功の申告が完了判定にならないことを検査する。
- [x] `node --test test/review.test.js`で機能不足による失敗を確認する。
- [x] 設計書の上限と許可リストを実装する。読込前のsize検査、読込競合の取消、型不正の全体拒否、未知キーの非実行を含める。
- [x] 同じテストを再実行して成功を確認する。

## 2. 画面とライフサイクル

Files: `renderer/review.js`, `renderer/index.html`, 必要な場合だけ`renderer/style.css`、`test/review-ui.test.js`。

- [x] DOMまたは既存VM fixtureで、破棄・読込失敗後の旧表示除去・古い非同期読込の無効化・テキストのescapeを先に検査する。
- [x] `node --test test/review-ui.test.js`で失敗を確認し、既存`app.js`より前にscriptを読み込んで`KT_VIEWS.review`に登録する。
- [x] 標準file input、日本語の目的・対象・版・各段階・証跡・coverage・人が見る項目を実装する。画面を離れる時にファイルとモデルを破棄する。
- [x] ブラウザで合成台帳の読込、リンクの拒否、再読込、画面遷移、狭幅表示を確認する。

## 3. 検証とレビュー

Files: `config/review.example.json`, `docs/development.md`, `README.md`, 設計書。

- [x] 合成見本と利用手順を追加し、台帳の申告を独立検証したことにはならないと明記する。
- [x] `npm run check`を低並列で実行する。失敗を修正し再検証する。
- [x] `npm run oss-check`、`npm run oss-check -- --history`に加え、新規未追跡ファイルをscannerへ直接渡して確認する。
- [x] 独立レビューの指摘を修正し、必要なテストを再実行する。
- [x] 実装・検査・実画面・未確認環境を分けて私有証跡へ記録する。commit/push/PR作成/merge/配布は今回の読み取り許可を拡張せず、レビュー可能な差分と結果を先に提示する。

## 実機調査（上記と独立）

- 正典台帳の稼働6台と除外対象を固定し、SSHの実効設定・identity・remote command・調査結果を分ける。
- 既存認証でGitHubのrepo・PR・CIを必要範囲だけ読み取る。設定・Issue・コメント・レビュー・PRは書き込まない。
- Windowsの既存collector転送は使わず、既設ファイルのhash一致を確認して実行するか、保存しない読取payloadを使う。benchmarkとnetworkを無効にし、停止中WSLを起動しない。
- taskルートの私有成果物へ時刻・ロスターhash・probe hash・結果・欠測を記録する。機体や個人情報をrepoへ入れない。

## 戻し方と完了境界

製品変更は専用ブランチの新しい画面に限定する。既存DB・機体設定・プロセスには変更を加えない。画面を外せば従来導線へ戻る。ローカル表示検証と正式反映・配布・Windows実機受入を分け、後者が未実施なら依頼全体を完了としない。
