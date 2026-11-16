# Rust + GPUIプレビュー

`tune-gpui`はtune-coreを直接使うネイティブ画面。概要、機体詳細、分析、保存済みスコア・CPU・空きメモリの推移を表示する。状態、リソース、プロセス、定期処理、ログ、道具、AI、セキュリティ、スペック、電力・Clock、構成情報、実行記録の保存データも読める。台帳の除外状態と理由を表示し、除外機の過去所見を現在の警告数に含めない。

各画面を開く操作と「再読込」はローカルの保存データを読む。実機への収集・状態の再計算・変更操作は自動実行しない。DBの読み込みは専用のバックグラウンド処理にまとめ、途中の画面切り替えを集約する。遅れて届いた旧画面の結果を捨て、長い一覧は表示範囲の行だけ描画する。

「全機を分析」と、ログ・道具・AI・スペック画面の収集ボタンは、明示的なクリックでだけ既存Engineの収集を始める。開始直前に台帳を読み直し、読めない場合は前回の設定で続行しない。運用対象外と共用機の個別opt-inを保持する。収集はアプリ全体で一つだけ実行し、画面移動後も実行中と直近結果を表示する。検索・再読込の通知で結果を消さない。結果は成功・部分成功・失敗・対象外を分け、ログの取得元別エラー、棚卸しの未取得元、AIの上限到達・ファイルエラー・残り枠取得失敗、スペックの直近失敗を成功件数に含めない。結果は最大24行を表示し、詳細コピーは各2048文字までに制限して省略を明記する。スペックの非対応・正常な空sectionは失敗とは扱わない。

検索欄は日本語IME・選択・コピー・貼り付けに対応し、Enterまたは「検索」で実行する。ログはFTS5式、AIはセッション検索、構成情報は有界表示列の検索で、他の画面は取得した表示行を絞る。検索条件は256バイト以内。AIの30日集計や別集計・上部件数の範囲は各画面に記載する。一覧行を選ぶと省略前の詳細を読み、コピーできる。

キーボードではAlt+↑/↓で画面・機体を移動、Cmd/Ctrl+1で概要、Cmd/Ctrl+Fで検索、Tabで検索と一覧を切り替え、↑/↓で行詳細を選ぶ。F5またはCmd/Ctrl+Rは保存データを再読込する。Cmd/Ctrl+Wでウィンドウを閉じる。

macOSのメニューバー「OS 連携」からは「OS 状態を照会」「通知の許可を要求」「通知設定を開く」「合成テスト通知を送信」「ログイン起動を有効にする」「ログイン起動を無効にする」を選べる。ダッシュボードの「詳細」と同じ操作で、結果と前回の観測時刻もそこで確認する。操作中やウィンドウがない間は使えない。メニューを開くだけではOSへ問い合わせず、許可要求やログイン起動の操作後も観測値は前回照会のままなので、改めて「OS 状態を照会」を選ぶ。OSの通常の同意や配布条件は引き続き必要で、通知の受理は実表示の証明ではない。収集は開始しない。アプリメニューの「Katala Tune を終了」は既存の終了処理を使う。

```sh
cargo run -p tune-gpui --release --locked
cargo run -p tune-gpui --release --locked -- --config /path/to/ledger.json --data /path/to/preview-data
```

実行ファイルは`katala-tune-gpui`。Tauri版の`katala-tune`を上書きしない。既定のDBは通常のdataディレクトリの`gpui-preview`配下に分ける。`--data`で指定したDBは初期化・移行・分析保存の対象となるため、既存版と同時に同じDBを使わない。

新版の`Engine::open`は、解決済みdata内の`.katala-tune-engine.lock`を非待機で排他取得してから台帳生成・DB初期化・旧ログ取り込みへ進む。同じdataでは実行ファイルやconfigが違っても二つ目のEngineを拒否し、別dataは共存できる。ガードはSQLiteのStoreに保持するため、Engineを閉じてもDBのArcが残る間は所有を返さない。正常・異常終了後のlockファイルは残し、存在だけでは使用中と判定しない。busy、I/O失敗、不安全なDB・lockパスは区別して返し、自動再試行はしない。

