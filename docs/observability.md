# AI エージェント・常駐プロセス・ネットワークの見える化と、預かったデータの検証（設計 2026-10-07）

マルチマシンで、AI エージェント（Claude Code・Codex）・常駐プロセス・ネットワークを1か所で見て、異常に気づき、数字の出どころを辿れるようにする。セキュリティ対策のログ収集にも使える形にする。**会話の本文・ツールの入出力は今までどおり取り出さない・保存しない。**

## 何を作り、何を作らないか

- 作る: 機体をまたいだ集計と異常の検知、費用の推定、公式の経路での残り枠、数字の出どころの台帳
- 作らない: 会話の閲覧・検索（本文を持たない方針と合わない。単機のビューアは agentsview などの既存の OSS に任せる）、Cookie・OAuth トークン・Admin API キーを読む機能

## 1. 出どころの台帳（最優先）

他の指標の信頼はこの上に乗るので、最初に作る。

- 取り込みのたびに `ingest_run`（実行 ID、機体、調査スクリプトの版と SHA-256、Tune の版、開始・終了）を1行
- 読んだファイルの区間ごとに `source_span`（機体、ファイルの鍵、開始・終了のバイト位置、区間の SHA-256、行数、取り込んだ行数、飛ばした行数）
- セッションの行と時間ごとの量は、どの `source_span` から来たかを持つ。(機体, ファイル, 開始, 終了) に一意制約を付け、二重の取り込みを拒否する
- 続きの位置は「前回の終了 = 今回の開始」を毎回確かめ、位置の手前の数 KB のハッシュも持つ。合わなければ、そのファイルを最初から読み直す（書き換え・ローテーションへの備え）
- 画面: 数字を押すと「機体 → ファイル → 区間 → スクリプトの版 → 取り込み時刻 → 検算（件数・ハッシュ）」を辿れる。状態は「検証済み／元ファイルなし（指紋のみ）／不一致」の3つ
- 検証ジョブ: 夜間に少数の区間を元ファイルから読み直して照合する。元ファイルが消えたあとは指紋しか残らない（再計算はできない）ことを画面に書く
- 改ざんの検知: `ingest_run` のハッシュを連鎖させる。DB を丸ごと書き換えられる相手には効かないことも書く

## 2. 使用量と費用（自前で計算する）

ccusage・CodexBar は取り込まない（全走査型で差分の収集と噛み合わない、Node の依存、機体ごとの版のずれ、CodexBar は認証情報を読む）。Tune はトークン数をすでに差分で集めているので、要るのは次の3つだけ。

- **単価表**: 使うモデルの分だけを同梱し、取得日・出典・版を記録する。費用の各行に単価表の版を持たせる。未知のモデルは 0 円ではなく「単価不明」。表示は「API 単価換算の推定（サブスクの実請求ではない）」と明記する。出典の候補は LiteLLM の価格表（同梱の前にライセンスを確かめる）
- **数え方**:
  - Claude: 同じ応答が複数行に書かれるので、応答 ID ごとに最後の行を採る（今の調査スクリプトはこの方式）。キャッシュの書き込みと読み込みは別の単価
  - Codex: 入力の通常分は `input − cached`、`cached` はキャッシュ読み込みの単価。推論のトークンは出力に含まれているので足さない（今の画面も足していない）
- **ファイルをまたぐ重複を除く**: この機体の実データで、応答の 0.9%（46,883 件中 417 件、大半はサブエージェントのファイル）が複数のファイルに出ていて、出力トークンの 1.2% が二重に数えられていた。調査スクリプトが応答ごとの（ID のハッシュ, 量, 時刻）を出し、DB が機体ごとに ID で重複を除いてから集計する形に変える

## 3. 残り枠（公式の経路だけ）

ログからの推定では、上限値も窓の始まりも決められない。正確に出すには公式の値が要る。

- Claude Code: statusline に渡される JSON の `rate_limits`（5 時間・7 日の使用率とリセット時刻）。各機体の Claude Code に「値をファイルに書くだけ」の statusline を設定する必要がある（**他の機体の永続設定の変更**）。値はセッションが動いている間しか更新されないので「最後に観測した値と時刻」として出す
- Codex: `codex app-server` の `account/rateLimits/read`（窓の長さ・リセット時刻）。認証は Codex 自身が持ち、Tune は数値だけ受け取る
- 窓は長さで見分け、欠けた窓は「未報告」とする（無制限とは扱わない）

## 4. AI の挙動（本文なしで判定できる範囲）

| 見るもの | 使う値 | 今の取り込みで |
|---|---|---|
| ツールの失敗率（機体・ツールごと） | ツール名・成否 | できる |
| 同じ失敗の繰り返し（ループ） | 同じツールの連続した失敗、（任意で）引数の指紋 | 連続した失敗まではできる |
| 権限の拒否・フックの失敗 | 拒否の記録・フックのエラー | フックのエラーはできる。拒否は調査スクリプトに足す |
| API エラー・リトライ・レート制限 | エラーの記録 | 一部できる |
| コンテキストの圧縮・モデルの切り替え・長時間のセッション | 圧縮の印・モデルの変化・期間 | 期間とモデルはできる。圧縮の印は確かめてから |
| 何をしたか・結果が正しいか | 本文が要る | **しない** |

