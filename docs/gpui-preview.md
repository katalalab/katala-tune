# Rust + GPUIプレビュー

`tune-gpui`はtune-coreを直接使うネイティブ画面。概要、機体詳細、分析、保存済みスコア・CPU・空きメモリの推移を表示する。台帳の除外状態と理由を表示し、除外機の過去所見を現在の警告数に含めない。

```sh
cargo run -p tune-gpui --release --locked
cargo run -p tune-gpui --release --locked -- --config /path/to/ledger.json --data /path/to/preview-data
```

実行ファイルは`katala-tune-gpui`。Tauri版の`katala-tune`を上書きしない。既定のDBは通常のdataディレクトリの`gpui-preview`配下に分ける。`--data`で指定したDBは初期化・移行・分析保存の対象となるため、既存版と同時に同じDBを使わない。

`scripts/e2e-gpui.sh`はモデルと起動引数のテストを実行し、見本台帳と一時DBで起動smokeを行う。`--selftest`は`--config`と`--data`の両方が必要。不正な引数・不正な台帳ではDBを開く前に失敗する。起動smokeの成功は描画完了、ボタン操作、収集や保存のGUI受入を示さない。

ログ画面とライブ表示も利用できる。`--open logs` または `--open <node-id>` で起動時に開く画面を指定できる。任意の自己検証 `--live <node-id>`、`--logs`、`--network <node-id>`（受動の到達性確認）、`--confirm-demo`（確認欄を出して「やめる」で終える）は `--selftest --config <ledger> --data <preview-data>` と組み合わせる。`--open <page>` は status・logs・topology・resources・power・tools・ai・specs・processes・security・network・link・review・actions・schedule を受け付ける。

画面は、概要・機体詳細（推移とライブ）・状態・リソース・プロセス・セキュリティ・ネットワーク・接続・レビュー・電力・Clock・スペック比較・道具・AI・ログ・構成図・実行記録・スケジュールを持つ。
どれも保存済みの値を読み取りで見せる。次の点は既存版と違う。

- 接続は、ペア済みの tune-agent の一覧だけ。ペアリング（コード入力・鍵の作成）と解除はここではしない（既存版か CLI）。
- ネットワークは「全機を確認」を押したときだけ各機体へ接続し、受動の到達性（真偽）だけを出す。
- レビューは task-completion.v1 を一時表示するだけで、画面へのドラッグか `--review <ファイル>` で読む。完了・受入は判定せず、外部参照の取得も保存もしない。
- 変更操作は、所見に付く提案の「提案: ○○…」と、実行記録の「元に戻す…」から、確認欄を通したときだけ実行する。確認欄は「やめる」が既定で、「実行する」を押したときだけ進む。
  やめる・背景を押す・別の確認に置き換わる・2 分の時間切れ・終了はすべて進まない。許可リスト、共用機・保護対象の拒否、確認後の台帳の読み直し、実行記録は tune-core の `confirm_and_run` / `undo` が行い、GPUI 側は確認だけを持つ。
  承認して実際に実行する経路は、macOS のローカル機体で、テストが自分で起動した `sleep` を承認の関門を通して終了させる結合テスト（`gate.rs`）で通している。運用中の機体への実行は、まだ行っていない。

常駐・通知・自動更新はまだ移植していない。既存版を置換する正式リリースとしては扱わず、別DBで試す。Windowsではreleaseビルド、DirectXと実画面の起動を別途確認する。GPUI依存はmacOS / Windowsだけに限定し、LinuxではcoreとCLIの検証を継続する。

容量は実行ファイル単体とapp bundleを分けて測る。WebViewを使わなくても、機能範囲・画面・台帳・DBが違う状態のRSSを一律の軽量化率として比較しない。

LinuxではGPUIを起動せず、引数検証後に未対応を示して終了する。GPUIの間接依存 `xattr 0.2.3` と新しいlibcのLinux非互換を避けるためのプラットフォーム境界でもある。libcの一時的な固定は解除し、既存mainと同じlockfileを使う。WindowsのGUI起動ではコンソールを開かない。