これは参加する新版Engineの保存先だけの協調排他である。旧stable、raw `Store::open`、Engineを通さないCLI収集経路は参加せず、lock取得は旧版停止や収集の唯一性を証明しない。Engineを使うCLIは照会目的でもsameDataが使用中なら失敗する。検出できるDB・SQLite sidecar・lockのsymlink、非通常ファイル、複数hardlink（Windowsではreparseも）を拒否するが、稼働中のdirectory置換や悪意ある同userとのnamespace競争は保護しない。既存版との正式移行とcollector所有権は別の受入項目である。

GPUIは起動引数の検証後、config/dataの相対パスと既存の親ディレクトリを解決し、helperの探索先をそのdataの`bin`へプロセス内で一度だけ固定する。`--data`は環境変数より優先し、省略時は`KATALA_TUNE_DATA_DIR`（無指定なら通常のdataディレクトリ）の`gpui-preview/bin`を使う。通常・常駐・非表示・診断起動で共通し、環境変数を書き換えない。別のdataへの再束縛は拒否する。未束縛のCLI/Tauriの探索先は従来どおり。

起動だけではhelperをコピー・インストール・更新しない。WindowsのProbeと承認付き変更操作には、対象Previewの`data/bin/tune.exe`へ検証済みartifactを明示的に配置し、その版に一致する承認レコードをPreview用台帳に用意する必要がある。欠けている場合やハッシュ・承認が合わない場合は既存の検証で失敗し、HOMEのhelperへ切り替えない。Liveのnative samplerは別の`data/bin/tune-agent`（Windowsでは`tune-agent.exe`）で、実行ファイル隣やHOMEから補わない。native samplerが無い場合は既存のスクリプト経路を使うため、その実行環境と収集の成功は別途確認が必要。`tune.exe`の承認はnative samplerの配布や検証を兼ねない。

`gpui-preview` CIはmacOS arm64とWindows x64で、同じソースからreleaseのGUI・診断CLI・native samplerをビルドする。artifact名は`gpui-preview-<runner OS>-<run ID>-<attempt>`。`manifest.json`の`katala_gpui_preview.v2`は、`katala-tune-gpui`（`gui`）、`tune`（`diagnostic-cli`）、`tune-agent`（`native-sampler`）の三つだけを記録し、Windowsでは各ファイル名に`.exe`が付く。各項目に実バイト数とSHA-256を持ち、manifest全体に版・revision・sourceTree・platform・architecture・release target・workflow/run/attemptを記録する。対象外のOS/arch、非既定のCargo target、欠落・空・256 MiB超・symlinkの入力、既存の出力先を拒否する。途中で保存に失敗した出力にはmanifestを作らず、再利用もしない。

このmanifestは出所とファイルの照合材料であり、CI全体の成功や導入の承認を証明しない。操作者はGitHub runの実際の成功、ZIPのdigest、レビュー済みsourceTree、対象機のOS/arch、展開した三ファイルの名前・サイズ・SHA-256を別途照合する。過去の`katala_gpui_preview.v1`はGUIとCLIの二ファイルの記録として保持し、v2として読み替えない。

展開済みのv2配布元は、Node 24で明示的に読み取り検査できる。`--source`には展開したディレクトリの絶対パスを指定し、検査したいmanifestのOS/archを両方指定する。

```sh
node scripts/inspect-gpui-artifact.js --source /absolute/extracted/gpui-preview --platform darwin --architecture arm64
node scripts/inspect-gpui-artifact.js --source C:/absolute/extracted/gpui-preview --platform win32 --architecture x64
```

引数は三つとも必須で、未知・重複引数や相対sourceは拒否する。moduleからは`inspectArtifact({source, platform, architecture})`を呼ぶ。対応する宣言は`darwin/arm64/aarch64-apple-darwin`と`win32/x64/x86_64-pc-windows-msvc`だけで、検査ホストと異なる宣言も照合できる。実行形式やCPU互換性を検証する機能ではない。

