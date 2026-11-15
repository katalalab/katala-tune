# Rust + GPUIプレビュー

`tune-gpui`はtune-coreを直接使うネイティブ画面。概要、機体詳細、分析、保存済みスコア・CPU・空きメモリの推移を表示する。台帳の除外状態と理由を表示し、除外機の過去所見を現在の警告数に含めない。

```sh
cargo run -p tune-gpui --release --locked
cargo run -p tune-gpui --release --locked -- --config /path/to/ledger.json --data /path/to/preview-data
```

実行ファイルは`katala-tune-gpui`。Tauri版の`katala-tune`を上書きしない。既定のDBは通常のdataディレクトリの`gpui-preview`配下に分ける。`--data`で指定したDBは初期化・移行・分析保存の対象となるため、既存版と同時に同じDBを使わない。

`scripts/e2e-gpui.sh`はモデルと起動引数のテストを実行し、見本台帳と一時DBで起動smokeを行う。`--selftest`は`--config`と`--data`の両方が必要。不正な引数・不正な台帳ではDBを開く前に失敗する。起動smokeの成功は描画完了、ボタン操作、収集や保存のGUI受入を示さない。

ログ画面とライブ表示も利用できる。`--open logs` または `--open <node-id>` で起動時に開く画面を指定できる。任意の自己検証 `--live <node-id>` と `--logs` は `--selftest --config <ledger> --data <preview-data>` と組み合わせる。

既存版のセキュリティ、AI、Topology、承認付き変更操作、常駐・通知・自動更新はまだ移植していない。既存版を置換する正式リリースとしては扱わず、別DBで試す。Windowsではreleaseビルド、DirectXと実画面の起動を別途確認する。LinuxのGPUIリンクには`libxkbcommon-x11-dev`等の開発ライブラリが必要。

容量は実行ファイル単体とapp bundleを分けて測る。WebViewを使わなくても、機能範囲・画面・台帳・DBが違う状態のRSSを一律の軽量化率として比較しない。

GPUI 0.2.2のHTTPクライアントは `zed-async-tar` 経由で `xattr 0.2.3` を使う。この依存がLinuxで削除された `libc::ENOATTR` を参照するため、GPUIのLinux依存とlockfileは `libc 0.2.189` に固定する。GPUI側のxattr更新時に解除し、3 OSのCIで確認する。
