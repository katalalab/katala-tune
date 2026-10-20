# 手動配布用のビルド

`artifact-delivery.yml` は、レビュー済みの **その時点の main の40桁SHA** を `source_sha` に指定し、main ref で手動実行する。要求SHA、dispatch SHA、workflow SHA、実checkout HEAD が一致し、checkout が取得済み origin/main の祖先であることを確認する。main が進んで一致しなければ停止する。古いSHAへcheckoutし直す経路はない。

macOS arm64 と Windows x64 の2ジョブだけが動く。権限は `contents: read`。タグ、Release、公開feed、release environment、署名秘密鍵は使わない。既存のupdater設定と公開release workflowは変更しない。成果物はActions artifactに7日間保持する。artifactの閲覧範囲はリポジトリのActions権限に従うため、非公開の機体情報や秘密を入れない。

Rust 1.99.0、tauri-cli 2.12.1、Node 24.22.0、公式actionのcommit SHA、runnerのOS世代ラベル（`macos-15` / `windows-2025`）を固定する。GitHub hosted runnerの実イメージは更新され、immutableなイメージを指定する仕組みはない。実際の `ImageOS` / `ImageVersion` を記録し、イメージまで再現可能とは扱わない。

`cargo build --release --locked` でCLI（Windowsではagentも）を作り、sidecarとして配置する。その後、Tauri CLIでアプリをビルドする。Macは `--bundles app`、Windowsは `--no-bundle`、いずれも `-- --locked` を渡す。Tauri CLIの `tauri/custom-protocol` により素のrendererを埋め込む。frontendの追加ビルドや公開release用configは不要。

`scripts/artifact-delivery.js source` は、実HEAD/tree、Git上のCargo.lockのblobとSHA256、checkoutのCargo.lockのSHA256（CRLFを区別）、renderer・bridgeを含むソース、ビルド引数、sidecar入力、ツール実版、workflow ref/SHA、run ID/attemptを記録する。ビルド後にもソースと入力を再照合する。

Macは同梱CLIを署名前に入力と比較し、`Contents/Resources/katala-source.json` を追加してアプリ全体をad-hoc再署名する。署名・Info.plist・arm64・実行権限・全ファイルのハッシュを確認し、tar.gzを作り、安全なパスと通常ファイルのみであることを検査してから一時領域へ展開し、全ハッシュと署名を再確認する。シンボリックリンクやhardlink、予期しないファイルは現在の固定bundle構成では受け付けない。

Windowsは `katala-tune.exe` / `tune.exe` / `tune-agent.exe` の3点のみ。x64 PE32+実行形式、GUI/console subsystem、GUIの埋込Common Controls v6 manifest、入力sidecarとの一致を検査する。`manifest.json` に各ファイルのサイズとSHA256を記録する。既存の出力先は空でも上書きしない。未知のschema・余剰フィールドは拒否する。

ダウンロード後はmanifestのsource SHA・run ID/attemptとActions実行を照合し、ファイル・archiveのSHA256とサイズを検証する。Mac archiveは `checkArchive` で検査してから新しい空の場所へ展開し、`inspectPayload` でmanifestの `proof` に対するファイル・署名・アーキテクチャを再検査できる。Windowsも `inspectPayload` で3点を再検査する。生成された検査結果全体をmanifestと比較する。ヘルパーは同じレビュー済みsource SHAから使う。

これらは配布物の検証であり、Developer ID署名・notarization、Windowsコード署名、インストール、GUI/WebView2起動、実機での要求動作の証明ではない。通常アプリやDBを起動・変更する処理は含めない。

参照: [Tauri CLI](https://v2.tauri.app/reference/cli/)、[Tauri sidecar](https://v2.tauri.app/develop/sidecar/)、[GitHub runner](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)、[GitHub実行変数](https://docs.github.com/en/actions/reference/workflows-and-actions/variables)、[upload-artifact](https://github.com/actions/upload-artifact)。
