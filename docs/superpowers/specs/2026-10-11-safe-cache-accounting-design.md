# macOS 保存領域とキャッシュ候補の安全な分類

## 承認済みの範囲

既存snapshotをJS/Rustのrulesで再評価する最小修正。収集出力・DB schema・probe性能・VM操作・実行actionは変更しない。公開・正式反映は別工程。

## 確認した根拠

- `probes/mac_probe.py` はキャッシュ候補とVM・シミュレータ保存領域を同じ `caches` 配列へ返す。`du -sk` のKiBを `2**20` で割るため `gb` の実単位はGiB。
- 現rulesはpathの意味を見ず、2 GiB以上の項目をすべて加算する。`Library/Caches` とその子 `Homebrew` が重複する。
- 現cacheCommandsはCoreSimulatorやColima/コンテナの存在から削除コマンドを提案する。

## 採用案

収集側で分類する案は過去snapshotに効かず、schema変更も不要なので採用しない。rulesで既知pathを完全一致分類する。

- 再生成可能な**候補**: `~/.npm/_cacache`、`~/Library/Caches`、`~/Library/Caches/Homebrew`、`~/.cache`、`~/Library/Developer/Xcode/DerivedData`。
- 保存領域: `~/Library/Developer/CoreSimulator/Devices`、`~/.colima`、`~/Library/Containers/com.docker.docker/Data`。ユーザーデータを含みうるためキャッシュ合計から除外。
- 有限の非負数値だけを計測として扱う。未知path・文字列数値・null・欠測・負値は候補化しない。完全一致の最初の有効計測だけを採用する。
- 有効計測済みの親があれば子を合算しない。親が欠測なら子を採用。親子の排除を既存の項目しきい値（2 GiB）より先に行う。
- キャッシュ候補は従来の項目しきい値2 GiB、合計しきい値10 GiBを維持。保存領域は有効計測の合計10 GiB以上で別のinfo finding `vm-storage` を出す。
- 保存領域のタイトルは「VM・シミュレータの保存領域（削除候補ではない）」とし、合計と内訳をGiBで示す。ユーザーデータを含むことを根拠として明記し、commands/actionは付けない。
- macOSの候補表示はGiBを使う。タイトルの合計は1 GiB単位の丸め、内訳はsnapshotの値（現probeは0.01 GiB単位）を保持する。再生成可否・利用状況の確認が必要と助言し、低空きでも無条件の削除へ誘導しない。
- cacheCommandsからCoreSimulator削除とDocker pruneを取り除く。既存のnpm/Homebrew/DerivedDataコマンドは有効な既知計測がある場合に限り維持し、新規コマンド・actionは追加しない。
- Windowsのキャッシュ分類・しきい値はこの変更の対象外。

## 未確認・限界

path分類は保存内容や回収可能容量の保証ではない。別名・symlink・APFS共有ブロックはsnapshotだけでは識別できない。実機操作・GUI・Windows実行の確認はこの作業の範囲外。

## 成功条件と戻し方

JS/Rustの回帰テストでVM-only、親子重複、親欠測、不正値、未知path、mixed snapshot、低空き時のコマンドを検証し、既存parityで全文一致を確認する。必須checkと履歴のOSS検査を通す。戻す場合はこの修正commitだけをrevertし、schemaの復旧やデータ操作は行わない。
