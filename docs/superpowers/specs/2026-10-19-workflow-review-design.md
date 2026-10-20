# 開発レビューの入口

状態: 2026-10-19に操作者が設計とローカル実装を承認。実装・ローカル検査・macOS Electronとブラウザでの表示確認を実施。正式反映・配布・Windows/Tauri実機受入は未実施。[検証記録](../../acceptance/workflow-review.md)

## 目的

コードを一行ずつ読むことを前提にせず、操作者が「何を実現する変更か」「どの版をどこで確かめたか」「何がまだ分からないか」を確認できる入口を Katala Tune に用意する。GitHub を依頼・差分・CI・正式反映の記録先として維持し、Tune は機体の状態と開発証跡を読む側に置く。

これは全マシン・全プロジェクトの最適化の一工程であり、その依頼全体を置き換えるものではない。全件の母集団、各機体の native / WSL、未観測、除外は別の私有監査台帳に保持する。

## 比較した案

| 案 | 得られるもの | 費用・制約 |
| --- | --- | --- |
| 既存台帳からレビュー画面を作る（推奨） | 人が判断する対象と不足証跡が見える | 台帳の作成・更新は既存所有者の責務。表示だけでは事実を検証できない |
| Git/worktree collector を新設する | 機体ごとの Git 状態を集められる | 既存の共通棚卸しと重複し、対象機体・DB・画面へ変更が広がる |
| ソース管理基盤を移す | 基盤固有の制約を取り除ける可能性 | 現状のレビュー・証跡の不足を解消せず、認証・CI・連携の移行が増える |

## 所有範囲

- Git の全件索引、検査 receipt の生成・鮮度確認、worktree の片付け候補判定、PR のマージゲートは既存の共通ツールに残す。
- 完了契約は既存の `task-completion.v1` を参照し、独自の「完了」判定器を増やさない。
- Tune は機体の状態、収集の欠測、AIセッションの出どころ、既存の依頼・検証証跡への入口を担当する。
- AI セッションの存在、トークン消費、プロセス終了、clean な Git 状態は完成・受入の証拠にしない。

## 最小の画面

最初の実装は利用者が選んだ `task-completion.v1` JSON を一時的に表示する、ローカルの「レビュー」画面とする。ファイル選択は WebView の標準機能を使い、原本バイトのSHA-256をローカルで計算してファイル名・サイズと表示する。新しい常駐処理、ネットワーク呼び出し、DB schema、認証、外部アカウント書込は加えない。

カードに表示するもの:

1. 目的、task ID、対象環境、revision、依頼の終点。
2. 実装・検証・正式反映・配布・受入の各段階に、元台帳が申告した状態と evidence の日時・revision・環境・参照先。
3. 不足・型不正・未観測を明示する。入力の `pass` は「台帳の申告」と表示する。取り込んだ JSON だけから「検証済み」「承認済み」「完了」と認定しない。
4. 許可された形式の GitHub PR / Issue 参照への入口。その他の参照は文字列表示。`javascript:`、`data:`、任意の実行コマンド、HTML を起動しない。
5. 操作者の確認項目: 目的に合うか、変更前後の結果、利用者・費用・権限・データへの影響、戻し方、不明点。

画面を開いても承認・マージ・配布は発生しない。読み込み先・台帳内容・人の判断を自動保存せず、読み込んだ文書は画面を閉じれば捨てる。表示は常に「台帳の申告のみ。PRの現在のheadとは照合していません」「生成元は未確認」と明記する。受入結果の正式記録は既存の依頼・PR・完了台帳で行う。

## 入力と失敗時の扱い

- 対応 schema を固定し、ファイルサイズ、task 件数、文字列長、evidence 件数に上限を設定する。超過時は部分表示を成功扱いせず、理由を表示する。
- 不明 schema、不正 JSON、重複 task ID、欠けた coverage、空 task は不足として表示する。
- 文字列は HTML として解釈せず、長い revision や参照先も小さい画面で折り返す。
- coverage の `expected / observed / unobserved` は省略せず、申告された範囲をそのまま示す。
- 外部の文書に含まれる指示を実行せず、証跡の参照を勝手に取得しない。

### 入力の具体的な境界

