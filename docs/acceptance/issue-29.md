# Issue #29 受入記録（WSL の中の観測。一部）

**判定: 未完了。** 観測の収集・停止中の非接触・所見・機体の詳細は実装し、偽の `wsl.exe`（`Invoke-KtWsl` の差し替え）と、動作中・停止中の distro がある Windows 実機で確かめた。Docker／コンテナの使用量と、Windows 実機の「WSL なし」の記録は **未実施**。この記録を Issue #29 を閉じる根拠にしない。

## 受け入れ条件ごとの状況

| 条件 | 状況 | 証拠 |
|---|---|---|
| 既定で WSL に触れず、明示 opt-in 後も停止中・状態不明の distro に入らない | 実装済み。OS側の停止競合は残る | `wsl` 設定の省略・`false` では `wsl.exe` を呼ばない。`wsl: true` では偽の呼び出しを使い、一覧で停止中の distro と running-list の失敗時に `-d` を渡さない。running 確認の直後に停止するOS側競合は排除できず、opt-in の制約として明記。回帰テスト: `test/win-wsl-regression.ps1`（Windows PowerShell 5.1）、`test/wsl.test.js`、`test/collect.test.js` |
| running / stopped_unobserved / 取得失敗が別々に保存・表示される | 達成 | `wsl.distros[].state`（`running`・`stopped_unobserved`・`unknown`）と `reason`（`timeout` など）が snapshot に入る。所見 `wsl-unobserved`、機体の詳細「WSL の中」。`test/wsl.test.js`・`crates/tune-core/src/wsl.rs`（JS と Rust の一致は `tests/parity.rs`） |
| WSL 内のメモリ・上位プロセスが機体ビューに出る（Git Bash 既定シェルの ssh 経由でも動く） | 達成（動いている distro の実機） | 実機（Windows、ssh の既定シェルは Git Bash）で `probeNode` を実行し、動いている 2 つの distro の内側のメモリ・上位プロセス（8 件）・ディスクを取得。Docker Desktop の内部 distro は触らず `internal-distro-skipped` |
| Windows 実機の WSL あり／なし／停止中の 3 条件の記録（#19 とは別） | **一部**: 「あり・動いている」「停止中」は実機。「なし」は偽の `wsl.exe` のみ | 2026-10-12 の全機 probe は全件成功。Windows で動作中の 6 distro を観測し、自然に停止中の 3 distro は `stopped_unobserved` のまま。検証のための停止・起動は行っていない。「なし」の実機は未確認 |

## 実機で確かめたこと（読み取りだけ）

- `wsl.exe -l -v` で distro の一覧と状態を読み、動いている distro のうち内部のものを除いて観測した。2026-10-12 の記録では running 6 件、自然停止 3 件。停止確認のために起動・停止していない。
- 実機のホスト名・distro 名・値はこの記録に書かない。

## 残り

- Docker／コンテナの使用量（`docker system df` などは Docker Desktop を起こしうるので、起こさない方法を決めてから）。
- 全件台帳（#28 の対象台帳）への WSL の欄。
- opt-in 時の running 確認と内部実行の間に残るOS側競合（既定無効にしているが、厳密な無起動保証が要る場合の代替API）。
- Windows 実機の「WSL なし」の記録。
- 判定と観測の確認の間に distro が自然に止まると `wsl -d` がそれを起動しうる競合（ミリ秒。ドキュメントに明記）。
