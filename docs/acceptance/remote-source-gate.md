# Remote topology source gate

この変更は remote topology の gate 1（返送する manifest の同一性）だけを実装する。通常 remote topology の有効設定は変更しない。original base は `d013572`、最新 main `02b62e89423de13cd8e2c8ef5460fa3aaf4f13f1` を通常 merge で取り込んだ。並行変更の取り込み後の再検証、正式反映、Windows native、通常 app の実機受入は別工程であり、この fixture 検証で代用しない。

## Private request の契約

SSH environment は、既存の roots に加えて `source_allowlist` を必須とする。以下は架空の schema 例であり、許可済みの機体や source を表さない。HEAD、path、UTF-8 bytes、SHA は承認した実体の値へ置き換える。台帳や実環境の値はリポジトリへ保存しない。

```json
{
  "roots": ["/approved/checkout"],
  "configs": [],
  "source_allowlist": [
    {
      "root_index": 0,
      "relative_path": ".",
      "expected_head": "1111111111111111111111111111111111111111",
      "manifests": [
        {
          "relative_path": "package.json",
          "bytes": 2,
          "sha256": "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        }
      ]
    }
  ]
}
```

`root_index` は同じ要求の roots への添字、checkout の `relative_path` はその root からの相対パス、manifest の `relative_path` は checkout からの相対パス。root は絶対・字句的に正規のパスとし、重複区切り、`.`、`..` の path component は拒否する。Windows は既存の drive 付き絶対パスと `\` / `/` を受け入れ、root fingerprint は小文字・`/` 区切りに揃える。checkout / manifest のパス照合は大文字小文字を区別する。symlink / reparse point は root の祖先から選択経路まで検査して除外する。検査と open の間の全 filesystem race を原子的に排除する保証はない。

HEAD は小文字の 40 / 64 桁 Git OID、SHA は小文字の 64 桁。重複 checkout / manifest、未知の path 種別、root 範囲外、file / workflow byte 上限違反を拒否する。policy の checkout 数は entry budget、manifest 数と合計 bytes はそれぞれ対応する環境 budget、serialized request は既存 request byte budget に拘束する。policy がない、不正、空、または `configs` が非空なら、Rust adapter は SSH を起動しない。設定 JSON の選択収集は hash のみを返す既存機構も含め、今回の strict remote mode では許可しない。local collector の設定収集は変更しない。settings 収集全体は未完了であり、expected configuration bytes / selected keys の admission は後続の残件とする。

この許可は過去の収集 metadata やキャッシュから自動生成しない。承認済みの入力を別途用意する。可変 checkout の過去の HEAD 一致を、将来の内容に対する許可へ流用しない。

## 実行時の境界

両 probe は manifest を一度開いて bounded な byte buffer を読み、expected HEAD / checkout / manifest path / byte length / SHA を照合してから、その同じ buffer を UTF-8 decode する。不一致の buffer を本文へ入れない。収集の末尾で HEAD を再読し、返送直前にも全 repository / manifest を再検証する。不一致があれば repository、manifest、configuration、progress を capture 全体から消し、固定文言の failure のみを返す。正常時の Git 起動は checkout ごとに private/common/origin/HEAD の 4 回と末尾 HEAD の 1 回。

Rust は bounded transport と envelope schema 検証の後、manifest parser / source cache / graph 保存より前に同じ allowlist を再検証する。受信側でも予期しない checkout / manifest、重複、改変、revision と kind の不一致は本文ごと破棄する。policy は engine generation と probe cursor fingerprint の両方に含む。

許可した source が欠ける場合、許可範囲内の取得済み本文だけを保持し、coverage を incomplete とする。該当 checkout の manifest 不足も Partial とし、空 capture を Complete にしない。継続ページに許可 source の一部しか現れない場合も incomplete であり、前のページや前回の metadata を使って Complete へ昇格しない。全許可 source を同一 capture で確認できない大きな要求は、root と許可 source を狭めるか budget を承認範囲で調整する必要がある。

これは原子的な filesystem snapshot の保証ではない。返送する本文は承認済み byte buffer に固定し、前後の HEAD を検査するが、OS metadata/read 自体の停止や remote reader と子プロセスの完全終了は別 gate の対象である。

## 検証と残りの gate

fixture は、許可なし・不正 policy で transport 0、HEAD / path / bytes / SHA / kind / 重複 / 追加 source の拒否、未観測・空 capture と continuation の incomplete、POSIX probe の実 stdout に未許可本文がないこと、読取り後 HEAD 変更の拒否を検証する。Windows probe は ASCII / BOM / 禁止操作の静的検査のみであり、Windows native の実行受入は未検証。

- Gate 2: 製品 Windows reader の所有 PID・OS start・session identity と終了 ACK、正常・timeout・出力上限の各経路で reader と子の退出証跡。既存 `run_bounded` の direct-child `start_kill` / `wait` を全子プロセス終了の証明には使わない。
- Gate 3: 通常 recurring scheduler と、外部 per-host SSH finite window / budget / exit observer の協調。期限付き slot を通常 scheduler の恒久許可へ転用しない。

gate 1 の fixture 成功は gate 2 / 3、配布、通常 remote topology の enable、実機受入を意味しない。
