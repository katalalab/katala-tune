# リリースと自動更新（Tauri 版）

**main で版を上げてマージすれば、そのまま全部の操作卓に届く。** `auto-release.yml` がその版のタグ `v<版>` を作って `release.yml` を動かし、macOS（universal: Apple Silicon と Intel）と Windows x64 をビルドし、更新署名を検証してから GitHub Releases に公開する。通常の版（`0.6.1` など）は latest として公開し、動いているアプリが自動で入れる。`-` つきの版（`0.7.0-rc.1` など）は prerelease で、自動更新の対象にしない（手動で試す用）。

## 版の決まり

- SemVer（`主.副.修正`）。直したら修正（`0.6.1`）、機能を足したら副（`0.7.0`）。DB の表を消す・変える、台帳の形を変えるなど後戻りできない変更は主を上げる（1.0 までは副）
- 版はワークスペースで1つ（アプリ・`tune` CLI・`tune-agent` サンプラーは同じ版）。`npm run version:set -- 0.6.1` で package.json・package-lock.json・Cargo.toml・Cargo.lock・tauri.conf.json をそろえる。CI（`node scripts/version.js --check`）がずれを止める
- 変更点は `CHANGELOG.md` の `## <版>` に書く。リリースノートの先頭にその節が入り、続けて GitHub が PR から作る一覧が入る
- 一度出した版は出し直さない。直すときは次の版を出す（updater は版を下げない）

## 出し方

1. PR で直す・足す。同じ PR か続く PR で版を上げ、`CHANGELOG.md` に `## <版>` を足す
   ```sh
   npm run version:set -- 0.6.1
   npm test && cargo test --workspace
   ```
2. main にマージする。`auto-release.yml` が新しい版を見つけてタグを作り、`release.yml` を動かす（手でタグを push しても同じ）
3. `release.yml` がタグと版の一致・main に含まれること・公開鍵を確かめ、両 OS をビルドし、同梱する道具（`tune`・Windows の `tune-agent`）を入れ、更新署名を公開鍵で検証してから（下の「配布物の検証と切り戻し」）公開する。公開後に `latest.json` を読み返し、版が合っていることを確かめる
4. 動いているアプリは6時間以内に気づき、署名を確かめてから入れる（下の「自動更新の決まり」）

止めたいとき: リポジトリ変数 `RELEASE_DRAFT` を `true` にすると、公開せず下書きに置く（中身を確かめてから手で公開する）。

## 自動更新の決まり

- アプリは起動の1分後と6時間ごとに `https://github.com/katalalab/katala-tune/releases/latest/download/latest.json` を見る
- 自動更新がオン（既定。メニューバー・通知領域の「自動で更新する」、`tune update '{"auto":false}'` で切る）なら、新しい版を裏でダウンロードし、**署名を確かめてから**、**公開から2時間たったあと**、ウィンドウを閉じていて分析・取り込み中でないときに入れて再起動する。2時間のあいだに問題が分かった版は、prerelease か下書きに戻せばどの機体にも入らない（main は CI 必須だがレビュー必須ではないので、ここが止める余地になる。人の承認を挟むなら `release` 環境に Required reviewers を入れる）。ウィンドウを開いているあいだは待ち、「準備ができました」と通知する。入れる前にライブ・常時監視のサンプラーを止める
- オフなら通知だけ。メニューの「更新を確認…」から、確認ダイアログ（既定のボタンは「やめる」）で「入れる」を選んだときだけ入れる
- 署名の確かめ方（合わなければ入れない）
  - `tauri.conf.json` の `plugins.updater.pubkey`（公開鍵）で、ダウンロードした物の署名を確かめる
  - `requireSignedVersion: true`: 署名に入った版と latest.json の版が同じでなければ入れない（古い版の署名つきの物を新しい版と偽って入れさせる、を防ぐ）
  - 新しい版でなければ入れない（版を下げない）
- macOS は .app を入れ替えて起動し直し、Windows はインストーラー（NSIS、passive）がアプリを終了して入れ替え、同じ引数（`--hidden` など）で起動し直す
- 同梱の道具（`tune`・`tune-agent`）は、起動のたびにアプリの隣から `~/.katala-tune/bin` へ写す（中身が違うときだけ。結果は `tune update` の `helpers`）。Windows のハブは、サンプラーの版が古い・無いほかの Windows の機体へ同梱のサンプラーを送り、SHA-256 を確かめてから入れ替える（1 機体 1 時間に 1 回まで）
- 状態（版・最後に確かめた時刻・用意した版・エラー）はダッシュボードの「版」と `tune update` で見る
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

