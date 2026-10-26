# macOS Disk欠測の契約

Issue #33のうち、既存Disk B/sの欠測だけを対象とする。IOPS・待ち時間・paging・通信飽和・分析snapshotへの新指標追加は別の残件。

- Python/Rustとも、列挙失敗・対象なし・Statistics欠測を0 B/sにしない。
- 既存の `read_bps` / `write_bps` は方向ごとに数値またはnull。両方向が未取得でも `{ "read_bps": null, "write_bps": null }` を返す。読み手は従来の `disk:null` やdisk欄なしも許容する。
- 各方向は、同じdisk集合について全対象の有効な累積counterが連続2点あり、時間差が正で有限の場合だけrateを算出する。実測差0は数値0のまま保持する。
- 一部diskの欠測を除いて合計しない。片方向だけの欠測では、完全に取得できているもう一方向は継続できる。
- 取得失敗後の最初の有効値は新基点となり、その次の値からrateを再開する。初回欠測でも初期化済みreaderを無効化せず再観測する。列挙の更新間隔は従来どおり30秒。
- registry-entry IDは内部だけで対象交換を判定する。対象交換/増減・counter減少がある区間は欠測とし、別diskの増加でresetを隠さない。IDはwire/DBへ出さない。
- 読み手はnullをpointのdr/dwから省略し、画面の現在値は「–」。実測0は「0 B/s」。分集計は有効値のみで平均/最大を求め、全欠測方向はキーを省略する。過去に保存した0が欠測だったかは復元できないので書き換えない。
- 短い欠測区間を既存sparklineが線でつなぐことはある。線の連続性を観測の完全性の根拠にしない。新しいstatus schema・DB移行・線の描画変更は追加しない。

## 合成回帰

`test/fixtures/disk-rates.json` の固定期待値をRust/Pythonの双方へ渡す。正常/idle、初回欠測、方向別/対象別欠測と復帰、同数diskの交換、他diskの増加に隠れたreset、無効時間差を含む。

`test/fixtures/disk-missing.py` は実コードのDisk/mainをASTで読み、OS取得境界と時計のみをfakeに置き換える。実IOKit・SSH・GPU負荷・実sleepは不要。列挙失敗/途中無効化、CF型/数値の不正、ID取得失敗/重複、例外後の再基点化を検査する。`test/disk-missing.test.js` がこの試験と表示/最新pointの遷移を実行する。

RustのDisk単体回帰とPythonとのrate parityに加え、tune-coreの回帰はnull/旧sample/partial/実測0のparse→point、分集計→一時DB再open→seriesのunknown維持を検査する。例えばread `[120,未取得,60,0]` は平均60/最大120となる。未取得を0として平均45にしない。

これらは合成値の扱いの証拠であり、実IOKit/CFの故障注入や、Issue #33全体の両OS実測・飽和判定・分析時間・snapshot拡張の受入完了を示すものではない。