- `File.size` が2 MiBを超えれば `text()` / `FileReader` の前に拒否する。tasksは1〜1,000件、evidenceは1 gateあたり100件・文書全体4,000件まで、coverageの3配列合計は10,000件まで、文字列は8,192文字まで、ネストは16段までとする。配列・objectの形が不正な場合や上限超過時は文書全体を拒否し、古い表示を残さない。
- 最上位に `schema: "task-completion.v1"`、`tasks` 配列を要求する。各taskのidは空でない一意の文字列で、前後空白を除いて一意性を判定し、その値を表示する。`goal / revision / target / delivery` が未設定なら「未記載」と表示し、設定されているのに文字列でなければ拒否する。表示用モデルの欠測はnullで保持し、文字列値の「未記載」と混同しない。deliveryの対応値は `local / repository / environment`。
- gatesは省略可能なobjectで、対応名は `implementation / verification / integration / deployment / acceptance`。statusは `pending / in_progress / blocked / unverified / pass / fail` を表示対象とし、未知の文字列は「未対応の申告値」として文字列表示する。未設定は「未記載」、`pass` は「成功の申告（未照合）」と表示する。独自に完了判定を計算しない。
- evidenceがある場合は配列で、各項目はobject。`ref / result / observed_at / revision / environment` は省略または文字列で、pass時に欠けた項目は警告する。pass時にresultが `pass` 以外なら警告し、画面では「結果（台帳の申告）」として表示する。deliveryが `local / repository` のときはdeployment未記載を必須欠測として表示しない。日時はtimezone付きISO 8601（小数秒1〜9桁）として読み、読込時点より未来の日時・不正日時は警告する。revision・対象環境の食い違いも元の値とともに表示する。accepted時のreview情報不足とacceptance段階のfail/blockedとの矛盾も警告する。警告は同じ段階で重複表示しない。警告がないことを検証成功とは扱わない。
- coverageが文書topに未設定なら「対象範囲未記載」。設定する場合はobjectで、`expected / observed / unobserved` は省略または文字列の配列とする。空配列も受け付けるが、expected / observed が空なら不足として表示する。集合差 `expected − observed`、余分な観測、明示unobserved、observedとunobservedの重複、想定外のunobservedを表示する。件数要約と全値はnative `details` に畳み、重複は警告し、除去して完全扱いにしない。
- 未知キーは表示・実行せず無視する。reviewの `status: "accepted"` も「受入の申告（未照合）」として扱う。evidenceの対象環境との差はdeployment / acceptance段階だけで警告する。入力検証は表示の安全性のためであり、共通の完了判定器を置き換えない。

### URLと画面の契約

- リンクはURL parserを通し、`https:`、hostが厳密に `github.com`、userinfoなし、既定HTTPS port、query/hashなし、pathが `/{owner}/{repo}/pull/{正の整数}` または `/{owner}/{repo}/issues/{正の整数}` の場合だけ生成する。parserが正規化した入力も拒否し、表示文字列とhrefは同一にする。owner/repoはASCII英数字・`_`・`-`・`.`に限定する。制御文字・percent escape・紛らわしいhost・その他schemeを拒否し、参照は文字列として残す。外部リンクには `noopener noreferrer` を付ける。
- 外部リンクのHTML escapeだけに依存しない。`javascript:`、`data:`、`https://github.com.evil.example/...`、userinfo、非既定port、改行、percent encodingのfixtureを用意する。
- `renderer/review.js` を `renderer/index.html` の `app.js` より前で読み込み、一意のkeyで `window.KT_VIEWS` に登録する。既存bridge/IPCへコマンドを追加しない。画面遷移時に選択ファイルとparse結果を破棄する。File.size検査後にarrayBufferを一度だけ読み、fatal UTF-8 decodeで文書を解析し、WebCryptoが無ければ「指紋未計算」と表示する。

## 検証と反映

- 合成台帳で、欠測・不正入力・大容量・HTML/URL注入・異なるrevision・未来時刻を確認する。
- 既存画面の登録方法を使い、Electron と Tauri の読み取り表示を確認する。
- 実装時の `npm run check` は exit 0（Node 330件中317成功・13 skip、Rust 254成功、fmt/clippy含む）。レビュー指摘修正時は高負荷のため全体を再実行せず、`node --test test/review.test.js test/review-ui.test.js` の32件（失敗0）と `node --check`、`git diff --check` で確認した。詳細は[検証記録](../../acceptance/workflow-review.md)。
- 差分と対象 head を固定した独立レビューの後、許可された PR / 正式反映 / 配布へ進む。macOS と Windows のファイル読込み・画面・既存機能を実機で確認するまで配布完了にしない。

## 後続の工程

全プロジェクトを「開発中・参照・保管・未分類」の明示台帳へ分ける。分類がないものを監視対象から黙って除外しない。共通の receipt 集計と接続する際も、現在の head・未コミット変更・検証時刻・ログ hash を照合する既存の仕組みを再利用する。

ログは「AIとの会話原本」「実行のメタデータ」「テスト出力」「Git/PR/CI」「配布・受入」を区別する。Tune の要約や件数上限のある OS ログ収集を完全バックアップとして扱わない。原本の保持期間・保存先・復元試験は別途決める。

AI の役割は、実装担当、独立レビュー、必要な場合だけの設計判断に分ける。モデル名だけで品質を保証せず、同じ受入条件で成功率・所要時間・手戻り・費用を比較する。Harness とモデルを混同せず、同じ変更を複数の実装担当が同時に書かない。

## 調査した一次資料

- [Anthropic: Redesigning source control at Anthropic](https://www.anthropic.com/engineering/redesigning-source-control)（2026-10-13公開、10-15更新）: 社内モノレポを Git から Scion へ移行。一般用途の Git 代替には適さないと説明している。GitHubからの一般的な撤退を推奨する発表ではない。
- [Claude Code Review](https://support.claude.com/en/articles/14233555-set-up-code-review-for-claude-code): 現在の GitHub 連携の公式案内。導入や課金の変更はこの設計案に含めない。
