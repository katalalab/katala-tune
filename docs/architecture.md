# 構成（2026-10-07 決定）

**Rust ＋ Tauri 2 ＋ Web UI（TypeScript）**。デザインしやすさは Web 側に、データの読み込み・集計・検索・分析は Rust 側に任せる。

| 層 | 役割 | 置き場所 |
|---|---|---|
| Web UI（TypeScript） | レイアウト、表、グラフ、アニメーション。Notion 風 | `renderer/`（TypeScript 化のときに `ui/` へ） |
| Tauri 2 | Mac / Windows のアプリ化、画面と Rust の橋渡し、常駐・通知・ダイアログ | `src-tauri/` |
| 分析エンジン（Rust ライブラリ） | 台帳、保存（SQLite）、調査の実行（SSH・ローカル）、判定、状態、ログ、道具の台帳、AI エージェントのセッション、変更操作の計画と検証 | `crates/tune-core`（Tauri に依存しない） |
| 検証用 CLI | Tauri なしで分析エンジンを動かす | `crates/tune-cli` |
| 各機体での調査 | 読み取り専用。1 行の JSON を返す | 今は `probes/`（Python・PowerShell）、後で `tune-agent`（Rust の単一バイナリ） |
| 機体どうしの接続 | 機体鍵・ペアリング（SPAKE2）・端末間暗号化（Noise）・要求と応答。経路に依存しない（docs/connectivity.md） | `crates/tune-link` |
| 各機体の常駐 | ペア済みの操作卓だけに読み取り専用の調査を返す。OpenTelemetry の受け口（docs/observability.md） | `crates/tune-agent` |

## なぜこの組み合わせか

- **軽さ**: Tauri は OS の WebView（macOS は WKWebView、Windows は WebView2）を使い、ブラウザエンジンを同梱しない。Electron 版は約 250MB で、常駐中もメモリを使い続ける
- **デザイン**: 余白・文字組み・SVG のグラフ・データベース風の表は Web が一番作りやすく、Mac と Windows で同じ見た目になる
- **分析の速さと安定**: 大量のログやセッションの集計を Rust で行い、画面のスレッドを止めない
- **Swift は選ばない**: SwiftUI は Apple のプラットフォーム専用で、Windows では別の UI（WinUI など）を持つことになる

## データの流れ（軽く保つための決まり）

1. **元データは Rust 側（SQLite）に置く**。画面には集計結果と、表示する範囲だけを返す
2. 表とログはページングする（件数の上限は Rust 側で守る）。グラフは Rust で集計した点（数百点まで）だけを返す
3. 重い処理（調査・取り込み・集計）は非同期で動かし、画面を待たせない。進み具合はイベントで知らせる
4. 大きな配列を返すときは JSON 以外（ArrayBuffer など）も検討する（Tauri の資料: https://v2.tauri.app/develop/calling-rust/）

## 各機体での調査

- 契約は「標準入力でスクリプト（将来はバイナリ）を受け取り、標準出力に 1 行の JSON を返す。読み取り専用」。この契約を保てば、スクリプトから Rust のバイナリに替えても分析エンジン側は変わらない
- 続きの位置（ログ・AI エージェントのセッション）は分析エンジンが覚えて毎回渡す。機体側には状態を書かない
- `tune-agent` の配布: アプリが SSH で `~/.katala-tune/` に置き、ハッシュを確かめてから実行する。読めるスクリプト（`probes/`）は予備として残す

## ライブ表示（画面が見ているあいだだけ 1 秒ごと）

60 分ごとの分析とは別に、「リソース」と「機体」の画面で「ライブ」を入れると、CPU（全体・コアごと）・メモリ（使用率・swap / コミット）・ディスクとネットワークの速度・GPU（nvidia-smi があるとき）を 1 秒ごとに、上位プロセスを 5 秒ごとに流す（Tauri 版だけ）。

| 層 | 中身 | 置き場所 |
|---|---|---|
| サンプラー | 読み取り専用。1 行目に hello、以後 1 行 1 JSON を出し続ける。macOS は標準ライブラリだけ（ctypes で OS の統計を読み、他ユーザーのプロセスは setuid の ps を 15 秒ごと）。Windows は Rust のネイティブ版（`tune-agent sample`。PDH で性能カウンタを直接読み、プロセスは NtQuerySystemInformation 1 回。`~/.katala-tune/bin/tune-agent.exe` に置く）を先に使い、無ければ PowerShell 版（.NET の性能カウンタ）。GPU はどちらも 120 回で自分から終わる `nvidia-smi dmon` | `crates/tune-agent/src/sample*.rs`・`probes/live_mac.py`・`probes/live_win.ps1` |
| 取得の経路 | 今は子プロセス（この機体はローカル、ほかは SSH）。将来は tune-agent の暗号化通信に差し替える | `tune-core::live::Route` |
| 解析・保持 | 行を読んで範囲に収め、機体ごとに直近 300 点を Rust 側に持つ | `tune-core::live`（`parse_line`・`Ring`） |
| 管理 | 開始・停止、画面からの合図（liveStart の呼び直し）が 2 分途切れたら自動停止、同時に 6 台まで、続けての失敗は 3 回までつなぎ直す、アプリの終了・ウィンドウを閉じたときの停止 | `tune-core::live::Live`、`src-tauri/src/live.rs` |
| 画面 | 1 秒ごとに、変わった機体の新しい点だけをまとめた 1 つのイベント（`live`）を受け、直近 3 分のスパークラインを描く | `renderer/live.js`・`Charts.spark` |