- 引数の指紋（機体ごとの秘密の塩でハッシュ。塩は機体の外に出さない）があるとループの検知が正確になる。短い値（パスなど）は総当たりで推測されうるので、入れるかは操作者が決める
- より正確な値（ツールの所要時間・リトライ回数・圧縮の前後・権限の判断）は、Claude Code・Codex の OpenTelemetry で取る。各機体の `tune-agent` が OTLP を 127.0.0.1 だけで受け、本文の出力（`OTEL_LOG_USER_PROMPTS` など、Codex の `log_user_prompt`）はすべてオフ。Codex の `tool_result` には引数と出力の断片が入るので、受け口で落としてから保存する

## 5. 常駐プロセスと AI の突き合わせ

AI のセッション中に落ちた常駐、負荷の高い時間帯との重なり、機体ごとの CLI の版のずれを、同じ時間軸で並べる。機体ごとの時計のずれに注意する。

## 6. ネットワークとセキュリティのログ

パケットの中身は記録しない（重く、他人の通信の中身にも触れるため）。**接続のメタデータとOSのセキュリティの記録だけ**を集め、機体をまたいで「いつもと違う」を見つける。どれも読み取り専用。

| 見るもの | macOS | Windows | 気づけること |
|---|---|---|---|
| 待ち受けているポートとプロセス | `lsof -nP -iTCP -sTCP:LISTEN`・`-iUDP` | `Get-NetTCPConnection -State Listen`・`Get-NetUDPEndpoint` と所有プロセス | 意図しない公開（0.0.0.0 での待ち受け）、新しく開いたポート |
| 外向きの接続（プロセス・宛先・ポート） | `lsof -nP -iTCP -sTCP:ESTABLISHED`（標本） | `Get-NetTCPConnection -State Established` と所有プロセス | 初めて見る宛先・ポート、普段通信しないプロセスの通信、同じ宛先への大量の接続 |
| DNS の名前解決 | （OS に手軽な記録が無い。必要なら後で） | `Get-DnsClientCache` | 宛先の名前（IP だけより読みやすい） |
| ログインの失敗・成功 | 統合ログの sshd・`authd`（件数と送り元） | セキュリティログ 4624／4625（件数・種別・送り元。管理者権限が要るものは取れる範囲で） | 総当たり、見覚えのない送り元 |
| 常駐の追加（持続化） | LaunchAgents・LaunchDaemons の増減 | タスク・サービス・Run キーの増減 | 知らないうちに増えた自動起動（道具の棚卸しと同じ「増減の記録」で） |
| 防御の状態 | XProtect・Gatekeeper・ファイアウォールの状態 | Defender（リアルタイム保護・定義の日付・検出の記録）・ファイアウォールのプロファイル | リアルタイム保護が止まっている機体を状態の画面で異常にする |
| Tailscale | 接続先・鍵の期限 | 同左 | 想定外の機体からの接続、期限切れ |

- **集め方**: 60 分ごとの分析に「待ち受け・常駐・防御の状態」を足し、接続の標本はリアルタイム取得（画面を見ている間）と、分析のときの1回だけ。ログイン・Defender の記録は今のログ取り込みに種類を足す
- **いつもと違う**: 機体ごと・プロセスごとに「これまでに見た宛先とポート」を覚え、初めてのものだけを知らせる（最初の 1 週間は覚えるだけ）。共用機は提案だけ
- **保存**: 宛先の IP・名前・送り元は手元の DB にだけ置く。台帳の機体ごとに収集を止められるようにする（`"network": false`）。保持は 30 日、集計は残す
- **やらないこと**: パケットの取得、通信の遮断・ファイアウォールの書き換え（気づくまでにとどめ、手当ては許可リストの操作として別に設計する）
- **後で足せるもの**: Windows の Sysmon（プロセス起動とネットワーク接続の詳しい記録。入れるかは操作者の判断）、`tune-agent` での常時の接続の記録

## OpenTelemetry の受け口（tune-agent。実装 2026-10-07）

`tune-agent run` が `http://127.0.0.1:4318`（`--otlp-port`。`--no-otlp` で開かない）で OTLP/HTTP を受け、本文・引数・出力の断片を落としてから `~/.katala-tune/agent/otel/otel.jsonl` に 1 レコード 1 行で書く（16 MiB で回し、3 世代まで。ファイルは 0600）。後で調査がこのファイルを読む（`otlp.read`。次の段階）。中身は `crates/tune-agent/src/otlp.rs`。

