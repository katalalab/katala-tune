# CLI の使い方と版の確認

`tune --help`（`-h`）は公開コマンド一覧、`tune help specs` または `tune specs --help` は各コマンドの引数・出力形式・副作用を表示する。ヘルプは台帳・DB・接続の状態を開かず、不正な設定ファイルがあっても使用できる。

`tune --version`（`-V`、`tune version`）はインストールされたCLIの版を表示する。`tune version --json` は `katala_cli_version.v1` として版・OS・アーキテクチャ・仕様schema・実行ファイルのSHA-256を返す。版はビルド時のCargo package versionで、`cli_version` は `version` と同じ値。設定・DB・接続は不要。

`tune cli-info --json` は既存のschema version 1のコマンド一覧を返す。各コマンドの `arguments`、`output`、`effects` に加え、CLIの版・OS・アーキテクチャ・仕様schemaを含む。`effects` はオプションやプラットフォームを通じて起こり得る副作用の一覧で、入力検証schemaや実行許可ではない。たとえば `specs` の収集はDBを書き換えるが、`--saved` では書き換えない。

`tune versions [機体ID ...]` は対象のCLIの版・指紋を読み取り、実行側の期待版との一致をJSONで表示する。台帳を読むがDBは開かない。引数省略時は稼働中の収集許可機が対象。共用機は台帳の `specs: true` による明示的な読取許可が必要で、`specs: false` は除外する。未知のID、未対応オプション、収集対象外の指定は接続前に拒否する。古いCLIや接続失敗は結果の欠測・エラーとして確認する。不一致・取得失敗があればJSONを標準出力に残して終了コード1、全機体が一致すれば終了コード0を返す。

`tune specs --saved [機体ID ...]` は保存済みの仕様と最終試行だけをJSONで表示する。SSHやローカル収集を実行せず、既存DBを読取専用で開く。DB未作成の場合は `report: null` を返し、DBやディレクトリを作成しない。引数省略時は台帳の全機体を表示し、退役・共用・収集停止の機体も閲覧できる。出力の `read_only: true` はこの保存済み表示を示し、別プロセスでの収集中かどうかは判定しない。

`tune specs [機体ID ...]` は収集・保存、`tune specs-local` は台帳・DB不要の単体収集。[機体仕様の詳細](machine-specs.md)を参照。指定したIDとオプションはDB初期化前に検証する。その他の従来コマンドのJSON出力形式は変更していない。