止めたら機体に残らないこと（実機で確かめた失敗から）:

- サンプラーには標準入力を開いたまま渡し、閉じたら終わる（macOS・Windows のローカル実行）
- SSH の多重化（ControlMaster）は使わない。多重化の親が接続を持ち続けると、ssh を止めても機体側に伝わらない
- Windows の SSH（Git Bash 経由）は、セッションが終わってもサンプラーの標準入出力が開いたまま残る。サンプラーは自分の上にいる sshd のプロセスを見張り、終わったら止まる
- どの場合も、サンプラーは 15 分（`sampler_max_age`）で自分から終わり、続けるなら数えずにつなぎ直す

## 電力・Clockと手動の接続診断

- 電力・Clockは通常のprobeとライブサンプラーに取得元付きの数値を追加する。`tune-core::power` が校正・kWh・費用を計算し、`power_observation` が取得元・欠測・セッション・観測窓を区別して積分する。操作卓ごとの計算条件と観測は `engine_power` が私有DBに保存し、`renderer/power.js` が表示する。[電力・Clock](power-clock.md)
- 電源設定は既存の確認付きaction経路を使い、対象側ロック・読戻し・条件付き復元を追加する。probeやライブサンプラーから機体を変更しない。
- 手動診断は `tune-core::network`、`probes/mac_network.py`・`win_network.ps1`、`renderer/network.js`。native CLIも同じエンジンを利用し、DB・台帳・自動スキャンには結果や設定を追加しない。[手動の接続診断](network-connectivity.md)
- WindowsのSSH選択は既存Git同梱版の絶対パス、OS標準の絶対パス、PATHの順。Windows接続診断のprobeは短い固定loaderとBase64単一行（LF終端）に分け、ReadLineで受信してEOF待ちを避ける。SSH設定やシステムPATHは変更しない。

## 常時監視とハブ（画面が見ていなくても集める）

ライブと同じサンプラー・同じ管理（`tune-core::live::Live`）を使い、`tune-core::monitor::run` が 15 秒ごとに合図を送って流れを保つ。終わった 1 分ぶんを `metrics_minute`（1 機体 1 分 1 行、項目ごとに [平均, 最大]、14 日）へ集計する。

| 役割 | 動き |
|---|---|
| ハブ（常時監視オン・ハブ未選択） | 全機体を流し続けて集計する。分析・詳細なログは 1 時間ごと |
| 写す側（ハブを選んだ操作卓） | 1 分ごとに `ssh <ハブ> ~/.katala-tune/bin/tune(.exe) export --since <前回>` で集計と最新の分析結果を写す。ハブの数字が 5 分より古い（ハブのアプリが止まっている）ときは、常時監視がオンなら自分で集める。ハブの分析結果が新しいあいだは自分では分析しない。画面を見ているあいだのライブは自分で流す |

写しの形は `{v:1, host, at, latest, metrics:[…], snapshots:[…]}`。時刻はハブの時計で、写す側は `at` を次の起点にする（機体の時計のずれに左右されない）。常駐（startup）の登録はしない。アプリを開いているあいだだけ動く。

## 移行の順番

| 段階 | 中身 | 状態 |
|---|---|---|
| 0 | 公開（MIT）、点検（oss-check・gitleaks）、Electron 44、CI | 済み |
| 1 | `tune-core`・`tune-cli`・Tauri の殻。既存の画面を `window.tune` の橋渡しでそのまま動かす。DB と台帳は Electron 版と同じ場所・同じ表 | 作業中 |
| 2 | 画面を Notion 風に作り直す（グラフ・プログレスバー・データベース風の表の部品） | 作業中 |
| 3 | 道具の台帳と Do-gu、AI エージェントのセッションを `tune-core` と画面に足す | 道具と Do-gu: `tune-core`（inventory・dogu。JS 版との一致は tests/parity_inventory.rs）と画面「道具」。AI エージェント: `tune-core`（ai_sessions。取り込み・時間ごとの量・集計・ページング。出どころの台帳 provenance・重複を除いた量と費用の推定 ai_usage・prices・Codex の残り枠 codex_limits）と画面「AI」（Tauri 版だけ） |
| 4 | 画面を TypeScript にする（ビルドは Vite か esbuild） | 未着手 |
| 5 | SSH の管理（到達性・認証の経路・鍵の種類と古さ・known_hosts） | 設計 |
| 6 | `tune-agent`（各機体の調査を Rust の単一バイナリに） | 土台: `tune-link`（ペアリング・機体鍵・端末間暗号化）と `tune-agent`（pair・run・status、probe を 1 つ、OTLP の受け口）、画面「接続」の骨組み。調査・ライブの経路はまだ SSH |
| 7 | Electron 版を退役 | 段階 1〜3 が同じことをできてから |

## 目標値（測って確かめる）

- アプリの大きさ: 20MB 未満（macOS の .app）
- 待機中のメモリ: Electron 版の半分未満
- 全機体の分析: 6 台で 15 秒未満。道具の棚卸し: 1 台 3 秒未満。AI エージェントのセッション: 初回の後は 1 秒前後（差分だけ読む）

## 変えないこと

- 調査は読み取り専用。変更する操作は許可リスト・確認ダイアログ・実行直前の台帳の読み直しを通す（docs/safety.md）
- 外へ送るのは操作者が承認したときだけ。秘密・会話の本文・ツールの入出力・コマンドライン引数は取り出さない・保存しない
- 個人・環境の情報をリポジトリに書かない（`npm run oss-check`）