検査はv2の必須キー・型、SemVer、commit/tree/workflowのSHA、workflow/repository/eventの関係、1〜32桁の正のrun/attempt文字列、release target、三つの固定名とroleを照合する。manifestは64 KiB以下、各バイナリは1 byte〜256 MiBの通常ファイルに限る。開いたファイルのidentity・link数・サイズ・読取前後の更新情報と、実際に読み取ったbyte数・SHA-256を確認する。バイナリのhashは最大1 MiBずつ計算し、manifestのhashは整形し直さず元のbytesから求める。三つのうち一つでも失敗すれば部分成功を返さず、stderrに固定roleと段階だけを出して非ゼロで終了する。

成功JSONは`status: "integrity-checked"`と整合確認・出所/承認/導入未確認の文言、正規化した`source`、`manifestSha256`、検証済みmetadataの`manifest`、三つの`files`、唯一の`nativeSampler`を返す。入力ディレクトリ自体のsymlinkは正規化するが、その下のmanifest/binaryの検出可能なsymlink・junction・realpath逸脱、複数hardlink、非通常ファイルは拒否する。Node標準APIはWindowsの任意reparse tagを公開しないため、その全種類を網羅して拒否したとは扱わない。同じuserによる悪意ある並行namespace置換を防ぐ保証もない。この結果は検査時の情報で、後日のcopy許可や永続的な承認tokenには使えない。

検査はdownload・展開・外部問い合わせ・実行・署名検査・DB操作・配置を行わず、余分なdirectory entryも読んだり変更したりしない。sourceや配置先へbytesを書き込まない。CI全成功、配布元とsourceTreeの出所、OS/arch、Mac署名・quarantine・実行、Windows Defender・実行の確認は操作者の別ゲートとして残る。通常起動時のdata/bin限定探索とWindows専用helper承認の契約も変わらない。

道具（Tools）の「収集ヘルパーを導入…」から、展開済みのv2配布フォルダをOS標準の選択画面で一つ指定できる。ZIPの展開やダウンロードは行わない。導入先、旧・新SHA-256、版とCI情報を確認欄に表示し、「実行する」で進む。フォルダ選択の取消や確認の取消・時間切れ・置換・受理前のウィンドウ終了では配置しない。承認後も配布元を再検査し、最初に表示した配布物と導入先のidentityが変わっていれば拒否して再確認を求める。

導入できるのは、この起動中に収集・診断を一度も受理していない場合だけである。Liveの準備中、Network診断、監視・自動スキャン・手動収集も含む。停止・Pause・接続失敗・画面の再表示では導入可能へ戻らない。必要ならアプリを終了し、収集・診断を始めずに開き直す。導入処理中は新しい収集・診断を受け付けない。受理済みの導入はウィンドウを閉じても続け、Quitは配置または復元の終了まで待つ。結果は道具画面に読み取り専用で表示し、常駐中の再表示でも保持する。復元やcloseの完了を確認できない場合は収集と再導入を止め、回復用情報を残す。自動更新・自動回復は行わない。

CIのstagingは通常のbin、Preview data、DB、台帳を変更しない。特に同梱releaseの`tune.exe`は診断用で、Windowsの固定Probe・承認付き操作に使うhelperとして自動承認されない。それらには[Windows helperの導入・更新](windows-helper-approval.md)の専用all-success CI release CLI artifactと非公開の`action_helper` v2承認記録を別途使う。native samplerの出所確認は、そのaction helperの承認を兼ねない。

Rust coreには、明示導入のための `helpers::native_install` を用意している。`prepare_native_sampler(absolute_source)` はNodeの結果を受け取らず、raw v2 manifestと三つのファイルを自身で開いて検証する。未知・重複field、リンク・reparse・複数hardlink、読取・close・identity・hashの不一致を拒否し、既存の構造検査を通したsamplerのbytesだけを最大256 MiB保持する。宣言可能なOS/archは実ビルドのmacOS arm64またはWindows x64に固定し、他の組合せは未対応とする。

