# Firmware capability inventory and maintenance plan

## Goal

Rust CLI から macOS / Windows の必要最小限の firmware 情報を一度だけ読み、実行不可の保守計画を作る。既存の監視・分析や配布済みアプリの機能と混同しない。

## Architecture

`tune_core::capabilities` が台帳を再読した CLI の対象を検証し、固定 probe を stdin 経由で逐次実行する。空 ID は local のみ。unknown / offline / opt-out / unsupported OS を接続前に拒否する。DB、helper、設定、常駐処理を作らない。通信は既存 bounded transport の client 側上限35秒（SSH接続10秒を含む）、stdout 64 KiB / stderr 8 KiB。SSH は BatchMode、既知ホスト厳密確認。出力は schema と platform を照合し、許可されたフィールドだけ再構成する。

## Tech stack

既存 Rust / serde_json / tokio と OS 標準 Python / PowerShell のみ。Windows は CIM の明示プロパティと Secure Boot cmdlet、macOS は system_profiler の選択解析。依存を追加しない。

## Checklist

- [x] RED: 対象拒否、provenance、状態区別、sanitize、固定 stdin、plan 常時 disabled、CLI 無副作用
- [x] GREEN: bounded collector と厳密 CLI を実装
- [x] ローカル検証: core / CLI tests、check、OSS check
- [ ] 有限実機読取: 親工程で platform と欠測を確認
- [ ] 親によるレビュー、正式反映、対象環境確認

## Evidence and limits

観測日時、採取元、読取結果と欠測を区別する。Secure Boot の false は disabled、permission denied / unsupported / error は別の状態。macOS は RecoveryOS 等の別の権限経路を未評価として not_collected。収集成功は OEM の書込対応を意味しない。機種の私有台帳や実機の生データは公開 repo に入れない。

## Maintenance and rollback

firmware update / BIOS setting / Secure Boot / TPM / BitLocker / boot order / reboot は実行不可。fresh evidence、機種と firmware の一致、OEM 手順、復旧経路、physical access、安定電源、backup、暗号化回復鍵の保有確認（鍵の値は取得しない）、維持窓、実行直前台帳再読、アプリ確認が未確認であることを列挙する。read-only refresh だけ可能。実際の状態変更が無いため復元操作は不要。機能を戻す場合はコードを取り消す。実機 firmware の rollback は OEM の対応を未検証のまま約束しない。

## Verification record

追加機能の core unit 3 / integration 7 / CLI integration 2 は既存 test binary の単独実行で成功。JS tests は 393 件中 377 pass / 16 skip / 0 fail。全 core/CLI は既存 mac_sampler_runs_here 1件失敗（OS採取の結果欠測）を含むため成功とは扱わない。初回は mise shim の未 trust による既存 Python tests 4件失敗、検証 PATH を OS Python 優先にすることで解消。全 workspace 検証は容量制約で一時停止。Windows mock の cfg test は macOS では実行されないため、実機で別途確認が必要。

独立レビュー修正: Python を -I で隔離し PATH 優先・remote OS fallback とした。cwd/PYTHONPATH の json.py・subprocess.py・sitecustomize.py の trap が修正前に動作し、修正後に動作しないことを実証（RED→GREEN）。保守計画に操作別 recovery 条件・権限・reboot と機体別 support/evidence refs を追加。追加 integration は10件成功。

### Initial local verification (4fc8bb5; historical)

`PATH=/usr/bin:/bin:/usr/sbin:/sbin:$PATH CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1 npm run check` は exit 0。JS 393件（377 pass / 16 skip）、全 workspace Rust 390 pass / 2 ignored、fmt・clippy（all targets、warnings deny）・OSS check が成功。追加試験は core unit 3 / integration 10 / CLI 2 が成功。既存 mac_sampler_runs_here も最終全体検査内で成功し、閾値・skip は変更していない。記録はローカル検証に限る。Windows native mock cfg test・6台の実機 read・GUI/配布・正式反映・firmware write/recovery はこの記録の受入に含めない。

## Review-2 correction and pending verification

SSH は feature-local な UpdateHostKeys=no / ClearAllForwardings=yes / PermitLocalCommand=no を追加し、既存 auth/identity/proxy/alias を維持する。実 ssh -G と一時設定の逆値・forward・LocalCommand fixture で上書きを検証する（接続なし）。

Windows は同じ PowerShell executable の owned Process child を固定 stdin bootstrap で実行する。Start 成功直後から同じ Process association を cleanup 対象にし、Handle/ID/start は私有に保持する。worker17秒、cleanup acknowledgement の有限待機期限19.5秒（ackなしはcleanup_unconfirmed）、pipe grace最大0.5秒（absolute20秒）、stdin write+flush合計2秒。終了・pipe ack が無い場合は report を破棄する。これは owned collector の寿命管理だけで、既存 jobs・設定・maintenance action を変更しない。

新 envelope の completed と remote timeout / cleanup unconfirmed / worker failed を区別する。outer client timeout/overflow/failure は remote completion を未確認とし、合成 RunResult の分類テストを実機終了証拠と混同しない。Windows native cfg tests は worker 内 marker に mock を挿入し、従来8状態と sleep45 (> production17s) を実行する。

前版4fc8bb5の実測を新headの受入として使わない。新headの独立review、native SSHでのowned PID/start/ParentPIDと終了後0件、全体checkは統合側の工程でpending。

### Review-2 targeted verification

既存targetを使い jobs=1 / test threads=1 で core capability unit6件、integration11件、CLI2件が成功。実 ssh -G の逆値fixtureで UpdateHostKeys=true の RED を確認後、feature-local 上書きとforward消去の GREEN を確認した。envelope追加前の成功reply/timeout分類の RED も確認し、終了/pipe ack・外側transport失敗の未確認・Mac旧契約の GREEN を確認した。対象core/CLIのall-targets clippy（warnings deny）、全体fmt check、OSS checkはexit0。Windows native cfg testsはこのmacOS実行では対象外。親のnative PS5.1実測は別receiptで管理し、新headのfull check・独立review・正式反映はpendingのまま区別する。
