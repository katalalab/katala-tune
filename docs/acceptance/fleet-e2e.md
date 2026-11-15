# Rust Fleet E2Eの検証記録

2026-11-15 JST。収集・分析・保存・再読込・ログ同期を新規DBで実行する`tune e2e`を追加。GUI操作・配布・自動更新の受入とは分ける。

`npm run check`はNode 24、Rust 1.99.0、macOSで成功した。Rustは`RUST_TEST_THREADS=1`、Pythonはmise shimではなくHomebrewの実体をPATHで先にした。JSは662成功・27skip、Rustは774成功・8ignored、失敗0。既定の並列実行では既存の`live::tests::mac_sampler_runs_here`が任意センサーの起動待ちで不安定となり、単独と直列では成功した。初回には一時HOMEでmise設定を未信頼と判定するPython起動失敗もあった。これらを製品修正の成功に置き換えて報告しない。

追加のCLI統合テストとdiscoveryテストは合計10件成功。SSH境界だけをfixtureにし、Engine・収集・分析・SQLite・再読込・ログカーソルは実物を使った。引数、IDの型・重複、空台帳、未対応OS、既存DB保護、除外、収集失敗、JSON出力後の失敗終了、ログ元の部分失敗、カーソル欠落、失敗時に本文を出力しないことを確認した。収集境界のJS/Rust回帰テストも、修正前の失敗と修正後の成功を確認した。

実台帳8台で4台が全段階成功、4台は未完了。成功はmacOS 1台・Windows 3台。残りは到達不能とWindows helper不足で、対象から除外せず失敗として残した。機体ID・取得データ・DBと詳しい結果はprivate記録にだけ保存した。この結果は全台への配布を示さない。

現在の作業内容のOSSチェックとstaged gitleaksは成功。`oss-check --history`は、変更前からmainにあるコミットメッセージの台帳語2件を検出して失敗する。今回の新規コードやコミットに機体情報を追加せず、共有履歴は書き換えていない。

独立レビューで指摘されたJSON異常終了の成功扱い、空snapshot、本文を含むエラー、NULLカーソル、不正なIDを修正。追加テストでそれぞれの失敗・成功を確認した。実行中の台帳編集、Windowsでの追加コマンドの直接起動、GPUIの全機能移植・GUI操作・更新・配布は未確認。
