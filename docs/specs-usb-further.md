# USB / audio の追加公式 ID 照合

2026-10-25 にメーカーの公開リリースノートと、Microsoft Update Catalog で配布されるメーカー作成 INF を追加確認した。インストーラーやドライバーは実行せず、アーカイブをデータとして展開し、対応表と名称の定義だけを読んだ。

## 確認できた範囲

| 公開 ID | 確認した一次資料 | 照合範囲 |
|---|---|---|
| USB 046D:C52B | [Logitech の公式 FW リポジトリ、固定コミットの RQR12.09 リリースノート](https://raw.githubusercontent.com/Logitech/fw_updates/1ddde61310573ecea54c4204b401393a90f4bbae/RQR12/RQR12.09/RQR12.09_B0030.txt)、8–9 / 14 行 | Unifying 受信機ファミリー。文書の FW 版を実機の版として使用しない |
| USB 046D:C548 | [Logitech 1.10.80.0 の配布メタデータ](https://www.catalog.update.microsoft.com/ScopedViewInline.aspx?updateid=bb4d0eb2-ad27-43b4-a82a-4f938ed75d66)、CAB 内 `unifhid.inf` 46 / 50 行、Strings 節 | メーカーが receiver / input と記すファミリー。資料の名称がない Bolt 製品名は追加しない |
| USB 046D:C53F / C547 | [Logitech 1.1.84.7627 の配布メタデータ](https://www.catalog.update.microsoft.com/ScopedViewInline.aspx?updateid=4c1ee655-40d5-4b65-8c5f-3ac93ee73750)、CAB 内 `logi_lamparray_usb.inf` 23 / 27 / 104 / 108 行、`LS2RecvName` | LIGHTSPEED 受信機ファミリー。特定のマウス・キーボード、接続状態、設定は確定しない |
| USB / audio 0B05:1AF1 | [Realtek 6.3.9600.2376 の配布メタデータ](https://www.catalog.update.microsoft.com/ScopedViewInline.aspx?updateid=47aa7208-7a7e-486e-a2a2-61e5fa139858)、CAB 内 `RtDUsbAD_asus.inf` 64 / 233 / 235 行 | Realtek USB Audio ドライバーファミリー。ALC 型番、基板 SKU、音声仕様、現在のチャンネル設定は推定しない |
| USB 05E3:0749 | [Genesys Logic 4.5.15.0 の配布メタデータ](https://www.catalog.update.microsoft.com/ScopedViewInline.aspx?updateid=527cbc57-9003-44ab-8464-a2da8647db11)、CAB 内 `GeneStor.inf` 39 / 52 / 258 行 | USB3.0 Card Reader ドライバーファミリー。USB3.0 は資料の名称で、実機の現在速度ではない |
| USB 1532:0091 | [Razer の配布メタデータ](https://www.catalog.update.microsoft.com/ScopedViewInline.aspx?updateid=7efb2d82-8883-4d93-855d-cd3c3441c9ba)、CAB 内 `rz0091dev.inf` 22 / 26 / 30 / 34 / 102 行 | Viper 8KHz USB 入力機器ファミリー。名称の 8KHz を現在のポーリング設定として使用しない |

Microsoft は配布元、Logitech / Realtek / Genesys Logic / Razer は各 INF が記す提供元である。配布メタデータだけで仕様を作らず、実際の INF 対応行と文字列定義を確認した。カタログの主証跡はアーカイブ全体の SHA-256 と内部 INF の SHA-256、補助証跡は取得したメタデータ HTML の SHA-256 を持つ。7-Zip は CAB 末尾の追加データを報告したが、展開は成功しており、SHA-256 は取得した全バイトを対象とする。

C548、1AF1、0091 の INF 対応行は `MI_00` を明示する。収集済みの公開フィールドにはインターフェース番号がないため、同じ VID/PID を持つ複合・HID 記録には親機器のファミリーだけを付ける。別インターフェースに MI_00 の機能や能力を割り当てない。C53F / C547 はサフィックスのない基本 ID が直接掲載されている。型番が既に完全一致する場合は、その既存照合を優先する。

固定した保存済み基準では、上の追加は未照合 41 観測（USB 40、audio 1）に対応する。Razer の型番完全一致済み 2 観測は追加改善件数に含めない。件数は OS が報告した機能・端点単位で、物理製品数ではない。古い調査メモの USB 71 件とは母集団が異なるため、今回は実際の基準 JSON の公開 VID/PID を再集計した USB 89、audio 2 件から数えた。

## 残る取得・照合の制約

この追加だけでは公開 ID を持つ USB / audio の未照合 50 観測が残る。内訳はハブ 22、Apple / ASUS LED などの入力・内部機能 13、Billboard を含む複合機能 4、C-Media 系 USB / audio 4、その他 USB 周辺機能 7 である。別分野の追加照合で同じ観測を解決した場合、この数も変わる。

Microsoft Update Catalog に対して、残る VID/PID を個別検索した。Genesys 0749、Logitech 受信機、Realtek audio、Razer は対応 INF を取得できた。Genesys 0608 / 0610 / 0626、Realtek 0411、ASMedia 2074 / 3074、VIA 0813 / 2813、Apple 024F、ASUS 19AF、C-Media 0022、Billboard / 独自 HID の検索では、今回採用できる完全一致対応表を取得できなかった。Realtek 5411 の検索結果にはカメラ用パッケージが出たが、検索結果をハブの証明として採用していない。

ハブの販売ページ、第三者 ID 表、Linux 利用者の投稿・列挙ログだけではシリコン型番や販売 SKU を確定しない。C-Media の ID を OWC の完成品型番に読み替えず、Blackmagic の一般名だけから製品を補完しない。公式 INF、メーカーの ID 表、または正確な製品型番が実際に得られた範囲で追加する。

iLok は [公式 Windows アーカイブ](https://installers.ilok.com/iloklicensemanager/LicenseSupportInstallerWin64.zip)と [公式 Mac アーカイブ](https://installers.ilok.com/iloklicensemanager/LicenseSupportInstallerMac.zip)を実際に取得した。Windows 版のドライバー用 EXE を PE データとして開き、内部の InstallShield ストリームまでは取得できたが、7-Zip で読める INF を得られなかった。Mac 版は ZIP → DMG → PKG → License_Service の Payload をアーカイブとして確認したが、確認範囲に 088E:5036 の対応表を得られなかった。取得失敗と取り違えず、ID 対応根拠の未取得として未照合を保持する。インストーラー、サービス、ドライバーは実行していない。

回帰テスト `specs_usb_further.rs` は実際の正規化と受信側再検証を通し、公開 ID の正例、異なる vendor / section の負例、一般名称から製品を確定しないこと、親ファミリー照合から設定・仕様を生成しないことを確認する。
