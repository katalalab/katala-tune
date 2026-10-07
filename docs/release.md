# リリースと自動更新（Tauri 版）

タグ `v*` を push すると `.github/workflows/release.yml` が macOS（universal: Apple Silicon と Intel）と Windows x64 をビルドし、GitHub Releases に**下書き**として置く。中身を確かめて公開すると、アプリの自動更新の対象になる。

## 自動更新の決まり

- アプリは起動の1分後と24時間ごとに `https://github.com/katalalab/katala-tune/releases/latest/download/latest.json` を見て、新しい版があれば**通知するだけ**。勝手には入れない
- 入れるのはメニューバー（Windows は通知領域）の「更新を確認…」から。確認ダイアログ（既定のボタンは「やめる」）で「入れる」を選んだときだけ進む
- 入れる前に署名を確かめる。合わなければ入れない
  - `tauri.conf.json` の `plugins.updater.pubkey`（公開鍵）で、ダウンロードした物の署名を確かめる
  - `requireSignedVersion: true`: 署名に入った版と latest.json の版が同じでなければ入れない（古い版の署名つきの物を新しい版と偽って入れさせる、を防ぐ）
  - 新しい版でなければ入れない（版を下げない）
- 入れたあと再起動する。macOS は .app を入れ替えて起動し直し、Windows はインストーラー（NSIS、passive）がアプリを終了して入れ替える
- latest.json は CI が組み立てる（`darwin-aarch64`・`darwin-x86_64` は同じ universal の .app.tar.gz、`windows-x86_64` は setup.exe）

## 署名の鍵（最初に1回、操作者が行う）

秘密鍵とパスワードはリポジトリ・CI のログ・コマンドの引数に出さない。

1. 鍵を作る（パスワードは聞かれたときに入力する）
   ```sh
   cargo tauri signer generate -w ~/.tauri/katala-tune.key
   ```
   `~/.tauri/katala-tune.key`（秘密鍵）と `~/.tauri/katala-tune.key.pub`（公開鍵）ができる
2. 秘密鍵とパスワードを 1Password に保存する（鍵を失うと、今の利用者に更新を届けられなくなる）
3. GitHub の Settings → Environments で環境 `release` を作り、Deployment branches and tags を「Selected」にしてタグ `v*` だけを許す（必要なら Required reviewers も入れる）。その環境の secret に入れる
   ```sh
   gh secret set TAURI_SIGNING_PRIVATE_KEY --env release --repo katalalab/katala-tune < ~/.tauri/katala-tune.key
   gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --env release --repo katalalab/katala-tune   # 対話でパスワードを入力する
   ```
4. 公開鍵（`.pub` の中身、1行）を `src-tauri/tauri.conf.json` の `plugins.updater.pubkey` に入れてコミットする。仮の値（`REPLACE_WITH_TAURI_SIGNING_PUBLIC_KEY`）のままだとリリースの CI が止まる
5. 手元の秘密鍵ファイルは 1Password に入れたら消してよい

更新署名の公開鍵は `tauri.conf.json` に保存する。秘密鍵の保存先や管理者向けの復旧記録は公開リポジトリへ含めない。

鍵を替えると、古い公開鍵を持つアプリは新しい署名の更新を受け付けない。替えるときは、新しい公開鍵を入れた版を古い鍵で署名して一度出してから替える。

## 出し方

1. 版をそろえて上げる（package.json・package-lock.json・Cargo.toml・Cargo.lock・src-tauri/tauri.conf.json）
   ```sh
   npm run version:set -- 0.4.0
   npm test && cargo test --workspace
   ```
2. PR で main に入れる
3. main でタグを打って push する
   ```sh
   git tag v0.4.0 && git push origin v0.4.0
   ```
4. CI がタグと版の一致・公開鍵が仮の値でないことを確かめてからビルドし、下書きのリリースに置く:
   `KatalaTune_<版>_universal.dmg`（初めて入れる人向け）・`KatalaTune_<版>_universal.app.tar.gz`（と `.sig`）・`KatalaTune_<版>_x64-setup.exe`（と `.sig`）・`latest.json`
5. 下記の公開前チェックを完了してから下書きを公開する。公開した時点で、動いているアプリの「更新を確認…」に出る。未検証の環境が残る初期配布は prerelease とし、通常の latest 自動更新の対象にしない

