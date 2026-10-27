# Rust + Tauri 開発環境

標準のアプリは Rust + Tauri 2。画面はビルド工程を要しない素の JavaScript / CSS、分析は `tune-core`、端末検証は `tune-cli` に分ける。常駐サービスやフロントエンド用サーバーは追加しない。

## 必要なもの

- Node 24（`.nvmrc` / `package.json`）、Rust 1.99.0 と rustfmt / Clippy（`rust-toolchain.toml`）
- Tauri CLI 2.12.1: `cargo install tauri-cli --version 2.12.1 --locked`
- macOS: Xcode または Command Line Tools。universal 配布は `rustup target add aarch64-apple-darwin x86_64-apple-darwin`
- Windows: Visual Studio C++ Build Tools の Desktop development with C++、Windows SDK、WebView2、MSVC の Rust toolchain
- Linux の検証: WebKitGTK 4.1、libayatana-appindicator、librsvg、libxdo、OpenSSL の開発用パッケージ（CI を参照）

公式の [Tauri 前提条件](https://v2.tauri.app/start/prerequisites/) に従う。インストールが必要な環境だけを変更し、停止中 WSL を起動したり他機のグローバル既定を変えたりしない。

```sh
npm ci --ignore-scripts
npm run doctor
npm run check
npm start
npm run package
```

`doctor` は読取専用。Windows の C++ / WebView2 の受入は実ビルドと GUI で確認する。`check` は版の一致、JS、公開情報、Rust fmt / Clippy / tests の失敗で止まる。Windows の Tauri テスト executable は Common Controls v6 manifest が無いため除外するが、CI でアプリ本体をリンクする。GUI と更新の実機確認は単体テストで代用しない。

## トポロジーの開発と受入

トポロジーは private `nodes.json` の `topology.enabled` と環境ごとの allowlist が両方そろったときだけ収集する。公開 repository に実機名、root、owner、SSH alias、設定値、credential を追加しない。

```sh
tune topology-refresh [node-id ...]
tune topology-watch --interval-seconds 60 --iterations 2 [node-id ...]
tune topology-export --view all --format json
```

disabled、cold、no-change、delta、failure は同じ private scope で別 receipt として確かめる。Partial / Failed receipt は成功 graph を置換しない。root や entry の上限で継続 cursor が残る場合は、次回が同じ位置を無言で繰り返さず scope coverage と progress を記録する。

Tauri QA は本番アプリを終了・置換しない。別 identifier / product name の `TAURI_CONFIG` overlay、専用 `KATALA_TUNE_CONFIG` と `KATALA_TUNE_DATA_DIR`、fresh DB（update.auto=false、monitor disabled、hub null、schedule master false）を使う。`KATALA_TUNE_DATA_DIR` が明示されたときだけ helper と local native sampler は `<data>/bin` を使う。overlay build 後も bundle / 起動 / GUI 操作は別の受入段階で、既存 HOME の helper や設定を変更してはならない。

クラウド read audit と Figma のレビューは topology の収集・成功条件から独立させる。認証済み read の receipt があっても、cloud 設定変更、Figma の自動更新、機体への配布を行った証拠にはならない。

## データと検証の分離

本番の台帳・DB をテストデータへ上書きしない。検証は CLI の `--help` にある環境変数指定と合成台帳を使う（`logs` の DB は `--db` で指定）。アプリの版・実行環境・操作結果を揃えて記録する。分析は CPU 計測を含むので、負荷試験の禁止中の機体を除き、除外理由を残す。

### CLI の発見と副作用

`tune --help` / `tune help` は全公開コマンド、`tune help <command>` は引数・出力形式・副作用を表示する。成功時は stdout と exit 0、未知の help 対象や不正な discovery 引数は stderr と exit 2。内部 native reader のプロトコルは対象外。

`tune cli-info --json` は `schema_version: 1`、`cli_version`、`commands` を返す。各項目は `name`、`arguments`（usage 構文の文字列配列）、`summary`、`output.default/formats`、`effects`、`notes`。引数の `[]` は任意、`<>` は必須、`...` は繰返しを表す。これは入力検証用 JSON Schema ではない。既存コマンドの出力を包む形式でもない。

`effects` は台帳/DB/peer状態の読書き、機体接続、helper配布が実行条件次第で起こり得るかを示す。初期作成・移行も書込に含むので、`last` / `status` / `export` などの照会も `writes_db: true` になる。条件は `notes` を参照。collector の子プロセス・一時ファイルや接続ツール自身の状態もあり、許可判定や sandbox の代わりにはしない。

help と cli-info は台帳・DB・鍵を開かず、機体へ接続しない。公開コマンドを増やすときは `crates/tune-cli/src/discovery.rs` の一覧と実行バイナリの discovery テストも更新する。

Electron は比較用。必要なときだけ `npm rebuild electron` でバイナリを取得する。標準の起動・配布は Electron を使わない。共通 DB に旧版と新版が同時に書かないよう、片方を終了して切り替える。

## レビュー・反映

PR は3 OSのJS/Rustチェックを必須にし、チェック名を変えない。性能変更は `npm run benchmark:live` と `cargo run --release -p tune-core --example performance` の比較を残す。版は `npm run version:set -- <版>` で変更し、タグと一致させる。

レビュー画面は `task-completion.v1` の表示専用で、共通checkerやPRの現在headを照合しない。`config/review.example.json` は入力形の例であり、画面から保存・ネットワーク・承認操作は発生しない。coverageの3配列は合計10,000件までに拒否し、欠測は表示モデルでnull、画面では「未記載」と表示する。表示変更は `node --test test/review.test.js test/review-ui.test.js` で境界、表示、非同期読込を確認する。

レビューした head SHA を固定し、CI・未解決会話・main の包含を再確認してマージする。配布は署名つき下書きから始める。署名・版の不一致、キャンセル、正常更新を確認し、公開後は実際の配布 URL と SHA256 を読み返す。[リリース手順](release.md) を参照。

## Windows native CI 診断

Windows の通常 workspace test は従来のバイナリと判定で実行する。その後、専用 target と `katala_ci_diagnostics` cfg で synthetic native 3役 fixture を別に一回実行し、認証や Job の失敗を閉じた数値分類で観測する。通常テストの失敗後も診断を実行するが、これは別の試行であり、非再現なら元の失敗段階は不明のまま。正常な通常テストを診断結果で置き換えず、期限・identity・receipt・出力秘匿も変更しない。

診断ログには `native_role` の固定名、経過時間、code、出力長だけを通す。数値の定義と予約コードは `crates/tune-core/src/topology/adapters/native_private_diagnostics.rs` にあり、0 は成功、2 は汎用拒否、124 は期限切れ。未知コードや証拠不足は診断上も汎用拒否として扱う。raw log は runner 上の一時ファイルに限定し、artifact に含めない。
