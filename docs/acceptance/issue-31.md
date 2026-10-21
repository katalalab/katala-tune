# Issue #31: GPU 学習ジョブ保護

NVIDIA の `--query-compute-apps=pid,used_gpu_memory` を読み取り専用・4秒・16行・64KiBで収集する。`used_gpu_memory` が `N/A` でも PID が正なら計算中として扱い、0 に置き換えない。空の成功、取得不能、形式不正は別の `gpu_compute.availability` / `reason` で表す。

CPU 停止候補は、compute PID 本人、開始時刻の順序も確認できる親子、`protect_gpu: true`、compute 観測不能で作らない。GPU 使用率が 5% 以上の場合は PID 特定の根拠とは表示せず、機体全体の GPU 活動として停止候補を保留する。5% は短いアイドル値を除いて学習・推論への干渉を避ける保守的な既定値である。

Windows の `kill-process` と `set-gpu-power-limit` は、承認と台帳再読の後、変更の直前に GPU 使用率・compute PID を再照会する。NaN・無限値・PID 桁あふれも取得不能として扱う。NVIDIA SMI が無く、最大32件の映像アダプター照会で NVIDIA 非搭載を確認できた機体だけはこの照会を適用外として既存操作を通す。GPU 活動、compute PID、または GPU 有無を含む取得不能は exit 9 で拒否され、既存 runner の実行記録へ理由が残る。`protect_gpu` は省略可能な boolean で、無い既存台帳の挙動は変わらない。

現行の WSL 経路は読み取り専用の観測と `.wslconfig` の助言のみで、`wsl --shutdown` や上限変更を実行する action はない。助言は GPU 計算中または GPU 状態が不明なら反映・shutdown を保留する条件を明記する。

長い remote Windows GPU action は固定 PowerShell receiver と stdin で送る。controller の SSH timeout は remote action の停止証明ではなく、controller/target の時計差も未証明である。このため実機操作の有効化と受入は保留し、native Windows で受信期限・切断・timeout 後の状態を確認するまで fixture 検証だけを根拠にする。