## 注意

- Apple の公証（notarization）と Windows のコード署名はしていない。OS の警告や管理ポリシーで起動が止められる場合がある。更新ファイルの署名は OS の配布認証とは別で、組織の保護設定を解除して導入しない
- 識別子は `org.katala.tune.tauri`（Electron 版の `org.katala.tune` と分けている）

## 確かめ方（手元、本物の鍵を使わない）

使い捨ての鍵を作り、`--config` で公開鍵と知らせ先（`http://127.0.0.1:<port>/latest.json`、開発ビルドだけ http を許す）を差し替えた古い版と新しい版をビルドし、
開発ビルドの検証用の環境変数 `KATALA_TUNE_DEV_UPDATE=1`（起動してすぐ「更新を確認…」と同じ流れを動かす）と
`KATALA_TUNE_DEV_CONFIRM=approve|cancel`（確認ダイアログを出さずに答える）で、次を確かめる。どちらの環境変数もリリースビルドには入らない。

- 「やめる」なら何もダウンロードせず、入れない
- 別の鍵の署名・知らせの版と署名の版が違う物は、ダウンロードしても入れない（.app はそのまま）
- 正しい署名なら入れて再起動し、新しい版で起動し直す

使い捨ての鍵は確認のあと消す。コミットしない。

## 配布物の検証と切り戻し

CI は全3 target の feed の版・URL・署名文字列と実ファイルの一致、欠落・空ファイルを検証し、`SHA256SUMS` を同梱する。暗号としての署名検証は updater の責務で、文字列の一致を暗号検証とは呼ばない。公開前は下書きの全 asset を取得し、SHA256 を照合して実機で確認する。

### 公開前チェック

- タグの commit がレビュー済み main に含まれ、版が一致し、必須 CI と配布ビルドが成功している
- 下書きから実ファイルを取得し、6 asset の SHA256 が同梱の `SHA256SUMS` と一致する。feed の3 platform の URL・版・署名がそのファイルに対応する
- Mac `.app.tar.gz` と Windows `.exe` の署名を、製品に埋め込んだ公開鍵で暗号検証する。署名の trusted comment の版も feed と一致する。使い捨て鍵での成功だけでは本番の鍵の対応を確認したことにしない
- Mac は DMG から隔離した場所へコピーして起動し、アプリの版、台帳読込、分析、ログ、AI、確認ダイアログ、終了を確認する。universal executable の arm64 / x86_64 を確認し、実際に動かした CPU の種類を記録する
- Windows は既存のアプリとデータを保持し、NSIS で隔離した場所へ導入する。WebView2 の画面、台帳読込、分析、ログ、確認ダイアログ、終了、アンインストールを確認する。署名つき更新は旧版から入れ替え・再起動後の版まで確認する
- 更新のキャンセル、別の鍵、署名の版の不一致がアプリを変更しないことと、正常更新が再起動後の版に反映されることを確認する
- リリースノートに導入方法、検証済み OS / CPU、未検証の環境、OS 配布認証の有無、切り戻し手順を記載する。公開後に実際の配布 URL・feed・SHA256 を読み返す

検証記録はタグ・commit・CI run ID・OS / CPU・asset 名 / サイズ / SHA256・操作と結果を残す。台帳、端末名、ログ本文、秘密鍵は公開しない。未検証を成功に置き換えない。

### 導入前のデータ保護

旧 Electron 版と Tauri 版をともに終了し、台帳 `~/.config/katala-tune/nodes.json` とアプリのデータディレクトリをコピーして退避する。DB が WAL を使っている間に `.db` だけをコピーしない。検証では `KATALA_TUNE_CONFIG` と `KATALA_TUNE_DATA_DIR` で台帳・データを隔離し、自動スキャンを無効にする。旧版と新版を同じ DB に同時接続させない。

公開後に配布の問題が判明した場合は、まず問題の release を prerelease または draft に戻し latest の対象から外す。DB を古い版に上書きしない。updater は版を下げないため、修正版をより高い版で旧署名鍵により配布する。旧アプリの手動復旧には事前の DB バックアップと schema の互換確認が必要。鍵を捨てたり保護ルールを外して出し直したりしない。
