# Macの追加取得経路の再確認

2026-10-25 に、許可された2台のApple Silicon Macで、既存のRust/native-provider/SSH構成から利用できる読取専用APIを追加調査した。この調査で新たに埋められる不足フィールドと、公式資料で新たに一意照合できる部品はなかった。パネル製造元2項目とawdlのドライバー版2項目を保持する。

## awdlの関連付け

Appleの [`IOBSDNameMatching`](https://raw.githubusercontent.com/apple-oss-distributions/IOKitUser/main/IOKitLib.h) は、BSD名が一致するIOServiceを照合するAPIである。両機で `awdl0` の完全一致を問い合わせたが、IOServiceは返らなかった。同じ問い合わせで物理Wi-Fiのサービスは返り、その親を最大8段だけ追うと、SkywalkとWLANのDriverKit bundleを確認できた。この成功は物理側の照合経路が動くことを示すが、awdlとの関連付けにはならない。

読んだ情報は `BSD Name`、インターフェースの接頭辞・番号、`IOClass`、`IOProviderClass`、`CFBundleIdentifier` と `CFBundleIdentifierKernel` に限定した。Registry全体・プロパティ一覧・ネットワークアドレスは取得していない。親の取得はAppleの `IORegistryEntryGetParentEntry` を用いた。このAPIは指定planeで最初に接続された親を返すため、親に見つかったbundleを全仮想インターフェースのドライバーとみなすこともできない。

したがって、物理Wi-Fiのファームウェアに明記された `IO80211_driverkit` の版、Skywalkファミリーの版、OSの版をawdlへ転用しない。今回の結果は、調査したOS/APIの経路で関連付けが得られなかったという意味であり、あらゆる将来のOSで取得不能と断定するものではない。

## パネル製造元

両機で `IODisplayConnect`、`AppleBacklightDisplay`、`AppleCLCD2` のクラス照合は空だった。`IOMobileFramebuffer` と `DCPAVServiceProxy` は存在した。フレームバッファの `DisplayAttributes` からは、`ProductAttributes` 内の製造者・製品・表示名に関する限定キーだけを読んだ。`LegacyManufacturerID` は1552（0x610）だったが、パネルOEMの会社名を示す文字列はなかった。

Appleの [`IOGraphicsTypes.h`](https://raw.githubusercontent.com/apple-oss-distributions/IOGraphics/main/IOGraphicsFamily/IOKit/graphics/IOGraphicsTypes.h) は表示機器のvendor/product IDを定義し、[`IODisplayCreateInfoDictionary`](https://raw.githubusercontent.com/apple-oss-distributions/IOKitUser/main/graphics.subproj/IOGraphicsLib.h) はフレームバッファに関連する表示機器情報を返すと説明する。これらの表示用IDや完成品のブランドから、搭載パネルを製造した会社は決まらない。EDID本体・シリアル番号・製造週や製造年は保存していない。今回も、表示用IDを `manufacturer` に変換しない。

## 内部SSD・内蔵部品

`IONVMeController`、`AppleANS2NVMeController`、`AppleNVMeController` の照合では、同じ `AppleANS3CGv2Controller` が見つかった。クラス照合は継承関係にも一致するので、照合に使用したクラス名を実際のコントローラー世代へ変換しない。親を最大8段追って得た公開 `compatible` はRTBuddy/ASC wrapperやSoCの機能を示し、SSDの内部識別名との公式対応ではなかった。

[Linux upstreamのApple NVMeドライバー](https://github.com/torvalds/linux/blob/master/drivers/nvme/host/apple.c) は `apple,t8015-nvme-ans2`、`apple,t8103-nvme-ans2`、`apple,nvme-ans2` のDevice Tree互換文字列を照合する。これはApple ANS機能の一次資料だが、`APPLE SSD AP0512Z`、`APPLE SSD AP2048Z`、汎用表示の `Apple SSD Controller` との対応表ではない。今回のネイティブ結果にも、そのLinux照合キーと同じコントローラーIDはなく、販売SKUやNAND製造元の証明には使わない。

[AppleのMacBook Pro公式仕様](https://support.apple.com/en-gb/111340) は完成品の内蔵ディスプレイ、スピーカー、マイク、USB/Thunderboltポートの機能・選択肢を説明する。しかし、`Color LCD`、`MacBook Pro Speakers`、`MacBook Pro Microphone`、USBバス名、Thunderboltバス名を、交換部品番号や特定の製造者へ対応付ける資料ではない。同じCoreAudio表示名は世代をまたぐため、特定の機種のスピーカー数・マイク数を表示名だけで適用しない。これらは完成品の公式仕様として参照できても、新たな部品の一意照合には数えない。

## 証跡と実装判断

限定プロパティを読む一時JXA、結果JSON、取得したApple/Linux一次資料の全文とSHA-256、採用しなかった候補の理由はGit外に保存した。新たな値の出所を証明できなかったため、本調査によるcollector・カタログ・不足件数の変更はない。既存のawdl非転用・一意なGPU bundle・provider失敗保持の回帰テストを維持する。
