# 変更履歴

## 未リリース

- 台帳に機体の運用状態（`status`: online・retired・no-ssh・paused・out-of-scope）を追加。online 以外には接続も変更もせず、到達不能（online）と退役を区別する。対象台帳を「状態」の画面と `tune ledger` に出す。`status` を持たない従来の台帳はそのまま動く。
- Windows の分析に WSL の中の観測を追加（読み取り専用）。動いている distro だけ内側のメモリ・PSI・上位プロセス・ディスクを読み、停止中は起動せず `stopped_unobserved`。内側の逼迫・`vmmem` との差・未観測の所見と、機体の詳細の「WSL の中」。Docker／コンテナの使用量は未対応（docs/acceptance/issue-29.md）。

## 0.4.0

- Rust + Tauri 2 を標準の開発・配布経路にし、OS の WebView を利用する。
- 道具の棚卸し、Do-gu の確認つき登録、本文を保存しない AI セッション集計、表示中だけ動くライブ計測。
- Windows ログ取り込みの固定ファイル競合と、SSH 終了後の待機を修復。
- Windows ライブ計測をファイルを残さない標準入力に変更し、スクリプトの実行スコープを修復。
- AI ログの一行を 1 MiB に制限し、超過分のメタデータ欠測を明示する。
- ライブの差分描画とログの文字列処理を軽量化。
- ログ由来のディスク故障の誤判定、launchd 起動制約、意図的な非0終了の扱いを修正。
- Mac universal / Windows x64 の更新署名つきリリース、版の照合、配布 manifest と SHA256 の検証。CI は更新署名を `tauri.conf.json` の公開鍵で暗号として検証する（鍵 ID・本体の署名・global signature・署名に入った版）。
- Rust / Node の版固定、読取専用の環境点検、3 OS の CI と Mac / Windows のアプリリンク検証。

更新は通知と確認を経て実行する。Apple 公証・Windows 発行元コード署名は別途必要で、現時点の配布物には付かない。Tauri updater の更新署名と OS の発行元署名は異なる。