`inspect_bound_target()` は束縛済みdata/binのsamplerだけを観測し、未束縛や読取失敗をMissingとしない。`install_native_sampler(prepared, snapshot)` は確認済みbytesと配置直前のsnapshot照合を使い、同じbin内に排他的に作成した一意stageから配置する。dataを再帰作成せず、binが無い場合だけ明示導入内で一つ作成する。同hashでも安全性と実行permissionを確認し、権限を暗黙修復しない。action helper、設定、無関係な `.new` / `.old-*` は変更しない。

旧samplerの退避と新samplerの配置には上書きしないnative renameを使い、配置後の検証まで今回のbackupを保持する。失敗結果は `Rejected`、`RolledBack`、回復用pathを持つ `RecoveryRequired` を区別する。未知の置換先を上書きして復元せず、backup削除だけの失敗は配置済み・backup保持のreceiptになる。receiptやhelper metadataをDB・ファイルへ保存しない。

この公開core API自体は導入承認や収集停止を保証しない。GPUIは全収集開始とpending Liveを共有MonitorHostで直列化し、never-admittedの判定、同OS userの協調claim、ウィンドウGate、受理とworker登録を一つの受付に結び付ける。`revalidate_bound_target(original_snapshot)`は元のdata/bin/file identityを作成なしで再照合する。終了時は登録済み導入workerを時間制限なしで待ち、その後に既存のLive/journal終了予算を開始する。起動・表示・再読込・設定読取だけで配置や収集を始める動作は加えない。

これは電源断に対してatomicなupdaterではなく、同userによる悪意あるnamespace置換を完全には防がない。署名・quarantine・Defender・CPU/プロトコル適合・CI出所・remote orphan不在の保証も含まない。Windowsはlocal NTFSのheld handleとno-replace操作を使用し、稼働ファイルのsharing拒否を回避するためのprocess停止やlease緩和は行わない。Windows native CIとOS上の実行受入は別の検証ゲートである。追加した導入UIのmacOS/Windows実フォルダ選択、確認、receipt、再表示、通常/native QuitのE2E受入は別途必要で、合成ファイルのテストだけから実機受入済みとは判断しない。

`scripts/e2e-gpui.sh`はモデル・保存データ・入力編集・起動引数のテストを実行し、見本台帳と一時DBで全17画面の読み込みsmokeを行う。続いて合成の分析・ログ・操作記録を持つDBでも実行ファイルから全画面を開き、前後の観測・収集カーソル・状態・操作記録が不変であることを確認する。この追加テストはdesktop GPUセッションが必要なため、通常の`cargo test`ではignoreし、このスクリプトから明示実行する。

通常起動は概要を表示するPreviewで、最後のウィンドウを閉じると実行中の収集と保存ACKを待って終了する。`--resident`を付けた場合だけ、最後のウィンドウを閉じてもアプリとトレイを保持する。`--resident --hidden`は最初からユーザー画面を作らない。トレイの「開く」「ダッシュボード」「状態」で一つのウィンドウを表示でき、「終了」は実行中の処理を待って終了する。Windowsでは左クリックでも開き、右クリックでメニューを表示する。macOSではメニューバーのトレイメニューとDockからの再表示に対応する。

常駐開始・非表示起動・再表示・二重起動は収集の許可にならない。保存設定が有効でも監視と自動スキャンは開始しない。ウィンドウを閉じると、その画面のLive所有権と未完了の確認要求だけを解放する。既に明示開始した監視・自動スキャン・手動収集と直近の手動結果は同じアプリ内で保持する。終了時には現在の画面の受付を閉じ、既存の収集完了とLive/journal終了を待つ。保存ACKが確認できない場合は成功終了として扱わない。

