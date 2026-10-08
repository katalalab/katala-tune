# Katala Tune

手元の Mac / Windows 機を SSH で並列に調べ、所見と最適化の提案を出し、ログを集めるデスクトップアプリ（Rust ＋ Tauri 2、macOS / Windows）。
変更を加える操作は許可リストにあるものだけで、毎回確認ダイアログで承認し、実行の直前に状態を確かめ直してから実行する。

- 配布版: [GitHub Releases](https://github.com/katalalab/katala-tune/releases)（macOS universal / Windows x64）
- 開発: `npm run doctor` → `npm start`（Tauri）。[開発手順](docs/development.md)
- 配布物の作成: `npm run package`。版・署名・更新の手順は [リリース手順](docs/release.md)
- 端末から: `cargo run -p tune-cli -- --help`。従来の JS 検証用 CLI は `npm run probe` / `npm run logs`

## 初めて使うとき

1. [Releases](https://github.com/katalalab/katala-tune/releases) から、Mac は `universal.dmg`、Windows x64 は `x64-setup.exe` を取得する
2. 旧版を使っている場合は終了し、台帳とデータを退避する。[バックアップと切り戻し](docs/release.md#導入前のデータ保護) を参照。同じ DB に旧版と新版を同時接続させない
3. Mac は DMG 内のアプリをコピー、Windows はインストーラーで導入する。起動して「台帳を開く」から機体を設定する。この機体だけ調べる場合は `local_hostname` にこの機体の hostname を指定する。他の機体は既存の SSH 接続を使う。設定例は下の「機体台帳」を参照
4. 「状態」で自動スキャンの設定を確認し、手動の分析から動作を確認する。変更操作は内容を確認して承認したときだけ実行する

初期配布の `Pre-release` は手動で導入するプレビュー版で、通常の自動更新の対象にしない。検証済み環境と未検証の環境は各リリースノートに記載する。Apple の公証と Windows のコード署名は未対応で、更新ファイルの暗号署名とは別。OS や組織の管理ポリシーで起動できない場合は、保護を解除せず管理者の導入手順に従う。

アプリのウィンドウを閉じても常駐は続く。完全に止めるときはメニューバー／通知領域の「終了」を使う。

## 画面

- **概要**: 機体ごとのスコア・CPU・メモリ・ディスク、優先して見るもの
- **状態**: アプリ自身の機能（台帳・DB・自動スキャン・連携）と、機体ごとの機能（分析・ログ取り込み・定期処理・期待するサービス／ジョブ／プロセス・ディスク・メモリ・安定性・Defender・エラーの繰り返し）を 正常／注意／異常／不明 で表示する。各項目は根拠と「いつからその状態か」を持ち、変わったときだけ履歴に残る。異常になったとき・異常から戻ったときに通知する
- **リソース**: 全機体の CPU・メモリ・swap/コミット・ディスク・GPU・稼働日数・計測を1表で。Tauri 版は「ライブ」で 1 秒ごとの値を流せる（見ているあいだだけ。docs/architecture.md の「ライブ表示」）
- **プロセス**: 全機体の CPU・メモリ上位を横断して検索・並べ替え・終了（確認と直前の同一性チェックつき）
- **スケジュール**: launchd（ユーザーの LaunchAgents）とタスクスケジューラ（Windows 標準以外）を横断して、予定・状態・前回の結果・次回を表示。無効化／有効化／今すぐ実行（元に戻せるものは戻せる）
- **機体**: 所見と提案、プロセス、定期処理、ログ、履歴（計測とスコアの推移）、機体情報
- **ログ**: 全文検索、同種ログ（数字・ID・パスを伏せて同じ形のものを集計し、何台で出ているかを表示）、取り込みの状態（最終成功・捨てた件数・エラー）
- **道具**: 全機体の CLI・パッケージ・アプリと版を機体×道具の表で（版の違いを強調）、追加・削除の履歴、Do-gu との照合とデッキの下書き・登録（確認ダイアログつき）
- **AI**（Tauri 版）: Claude Code・Codex のセッションの要約。機体×日の使用量（トークン・セッション数）、モデル別、失敗の多いツール、長いセッション、PR、機体ごとの版、セッションの一覧。会話の本文は取り出さない
- **セキュリティ**（Tauri 版）: 機体×項目（待ち受け・防御・常駐の増減・ログイン・初めての接続先）の表と、外から届く待ち受け、防御の状態、自動起動の増減の記録、ログインの送り元、初めての接続先。接続のメタデータと OS の記録だけで、パケットの中身は取らない。Electron 版では、防御と待ち受けの判定が「状態」と各機体の所見にだけ出る
- **実行記録**: このアプリから実行した操作と、元に戻す操作

## 自動スキャン

アプリを開くとメニューバー（Windows は通知領域）に常駐し、ウィンドウを閉じても動き続ける。既定は分析 60 分・ログ 15 分ごと（「状態」画面かメニューで変更・停止）、道具の棚卸しは 24 時間ごと（「道具」画面で変更）、AI エージェントのセッションの取り込みは 30 分ごと（Tauri 版。「AI」画面で変更。初回など 25 秒で区切って続きがあるときは 2 分後に続きを読む）。「ログイン時に起動」をオンにすると、ログイン後にウィンドウを開かずに常駐する（この設定は画面で操作したときだけ変わる）。

外部のサービスや中身を確かめられない道具は使わない。各機体で動くのはこのリポジトリの `probes/` にある読めるスクリプトだけで、調査先には Python 標準ライブラリと OS 標準の PowerShell を使う。Tauri 版は OS の WebView を使い、Chromium を同梱しない。

macOS ではサイドバーが半透明（vibrancy）、Windows 11 では Mica。ライト／ダークとアクセントカラーは OS の設定に従う。

## 何をするか

| | 内容 |
|---|---|
| 分析 | 各機体で読み取り専用の調査を並列に実行。時間は台数・接続状態・計測の設定による。macOS は `probes/mac_probe.py` を python3 の標準入力へ、Windows は `probes/win_probe.ps1` を `~/.katala-tune/` に置いて PowerShell 5.1 で実行。アプリを動かしている機体はローカルで実行 |
| 判定 | `lib/rules.js`（CPU の飽和と暴走、メモリ圧迫・swap・コミット、メモリの大口、実効の空きによるディスク判定、熱、電源プラン、BSOD、WSL の上限、Defender、colima/Docker の割り当て、キャッシュ）と、ログ由来の所見（`lib/logs.js`: WHEA、GPU ドライバのリセット、メモリ枯渇、ディスクエラー、クラッシュ、カーネルパニック、jetsam、NeonMonitor の自動保護） |
| ログ | Windows のイベントログ（System / Application）と NeonMonitor の `guard.log`、macOS の DiagnosticReports とカーネルのエラーを、前回の続きから取り込む。冪等・秘密の伏せ字・件数上限つき。開いている間は15分ごとに自動で取り込む |
| 接続診断 | 開発用の `npm run network` で経路・IP・DNS設定、`-- --probe` でHTTPS疎通を読み取り専用で確認。画面への診断統合・接続先分析は未実装 |
| 計測 | 台帳に `"benchmark": false` を指定した機体では負荷計測を実行せず、状態だけを読み取る。省略時は従来どおり1スレッドの固定計算を5回（python）。前回との差で「最適化が効いたか」を見る（±15% 未満は誤差扱い） |
| 実行 | プロセス終了（同一性・負荷を直前に再確認、終了を確認できなければ「終了未確認」）、Windows の電源プラン切り替え、タスクの無効化／有効化／今すぐ実行、launchd ジョブの停止／読み込み／今すぐ実行（元に戻せるものは戻せる） |
| 道具 | 各機体のインストール先を読むだけ（`probes/mac_inventory.py`・`probes/win_inventory.ps1`）。パッケージマネージャもネットワークも使わない。読めなかった取り方は削除と見なさない |
| ネットワークとセキュリティ | 60 分ごとの分析に足す（読み取り専用。docs/observability.md の 6）。待ち受けているポートとプロセス（全部の口・特定のアドレス・この機体の中を分け、前回の分析と比べて新しく外から届くもの）、防御の状態（macOS: アプリケーションファイアウォール・Gatekeeper・XProtect、Windows: Defender・登録されたウイルス対策・ファイアウォールのプロファイル・直近 30 日の検出）、自動起動の増減（LaunchAgents・LaunchDaemons／タスク・サービス・Run キー・スタートアップ。初回は記録だけ）、外向きの接続の標本（機体・プロセスごとに覚え、最初の 7 日は覚えるだけ。宛先は手元の DB にだけ置き 30 日で消す）。ログインは取り込みに足す（macOS の sshd の失敗、Windows のセキュリティログ 4625・4624 のネットワーク／リモート。読めなければ「権限が無い」）。保護が止まっている機体は「状態」で異常。台帳の機体に `"network": false` で全部止める。共用機は接続先とログインを既定で集めない |
| AI エージェント | `probes/ai_sessions.py` を各機体の python で流し（Windows は python3 → python → py。無ければ「python が無い」）、続きの位置は DB が覚えて渡す。数・時刻・モデル・トークン・PR の URL だけを取り出す。台帳の機体に `"ai_sessions": false` で取り込まない |
| 保存 | Tauri 版は Rust の SQLite、互換確認用の JS 版は Node 内蔵の `node:sqlite`。`<userData>/data/katala-tune.db` |

安全のための決めごとは [docs/safety.md](docs/safety.md)。NeonMonitor のレビューで見つかった自動終了ツールの失敗の型を、どう避けているかもここにある。

## 機体台帳

`~/.config/katala-tune/nodes.json`（初回起動で `config/nodes.example.json` が置かれる）。

```json
{
  "protect": ["python.exe", "Code"],
  "nodes": [
    { "id": "my-mac", "alias": "my-mac", "os": "macos", "local_hostname": "My-MacBook-Pro" },
    { "id": "gpu-pc", "alias": "gpu-pc", "os": "windows", "note": "メモ" },
    { "id": "family-pc", "alias": "family-pc", "os": "windows", "shared": true, "network": false }
  ]
}
```

- `alias` は `~/.ssh/config` の Host 名。非対話（BatchMode）で入れる鍵が要る。Windows 側は OpenSSH サーバーと、既定シェル（Git Bash または PowerShell）で `powershell.exe` が動くこと
- `local_hostname` がこの機体の hostname と一致すると、SSH を使わずローカルで実行する
- `shared: true` は提案のみ。`protect` は終了を提案しても実行しないアプリ名
- `network`（任意）: `false` でネットワークとセキュリティ（待ち受け・防御・常駐の増減・外向きの接続・ログイン）を集めない
- `network_peers`（任意）: 外向きの接続先（宛先）を残すか。書いていなければ、共用機（`shared: true`）は残さない（他の人の通信の宛先を集めないため）。`true` で明示すれば共用機でも残す。`false` でどの機体でも残さない
- `network_logins`（任意）: ログインのアカウント名と送り元を残すか。書いていなければ、共用機（`shared: true`）は集めない。`true` で明示すれば共用機でも集める。`false` でどの機体でも集めない
- `expect`（任意）: その機体で動いているはずのもの。`{ "services": ["Tailscale"], "jobs": ["\\MyTask", "com.example.job"], "processes": ["ollama"] }`。「状態」で見張る
  - `expect.ignore_jobs`（任意）: 意図どおり 0 以外で終わる定期処理のラベル・タスク名。「状態」の「定期処理」の失敗に数えない（根拠に「既知 N 件を除く」と出る）。例 `{ "ignore_jobs": ["com.example.check-and-exit-1", "MyProbeTask"] }`
- `schedule`（任意）: `{ "enabled": true, "probe_minutes": 60, "logs_minutes": 15 }`
- `fleet`（任意）: 作者のフリートで使っている常時監視（katala-fleet、非公開）の概況を `op-agent` 経由で表示する連携。設定しなければ使わない

## 開発

```sh
npm ci --ignore-scripts   # Electron 本体を取得せず、検証用の開発依存だけを入れる
npm run doctor           # 必要なツールの読取確認
npm run check            # 版・JS・公開情報・Rust の検証
npm start                # Rust + Tauri で起動
```

互換比較が必要なときだけ `npm rebuild electron` 後に `npm run electron:start` で旧 Electron 版を起動する。旧版の配布コマンドには `electron:` を付ける。同じデータを使う両版を同時に動かさない。

`npm run oss-check` は台帳（`~/.config/katala-tune/nodes.json`）の id・alias・hostname と、この機体のユーザー名・ホスト名、Tailscale のアドレス、`op://` 参照、実在のホームパス、メールアドレスがリポジトリに無いかを調べる。探す語は台帳から実行時に読むので、リポジトリには書かない。CI（`.github/workflows/ci.yml`）は PR と main への反映で動く。

ログは、中央の DB へ後でそのまま送れる表の形で保存している。

Tauri 版のリリース（タグ `v*` で macOS / Windows をビルドして GitHub Releases の下書きに置く）と自動更新（確認して承認したときだけ、署名を確かめてから入れる）は [docs/release.md](docs/release.md)。版は `npm run version:set -- <版>` でそろえて上げる。

## ライセンス

MIT（[LICENSE](LICENSE)）
