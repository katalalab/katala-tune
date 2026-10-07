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

2026-10-07 に作成済み: 1Password の Katala-Agents「Katala Tune updater signing key (minisign)」（private_key・password・public_key）、GitHub の環境 `release`（タグ `v*` だけ）の secret 2 つ、`tauri.conf.json` の公開鍵。

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
4. CI がタグと版の一致・公開鍵が仮の値でないことを確かめてからビルドし、更新署名を公開鍵で検証して（下の「配布物の検証と切り戻し」）、下書きのリリースに置く:
   `KatalaTune_<版>_universal.dmg`（初めて入れる人向け）・`KatalaTune_<版>_universal.app.tar.gz`（と `.sig`）・`KatalaTune_<版>_x64-setup.exe`（と `.sig`）・`latest.json`
5. 下書きを確かめて公開する。公開した時点で、動いているアプリの「更新を確認…」に出る

## 注意

- Apple の公証（notarization）と Windows のコード署名はしていない。初めて入れるときは、macOS は Finder で右クリック →「開く」、Windows は SmartScreen の「詳細情報 → 実行」が要る。自動更新で入れた版にはこの手順は要らない
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

CI（`scripts/verify-release.js`）は、下書きに置く前に更新ファイル（`.app.tar.gz`・`setup.exe`）の `.sig` を `tauri.conf.json` の公開鍵で暗号として検証する。アプリの updater と同じく、鍵 ID の一致・本体の署名（BLAKE2b-512 の前ハッシュ。旧形式は前ハッシュなし）・trusted comment を覆う global signature の3つを確かめ、`requireSignedVersion` と同じく署名に入った版がリリースの版と同じことも確かめる。合わなければリリースを作らない。
あわせて全3 target の feed の版・URL・署名が検証した `.sig` と同じこと、欠落・空ファイルを確かめ、`SHA256SUMS` を同梱する。検証は外部の依存を足さず Node の crypto で行う（テストは使い捨ての鍵で作った公開鍵・署名・ファイルだけを `test/fixtures/release` に置き、秘密鍵は残していない）。DMG は自動更新に使わないので更新署名は無い。
公開前は下書きの全 asset を取得し、SHA256 を照合して実機で確認する。

公開後に配布の問題が判明した場合は、まず問題の release を prerelease または draft に戻し latest の対象から外す。DB を古い版に上書きしない。updater は版を下げないため、修正版をより高い版で旧署名鍵により配布する。旧アプリの手動復旧には事前の DB バックアップと schema の互換確認が必要。鍵を捨てたり保護ルールを外して出し直したりしない。