通常起動は、同じ実行ファイル・解決済みconfig/dataパス・現在ユーザーの組み合わせで単一起動になる。二度目の起動はDBを開かず、既存アプリの表示だけを要求する。`--open`は256バイト以内で、既存アプリが表示先を検証する。二度目の`--review`は未対応として理由つきで拒否するため、既存ウィンドウへのドラッグか別のdata領域を使う。別の実行ファイルやdata領域はactivation上は独立したPreviewになる。新版Engine同士のsameDataは前述のdata claimで拒否するが、未参加の旧版と同じDBは指定しない。実行ファイル移動・更新時やTauri版との所有統合は正式移行で扱う。

Windowsの常駐時は、GPUI 0.2.2が最後のネイティブウィンドウ破棄で終了するため、空の非表示keeperを一つ保持する。`show=false`、`focus=false`、taskbarに出ないtool windowとして作り、画面・Reader・UI Live・収集処理を持たせない。指定boundsは1×1で、非表示時に保留されるplacementとは別に、生成時の公開resize APIでもcontentの1×1を要求する。実際の物理HWNDサイズはscale factorなどの影響を受け、renderer確保量が1×1相当になるとは限らない。platform/renderer資源は必要で、ゼロGPU確保を保証するものではない。実サイズとCPU/RSSは実機受入で確認する。通常Preview、診断、macOSにはkeeperを作らない。ユーザー画面のCloseで現在の確認GateとUI Liveだけを失効し、承認済みの収集は保持する。keeperが失われた場合は終了処理へ進み、明示的な終了でも保存ACKを同期的に待ってからkeeperを含む全ウィンドウを破棄する。

`--hidden`単独、常駐オプションと`--selftest`などの診断オプションの混在は、DB・トレイを作る前に拒否する。診断は単一起動とトレイを使わない。常駐の自動検証は偽UI/OSと一時DB・一時IPCで行い、実トレイ操作、Dock再表示、Windows実画面・権限・ログイン起動の受入は別途必要。既存版との全機能同等性や実測性能を示すものではない。

`--selftest`は`--config`と`--data`の両方が必要。不正な引数・不正な台帳ではDBを開く前に失敗する。各画面の読み込みが失敗・タイムアウトすれば非ゼロで終了する。成功出力は`selftest ok nodes=<台数> views=17`。このsmokeはボタン操作、実IME、アクセシビリティ、収集や保存のGUI受入を示さない。

保存データの表示には収集範囲の上限がある。ログはプライバシー適用前の最新2000行の窓で、表示件数から全件数や終端を判断しない。AIはUTC基準30日集計と検索一致の最新100セッション（全期間）を区別する。費用は既知単価分だけの推定。構成情報は有界UI射影を使い、採用済みと候補、省略理由、選択行だけの検証を区別する。電力の欠測をゼロにしない。

ダッシュボードは保存済みの監視データを60分・6時間・24時間で表示する。取得元・時刻・欠測・失敗・除外を分け、設定上の監視方針を実際の収集中状態とは扱わない。表示は先頭200台で、coreの取得件数や応答量全体の上限ではない。既存Liveの新しい値がある場合だけ同じ時刻のセンサー値を使い、表示だけではLive・自動監視・hub同期・収集記録のwriterを開始しない。

Dashboard の「監視開始」「自動スキャン開始」は独立した明示操作。保存設定が有効でも、起動・表示・再読込・設定保存だけではどちらも開始しない。「詳細」から監視・ハブ設定と分析・ログ・道具・AI の間隔を保存できる。自動スキャンは開始5秒後、その後60秒ごとに既存 core の予定を確認し、手動収集中はその回を見送る。各回の完了を待ち、停止・終了後は新たな回を開始しない。現在の台帳や保存設定が読めない場合は停止し、修復・保存後も明示的な再開が必要。終了時は実行中の収集を待ち、その後に Live と収集記録を合計3秒の予算で終了する。

