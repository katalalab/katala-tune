# Windows helper の導入・更新

Windows の通常収集と GPU 操作は、台帳の `action_helper` v2 が承認する `tune.exe` だけを使う。従来の台帳に記録がなければ収集は失敗し、旧 PowerShell 経路には戻らない。機体を自動探索して得たハッシュだけで承認してはいけない。

## 検証するもの

作業は公開リポジトリ外の専用ディレクトリで行う。まずレビュー対象の40桁コミット、CI run ID、attempt を固定する。PR 候補はそのコミットを対象とする `CI` 全ジョブの成功が必要である。失敗した run の途中成果物や以前のコミットの成功を流用しない。

1. GitHub API の run と artifacts を取得する。repository、workflow path `.github/workflows/ci.yml`、event、head SHA、run ID/attempt、全体の success、artifact の name・期限・digest を確認する。artifact 名は `tune-cli-windows-x64-<run ID>-<attempt>`。
2. その artifact ID の ZIP をダウンロードし、**展開前に ZIP の SHA-256 を GitHub API の `digest` と比較**する。`gh run download` だけでは、この記録を省略しない。ZIP の項目が通常ファイルの `tune.exe` と `manifest.json` だけで、絶対パス、親ディレクトリ参照、リンク、重複がないことを確認し、新しい空の場所へ展開する。
3. `manifest.json` の schemaVersion が1、artifact の file が `tune.exe`、サイズ・SHA-256 が実ファイルと同じ、build が `tune-cli` / `debug` / `x86_64-pc-windows-msvc` / `win32` / `x64`、Rust が指定版であることを確認する。source の `githubSha` と `head` が同じで、CI の repository・runId・runAttempt・event・workflowRef・workflowSha が実 run に対応することを確認する。
4. PR では GitHub の検証用 merge SHA と PR head SHA は異なる。`source.pullRequest.headSha` がレビュー済み head、baseSha が対象 base、`source.tree` がレビューした統合 tree、Cargo.lock の canonical hash がその tree の内容と一致することを照合する。merge tree が変わったら再レビューする。manifest の自己申告だけで出所を承認しない。
5. 対象 Windows で保護を有効にしたまま、展開したファイルと候補アプリ一式を Defender でスキャンする。検出時は実行せず停止する。検出名・時刻・ハッシュ・定義版を非公開の証跡へ残す。clean はその検査結果であり、過去の検出の誤検知判定にはならない。

ここまでを確認した操作者が、次の6項目の記録を作る。値は検証済み manifest と元ファイルから取る。数値は正の安全な整数、ハッシュは小文字16進。`source_sha` はレビュー済み統合ソースの `source.githubSha`、`manifest_sha256` はダウンロードした **元の manifest バイト列** の SHA-256 とする。

```text
schema          katala-tune/action-helper/v2
source_sha      source.githubSha (40 hex)
tune_sha256     artifact.sha256 (64 hex)
manifest_sha256 SHA-256(manifest.json) (64 hex)
run_id          ci.runId (integer)
attempt         ci.runAttempt (integer)
```

## 切り替え

通常アプリ・監視・該当操作を止め、現在の helper と台帳を専用の退避場所へ保存する。対象は当該ユーザーの `~/.katala-tune/bin/tune.exe`、隔離検証では `KATALA_TUNE_DATA_DIR/bin/tune.exe` である。承認対象バイナリを別名で配置し転送後の SHA-256 を再確認してから置換し、同じ機体の台帳の `action_helper` を上の記録に更新する。ほかの台帳項目・保護リストを維持する。台帳とバイナリの切り替え途中はハッシュ不一致で拒否されるため、監視の再開は両方の照合後に行う。

Windows アプリ起動時の同梱ツール同期は、既存の `tune.exe` を自動で上書きしない。異なる同梱版がある場合、`tune update` の helpers に `approval_required` が出る。CLI/helper の更新は毎回この手順で承認する。agent は従来の同期規則に従う。アプリ版と承認済み CLI 版が異なる期間は `tune --version` / `tune versions` で区別する。

最初は専用設定・専用 DB・自動更新と監視を無効にした1台で、ローカルと SSH の収集、欠測理由、GUI の全カード、スペック表示、監視停止後の子プロセス終了を確かめる。操作は専用 fixture を対象にし、既存の作業や GPU 学習に使わない。確認ダイアログを省略しない。成功後に対象を広げる。

失敗時は監視を止めたまま、退避した **helper と台帳の組**を戻してハッシュを再照合する。検出済み・隔離済みのバイナリを復元する手順ではない。Defender の無効化・除外登録は行わない。公開配布の helper を承認する場合も、対応する配布 manifest と実際の成功 workflow から同等の出所確認が必要で、単に版番号が同じであることでは代用できない。
