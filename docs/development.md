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

## データと検証の分離

本番の台帳・DB をテストデータへ上書きしない。検証は CLI の `--help` にある DB 指定と合成台帳を使う。アプリの版・実行環境・操作結果を揃えて記録する。分析は CPU 計測を含むので、負荷試験の禁止中の機体を除き、除外理由を残す。

Electron は比較用。必要なときだけ `npm rebuild electron` でバイナリを取得する。標準の起動・配布は Electron を使わない。共通 DB に旧版と新版が同時に書かないよう、片方を終了して切り替える。

## レビュー・反映

PR は3 OSのJS/Rustチェックを必須にし、チェック名を変えない。性能変更は `npm run benchmark:live` と `cargo run --release -p tune-core --example performance` の比較を残す。版は `npm run version:set -- <版>` で変更し、タグと一致させる。

レビューした head SHA を固定し、CI・未解決会話・main の包含を再確認してマージする。配布は署名つき下書きから始める。署名・版の不一致、キャンセル、正常更新を確認し、公開後は実際の配布 URL と SHA256 を読み返す。[リリース手順](release.md) を参照。
