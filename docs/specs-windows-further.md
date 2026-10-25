# Windows の追加照合と欠測の再確認

2026-10-25 に、保存済みの比較結果から追加の公式配布物と OS ドライバーソースを調べた。配布物はダウンロードしてアーカイブとして展開し、INF 等のテキストだけを確認した。インストーラーやドライバーは実行していない。

## 追加の根拠

| 対象 | 取得した一次資料 | 確認した範囲 |
|---|---|---|
| PCI `8086:7F2F` | [Intel Chipset INF Utility](https://www.intel.com/content/www/us/en/download/19347/chipset-inf-utility.html) の [配布パッケージ](https://downloadmirror.intel.com/925343/SetupChipset.exe) | 内部 ZIP の `Drivers/MeteorLakePCH-S/MeteorLakePCH-SSystem.inf` 32 / 74 行の完全一致 ID と 133 行の Error Aggregation Handler 名。null driver の機能名であり、基板型番や実行中ドライバーの版を確定しない |
| PCI `10DE:1ADB` | [NVIDIA 591.74 配布パッケージ](https://international.download.nvidia.com/Windows/591.74/591.74-desktop-win10-win11-64bit-international-dch-whql.exe) | `PPC/nvppc.inf` 34 行の完全一致 ID と 85 行の USB Type-C Port Policy Controller 名。UCSI ポリシー機能だけを照合する |
| USB / network `0BDA:8153` | [Plugable の公式配布一覧](https://plugable.com/pages/wired-ethernet-network-adapters-drivers) からリンクされた [Realtek パッケージ](https://s3.amazonaws.com/plugable/bin/Realtek/0026-Install_USB_Win10_10025_07252018.zip) | `WIN10/64/rtux64w10.INF` 37 行の完全一致 ID と RTL8153 対応節。RTL8153 コントローラーファミリーを確認し、Plugable 製完成品の型番や現在の通信速度は推定しない |
| PCI `8086:F1A6` | [Linux v6.17 NVMe PCI ドライバーの完全なソース](https://raw.githubusercontent.com/torvalds/linux/v6.17/drivers/nvme/host/pci.c) | 3783 行の `PCI_VDEVICE(INTEL, 0xf1a6)` と Intel 760p / Pro 7600p コメント。OS ドライバーソースによる NVMe コントローラーファミリーの対応であり、メーカーによる販売 SKU の仕様確認とは区別する |
| audio `10DE:0093 / 009E / 00A3 / 00AD` | 同じ NVIDIA 配布パッケージ | `HDAudio/nvhda.inf` 116 / 124 / 128 / 135 行の制限のない codec ID と 551 行の NVIDIA High Definition Audio 名。PCI 側の audio controller ID へ転用しない |
| audio `10EC:0897` | [Linux v6.12 Realtek HDA codec ドライバーの完全なソース](https://raw.githubusercontent.com/torvalds/linux/v6.12/sound/pci/hda/patch_realtek.c) | 13338 行の `HDA_CODEC_ENTRY(0x10ec0897, "ALC897", patch_alc662)`。OS ソースで定義された codec ファミリーだけを照合し、数字の類似から基板型番や音声機能を推測しない |

Intel パッケージ全体の SHA-256 は公式配布一覧の値と一致した。Windows の Authenticode は Intel / NVIDIA パッケージと確認した CAT に `Valid` を返した。これは今回のローカル調査時の結果で、収集機能が署名を独立検証するという意味ではない。カタログには取得物全体の SHA-256、内部 INF の SHA-256、該当行と照合範囲を記録する。

HDAUDIO は既存の `Win32_SoundDevice` 読取結果に含まれていた ID を正規化する。[Microsoft の形式定義](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/identifiers-for-hdaudio-devices) に従い、`FUNC_01` の audio function、4 桁の vendor / codec device、公開されている場合の 4 桁 revision だけを扱う。インスタンス ID の末尾は保存しない。HDAUDIO の `SUBSYS` は implementation ID のため、PCI の subsystem vendor / device に分割しない。GPU 型番、端子数、サンプルレートや出力チャンネルはこの ID から導かない。

基準結果では PCI 3 件、USB / network 2 件、audio 7 件の計 12 件が対象になる。USB と network の同じコントローラー、GPU の補助機能を別の物理製品数として数えない。

## 不足 9 項目の別経路

公開IDには `hardware_id_bus` の名前空間を保持する。codec記録は `HDAUDIO` を必須とし、同じaudio区分に入るPCIデバイスが同じ数値を持っても照合しない。未知の有効なvendorは未照合のまま公開IDを保持し、Microsoftが無効と定義する `0000 / FFFF` は拒否する。異なるバスの互換IDや別codecの修飾子を混ぜないことも回帰テストで確認する。

Windows 4 台で、昇格を伴わない追加の読み取りを行った。CIM の各呼出しは 8 秒で制限し、調査結果は件数・真偽値だけを非公開領域に保存した。

- 基板版は、[Microsoft が案内する生の SMBIOS 取得経路](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-getsystemfirmwaretable) の `MSSmBios_RawSMBiosTables` から Type 2 の Version 文字列を検査した。既に版を取得できていた基板 1 件だけが非空で、不足している 3 件は生のテーブルでも空白または空だった。別の版や製品ページの PCB 表記を補わない。
- Hyper-V 論理 NIC 6 件は DriverDescription、DriverFileName、DriverDateData、NDIS version も未報告だった。[Get-CimAssociatedInstance](https://learn.microsoft.com/en-us/powershell/module/cimcmdlets/get-cimassociatedinstance) で実際の関連オブジェクトを確認すると、各 43–46 件の binding、advanced property、RSS、statistics、data path、IP interface 設定が返り、DriverVersion を持つ関連オブジェクトはなかった。binding のコンポーネントやホスト側のモジュールの版を NIC の版に置き換えない。

この 9 項目を埋める実際の取得経路は確認できなかったため、追加の実行時フォールバックは導入しない。

## 保留する ID と取得制約

NVIDIA パッケージ全体のファイル一覧と 47 件の INF を確認したが、PCI audio `10F9 / 228B / 22BA / 22EB` と xHCI `1ADA` に対応する完全一致の行はなかった。HDAudio の codec ID と PCI controller ID は別である。今回のパッケージには USB3 の独立した INF もなかった。

NVMe `C0A9:5415 / 5426`、Broadcom の報告 ID `14E4:4388` は独立したメーカー / OEM 対応表を確認できず、未照合を維持する。第三者の ID データベースや数字が似た販売名から確定しない。

Intel の旧 NVMe ZIP、Lenovo と StarTech の試した経路は HTTP 403、Crucial NVMe ZIP は HTTP 429 で本文を取得できなかった。ASRock の製品ページは bot challenge の HTML のみだった。これらは本文取得済みとして数えず、失敗 URL と結果を非公開の調査記録に残した。