機体間ペアリングと自動更新は未移植。常駐は前述の明示オプションで利用する。OS通知・ログイン起動はダッシュボードの「詳細」から明示操作するが、対応条件はOSと配布形態によって異なる。Windows Previewは、後述の導入契約を満たす場合に通知設定の明示照会・送信・設定画面操作と、通知クリックからのReopenを行う。登録はoperator/installerの責務で、アプリは作成・修復・解除しない。Classic Windows通知には許可要求ダイアログがないため、その操作は未対応と表示する。Windows実機での送信・warm/cold activation・cleanupの受入は未完了で、公開前の必須ゲートとする。macOSのログイン起動は正規のapp bundleと標準config/dataでの通常表示起動にのみ対応する。`--resident`と`--resident --hidden`のセッションからの有効化は、起動方法を引き継げないため拒否する。引数・環境変数による独自config/data指定も登録しない。OS状態の照会と無効化は常駐・非表示や独自パスのセッションからも行える。機体詳細のLiveは保持する。アクセシビリティも未完了。既存版を置換する正式リリースとしては扱わず、別DBで試す。Windowsではreleaseビルド、DirectXと実画面の起動を別途確認する。

Windows通知の導入契約はPreview専用のAUMID `org.katala.tune.gpui.preview`、表示名 `Katala Tune GPUI Preview`、CLSID `{7FA17089-16BD-4C17-8035-3CB6ACAF04AC}` を固定する。operator/installerが配置する公開metadataは、OSの`FOLDERID_LocalAppData`配下の`Katala Tune GPUI Preview/installation.json`。schemaは`katala-tune/gpui-preview-installation/v1`で、`schema`、`aumid`、`activator_clsid`、`executable`、`config`、`data`の6 string fieldsだけを持ち、上限は64 KiB。`executable`は導入exe、`config`と`data`は解決済みの絶対パスで、台帳・DBの内容、approval、credentialsは含めない。既知folderはOS APIから取得し、環境変数だけで決めない。

明示照会ではmetadataと現在のexe/config/data、`FOLDERID_Programs/Katala Tune GPUI Preview.lnk`のtargetと正確な`--config <config> --data <data>`、`AppUserModel_ID`と`ToastActivatorCLSID`を照合する。さらに`HKCU\Software\Classes\CLSID\{7FA17089-16BD-4C17-8035-3CB6ACAF04AC}\LocalServer32`のREG_SZ既定値が引用した導入exeと固定`--notification-com-server`を指し、REG_SZ `ServerExecutable`が引用なしの同じexeを指す場合だけ`ToastNotifier.Setting`を読む。同一user内の導入先整合の検査で、登録の作成・修復や認証の代わりではない。既存パスはfilesystemの正規化結果で比較し、未作成部分の曖昧な大文字小文字の違いは不一致として保守的に扱う。

登録欠落・不一致は未対応、access deniedとgroup policyは拒否、application/userによる無効化は無効、未知のsettingや型は未確認または照会失敗として表示する。OS設定が有効でも、送信可能・実表示済みを意味しない。通常起動、保存、診断モードは通知登録や設定を照会せず、照会自身も登録・送信・shell実行・shortcutのResolveを行わない。Windows実機での登録照合と設定観測は後続の受入が必要。

プレビューのGUI対象は既存アプリと同じmacOSとWindows。Linuxではcore・CLIを検証し、GPUIとそのプラットフォーム依存をビルドへ入れない。Linuxでこのバイナリを起動すると、台帳・DBを開かずに未対応として終了する。

容量は実行ファイル単体とapp bundleを分けて測る。WebViewを使わなくても、機能範囲・画面・台帳・DBが違う状態のRSSを一律の軽量化率として比較しない。

機体詳細のLiveと、システム全体を表すKマークは既存mainから保持する。Liveは明示的な開始操作でのみ動き、別画面への移動・ウィンドウの非アクティブ化・終了で停止する。GPUIの公開APIで非表示を確実に区別できないため、別アプリへの切り替えでも停止し、復帰時は利用者が再開する。停止・接続失敗の再試行上限に達した後はheartbeatで再開しない。CPUやメモリの欠測を0として表示しない。

