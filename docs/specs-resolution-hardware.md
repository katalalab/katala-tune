# Windows の欠測と公開ハードウェア ID の再確認

2026-10-25 の保存済み 6 台分の結果を起点に、Windows 4 台で読み取り専用の再確認を行った。件数は OS が報告したレコード数であり、物理部品数ではない。

## Windows の不足 9 項目

| 原因 | 項目数 | 実機で確認した内容 | 扱い |
|---|---:|---|---|
| `logical_interface_no_driver_record` | 6 | Hyper-V 論理インターフェース。MSFT_NetAdapter の DriverVersionString、DriverVersion、DriverDate、DriverProvider、DriverName、ComponentID は未報告。PNPDeviceID もなく、Win32_PnPSignedDriver、Win32_PnPEntity へ完全一致で結べない | driver_version の不足を保持 |
| `firmware_not_reported` | 3 | Win32_BaseBoard は製品名を報告するが Version は空 | version の不足を保持。製品ページや BIOS 版から装着基板の PCB リビジョンを推測しない |

6 件すべてについて InterfaceGuid は存在したが、Win32_NetworkAdapter.GUID とネットワーク class レジストリの NetCfgInstanceId の完全一致候補は各 0 件だった。名称だけで別の仮想 NIC やホスト側のドライバーへ結び付けていない。

別経路として、各 Windows 機の実在 PnP デバイス 1 件に対し Win32_PnPEntity.GetDeviceProperties を試した。4 台とも戻り値 0、指定した 4 プロパティを返した。確認したキーは DEVPKEY_Device_DriverVersion、DriverProvider、DriverInfPath、DriverDate。Invoke-CimMethod の OperationTimeoutSec は 8 秒とした。この経路は完全一致する PnP レコードを必要とするため、今回の 6 件を埋める根拠にはならなかった。実機で有効な取得対象を確認できなかったため、追加の実行時フォールバックは導入していない。

調査結果は件数・真偽値だけをローカルの非公開記録に保存した。インスタンス ID 全文、GUID、レジストリの個別パス、MAC/IP アドレスは調査出力やカタログへ保存していない。昇格、ドライバーのインストール、設定変更は実施していない。

