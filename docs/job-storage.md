# ジョブの容量を測る

ローカルの実際の出力ディレクトリを作り、手元の通常のビルドや変換コマンドを末尾に指定する。例えばCargoプロジェクトのルートで空のディレクトリ `build-output` を用意し、次を実行する。

~~~sh
tune storage-run --job-id build-cold --target . --report cold.json --expected-peak-bytes 1073741824 --os-reserve-bytes 32212254720 --other-reserved-bytes 0 --category intermediate=build-output --interval-ms 250 -- cargo build --target-dir build-output
~~~

2回目は `--job-id build-warm --report warm.json` に替え、出力先・入力・ビルド条件をそろえる。予測ピーク・OS用余裕・他の仕事の予約量はバイトで明示する。必要量が空き以下なら、呼び出し元の作業ディレクトリでプログラムと引数をそのまま実行する。CLIはジョブの予約を一括管理しないため、並列実行する際はスケジューラで予約量を調整する。余裕が足りないときや空きを取得できないときは処理を開始せず、新規レポートに理由を保存する。予約量は実行中の上限や強制停止条件にはならない。

`--target` は作業が実際に書き込むボリューム上の既存ディレクトリ。Macでは重要用途の実効空き、取得できなければ直接使える空きで開始を判定し、採用値と算出方法を `preflight` に示す。Windowsでは呼び出しユーザーが使える容量を用いる。WSLのゲスト内での起動は保留する。WSLで実行する場合は、Windows側の `tune` にVHDXのあるボリュームを指定し、そのディストリビューション内の空きも別途確認してから `-- wsl.exe -d <distro> -- <program> ...` を渡す。複数ボリュームに書くジョブは、測定対象以外の保存先にも個別の予算が要る。計測期間は指定した子プロセスの終了までで、バックグラウンドへ切り離した子孫プロセスの仕事は含まない。

`--category` は `original`（原本）、`artifact`（成果）、`intermediate`（中間物）、`log`（記録）から選び、対象ディレクトリからの相対パスを指定する。既存の別々のディレクトリを指定し、互いに内包させない。レポートは分類対象外に置く。リンクや別ボリューム、読めないエントリを含む分類値は欠測となる。走査は開始直前と終了時に行うため、書き込みが並行したディレクトリでは瞬間の厳密なスナップショットにはならない。

### WSLでホストとゲストの両方を確かめる例

Windows PowerShellで対象ディストリビューションの実VHDXの親ディレクトリを取得する。下例はUbuntuの専用出力先を `/work/run-out`、必要な追加量を1 GiBとする。VHDXがあるWindows側にはOS用に30 GiB、ext4側にも2 GiBを残す例で、既存ジョブの予約があればそれぞれの必要量に加える。Windows側のレポート保存先には未使用のファイル名を選ぶ。

~~~powershell
$distro = 'Ubuntu'
$guestDir = '/work/run-out'
$entry = @(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss' | Get-ItemProperty | Where-Object DistributionName -eq $distro)
if ($entry.Count -ne 1) { throw 'WSL distribution not uniquely identified' }
$vhd = Get-Item -LiteralPath (Join-Path $entry[0].BasePath 'ext4.vhdx') -ErrorAction Stop
tune storage-run --job-id wsl-build --target $vhd.DirectoryName --report wsl-build.json --expected-peak-bytes 1073741824 --os-reserve-bytes 32212254720 --other-reserved-bytes 0 -- wsl.exe -d $distro -- python3 /work/checked-build.py $guestDir
~~~

例の `/work/checked-build.py` はジョブ側で用意する。ゲストの空きを**子プロセス内でビルド直前に**検査するので、事前に量った値が古くなった場合も開始を保留できる。ホストの事前判定が通りゲスト側で容量不足となったとき、レポートは子プロセスの `failed` と終了コード75を記録する。実際のビルドが別のディレクトリにも書く場合はその書き込み先も確認する。

~~~python
from pathlib import Path
import os
import shutil
import sys

out = Path(sys.argv[1]).resolve(strict=True)
if not out.is_dir() or shutil.disk_usage(out).free < (1 + 2) * 1024**3:
    sys.exit(75)
os.execvp("cargo", ["cargo", "build", "--manifest-path", "/work/project/Cargo.toml", "--target-dir", str(out)])
~~~

## 読み方

- `peak_bytes` は開始時の空きから観測した最少の空きを引いた値、`net_bytes` は開始時と終了時の差。途中で消えた一時ファイルは純増に残らず、終了時の空きが増えれば純増は負になる。観測の間で解放されたピークは捉えられない。全サンプルで直接使える空きを用い、パージ可能量はここに混ぜない。
- `samples` には開始・終了と、指定間隔（既定1秒）の観測を記録する。ボリュームが変わった時や失敗時は `null`。長いジョブでは表示用履歴を間引き、開始と終了、全履歴に基づく最大消費・総観測数・欠測数を保持する。 `elapsed_ms` は単調時計、`started_at_ms` はUNIX時刻。
- 分類別の `logical_delta_bytes` は各ファイル名から見える論理長の合計の差。ハードリンクやクローンは別の名前ならそれぞれ数える。実ディスクでは圧縮・共有・スナップショット・他ジョブの影響もある。この実装は占有量を分類ごとに確定できないため、`residual_bytes` は未判定、`unattributed_net_bytes` は物理的な帰属が決まっていない純増全体を示す。欠測と0を区別する。
- レポートは1回につき新規ファイルを占有し、既存ファイルがあれば実行前に終了する。子プロセスが正常終了したら `completed`、異常終了は `failed` と終了コード、開始保留は `blocked` と理由を残す。ラッパー自体を強制終了した場合は初期の `failed/incomplete` が残る。実行時の引数、個人のパス、ホスト名をレポートに保存しない。ジョブIDは出力されるので公開できるラベルを使う。標準出力・標準エラーは子プロセスが引き継ぐ。
