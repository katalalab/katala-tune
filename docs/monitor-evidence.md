# 収集の保守記録

Rustの通常monitorとLiveは、接続・最初の測定・終了・再試行・分集計の保存結果を、既存SQLiteの専用 `monitor_events` 表に残す。OSログ、FTS、既存所見、既存メトリクスの意味は変更しない。画面が見ていなくてもmonitor経路から記録する。Tauriの `monitor_events` は日本語summary付きで読むだけのAPIで、dashboardの `journal` は保存経路の健全性を返す。ダッシュボードの既存details部品に「収集の保守記録」を設け、明示操作で最新50件・保存状況・欠落・最古時刻を読む。記録の自動pollや新しい設定操作は追加しない。

## 読み方

- `collector_id` はこの収集プロセスの起動単位、`event_seq` はその記録番号。`generation` はLiveのstream、`attempt` はopenごとの番号。max_ageで同じstreamが交代してもattemptは増える。
- `ts` はcollector側UTC epochミリ秒、`duration_ms` は単調時計で測った経過時間。remote時計との差を通信遅延と断定しない。
- `phase` は確認できたLive状態。open/hello/first_sample/end/retry/stoppedと、実際のexit code・wait・stdout/stderrの結果を分ける。EOF/read_error/cancelled/unknownは別。
- `configured_local` は台帳の経路指定。`collector_pid` は操作卓アプリ、`transport_pid` は手元の子（SSHなら手元のssh）のPID。PIDだけはOS作成時刻や遠隔producer identityの証明ではなく、停止対象の指定に使わない。
- 現helloにはremote producer PIDがないため `producer_pid` はnull。remote実行bytesのSHAも未観測なので `actual_source_sha256` はnull。`reported_version` はhelloの数値3要素の版だけで、遠隔バイナリの検証ではない。`app_version` は記録したアプリの版である。
- minute_commitの `committed=true` は既存Storeの保存が返った後だけ記録する。分には複数attemptが寄与し得るため、このeventのgeneration/attempt=0は個別producer未対応を示す。最初/最後のsample seq・受信時刻と接続eventを合わせて読む。特定PIDがその分を独占したとは主張しない。
- 正常な `max_age` は `planned_recycle`。既定900秒の定期交代はfailureに数えない。同じend eventのexit非zeroやEOF unknownは消さない。短い合成fixtureと実900秒運転の受入は分ける。
- 連続失敗時の既存2/5/10秒retryと、Disconnected後のmonitorの5分待ちは別policy。後者はoffline機体へ繰返し接触しないための既存定数で、欠測の実原因や5分が最適との実測根拠ではない。今回retry/停止動作は変更していない。
- monitor設定は一度だけ確認して採取条件を計算し、設定の読取失敗はsettings_unknownとして記録する。hubPullの読取失敗をfresh=falseとしてローカル採取へ戻す既存policyは維持するため、localという記録だけではハブの古さとhubPull読取失敗を区別できない。

## 有界read API

`Engine::monitor_events(&Filter)` / Tauri command `monitor_events` に `{filter:{...}}` を渡す。全項目optional、未知field・未知kind・bool/floatを整数へ変換しない。

| filter | 意味 |
|---|---|
| node_id | 正規のローカル台帳ID |
| collector_id / generation / attempt | 起動・stream・接続の対応 |
| kind | typed enum（open / first_sample / end / retry / minute_commit等） |
| since | UTC epochミリ秒以上 |
| before_id | 前pageのnext_before_id。新しい順の有界pagination |
| limit | 既定100、1～500に制限 |

応答はevents（日本語summary付き）、next_before_id、retention、health。照会でwriter開始・event生成・通知をしない。未開始のhealthはunknownで、過去のlossが0だったという意味ではない。healthのcounter_scopeはcurrent_collectorである。既存hub export/pullへ履歴を暗黙に追加していない。

## 保存失敗・容量

- 既存Tokioのqueue256、writer1つ、最大64件または1秒でbatch。producerはtry_sendだけで、Live lock内でDB I/O/awaitをしない。同期DB仕事は同時1つ、DB mutex競合は即Busy。blocking仕事をtimeoutで放置して新writerを増やさない。
- event全体はUTF-8 JSON 1024B以内。Node IDの切断で衝突させず、不正/長過ぎる入力は拒否して数える。自由文を受け取るAPIを持たない。
- `overflow_total` はqueue満杯、`rejected_total` は不正/過大event、`dropped_total` は閉じたqueueを含む総欠落。DB失敗、未確認ACK、last_commit_at、pruned_totalも別に返す。保存失敗/overflowはavailableへ丸めずdegradedとし、回復後にjournal_gapを保存する。
- 保持は最大14日またはglobal50,000行。6nodeの1分checkpointだけでも14日を全部保持できるとは限らない。実際のoldest/newest/rowsをretentionから確認する。約49MiBというpayload上限はSQLite/WAL/indexの物理ファイル容量上限ではない。VACUUMやglobal設定を変更しない。
- DBが使えない間の最後のevent、queue欠落、強制終了/電源断のtailは保存を保証しない。最後に停止記録がないことだけでcrashとは断定しない。writerのACK失敗・remote child不在・reader EOFは別の事実である。

## Privacy / compatibility

raw stderr/stdout、argv、環境変数、host/user/IP/path、SQL本文、hello.errors、未知end理由は保存しない。既知エラーも固定categoryだけにし、既存UIの自由文detailを永続化へ流さない。OSログの既存の秘匿化済み障害詳細は削減しない。個人の台帳IDは既存のprivate DB内にだけ置き、公開fixtureは架空IDを使う。

Rust/JSでCREATE TABLE IF NOT EXISTSのschemaを揃える。既存表や過去ログを消す移行はない。旧producerは欠けている値をunknown/nullのまま扱い、未来の未対応event versionは読み出し失敗にして成功扱いしない。generic logs/FTSの洪水判定へ正常checkpointを混ぜない。

## 検証と境界

専用表の欠如、typed filter未対応をsemantic REDで確認してから実装した。テストは一時DB/自分の短いfixture子だけでprivacy・queue overflow・DB競合・再読・TTL/row cap・max_age宣言/EOF・最初のsample/stall・保存ACKを確認する。同入力6node microbenchmarkは変換/rollup/Store/記録APIの比較であり、SSH・OS sampler・driverや通常アプリ全体の性能証明ではない。

記録機能のcommit Xとは別のcommit Yで、monitorのrolled watermarkを分集計の保存成功ACK後だけ進める。batch失敗時は成功通知を出さず、水位を保持して次のtickで残存sampleを再集計する。同じ機体・分の再保存は既存キーで置換する。ringの長さや採取間隔は変えないため、長期間のDB障害でringから失われたsampleの回復は保証しない。過去の欠測原因をこの修正から推定しない。

正式反映・Windows実Rust/PS fallback・900秒自然交代・QA app再起動読出し・通常環境前後保全は、source freeze/review後の実機gate。ローカルfixtureやAPIの存在を通常運用での受入と呼ばない。
