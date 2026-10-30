# Topology 受入台帳（2026-10-20）

この台帳は source freeze 前の fixture 実測を記録する。実機 fleet、GUI、配布を fixture の成功で置き換えない。freeze 後の実機・GUI受入はこの source を書き換えず、private receipt と manifest で追跡する。

| 範囲 | 実測 | 状態 |
|---|---|---|
| 型・parser・lease・semantic diff | `cargo test -p tune-core --test topology --locked` | fixture 37 pass |
| local / remote / GitHub adapter | `cargo test -p tune-core --test topology_adapters --locked` | 31 pass、承認済み SSH smoke 1 ignored |
| SQLite history / cache / public export | `cargo test -p tune-core --test topology_store --locked` | fixture 6 pass |
| CLI finite watch と public JSON export | `cargo test -p tune-cli --test topology --locked` | fixture 1 pass（2 iterations） |
| Tauri topology IPC と renderer | `node --test test/topology-view.test.js` | 11 pass。native GUI は別 |
| QA identifier overlay の compile | `TAURI_CONFIG=<private overlay> cargo build -p katala-tune --locked` | build pass。未起動 |
| 10環境 / 65 roots の finite 2-watch | private runtime overlay と preservation receipt | **未実施**。final source / binary manifest 後に実行 |
| Mac / Windows native GUI | isolated config / data / identifier | **未実施**。bundle と CUA 操作が必要 |
| AWS / Cloudflare read audit | topology とは別の既存認証 read | topology 完了の根拠にしない |
| Figma | 手動設計確認 | topology collector から自動更新しない |

収集の Complete は scope がその run で最後まで観測できた意味だけを持つ。Partial / Failed は理由、coverage、counters、source hash、時刻を receipt に残し、以前の Complete graph を保持する。保持済み graph の checkout が今回未観測なら、現在存在する・削除されたとは推論しない。
定期 topology refresh は実際に開始・終了した Complete / Partial / Failed run の `ended_at` で cadence を消費する。disabled、in-process busy、他 process の lease-held は cadence を消費せず、scope の last-success は引き続き独立して保持する。

local の selected JSON は regular file だけを no-follow / non-blocking で開き、開いた handle の種別と Unix identity を再確認する。FIFO と symlink は拒否し、読取りは 8 KiB ごとに共有 deadline を確認する。NFS 等で kernel の metadata / read 自体が停止した場合まで強制中断する保証はない。children counter は directory 数ではなく Git 子プロセスの起動試行数で、local / SSH で同じ単位を使う。

public JSON / Mermaid は既知の種別と出力内参照用の決定的な `entity-N` / `edge-N` 連番のみで、path、host / owner label、credential、設定値、manifest 本文、属性・宣言値・内部 canonical ID を出さない。同一 graph では byte stable だが、graph 変更、view 間、別時点の永続 identity は保証せず、graph 構造や観測時刻からの推定も防御保証しない。credential file / env は収集しない。選択 manifest 全文は bounded に読み parser 処理するが、raw 全文を DB、cache、出力へ永続保存しない。型付き宣言文字列に秘密を直書きした場合の自動識別・除去は保証しない。選択 workflow の job id / needs / uses / if / environment / trigger filter を含む宣言値は private DB、cache、internal view に残るため、秘密を直書きした manifest は入力から除外する。root / config を後日除外しても既存 graph、source cache、history は自動 purge されず、粒度別 purge UI / API は現時点にない。

- `.branch-recovery` is excluded from active checkout discovery because it is a Git recovery object store; its absence is never interpreted as a checkout removal.
- Public export の `observed_at_ms` は各 entity/edge に関係する scope の最終収集試行時刻の最大値であり、個別 fact を最後に再確認した時刻とは区別する。scope receipt が export 上限外・欠測・不正なら `currentness` は `unknown` とする。
設定カウンタには二つの範囲がある。private な環境単位の attempt budget は選択 file を開く前に制限し、capture counter は保持した実体だけを数える。`config_files` は configuration pseudo-repository に対応する成功した bounded regular-file read、`config_keys` は保持した選択 hash の数である。読めない選択は `config_file_read=false`、hash なし、bounded failure receipt を持つ pseudo-repository として残す。

## Opt-in local Docker

A local native environment may add:

```json
{"id":"native","transport":"node","roots":[],"docker":{"enabled":true,"contexts":["default"],"program":"/usr/local/bin/docker"}}
```

`program` is optional (known native installation paths only). `contexts` is
explicit and nonempty; it never enumerates or activates remote contexts. Docker
is disabled by default. Put this only in the private node ledger, within an
existing node's `topology.environments`. Existing CLI `topology-refresh` / `topology-export`
and UI refresh use this configuration and the existing scheduler. No daemon is
started and remote native/WSL collection is excluded in this adapter version.

Verify offline tests for remote endpoint rejection, bounded transports, alias
engine dedup, private field removal, missing health, stopped unhealthy state,
null/zero limits, log policies, SQLite and public exports. The UI shows Docker
state, unknown health, numeric limits and published port mappings. Public JSON
and Mermaid deliberately retain anonymous graph kinds/relations and receipt
currentness, not private attributes.

Separately, an authorized native canary must demonstrate actual read-only
collection, private database copy import, dedup/count/null comparison and
rendered UI. Local/mock checks do not prove Windows/native canary, installation,
distribution, normal database/config changes or production acceptance.