CI（`scripts/verify-release.js`）は、下書きに置く前に更新ファイル（`.app.tar.gz`・`setup.exe`）の `.sig` を `tauri.conf.json` の公開鍵で暗号として検証する。アプリの updater と同じく、鍵 ID の一致・本体の署名（BLAKE2b-512 の前ハッシュ。旧形式は前ハッシュなし）・trusted comment を覆う global signature の3つを確かめ、`requireSignedVersion` と同じく署名に入った版がリリースの版と同じことも確かめる。合わなければリリースを作らない。
あわせて全3 target の feed の版・URL・署名が検証した `.sig` と同じこと、欠落・空ファイルを確かめ、`SHA256SUMS` を同梱する。検証は外部の依存を足さず Node の crypto で行う（テストは使い捨ての鍵で作った公開鍵・署名・ファイルだけを `test/fixtures/release` に置き、秘密鍵は残していない）。DMG は自動更新に使わないので更新署名は無い。
公開後に全 asset を取得し、SHA256 を照合して実機で確認する。

### 公開後の確認

自動で公開するので、確認は公開のあとに行う（CI の検証が通らない物は公開されない）。問題があれば下の切り戻しに従う。

- タグの commit がレビュー済み main に含まれ、版が一致し、必須 CI と配布ビルドが成功している
- リリースから実ファイルを取得し、6 asset の SHA256 が同梱の `SHA256SUMS` と一致する。feed の3 platform の URL・版・署名がそのファイルに対応する
- Mac `.app.tar.gz` と Windows `.exe` の署名を、製品に埋め込んだ公開鍵で暗号検証する。署名の trusted comment の版も feed と一致する。使い捨て鍵での成功だけでは本番の鍵の対応を確認したことにしない
- Mac は DMG から隔離した場所へコピーして起動し、アプリの版、台帳読込、分析、ログ、AI、確認ダイアログ、終了を確認する。universal executable の arm64 / x86_64 を確認し、実際に動かした CPU の種類を記録する
- Windows は既存のアプリとデータを保持し、NSIS で隔離した場所へ導入する。WebView2 の画面、台帳読込、分析、ログ、確認ダイアログ、終了、アンインストールを確認する。署名つき更新は旧版から入れ替え・再起動後の版まで確認する
- 更新のキャンセル、別の鍵、署名の版の不一致がアプリを変更しないことと、正常更新が再起動後の版に反映されることを確認する
- リリースノートに導入方法、検証済み OS / CPU、未検証の環境、OS 配布認証の有無、切り戻し手順を記載する。公開後に実際の配布 URL・feed・SHA256 を読み返す

検証記録はタグ・commit・CI run ID・OS / CPU・asset 名 / サイズ / SHA256・操作と結果を残す。台帳、端末名、ログ本文、秘密鍵は公開しない。未検証を成功に置き換えない。

### 導入前のデータ保護

旧 Electron 版と Tauri 版をともに終了し、台帳 `~/.config/katala-tune/nodes.json` とアプリのデータディレクトリをコピーして退避する。DB が WAL を使っている間に `.db` だけをコピーしない。検証では `KATALA_TUNE_CONFIG` と `KATALA_TUNE_DATA_DIR` で台帳・データを隔離し、自動スキャンを無効にする。旧版と新版を同じ DB に同時接続させない。

公開後に配布の問題が判明した場合は、まず問題の release を prerelease または draft に戻し latest の対象から外す。DB を古い版に上書きしない。updater は版を下げないため、修正版をより高い版で旧署名鍵により配布する。旧アプリの手動復旧には事前の DB バックアップと schema の互換確認が必要。鍵を捨てたり保護ルールを外して出し直したりしない。

Windows の既存 `tune.exe` は承認済みハッシュを維持するため自動で上書きしない。通常収集にも v2 record が必要である。初回導入・更新・ロールバックは [Windows helper の承認手順](windows-helper-approval.md) を実施する。
