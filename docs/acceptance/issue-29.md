# Issue #29 受入記録（WSL の中の観測。一部）

**判定: 未完了。** 観測の収集・停止中の非接触・所見・機体の詳細は実装し、偽の `wsl.exe`（`Invoke-KtWsl` の差し替え）と、動作中・停止中の distro がある Windows 実機で確かめた。Docker／コンテナの使用量と、Windows 実機の「WSL なし」の記録は **未実施**。この記録を Issue #29 を閉じる根拠にしない。

## 受け入れ条件ごとの状況

| 条件 | 状況 | 証拠 |
|---|---|---|
| 停止中の distro に起動・コマンド実行をしないテスト（偽 `wsl.exe`） | 達成（偽の呼び出しで確認） | `test/win-wsl-regression.ps1`（Windows PowerShell 5.1）。停止中の distro に `-d` が一度も渡らないこと、動いているかを読めないとき・何も動いていないときも `-d` が 0 回であることを確認。壊した実装（停止中の分岐を無効にした版）ではこの試験が落ちることも確認。`test/wsl.test.js` が区間の構造（`wsl.exe` の起動は1か所、`--exec` は1か所で動いている distro の分岐の中だけ、変更系の操作を含まない）を確認 |
| running / stopped_unobserved / 取得失敗が別々に保存・表示される | 達成 | `wsl.distros[].state`（`running`・`stopped_unobserved`・`unknown`）と `reason`（`timeout` など）が snapshot に入る。所見 `wsl-unobserved`、機体の詳細「WSL の中」。`test/wsl.test.js`・`crates/tune-core/src/wsl.rs`（JS と Rust の一致は `tests/parity.rs`） |
| WSL 内のメモリ・上位プロセスが機体ビューに出る（Git Bash 既定シェルの ssh 経由でも動く） | 達成（動いている distro の実機） | 実機（Windows、ssh の既定シェルは Git Bash）で `probeNode` を実行し、動いている 2 つの distro の内側のメモリ・上位プロセス（8 件）・ディスクを取得。Docker Desktop の内部 distro は触らず `internal-distro-skipped` |
| Windows 実機の WSL あり／なし／停止中の 3 条件の記録（#19 とは別） | **一部**: 「あり・動いている」「停止中」は実機。「なし」は偽の `wsl.exe` のみ | 2026-10-12 の全機 probe は全件成功。Windows で動作中の 6 distro を観測し、自然に停止中の 3 distro は `stopped_unobserved` のまま。検証のための停止・起動は行わなかった。「なし」の実機は未確認 |

## 実機で確かめたこと（読み取りだけ）

- `wsl.exe -l -v` で distro の一覧と状態を読み、動いている distro のうち内部のものを除く全部で、`probes/win_probe.ps1` の観測区間が内側の値を返した。停止中の distro は無く、何も起動していない。
- 実機のホスト名・distro 名・値はこの記録に書かない。

## 残り

- Docker／コンテナの使用量（`docker system df` などは Docker Desktop を起こしうるので、起こさない方法を決めてから）。
- 全件台帳（#28 の対象台帳）への WSL の欄。
- 観測を止める台帳の設定（`"wsl": false` など）と、distro 数・時間の上限の調整。
- Windows 実機の「WSL なし」の記録。
- 判定と観測の確認の間に distro が自然に止まると `wsl -d` がそれを起動しうる競合（ミリ秒。ドキュメントに明記）。
