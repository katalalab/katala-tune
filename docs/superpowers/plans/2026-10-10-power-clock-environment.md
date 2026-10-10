# 電力・Clock環境 Implementation Plan

実装結果: Clock変更の項目は未実装。現行版はClockを観測・表示するだけで、変更操作は提供しない。

> **For agentic workers:** Use subagent-driven-development for the independent calculation module. Integration and remote application remain owned by the primary session. Steps use checkbox syntax for tracking.

**Goal:** 全機体の電力・Clockを表示し、計測区間の電力量と稼働時間の試算、対応機体の確認付き設定を提供して各機体へ導入する。

**Architecture:** 既存のSSH分析・ライブ・確認付きactionsを拡張する。電力計算は純粋なRust / JSモジュール、公開値は取得源と欠測を持ち、個別設定は私有台帳で保持する。

**Tech Stack:** Rust 1.99 / Tauri 2、既存SQLite、Node 24 / 素のJS、Python標準ライブラリ / Windows PS 5.1。

## Task 1: 実環境の読み取り

- [ ] 正典rosterのactive physicalを導出し、実効SSHを記録する。
- [ ] benchmark=falseで実機の配置・版・台帳・センサー・GPU UUID / bounds、権限を取得する。稼働中のappとDBは保持する。
- [ ] 本日の保護中workloadと到達できない対象を私有受入台帳へ記録する。

## Task 2: 電力計算（独立モジュール）

**Files:** `crates/tune-core/src/power.rs`, `crates/tune-core/src/lib.rs`, `lib/power.js`, `test/power.test.js`。

- [ ] 先に実証テストを書く。一定100Wを1時間は0.1kWh、100→200Wを1時間は0.15kWh、欠測・重複・逆順・再起動は二重積算しない。
- [ ] `summarize(snapshot, settings)` / Rust `summary(&Value, &Value) -> Value` を実装する。設定キーは `base_w`, `psu_efficiency`, `hours`, `rate_per_kwh`, `currency`。GPUのみから全体を断定しない。
- [ ] `integrate(points, interval_seconds)` / Rust同名関数に `t`(ms), `seq`, `epoch`, `source`, `watts` のpointsを渡す。`kwh`, `covered_seconds`, `coverage`, `duration_seconds`を返す。
- [ ] 既存snapshotのGPU `power_w`、追加CPU `power.package_w`、全体 `power.wall_w` / `power.soc_w`から取得可能なものだけを返し、0・null・NaNを区別する。
- [ ] `node --test test/power.test.js` と `cargo test -p tune-core power`で失敗→実装→成功を確認する。JS / Rustに同じ合成fixtureを用いる。spec reviewとquality reviewを別に実施する。

## Task 3: 読み取りとliveの電力・Clock

**Files:** `probes/win_probe.ps1`, `mac_probe.py`, `live_win.ps1`, `live_mac.py`, `crates/tune-core/src/live.rs`, `renderer/live.js`。

- [ ] GPU UUID・clocks graphics / SM / memory・power boundsを追加する。列が未対応でも既存のGPU値を取得する。
- [ ] CPU基準MHzと性能カウンタ推定、既存センサーのpackage Wを追加。Macは取得できるセンサーだけを利用し、未取得は理由を返す。
- [ ] liveの数値model / sanitize / ringに電力とClockを追加。画面から離れた後の終了条件を保持する。
- [ ] PSのBOM / ASCIIと実PowerShell parse、Mac Python構文、旧live入力との互換、実GPU標本を検証する。

## Task 4: 確認付き設定

**Files:** `crates/tune-core/src/actions.rs`, `lib/actions.js`, 新しい小さなpower-actions helper、actionsテスト。

- [ ] 電力上限操作にGPU UUID・実測bounds・前値・要求Wを必須とし、有限・範囲・同一性・前値一致を検証する。
- [ ] 操作対象側の排他、直前readback、変更後readback、失敗時復元と復元readbackを組み込む。確認・台帳再読込・実行記録の既存経路を通す。
- [ ] GPU / memory clock rangeは、前lockを復元できる状態が確認できた機体にだけ提供する。現在MHzを前lockとみなさない。
- [ ] Macの対応電源モードは権限を明示。未対応・権限不足では状態を変えず返す。既存Windows power planも読返しを検証する。
- [ ] `node --test test/actions.test.js` / `cargo test -p tune-core actions`で注入・共用機・欠測・競合・キャンセル・UUID不一致・復元失敗を検証する。

## Task 5: 画面・設定・保存

**Files:** `renderer/power.js`, `renderer/app.js`, `renderer/index.html`, `renderer/bridge.js`, `src-tauri/src/commands.rs`, `src-tauri/src/main.rs`, `crates/tune-core/src/engine_power.rs`, 既存DB meta。

- [ ] リソース表と機体詳細へ電力・Clock・取得源・カバー率を表示する。複数GPUをUUID別で扱う。
- [ ] 入力欄で稼働時間・校正値・単価を保存し、実測積分と試算を分ける。全体の合計には欠測機体を添える。
- [ ] per-node設定を私有DB metaへ保存する。liveの観測区間を有界で保存し、古いDBに破壊的migrationをしない。
- [ ] どの操作卓からも同じnode IDを選べる。shared / protect / privacyは維持する。
- [ ] DOMの欠測表示・注入拒否・設定未入力をテストする。Windows / MacのGUIで表示と確認のキャンセルを検証する。

## Task 6: 検証・導入・受入

- [ ] `npm run check`・`npm run oss-check -- --history`、Windows実ビルド、Mac実ビルドを同じsource revisionで実行する。
- [ ] 独立reviewでspec対応とcorrectness / securityを別に確認し、指摘を修正して再検証する。
- [ ] 既存app / config / DBを対象機の私有領域へ退避し、hashとRESTORE、30日保持を用意する。
- [ ] 対象機を1台ずつ導入し、source / binary hash、起動、負荷計測なしの全機分析と詳細画面を確認する。
- [ ] 対象集合の自機経路・他機間の全有向経路の接続 / probe成功を分けて記録する。
- [ ] 操作者の選択した設定の変更→読返し→復元→読返しを検証する。選択されていない値は変えない。
- [ ] 未取得・未対応・管理者 / GUI待ちと正式統合の残件を示す。承認済み導入は再確認しない。公開・PRマージ等の未許可操作は、具体的な差分と検証結果を揃えてから確認する。
