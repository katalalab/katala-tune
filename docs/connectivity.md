# 機体どうしの接続と、配布・更新（設計 2026-10-07）

SSH の設定や鍵を各自で用意しなくても、Mac・Windows・その他のデバイスを安全につなぎ、すぐに調べられる状態にする。あわせて、アプリと常駐を自動で配布・更新する。

## 接続

### つなぎ方（ペアリング）

1. 新しい機体に常駐（`tune-agent`、Rust の単一バイナリ）を入れると、6 桁のコードを表示する
2. 操作卓（Katala Tune）でコードを入力する
3. コードから PAKE（SPAKE2）で一時的な共有鍵を作り、その上で互いの**機体鍵**（Ed25519、機体ごとに生成し外に出さない）を交換する。コードは 5 分で失効し、1 回限り。続けて間違えたら、その機体のコード受付を止める
4. 以後は機体鍵どうしで相互認証した暗号化通信だけを受け付ける。ペアリングしていない相手からの接続は、内容を見る前に切る

PAKE を使うのは、6 桁のコードでも盗み見た通信から総当たりできないようにするため（オンラインで 1 回ずつしか試せない）。

### 経路（速い順に試す）

| 順 | 経路 | 中継 | 遅延 |
|---|---|---|---|
| 1 | 同じ LAN・Tailscale での直接接続 | なし | 最小 |
| 2 | NAT 越えの直接接続（QUIC のホールパンチング。iroh は約 9 割の網で直接つながるとしている） | なし | 小 |
| 3 | 中継（下記） | 暗号文だけを通す | 中継の場所次第 |

中身は常に機体鍵で端末間暗号化しているので、どの経路でも中継や網の途中では読めない。

### 中継をどこに置くか

**推奨: 利用者が自分の Cloudflare アカウントに中継をデプロイする**（`wrangler deploy` 1 回。Worker ＋ Durable Object の WebSocket 中継）。作者（Katala）は共有の中継を運営しない。

| 方式 | 利用者の費用（目安） | 作者の負うもの |
|---|---|---|
| 利用者の Cloudflare（Worker ＋ Durable Object） | 調査結果やログの要約（月に数百 MB）なら Free プランの範囲。ファイル転送などで月数十 GB でも Workers Paid の $5/月程度（Workers は外向きの転送量に課金しない） | なし |
| 利用者の Cloudflare（Realtime の TURN） | 月 1,000 GB まで無料、超過は $0.05/GB | なし。WebRTC（ICE）を使う前提になる |
| n0 の有料 relay（iroh 用） | $19/月〜 | なし |
| 作者が共有の中継を運営 | 利用者は 0 | 下記の危険をすべて負う |

作者が共有の中継を運営しない理由:

- **規約**: Cloudflare の Self-Serve 契約 2.2.1 は「(a) サービスへのアクセスを第三者に販売・貸与する、第三者に代わって申し込む」と「(j) VPN その他類似のプロキシサービスの提供に使う」を禁じている（2025-09-12 版）。他人の通信を通す共有中継は、これに当たると解釈される余地がある（書面で照会しない限り確実にならない）
- **法務（日本）**: 総務省のマニュアルは「他人の通信を媒介」を「他人の依頼を受けて、情報をその内容を変更することなく伝送・交換し、取次ぐこと」としている。暗号文をそのまま通す共有中継はこれに当たりうるし、暗号化していることは当たらない理由にならない（読みは推測。共有中継を出すなら、弁護士か総合通信局への確認が要る）
- **費用**: Cloudflare の予算アラートはメールの通知だけで、利用を止めたり上限を掛けたりしない
- **運用**: 共有中継が止まると全利用者が止まる

利用者が自分のアカウントで自分の機体をつなぐ形なら、これらは利用者自身の契約・請求の範囲に収まる。

### 遅延について

- Durable Object は最初に使われた場所の近くに作られ、その後は動かない。利用者の主な拠点の近くに置く（`locationHint`）
- TURN は anycast で最寄りの拠点に割り当てられる
- どちらも遅延の実測値はまだ無い。PoC で、直接接続・Durable Object・TURN の往復時間を測ってから決める

## 配布と更新

- タグ（`v*`）を打つと CI が macOS（arm64・x64）と Windows（x64）の Tauri 版をビルドし、GitHub Releases に置く
- アプリと常駐は Tauri の updater で自動更新する。更新ファイルは**更新用の署名鍵**（minisign）で署名し、アプリに埋め込んだ公開鍵で検証してから入れる。署名鍵の秘密鍵は 1Password（Katala-Agents）と GitHub Actions の secret にだけ置く
- OS の署名（Apple の公証・Windows のコード署名）は当面しない。macOS は初回だけ「開発元を確認できない」の許可が要る
- 常駐（`tune-agent`）の更新も同じ署名で検証する。署名が合わない更新は入れない

## 段階

| 段階 | 中身 |
|---|---|
| A | リリースの CI と updater（Tauri 版が入ってから） |
| B | `tune-agent` と、ペアリング・機体鍵・LAN／Tailscale での直接接続。読み取り専用の調査を今の probe と同じ形で返す。SSH は予備として残す |
| C | NAT 越えの直接接続（iroh を候補に PoC） |
| D | 利用者の Cloudflare に置く中継（`wrangler deploy` のテンプレート、帯域の上限、予算アラートの手順） |
| E | 遅延の実測を docs に残し、既定の経路を決める |

## 操作者が決めること

- 中継を利用者の Cloudflare に置く方針でよいか（作者は共有中継を運営しない）
- NAT 越えと中継を iroh（QUIC）で組むか、WebRTC（ICE ＋ Cloudflare の TURN）で組むか。PoC の遅延の結果で決める
- 安い共有経路として n0 の有料 relay を案内するか
- リモート画面やファイル転送（月数十 GB）を最初の範囲に入れるか

## 出典（2026-10-07 確認）

- Cloudflare Realtime TURN: https://developers.cloudflare.com/realtime/turn/faq/ ・ https://developers.cloudflare.com/realtime/sfu/pricing/
- Durable Objects の料金・制限・配置: https://developers.cloudflare.com/durable-objects/platform/pricing/ ・ https://developers.cloudflare.com/durable-objects/platform/limits/ ・ https://developers.cloudflare.com/durable-objects/reference/data-location/
- Workers の料金: https://developers.cloudflare.com/workers/platform/pricing/
- Cloudflare の規約: https://www.cloudflare.com/terms/ ・ https://www.cloudflare.com/service-specific-terms-developer-platform/
- 予算アラート: https://developers.cloudflare.com/billing/manage/budget-alerts/
- iroh: https://docs.iroh.computer/concepts/nat-traversal.md ・ https://docs.iroh.computer/concepts/relays.md ・ https://www.iroh.computer/pricing
- 総務省「電気通信事業参入マニュアル（追補版）」: https://www.soumu.go.jp/main_sosiki/hunso/data/pdf/111102_02.pdf