Microsoft の一次資料: [MSFT_NetAdapter](https://learn.microsoft.com/en-us/windows-hardware/drivers/network/msft-netadapter)、[Win32_NetworkAdapter の GUID / PNPDeviceID](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-networkadapter)、[GetDeviceProperties](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/getdeviceproperties-win32-pnpentity)、[Get-PnpDeviceProperty](https://learn.microsoft.com/en-us/powershell/module/pnpdevice/get-pnpdeviceproperty)。

## 追加できる公式の ID 照合

候補は 25 カタログ記録で、保存済み未照合レコード 41 件を対象にする。24 記録・39 件には本文全体を取得した根拠があり、残り 1 記録・2 件は公式の索引抜粋である。後者は本文確認済みとは数えない。

| 一次資料 | 確認した公開 ID | 保存済み対象件数 | 照合する範囲 |
|---|---|---:|---|
| [Intel 743844 Rev015、全 224 ページ PDF](https://cdrdv2-public.intel.com/743844/743844-015.pdf) | 4640、460D、467F、A700、A70D、A74D、A77D、A77F | 11 | Host / PCIe / VMD / telemetry 機能 |
| [Intel 832586 Rev007 Device IDs、全 HTML ページ](https://edc.intel.com/content/www/us/en/design/products/platforms/details/arrow-lake-s/core-ultra-200s-series-processors-datasheet-volume-1-of-2/device-ids/) | 7D35、7ECC、AD03、AE4D、AE4C、AD1D、7EC0、AD0B、AE7F、AE0D、AE23 | 11 | Core Ultra 200S/200HX 系の機能。AE0D は明示された AE00–AE1F の Direct eSPI 範囲に含まれる |
| [Intel 700 Series PCH Device IDs、全 HTML ページ](https://edc.intel.com/content/www/us/en/design/products-and-solutions/processors-and-chipsets/700-series-chipset-family-platform-controller-hub-datasheet-volume-1-of/device-and-revision-id/) | 7A50 | 3 | 明示された 7A50–7A57 の HD Audio 範囲 |
| [Intel サポート 000088762](https://www.intel.com/content/www/us/en/support/articles/000088762/technologies/intel-rapid-storage-technology-intel-rst.html) | 09AB | 2 | VMD が生成する managed/dummy 機能。全 HTML の US / JP / ES 取得は HTTP 403、公式索引抜粋のみ |
| [TP-Link T9UH の公式 Linux アーカイブ](https://static.tp-link.com/2018/201805/20180508/Archer%20T9UH_180428_Linux_beta.zip) | USB 2357:0106 | 2 | USB 無線 NIC のファミリー。usb_intf.c の完全一致 ID と Archer T9UH コメント |
| [TP-Link T3U Plus の公式 Windows アーカイブ](https://static.tp-link.com/2019/201912/20191223/Archer%20T3U%20Plus%28EUUS%29_V1_win10_191204.zip) | USB 2357:0138 | 2 | 公式 INF の対応ドライバーファミリー |
| [Logitech StreamCam 公式仕様](https://support.logi.com/hc/en-nz/articles/360042528854-StreamCam-Technical-Specifications) | USB 046D:0893 | 4 | USB複合・HID・WinUSB・audio端点が共有する機器ファミリー。端点ごとに別製品や映像・音声性能を推定しない |
| [ASRock 配布 Realtek LAN アーカイブ](https://download.asrock.com/Drivers/All/LAN/Realtek_LAN(v10.079.0421.2026).zip) | PCI 10EC:8125 | 6 | RTL8125 系 NIC のファミリー。公式 INF の revision 00 / 04 / 05 対応行を確認 |

ZIP と内部アーカイブは展開してテキストを読んだだけで、EXE やインストーラーを実行していない。832586 の PDF は HTTP 403 のため、本文確認範囲を取得できた HTML ページに限定した。カタログには取得本文またはアーカイブ全体の SHA-256 と確認した表・ファイル・行を保存する。ZIP の場合は、実際に確認した内部ソース / INF の SHA-256 も記録する。

`models: []` と section / vendor / device または product ID の組で照合し、一般的な OS デバイス名だけでは照合しない。CPU の core 構成や stepping、基板 SKU / リビジョン、TP-Link の実装ハードウェア版、NIC の現在リンク速度をこの照合から導かない。PCI と network、USB と network に同じ機能が現れても、別の物理製品が確定したことにはしない。

## 残る ID の確認方法

一般 USB ハブ、受信機、複合機器、筐体内コントローラーの VID/PID はファミリーで共用される場合がある。公式 INF の完全一致する ID 行、メーカーの ID 表、または銘板の型番が得られた場合に、その資料が示す範囲で照合する。第三者 ID データベース、接続先基板の製品ページ、Windows の一般デバイス名だけで販売 SKU を補完しない。

Realtek 8153 の追加経路として OEM の公式配布パッケージも探したが、試した Dell パッケージ取得は HTTP 403 だった。この結果を本文取得済みや別メーカーの完成品 SKU として扱わない。NVIDIA の audio / USB 補助機能や NVMe コントローラーも、GPU や SSD の販売名と同じ ID だと推測しない。

追加回帰テストは、公開 ID の正例、異なる vendor / section の負例、一般名称だけでは製品確定しないこと、PnP ID を持たない論理 NIC と未報告の基板 version の不足が残ることを確認する。

## 残る公開IDの分類

固定した基準の127件から41件を追加照合した後は86件が残る。USB 71件（ハブ22、受信機機能18など）、PCI 11件（NVIDIA補助機能6、NVMeコントローラー4、Intel機能1）、audio 2件、network 2件。件数はOSの機能単位である。

[ASMediaのASM1074公式製品ページ](https://www.asmedia.com.tw/products-list/d8cyq6FXzaUH8XJa/1C8Yq95SX9YT1zy8.html)には機能仕様があるが、観測した174C:2074/3074とのID対応は掲載されていない。型番とIDの数字の類似から結合しない。Genesys/VIAのハブ、Logitech受信機の追加検索は第三者ID表が中心で、メーカーによる対応を確認できていない。NVIDIAの補助機能やNVMeコントローラーも、GPU/SSDの販売名の仕様を転用しない。未照合理由と観測IDを保持し、公式ID表・対応INF・銘板の正確な型番が得られた範囲で更新する。