- 受けるのは `POST /v1/logs`・`POST /v1/metrics` の `application/json` だけ。protobuf・圧縮・トレース（`/v1/traces`）は受けない（依存を増やさないため。断ると 415・404 を返す）
- 待つのは 127.0.0.1 だけ。Host が 127.0.0.1・localhost・[::1] 以外の要求は断る（ブラウザからの書き込み・DNS rebinding を防ぐ）
- **落とし方は許可リスト**: 文字列の属性は、決めた名前（`event.name`・`session.id`・`model`・`tool_name`・`decision`・`success`・`error_type` など。一覧は `KEEP_STRING`）で、128 文字までのものだけ残す。数・真偽は残す。配列・入れ子・バイト列は残さない。ログの本文はイベント名の形のときだけ。トレース ID・exemplar は残さない。各行に落とした数（`dropped`）を書く
- だから `prompt`・`prompt_text`・`tool_parameters`・`tool_input`・`error`（Claude Code）、`arguments`・`output`（Codex の `codex.tool_result`）、`user.email`・`vcs.repository.url.full`・`workspace.host_paths` などは、設定を間違えて本文の出力をオンにしても残らない（`otlp::tests` で確かめている）

### 各機体での設定（手順だけ。配布は操作者の確認のあと）

tune-agent は Claude Code・Codex の設定を書き換えない。入れるときは機体ごとに次を足す。

Claude Code（`~/.claude/settings.json` の `env`。リポジトリの `.claude/settings.json` の `OTEL_*` は Claude Code が無視する）:

```json
{
  "env": {
    "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
    "OTEL_METRICS_EXPORTER": "otlp",
    "OTEL_LOGS_EXPORTER": "otlp",
    "OTEL_EXPORTER_OTLP_PROTOCOL": "http/json",
    "OTEL_EXPORTER_OTLP_ENDPOINT": "http://127.0.0.1:4318"
  }
}
```

本文を出す設定（`OTEL_LOG_USER_PROMPTS`・`OTEL_LOG_ASSISTANT_RESPONSES`・`OTEL_LOG_TOOL_DETAILS`・`OTEL_LOG_TOOL_CONTENT`・`OTEL_LOG_RAW_API_BODIES`）は入れない（どれも既定でオフ）。`OTEL_EXPORTER_OTLP_COMPRESSION` とトレースの出力も入れない。

Codex（`~/.codex/config.toml`）:

```toml
[otel]
log_user_prompt = false
exporter = { otlp-http = { endpoint = "http://127.0.0.1:4318/v1/logs", protocol = "json" } }
```

Codex の `codex.tool_result` には引数と出力の断片が入るが、受け口で落とす。資料の例は endpoint に `/v1/logs` まで書いているが、版によって付け方が違いうるので、入れた後に `tune-agent status` の `otlp.bytes` が増えること（増えなければ endpoint を `http://127.0.0.1:4318` にする）を確かめる。

確かめ方: Claude Code か Codex を 1 回使ったあと、`tune-agent status` の `otlp.bytes` が増え、`otel.jsonl` に `"event":"claude_code.api_request"` などの行が出ること。本文が残っていないことは、使った指示の一部の語で `otel.jsonl` を検索して 0 件であることで見る。

## 順番

1. 出どころの台帳と「この数字はどこから」画面
2. ファイルをまたぐ重複の除去と、費用の推定（単価表の版つき）
3. ツールの失敗率・連続した失敗（ループ）・権限の拒否・API エラーの機体比較
4. 残り枠（Codex の app-server から）
5. ネットワークとセキュリティ（待ち受け・常駐の増減・防御の状態 → ログインの記録 → 外向きの接続の「初めて」）
6. OpenTelemetry の受け口を `tune-agent` に持たせ、各機体の Claude Code・Codex で有効にする（本文の出力はすべてオフ）
7. 常駐プロセスとの突き合わせ、検証ジョブ

## 操作者の決定（2026-10-07）

- 費用は「API 単価換算の推定」として表示する
- 残り枠は Codex（app-server）から始める。Claude の statusline の設定は入れない（後で判断）
- ツール引数の指紋は保存しない（連続した失敗までで検知する）
- OpenTelemetry は各機体に設定してよい（本文の出力はすべてオフ。受け口は `tune-agent` に持たせる）

## 出典（2026-10-07）

- ccusage: https://github.com/ryoppippi/ccusage ・ https://ccusage.com/guide/ （「費用は推定」「現在の機体だけ」）・重複の問題 https://github.com/ryoppippi/ccusage/issues/888
- CodexBar: https://github.com/steipete/CodexBar （残り枠の取得に OAuth トークン・Cookie を使う経路がある）
- Claude Code: statusline https://code.claude.com/docs/en/statusline ・OpenTelemetry https://code.claude.com/docs/en/monitoring-usage ・フック https://code.claude.com/docs/en/hooks
- Codex の設定（OpenTelemetry）: https://learn.chatgpt.com/docs/config-file/config-advanced
- DeepSeek Harness: https://github.com/deepseek-ai/deepseek-harness （dsh-trace・dsh-flow でツールコール・承認・リトライ・圧縮を見せる考え方を参考にする）
- agentsview（単機のローカルビューア）: https://github.com/kenn-io/agentsview
