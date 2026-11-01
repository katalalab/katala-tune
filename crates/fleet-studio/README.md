# Fleet Studio

Katala Tune の保存済みの機体分析と構成情報を、読み取り専用で開く GPUI の試験的な入口です。既存の Tauri アプリや `tune` CLI はそのまま使えます。

## ビルドと起動

Rust 1.99.0 と GPUI 0.2.2 を使用します。GPUI は `=0.2.2` に固定し、このディレクトリの `Cargo.lock` で依存関係を固定しています。更新時は別 branch でバージョンと lockfile を更新し、Windows と macOS のビルド・起動・操作を再検証してください。GPUI の依存関係は、既存の Tauri アプリの lockfile と分離しています。

Windows の例（PowerShell、リポジトリのルートから）:

```powershell
$env:CARGO_TARGET_DIR = "$PWD/target/fleet-studio"
$env:CARGO_PROFILE_DEV_DEBUG = '0'
cargo build --manifest-path crates/fleet-studio/Cargo.toml --features gui --locked -j 1
& "$env:CARGO_TARGET_DIR/debug/fleet-studio.exe"
```

Windows で Rust コンパイラ自体が内部エラーやアクセス違反で終了する場合は、既存の生成物を消さず、別の `CARGO_TARGET_DIR` を指定して `CARGO_INCREMENTAL=0` で再実行できます。これは切り分け手段であり、コンパイラ障害の根本修正ではありません。

macOS の例:

```sh
cargo run --manifest-path crates/fleet-studio/Cargo.toml --features gui --locked -j 1
```

macOS はフル Xcode と Metal コンパイラが必要です。Command Line Tools の存在だけでは足りません。この版の macOS ビルドと画面は未検証です。Windows のネイティブ画面でビルド・起動を確認しています。

サンプルを明示して試す場合は、Windows では実行ファイルに `--sample` を追加します。`cargo run` では `-- --sample` を追加します。サンプル画面にはその旨を表示し、台帳・DB の読取や LLM 接続を行いません。

## データの出どころ

- 機体台帳は Katala Tune の `nodes.json`、分析・構成図は同じアプリの既存 DB を使います。`KATALA_TUNE_CONFIG` と `KATALA_TUNE_DATA_DIR` の既存指定も使えます。DB は `Store::open_read_only` で開き、作成・移行しません。保存済みの最終成功時刻と最新の失敗を分けて表示します。これは現在の SSH 接続結果ではありません。
- 分析の鮮度は、Katala Tune と同じ優先順（DB の設定、台帳、既定値）による `probe_minutes` の 3 倍を基準にします。構成情報は実際の facts 取得時刻から 24 時間を基準にします。LLM 一覧は 5 分、衛生監査は 1 時間を超えると古いと表示し、画面を開いたままでも 30 秒ごとに期限を再判定します。失敗や時刻不明は正常と扱わず、取得失敗時は前回の値と時刻を区別して残します。
- `この機体を監査` は既存 `tune-core::hygiene` を使い、台帳上で現在の hostname と一致する機体だけを読み取ります。`明示対象を監査（SSH）` は、ボタン操作時だけ既存台帳で `repo_hygiene: true` と明示された非共有の遠隔機へ既存の SSH 設定で接続します。この機体も対象です。退役・停止・対象外と共有機には接続しません。会社管理の機体は、適用される規則と権限を確認してから台帳上の対象を判断してください。SSH の実際の接続先 hostname はまだ照合しないため、表示する台帳名を実機の確認済み名として扱わないでください。各機の監査には時間・件数の上限があり、失敗・打ち切り・未観測を表示します。既存 probe は存在しない探索先やアクセス拒否を一部判別できないため、監査の網羅性は不明です。Git の ahead / behind は最後の fetch 時点の追跡参照との差で、現在の remote とは限りません。fetch、リポジトリの変更、モデルの推論は行いません。
- ローカルの Strata `127.0.0.1:18080`、Ollama `127.0.0.1:11434`、LM Studio `127.0.0.1:1234` には、それぞれモデル一覧の GET だけを送ります。各応答には 2 秒・256 KiB の上限を設けています。サーバーに届かない場合は不明／タイムアウトであり、停止を断定しません。以前の成功と今回の失敗は別の時刻で示します。
- 追加の LAN・Tailscale 接続先は、実行時の `FLEET_STUDIO_LLM_ENDPOINTS` に `表示名|IP:ポート|openai` または `表示名|IP:ポート|ollama` の形式で指定します。複数はセミコロンで区切り、最大 8 件です。名前解決は行わず、ループバック・プライベートネットワーク・Tailscale の IP のみ受け付けます。不正な指定は件数を表示します。設定は保存しません。

`R` で保存データを再読込、`A` でこの機体の衛生監査、`/` で検索し Enter で保存 DB の構成情報を検索します。検索確定後は矢印と Enter で表示中のリポジトリを開きます。監査結果のローカルディレクトリ、リポジトリの HTTPS 元情報、説明と Issue のリンクも開けます。検索は現時点でキー入力のみで、IME・貼り付けには未対応です。

## 制約と次の候補

この画面は `fleet-status` を実行せず、Katala Tune が保存した分析と既存の衛生監査 API を利用します。構成情報は Katala Tune の件数制限付き UI 投影を読み、表示した scope だけを確認できます。entity が無い失敗 scope は見えないため、表示分が新しくても全体の成否は不明です。1 回の検索は最大 200 件で、制限や未確定候補は表示します。保存済み構成図がなければリポジトリ一覧は不明になります。DB 読込・検索が 5 秒を超えた場合は待機中と表示し、同じ読込を重複起動しません（元の処理を強制終了する保証はありません）。LLM の追加接続先は自動発見しません。常時監視、設定変更、再起動、任意コマンド実行、モデルのロード・推論は提供しません。

次は macOS の実機検証、日本語 IME・貼り付け、各機の現在の到達性、監査対象の個別選択、関連ドキュメント・Issue の紐付けを優先します。

検証:

```sh
cargo test --manifest-path crates/fleet-studio/Cargo.toml --locked -j 1 -- --test-threads=1
```