`--open logs|status|topology|<node-id>`で初期画面を指定できる。`--selftest --live <node-id>`は保存画面の検証後に指定機体の実収集を別段階で行い、8秒で3点以上のCPU観測を要求する。明示的な実機用台帳を指定して使う。`--selftest --logs`は保存ログの読み出し件数も出力する。`scripts/e2e-gpui.sh`の任意の`E2E_LIVE_NODE` / `E2E_LOGS`経路でも実行できる。通常の保存画面smokeは実収集をしない。

Networkは画面表示・再読込で診断しない。設定確認（HTTPSなし）と、Apple・Cloudflareへの各1回の外部到達診断を別ボタンから実行する。他の明示収集と同じ単一Jobを使い、台帳・connectivity許可を実行直前に確認する。network:falseはsecurity用で、sharedもそれだけでは拒否しない。report取得成功と診断の注意・欠測を別に数え、結果はセッション内で再訪・別収集後も保持する。先頭200台、共通の収集receiptは24行まで表示する。IP・SSID・DNS名・本文や生の通信エラーは表示・コピーしない。

リソース画面の上部で機体を選ぶと、その機体のライブ（CPU・メモリ、1秒ごと）を一つだけ開始できる。画面を開く・再読込だけでは開始せず、別画面への移動・非アクティブ化・終了で止まる。機体詳細のLiveと同じ制御を共有する。ライブ枠はCPU・メモリに加え、センサーが報告する機体ではGPU使用率（複数GPUは最大値）も表示する。未報告の機体は枠を出さず、取得履歴のある機体の欠測は「–」で表示する。

## 承認付き変更操作

機体詳細の所見に提案の操作が付くときは「提案: ○○…」を押す。実行記録で元に戻せる行を選ぶと「元に戻す…」が出る。どちらも確認欄を通したときだけ実行する。確認欄は「やめる」が既定で、「実行する」を押したときだけ進む。
やめる・背景を押す・別の確認に置き換わる・2 分の時間切れ・終了はすべて進まない。許可リスト、共用機・保護対象の拒否、確認後の台帳の読み直し、実行前の「未完了」記録、実行記録は tune-core の `confirm_and_run` / `undo` が行い、GPUI 側は確認だけを持つ（`gate.rs`）。共用機の所見には提案が付かない。
結果は画面上部に一行で出す。取り消し・拒否・失敗・成功を取り違えない。

承認の判定は単体テストで確かめる（承認・取り消し・時間切れ・置き換え、Engine との結合で取り消し・共用機の拒否・時間切れでは実行されないこと）。macOS では、テストが自分で起動した `sleep` を承認の関門を通して終了させ、実行記録に成功が残ることも確かめる。`--selftest --confirm-demo` は確認欄を出して「やめる」で終える（何も実行しない）。運用中の機体に対して承認して実行する受入は、まだ行っていない。

## 接続とレビュー

- **接続**は、ペア済みの tune-agent の一覧だけを読む（公開鍵の指紋・名前・つなぎ先・ペアリングした時刻）。秘密鍵とコードは読まない。ペアリングと解除は鍵とコードを扱うため、この画面ではしない（既存版か CLI の `tune agent-pair` / `agent-unpair`）。
- **レビュー**は task-completion.v1（完了台帳）の申告を一時表示する。JSON ファイルを画面へドラッグするか、`--review <ファイル>` で読む。2 MiB・UTF-8 まで、文字列 8192 文字・ネスト 16 段・evidence 100/4000 件・tasks 1〜1000 件の制限で検証し、合わない入力は理由つきで断る。
  申告を並べるだけで、検証・受入・完了は判定しない。警告は申告どうしの食い違いや欠けを示すだけで、成否の判定ではない。外部参照の取得も保存もしない。
  `--selftest --review <ファイル>` は、読み込んだ表の行数（タスク + 対象範囲）を出す。

