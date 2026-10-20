# レビュー画面のローカル検証

2026-10-20。差分の基準commit: `6c4e82b0c7773764a8341655dc9e2aba8623359f`、レビュー画面の実装commit: `7c01598a5b897955e77f38ea403b919c75697a6e`。以降のレビュー指摘修正を含む作業ブランチ `codex/workflow-review-20261019` の記録で、配布版の受入ではない。

## 実装した操作

「レビュー」で `task-completion.v1` のJSONを選び、目的・対象版・各段階の申告・証跡・対象範囲を読む。原本SHA-256と未照合の表示を残す。25件ずつ表示し、対象範囲と証跡を展開できる。画面を離れると文書と非同期読込結果を破棄する。

## 確認済み

| 検査 | 結果 |
| --- | --- |
| 実装時の `npm run check`（実装commit `7c01598` 時点） | exit 0。Node 330件中317成功・13 skip・失敗0。Rust 254成功・失敗0。fmt/clippyを含む |
| レビュー指摘修正後の `node --test test/review.test.js test/review-ui.test.js` | 32件成功・skip/失敗0。読み込み中表示、pass時のresult警告、配布段階の任意表示、警告の単一表示、長い文字列、URLの空query/hash、IDのtrimを含む |
| `node --check renderer/review.js renderer/review-model.js test/review.test.js test/review-ui.test.js` | exit 0 |
| `git diff --check` | exit 0 |
| Chromiumでのrenderer表示（実装commit） | プレビュー用fixture bridgeを使用。1280×900と800×800で確認。console errorなし、狭幅で横はみ出しなし |
| JSON選択（実装commit） | 合成見本の原本SHA-256一致、不正JSON・2 MiB超過を拒否、旧表示を破棄 |
| 多件数入力（実装commit） | 私有台帳2件の読込と25件ページ移動を確認。個人・プロジェクトの情報はリポジトリへ保存しない |
| 通信（実装commit） | ブラウザ検証で台帳読込時のネットワーク要求0件 |
| macOS Electron 44（実装commit） | 分離したuserDataと既存fixture preloadを使用した実ウィンドウ。原本hash、画面表示、離脱後の破棄、console errorなしを確認 |

既存データベース、機体設定、実機の起動・停止、外部アカウントには書き込んでいない。出力の `pass` や `accepted` は申告として表示し、検証・承認・完了の判定を新設していない。

## 未確認

- レビュー指摘修正後のRustを含む `npm run check` 全体、ブラウザ・Electron・Tauriでの再確認。今回の修正は対象テストと構文・差分検査だけである。
- Tauriの実WebViewとWindows実機でのファイル選択・画面遷移。ブラウザとElectronの成功で代用しない。
- GitHub上の現在head、証跡参照の実在、生成元。画面で未照合と明示する。
- 正式ブランチへの反映、PR/CI、配布、導入版での本人受入。

公開前には差分全体の個人情報検査と、正式なheadに対するCI・レビューを再確認する。ロールバックはレビュー画面の追加ファイル、indexの2 script行、レビュー専用CSSを取り除く変更として行い、既存の概要・DB・機体台帳は保持する。
