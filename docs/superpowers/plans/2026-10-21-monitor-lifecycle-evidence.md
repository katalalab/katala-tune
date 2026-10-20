# Monitor Lifecycle Evidence Implementation Plan

**Goal:** 接続/収集/終了/再試行と分集計の保存結果を、秘密を含まないtyped metadataとして既存SQLiteへ有限保存し、日本語summary付きの有界read/health APIへ通す。
**Architecture:** 既存Store内の専用monitor_events表1つ。既存Tokio mpsc256/単一writer/64batchまたは1s、14日または50,000行、event全体1KiB。Live lock内DB I/O/awaitなし。generic logs/FTS・既存retry/stop・C topology/renderer/exportは変更しない。
**Tech Stack:** 既存Rust/Tokio/rusqlite/serde、JS node:sqlite互換schema。依存追加なし。

## Baseline / scope

- baseline HEAD: `7c92943979773ec806dc56f898ed20bb36a754bb`、開始時clean。専有branch/worktree内のみ編集。
- 読取設計9ffd6cd3…のB案を採用。追加設計dialog/人間承認/再委任なし。
- 変更候補: `crates/tune-core/src/{monitor_journal,db/monitor_events}.rs`（新規）、`live.rs`/`monitor.rs`/`engine.rs`/`db/mod.rs`/`lib.rs`、必要最小Tauri live/monitor/setup/commands、`lib/db.js` schema、対応tests/docs。
- producer PIDは現helloで不明。collector/ローカルtransport/remote producerを分離、unknownはnull。actual remote source SHAは不明、期待sourceと同一と主張しない。
- 既存DBにCREATE TABLE IF NOT EXISTSを追加するだけで既存表は移行/削除しない。JSも同じschema。旧producer/Source fixtureはEOF unknownを維持。
- 通常DB/GUI/API/SSH/認証/常設設定/remote install/再起動/push/mergeは0。ローカル検証を固定後、記録機能を独立commit Xとして保存する。

## Tasks / checkpoints

- [x] 1. Store schema/専用保持のsemantic RED→GREEN。一時DBのreopen、TTL/row cap、bounded queryとlegacy表保持。
- [x] 2. typed event/有限queueとsingle writer。privacy canary、1KiB、overflow、DBbusy/failed ACK、recovery/lossをRED→GREEN。
- [x] 3. ProcessRoute EOF/read/exit/unknownを分離。Live open/first sample/end/max_age/retry/stopを接続しFakeRouteで通常挙動を保持。
- [x] 4. monitor policy/5min retry/minute commit成功失敗とcheckpointを接続。Engine read/healthと薄いTauri日本語summary。
- [x] 5. Node24/Electron schema互換、repo全check/OSS、同入力6node無効/有効のCPU/RSS/seqdrop/queue/DBwriteを比較。未実機を分離しfreeze。

## Contracts / success

- typed enum以外の自由文stderr/argv/env/SQL/DB pathを永続化しない。既知エラーもcategoryのみ。
- eventはcollector ID+seq、node canonical local ID、stream generation+attempt、UTCとmonotonic duration。seq欠落/lossを成功扱いしない。
- writerはDB work最大1つ。timeoutでblocking taskを捨てて次々spawnしない。queue/DB errorはhealthへ、回復時gapを保存。queryで書込しない。
- 正常max_ageはfailureと分ける。5min disconnected retryは既存offline抑制policyで、過去gap原因としない。
- 保持capは14日保証ではない。row cap/earliest/drop/DB失敗を表示。SQLite/WAL物理サイズ上限とは別。
- shutdown ACK/EOF/read/wait unknownを0/成功へ丸めない。PIDから遠隔child消滅を推測しない。

## Separate follow-up / actual gates

- commit Xではmonitorのrolledが保存成功前に進む既存挙動を維持。後続commit Yで、保存成功ACK後のみ進める修正とRED→GREENを分離する。過去gapの原因とは断定しない。
- Windows実Rust/PS fallback・QA再起動読出し・自然900s max_age・通常運用反映はRoot review/freeze後の実機gate。fixtureは実機受入ではない。
- rollbackはこの専有差分を統合しない/必要なら将来journal無効。既存表/履歴を削除しない。