通知の送信は毎回freshな導入照合と`ToastNotifier.Setting`を通し、Enabledだけを送信へ進める。Disabled、Denied、未知値、API失敗では送信しない。固定ToastGenericのDOMにtitle/bodyを`CreateTextNode`で渡し、XMLの文字列補間をしない。launchは固定`reopen-v1`だけで、台帳・DBのpath、任意command、URI、button/inputをpayloadへ含めない。title/body合計のUTF-8入力は2048 bytesまでとし、XML 1.0の禁止文字と、escapingを含む保守的な5000 bytes上限を境界で拒否する。実DOMのUTF-8/UTF-16サイズも5000 bytes以内を確認する。黙ったtruncateや本文ログは行わない。Microsoftの[通知XMLの5 KB制限](https://learn.microsoft.com/en-us/windows/apps/develop/notifications/app-notifications/app-notifications-headers)より保守的な製品上限である。`Show`成功は要求の受理であり、通知の表示・配送を保証しない。設定ボタンは導入照合後に固定`ms-settings:notifications`だけを開く。通知の許可・global設定の上書きは行わない。

Windows専用`--notification-com-server`（後ろにOSの`-Embedding`を一つだけ許可）は通常startupより前に判定する。COM専用modeはEngine、DB準備、helper、tray、collector、通知送信を開始しない。KnownFolder/HKCU登録と現在exeを確認したMTAで、専用CLSIDのIClassFactory/INotificationActivationCallbackを登録する。callbackはAUMID完全一致・`reopen-v1`完全一致・user input count 0だけを受け付け、1件分のcoalescing queueへ渡して直ちに戻る。GPUIの応答は待たない。入力pointerはCOM marshalerの読み取り可能なNUL終端UTF-16契約に従い、長さを固定上限で検査する。

brokerは起動直前に導入先を再照合し、解決済みの同一exeへ`--config <verified config> --data <verified data> --open dashboard`だけを`std::process::Command`のargvで渡す。通常起動のsingle-instanceがDBより前に既存GUIへforwardし、hiddenなら既存session.open、終了済みなら通常primaryへ進む。collectorは自動開始しない。spawnやcallbackの受理はforeground表示の証明ではない。通常GUIはCOMfactoryを登録せず、warm/coldの両方で同じbrokerを使う。

brokerは最後のReopenから30秒のidleで参照・lockがなければ終了し、外部参照・繰返しcallbackがあっても120秒のlifetimeで新規受付を閉じる。これらは製品上の待機方針で、Windows APIのtimeout保証ではない。同期の登録読取り・spawn中の遅延はこの時間を超え得る。終了時はqueueを閉じ、factory cookieをrevokeし、保持interfaceをdropして同じworkerでapartmentを解放する。期限に残った外部interfaceは以後の利用が失敗し得るため、実機での再activationも受入対象とする。OS foreground制限や権限保護を迂回しない。

## 参加GUIの収集予約

新版TauriとGPUIは、同じローカルOSユーザーにつき収集できるプロセスを一つに制限する。別の実行ファイル・台帳・データ保存先でも共通で、対象機体が重ならない場合も同時収集はできない。GPUIの起動、保存画面の表示・再読込、設定保存、Reopenだけでは予約を取得しない。Live、監視、ハブ取得、自動スキャン、全手動収集（Networkの設定確認と外部到達診断を含む）の明示開始時に取得する。`--selftest --live`と収集を伴う終了診断も同じ制限を受ける。Tauriは起動時に自動処理を開始するため、primary setupのEngine準備より先に予約する。

拒否されたGPUIでは保存済み画面を利用でき、開始要求は無効のままになる。所有側のPause、Stop、手動処理の完了、ウィンドウを隠す操作、常駐中の最後のウィンドウ終了では予約を返さない。別GUIへ譲るには所有側のアプリを終了し、利用側で明示的に再試行する。失敗後の自動再開、PIDや時間による強制解除はない。異常終了時もOSがハンドルを閉じた後の明示再試行で取得する。ロックファイル自体は削除しない。

この予約は同じ保存先のEngineを保護するdata claimとは独立している。旧stable、未参加CLI/raw core、別OSユーザーや別hub、remote orphanの不在は保証しない。旧stableを止めたり設定を書き換えたりする移行処理ではない。
