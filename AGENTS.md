# katala-tune

Repo-specific delta only. The global baseline is inherited.

## Scope

機体の性能分析・ログ収集と、承認制の最適化を行う Electron アプリ（macOS / Windows）。常時監視は katala-fleet の役目で、ここはその場の深掘りと手当て。

## Constraints

- probe とログ収集は読み取り専用。設定・プロセス・ファイルを変える処理を probes/ に入れない。
- 変更する操作は `lib/actions.js` の許可リストに足し、`plan()` で引数を検証し、テストを書く。main.js の確認ダイアログと直前の台帳読み直しを通さずに実行しない。安全の型は docs/safety.md。
- `probes/*.ps1` は ASCII のみ・BOM 付き（PS 5.1 対策）。日本語は `\uXXXX` で書く。Windows の ssh 既定シェルは Git Bash で、`/xxx` 引数はパスに書き換えられる（`-xxx` で渡す）。PowerShell 本体にシングルクォートを入れない。
- PS 5.1 の `[DateTime]'1970-01-01T00:00:00Z'` は現地時刻として読まれる。epoch は `[DateTimeOffset]::new($d.ToUniversalTime()).ToUnixTimeMilliseconds()` で出す。
- ネットワークとセキュリティ（probes の netsec・`lib/netsec.js`・tune-core の netsec）は読み取り専用で、遮断・設定の変更を入れない。宛先の IP は snapshot に残さず（`strip`）、tune-core の `net_peers`（30 日）にだけ置く。判定を変えたら JS と Rust の両方を直し、`tests/parity_netsec.rs` で一致を確かめる。
- 状態（lib/health.js）は根拠を detail に書き、画面からの問い合わせでは更新通知を送らない（往復が止まらなくなる）。
- Windows の CPU は瞬間値なので、暴走判定は起動からの平均（avg_core）と両方で見る。macOS のディスクは実効の空き（自動で空く分を含む）で判定する。
- 機体台帳は個人情報なのでリポジトリに置かない（`~/.config/katala-tune/nodes.json`）。見本は config/nodes.example.json。
- 依存を増やさない（保存は node:sqlite、画面は素の JS）。開発時だけの依存は Electron 公式（electron・@electron/packager・@electron/fuses）に限る。
- 実行時の Node は Electron 内蔵のもの（Electron 44 = Node 24）。node:sqlite に触る変更は `ELECTRON_RUN_AS_NODE=1 ./node_modules/.bin/electron --test test/*.test.js` でも確かめる（パッケージ後のアプリは fuse で RunAsNode を閉じている）。
- 公開を前提に、個人・環境の情報（機体名・ホスト名・ユーザー名・tailnet・op:// 参照）をコード・テスト・コミットメッセージに書かない。`npm run oss-check` が通ること。

## Verification

`npm test`、`npm run probe` と `npm run logs` で全機体が ok、アプリの「全機を分析」で全カードにスコアが出ること。Windows の変更は Windows 実機でも確かめる。
