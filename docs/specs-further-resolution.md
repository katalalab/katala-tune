# 未照合192件・不足13項目の追加調査

開始時点の specs.7 の保存結果は6台・400観測、完全一致モデル57、公開ID照合89、未照合192、不足13項目だった。観測数は物理部品数ではない。元のJSONと追加調査資料はGit外に固定した。

## 追加できた根拠と取得方法

- [USB / audioの公式資料](specs-usb-further.md)：Logitech受信機、Realtek USB Audio、Genesysカードリーダー、Razer入力機能の6カタログ記録が41観測に対応する。Microsoft Update Catalog配布のメーカー作成INFとLogitech公式FWリポジトリを確認した。共用IDから接続先マウスやキーボードの型番、受信機の現在FWや設定を導かない。
- [PCI / network / codecの追加資料](specs-windows-further.md)：Intel機能、NVIDIA補助機能、Realtek Ethernet、Intel NVMeファミリーとHDAUDIO codecの6記録が12観測に対応する。メーカー／OEM配布物と、版を固定した公式Linuxドライバーの明示ID表を区別する。OSドライバーで定義されるcodecファミリーを完成品基板の型番として扱わない。
- Windowsの `DEVPKEY_Device_BusReportedDeviceDesc` から、汎用名に隠れていた `KURO-DACHI/CLONE/CRU3` を取得し、[メーカーの製品本文](https://www.kuroutoshikou.com/product/release/kuro-dachi-clone-cru3.html) と完全一致で確認した。これはSATAドライブ2台用のUSBスタンドであり、中の `KURO Disk0 / Disk1` のドライブ型番ではない。USB3.2 Gen1の公称上限5Gbps、SATA III/II/I、2.5/3.5型対応を出典範囲で表示する。

バス報告モデルは、取得プロセス内部のデバイスインスタンスIDが一意に対応し、文字列が信頼済みカタログの公開型番に完全一致する場合だけ `reported_model` に保持する。未登録の文字列にはシリアルや独自の識別名が入る可能性があるため保存しない。元のOSモデル名を上書きせず、USB区分で既存の公開ID照合が成立しないときだけ追加型番を照合に使う。内部SCSIディスクへUSB筐体の型番を転用しない。

バス報告用の呼出しは最大128デバイス、開始時刻の上限8秒、各CIM呼出し2秒とする。最後の処理中の呼出しを含めると約10秒までになり、既存の取得プロセス全体100秒の制限も維持する。例外に加え、CIMメソッドの非ゼロ戻り値もprovider失敗として残す。成功したが文字列を持たない場合と取得失敗を区別する。

HDAUDIOは `FUNC_01`、有効な4桁vendor/device、任意の4桁revisionを検証する。Microsoftが無効と定義するvendor `0000 / FFFF`、重複・不正なトークンを受け付けない。`SUBSYS` はPCIのsubsystem vendor/deviceへ分解せず保存しない。`hardware_id_bus` は `PCI / USB / HDAUDIO` に制限し、codecのカタログ記録はHDAUDIO名前空間を必須とする。同じvendor/device番号のPCI機能をaudio区分に報告してもcodecとして照合しない。MacのUSB providerも、その公開IDの名前空間をUSBとして保持し、CoreAudioからcodec IDを推測しない。

## 不足13項目と型番不明の再確認

- Windowsの基板版3項目は生のSMBIOS Type 2でも空だった。Hyper-V論理NIC 6件は追加のドライバーファイル／NDIS情報と43–46件の関連CIMオブジェクトでも版を公開していなかった。別の部品の版やBIOS版を補わない。
- [Macの追加調査](specs-macos-further.md)：awdlの完全一致IOServiceがなく、内蔵パネルの公開属性は表示用vendor/product IDのみで、OEM製造元を証明しない。4項目を保持する。SSDの実IOClassと公式Linuxの互換IDも対応を確認できず、内部SSD型番を推定しない。
- G.SKILLの個別DIMM文字列と販売キット、Patriotの `3000 C16 Series` と個別製品、Acerの `VG252Q` と派生suffixについて、メーカー／OEM資料に一意な対応を確認できなかった。容量・周波数や文字列の類似だけで公称タイミング・電圧を補わない。
- `MSFT_PhysicalDisk` の別経路でも `KURO Disk0 / Disk1` は汎用名のままだった。新しく確認した筐体型番を内部ドライブの型番に置き換えない。

公式本文を取得できたもの、配布物を取得して対応表だけを確認したもの、取得制限が残るものを区別する。CAB/INF/ソースファイルを読む調査で、インストーラー・ドライバーやクローン機能を実行しない。残る未照合観測や不足を削除して件数を改善しない。
