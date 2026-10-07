# 機体どうしの接続と、配布・更新（設計 2026-10-07）

SSH の設定や鍵を各自で用意しなくても、Mac・Windows・その他のデバイスを安全につなぎ、すぐに調べられる状態にする。あわせて、アプリと常駐を自動で配布・更新する。

## 接続

### つなぎ方（ペアリング）

1. 新しい機体に常駐（`tune-agent`、Rust の単一バイナリ）を入れると、6 桁のコードを表示する
2. 操作卓（Katala Tune）でコードを入力する
3. コードから PAKE（SPAKE2）で一時的な共有鍵を作り、それで守った通信の上で、互いの**機体鍵の公開鍵だけ**を交換して登録する。機体鍵（Ed25519 の鍵ペア）は機体ごとに生成し、**秘密鍵は機体の外に出さない**。コードは 5 分で失効し、1 回限り。続けて間違えたら、その機体のコード受付を止める
4. 以後は、登録した公開鍵で相手を相互認証できた接続だけを受け付ける。ペアリングしていない相手からの接続は、内容を見る前に切る

PAKE を使うのは、6 桁のコードでも盗み見た通信から総当たりできないようにするため（オンラインで 1 回ずつしか試せない）。

**鍵の役割を分ける**: 機体鍵（Ed25519）は署名用で、相手が誰かを確かめる（相互認証）ためだけに使う。通信の暗号化は、接続ごとに使い捨ての鍵交換（X25519 など）で作る**セッション鍵**で行い、その鍵交換を機体鍵で認証する（Noise プロトコル、または生の公開鍵で相互認証する QUIC / TLS 1.3。iroh は後者）。

### 経路（速い順に試す）

| 順 | 経路 | 中継 | 遅延 |
|---|---|---|---|
| 1 | 同じ LAN・Tailscale での直接接続 | なし | 最小 |
| 2 | NAT 越えの直接接続（QUIC のホールパンチング。iroh は約 9 割の網で直接つながるとしている） | なし | 小 |
| 3 | 中継（下記） | 暗号文だけを通す | 中継の場所次第 |

中身は常に、認証した機体どうしのセッション鍵で端末間暗号化する。中継（Worker）への WebSocket の TLS は外側の1枚にすぎず、TLS を終端する中継は TLS の中身までは見えるので、その内側に端末間暗号化を必ず保つ。だから、どの経路でも中継や網の途中では中身を読めない。

### 中継をどこに置くか

**推奨: 利用者が自分の Cloudflare アカウントに中継をデプロイする**（`wrangler deploy` 1 回。Worker ＋ Durable Object の WebSocket 中継）。作者（Katala）は共有の中継を運営しない。

| 方式 | 利用者の費用（目安） | 作者の負うもの |
|---|---|---|
| 利用者の Cloudflare（Worker ＋ Durable Object） | 見込みは月 $0〜5 程度だが**未確定**。Workers は外向きの転送量に課金しない一方、Durable Object は要求数（WebSocket の受信メッセージは 20 件で 1 要求）と実行時間に無料枠と従量料金がある。転送量だけでは決まらず、接続している時間・メッセージの頻度と大きさで変わるので、PoC で実測してから確定する | なし |
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

## 実装（段階 B の土台、2026-10-07）

今の調査・ライブは SSH のまま。ここで作ったのは、ペアリング・機体鍵・端末間暗号化と、tune-agent で読み取り専用の調査を 1 つ返すところまで。経路の切り替え（tune-core の `collect`・`live::Route` を tune-agent に載せ替える）は次の段階。

| 部品 | 置き場所 | 中身 |
|---|---|---|
| 接続のライブラリ | `crates/tune-link` | 機体鍵（`keys`）・ペア済みの台帳（`peers`）・経路の抽象（`frame`）・ペアリング（`pair`）・端末間暗号化（`channel`）・要求と応答（`proto`）・操作卓の側の TCP の呼び出し（`client`） |
| 常駐 | `crates/tune-agent` | `tune-agent pair`・`run`・`status`・`unpair`。調査は probes/ を埋め込んで動かす。OTLP の受け口（docs/observability.md） |
| 操作卓（検証用） | `tune agent-pair`・`agent-peers`・`agent-probe`・`agent-unpair`（`crates/tune-cli/src/agent.rs`） | コードは標準入力から読む |
| 操作卓（画面） | 「接続」（`renderer/link.js`・`src-tauri/src/agent.rs`。Tauri 版だけ） | アドレスとコードの入力・ペア済みの機体の一覧（骨組み） |

### 鍵

機体ごとに 2 つの鍵ペアを作る。秘密鍵は機体の外に出さない（交換するのは「名刺」= 公開鍵 2 つと署名だけ）。

| 鍵 | 使い道 |
|---|---|
| 機体鍵（Ed25519） | 身元。自分の Noise の静的鍵と名前に署名する（名刺）。**署名にしか使わない** |
| Noise の静的鍵（X25519） | 接続ごとの Noise の鍵交換で、相手に自分を確かめさせる。相手は「機体鍵の署名が付いた静的鍵」だけを受け付ける |
| セッション鍵 | 接続ごとに作る使い捨ての X25519（Noise の ephemeral）から作る。静的鍵が後で漏れても、過去の通信は読めない（前方秘匿） |

**Ed25519 の鍵を X25519 に変換して流用しない。** 理由:

- 「機体鍵は署名だけ」という上の決定（鍵の役割を分ける）にそのまま沿う
- 同じ鍵を署名と鍵交換の両方に使うことの安全性は、条件付きで示されている（Thormarker, "On using the same key pair for Ed25519 and an X25519 based KEM", 2021）が、その条件を自分の使い方で確かめる必要があり、変換（クランプ・ハッシュの扱い）を取り違える余地も生まれる。分ければ考えなくてよい
- 身元（機体鍵）を変えずに、静的鍵だけを入れ替えられる
- 身元の鍵が Noise の静的鍵に署名する形は、libp2p の Noise（IPFS などで広く使われている）と同じ

**置き場所**（本人だけが読めるファイル。Unix はファイル 0600・ディレクトリ 0700、Windows は継承を外して本人だけに許可（icacls）。読むときに他のユーザーが読める状態なら使わずに止める（ssh と同じ考え方））:

| 側 | ディレクトリ | ファイル |
|---|---|---|
| tune-agent | `~/.katala-tune/agent/`（`--dir`・`KATALA_TUNE_AGENT_DIR`） | `device.key`（目印 8 バイト＋秘密鍵 2 つ。72 バイト）・`peers.json`（ペア済みの操作卓の名刺）・`otel/otel.jsonl`（OTLP の受け口が書くもの） |
| 操作卓 | アプリのデータの場所の `link/`（`KATALA_TUNE_LINK_DIR`） | `device.key`・`peers.json`（ペア済みの tune-agent の名刺とつなぎ先） |

OS の鍵置き場（Keychain・資格情報マネージャー）は使わない。常駐は画面の無いところで鍵を読むので Keychain の確認が出うること、OS ごとの依存が増えること、同じユーザーで動くプログラムからは結局どちらも読めること、から。台帳（`peers.json`）は秘密ではないが、書き換えられると知らない鍵を受け付けるので同じ扱いにし、読むたびに名刺の署名を確かめ直す。

### ペアリング

```text
操作卓（SPAKE2 の A）                         tune-agent（B。tune-agent pair がコードを端末に表示して待つ）
  "KTP1" ‖ SPAKE2(A)                ─────▶  受付中か（期限・使用済み・停止）。だめなら 0x01 ‖ 理由 を返して終わり
                                    ◀─────  0x00 ‖ SPAKE2(B)                 ← ここから 1 回の試行として数える
  K = SPAKE2 の鍵                             K = SPAKE2 の鍵
  Noise_XXpsk0（PSK = K）1 通目       ─────▶  開けない = コードが違う → 0x02 ‖ 残りの回数 を返して終わり
                                    ◀─────  2 通目 ＋ tune-agent の名刺（暗号化）
  3 通目 ＋ 操作卓の名刺（暗号化）      ─────▶  署名と静的鍵を確かめて台帳へ
                                    ◀─────  暗号化した {"ok": true, "run_port": …}
```

- コードは OS の乱数で一様に作る 6 桁。5 分で失効し、1 回成功したら使えない。**3 回続けて間違えたら受付を止める**（tune-agent pair を機体でやり直す）
- 受付は接続を **1 つずつ順に** 処理する。SPAKE2 のメッセージを返した時点で 1 回と数え、成功以外（違う・途中で切れた・20 秒の時間切れ）はすべて失敗に数える。だから試せるのはオンラインで 1 回ずつ、1 回の受付で当たる確率は最大 3/1,000,000
- PSK を最初に混ぜる psk0 なので、コードが違えば 1 通目で分かり、tune-agent は名刺を送らない
- SPAKE2 の鍵をそのまま使わず、Noise の PSK に入れることで、鍵の確認（両側で同じ鍵か）と、名刺の交換の暗号化・静的鍵を持っていることの証明を Noise に任せる（鍵の確認を自作しない）
- コードは **端末（`/dev/tty`、Windows は `CONOUT$`）にだけ** 出す。標準出力・標準エラー・ログ・引数には出さない（端末が無ければ pair は動かない）。操作卓は標準入力（tune-cli）か画面の入力欄（Tauri）から受け取り、どこにも保存しない
- 待ち受けは pair が 47232、run が 47231（`--port`）。既定は 127.0.0.1 と、この機体の Tailscale のアドレス（`tailscale ip` の出力のうち 100.64/10・`fd7a:115c:a1e0::/48` の範囲のもの）だけ。0.0.0.0・`::` は `--listen` で渡されても断る。run は Tailscale が後から上がったら、そのアドレスでも待ち受けを足す

### ペアの後の接続

- `Noise_IK_25519_ChaChaPoly_BLAKE2s`（prologue `katala-tune/link/1`）。操作卓はペアリングで登録した tune-agent の静的鍵を知っているので IK（1 往復）
- tune-agent は 1 通目で操作卓の静的鍵を知る。**台帳に無ければ名刺も読まず、何も送り返さずに切る**
- 名刺の署名（機体鍵）が、Noise で確かめた静的鍵と一致することを毎回確かめる。操作卓も、台帳と違う tune-agent にはつながない
- IK の 1 通目は盗み見た人が再送できる。tune-agent は 1 通目では何も実行せず、ハンドシェイクの後の要求（操作卓の ephemeral 鍵と静的鍵が無いと作れない）だけを処理する
- 経路の抽象は「最大 65535 バイトのフレームを送る・受け取る」だけ（`frame::Transport`）。TCP は長さ 2 バイト＋本体（`Framed`）。将来の WebSocket 中継は 1 メッセージ = 1 フレーム、QUIC はストリームに同じ `Framed` を載せる
- 1 つのメッセージは平文を `[続きの印 1 バイト][本体]` に分けて最大 16 MiB。要求は `{"id","op","args"}`、応答は `{"id","ok","result"|"error"}` の JSON。今の操作は `hello`・`probe`（読み取り専用。args を使わない）だけ。`live.start`・`live.stop`・`otlp.read` は名前だけ決めて、次の段階で同じ形に載せる
- `probe` は今の probe と同じ JSON（probes/mac_probe.py・win_probe.ps1 をバイナリに埋め込んで動かす）。操作卓の `tune agent-probe` は `tune probe` と同じ形（`node_id`・`ok`・`data`・`wall_s`・`at`）で返す

### 選んだクレート（暗号は自作しない）

| クレート | 版 | 使い道 | 選んだ理由 | 監査（README の記載） |
|---|---|---|---|---|
| `spake2` | 0.4.0 | PAKE | RustCrypto の PAKEs。Rust の SPAKE2 で最も使われ（magic-wormhole の Rust 版など）、python-spake2 と互換 | 独立した監査は受けていない |
| `snow` | 0.10.0 | Noise | Rust で最も使われている Noise の実装（libp2p の Noise も使う）。Noise の仕様のテストベクタで試験する仕組みがある | 正式な監査は受けていない。中の暗号は下の RustCrypto と curve25519-dalek |
| `ed25519-dalek` | 2.2.0 | 機体鍵の署名 | 最も使われている Ed25519。`verify_strict` で小さい位数の鍵などを拒む | 記載なし |
| `curve25519-dalek` | 4.1.3 | X25519 の公開鍵の計算だけ | spake2・snow・ed25519-dalek と同じ版（クレートは増えない） | 記載なし |
| `chacha20poly1305`・`blake2` | 0.10.1・0.10.6 | snow の中の AEAD とハッシュ | snow の既定の選択 | chacha20poly1305 は NCC Group の監査あり |
| `sha2`・`rand_core`（getrandom）・`zeroize` | 0.10・0.6・1 | 指紋・OS の乱数・使い終えた秘密を消す | spake2 などが既に使っているもの | |

snow は `std` の feature を入れない（入れると ring まで入る。alloc だけで動く）。HTTP（OTLP の受け口）の解析だけ `httparse` 1（hyper の中で使われている、依存の無い解析器）を足した。

### 脅威と対策

| 脅威 | 対策 | 確かめているテスト（`crates/tune-link/tests/link.rs` ほか） |
|---|---|---|
| 盗聴（同じ網・将来の中継） | 中身は常に Noise で端末間暗号化。ペアリングの名刺も暗号化して運ぶ | `right_code_pairs_and_exchanges_public_keys_only`（通信に公開鍵・名前・コードが平文で出ない） |
| 盗み見た通信からのコードの総当たり | SPAKE2。盗み見た人は鍵を計算できず、候補を手元で確かめる手段が無い | `eavesdropped_pairing_cannot_be_brute_forced_offline`（正しいコードを含む 301 個の候補のどれでも Noise の 1 通目を開けない。対照として当事者なら同じ方法で開けることも確かめる） |
| オンラインの総当たり | 1 つずつ順に・3 回で停止・5 分・1 回限り | `consecutive_wrong_codes_stop_the_window_one_attempt_at_a_time`（5 つを同時に投げても 1 つずつ数えられ 3 回で止まる。止まった後は正しいコードでも通らない）・`code_expires_after_five_minutes`・`code_is_single_use` |
| ペアリングの最中に間に入る | 1 回の接続で 1 つの候補しか試せない。外れると 1 通目が開かず、名刺も届かない | `wrong_code_does_not_pair_and_reveals_nothing` |
| なりすまし（ペアの後） | IK で静的鍵を確かめ、名刺の署名（機体鍵）と一致することを確かめる | `tampered_or_wrong_agent_is_rejected` |
| ペアしていない相手からの接続 | 1 通目で切る。名刺も読まず、何も送らない・何も実行しない | `paired_console_can_talk_and_unpaired_keys_are_cut_before_content` |
| 1 通目の再送 | 要求はハンドシェイクの後だけ処理する | `replayed_first_message_executes_nothing` |
| 改ざん・順番の入れ替え | AEAD と Noise の nonce。復号できなければ切る | `tampered_or_wrong_agent_is_rejected` |
| 中継（段階 D） | 中継は外側の TLS を終端しても、内側の Noise は開けない。見えるのは「いつ・どれだけ」だけ | 段階 D で |
| 鍵の持ち出し | 本人だけが読めるファイル。他人が読めるなら使わない。秘密鍵・コードを Debug・ログ・status に出さない | `key_file_is_private_and_stable`・`debug_never_shows_secrets`・`codes_are_six_digits_and_hidden_in_debug` |
| 全部のアドレスでの待ち受け | 既定は 127.0.0.1 と Tailscale だけ。0.0.0.0・:: は断る | `never_listen_on_all_addresses` |

残る危険（今は対策していない・確かめていない）:

- 同じユーザーで動くプログラムは鍵のファイルを読める（OS の鍵置き場にしても大きくは変わらない）
- `spake2` の `Password` は内部の写しを消さない。コードがメモリに少し残りうる（5 分・1 回限りなので影響は小さい）
- 受付を止めた後に `tune-agent pair` をやり直せば、また 3 回試せる。やり直せるのは機体の端末の前にいる人だけ、という前提に立っている
- 受付は 1 つずつなので、つないだまま黙る相手がいると 20 秒ずつ受付がふさがる（妨害はできるが、コードは当てられない）。待ち受けは 127.0.0.1 と Tailscale だけなので、相手は tailnet の中に限られる
- IK の 1 通目（操作卓の名刺だけ。公開鍵と署名）は、tune-agent の静的鍵が漏れると読める
- 失効は台帳から消すだけ（`tune-agent unpair`・`tune agent-unpair`）。機体をまたいだ失効の一覧は無い
- Windows の鍵のファイルの ACL（icacls）と、Windows での tune-agent の動作は実機で確かめていない

## 配布と更新

- タグ（`v*`）を打つと CI が macOS（arm64・x64）と Windows（x64）の Tauri 版をビルドし、GitHub Releases に置く
- **アプリ**は Tauri の updater で更新する。更新ファイルは**更新用の署名鍵**（minisign）で署名し、アプリに埋め込んだ公開鍵で検証してから入れる。入れる前に確認を取る。署名鍵の秘密鍵は 1Password（Katala-Agents）と GitHub Actions の secret にだけ置く
- OS の署名（Apple の公証・Windows のコード署名）は当面しない。macOS は初回だけ「開発元を確認できない」の許可が要る
- **常駐（`tune-agent`）**は、Tauri の updater の対象ではない（updater は Tauri アプリ自身しか更新しない）。別の仕組みとして、取得（GitHub Releases か、ペアリング済みの操作卓から送る）→ 署名の検証 → 差し替え（失敗したら元に戻せる形で）→ 再起動、を作る。どちらから取得するか・署名鍵をアプリと分けるかは**未決定**（段階 B で決める）。署名が合わない更新は入れない

## 段階

| 段階 | 中身 |
|---|---|
| A | リリースの CI と updater（Tauri 版が入ってから） |
| B | `tune-agent` と、ペアリング・機体鍵・LAN／Tailscale での直接接続。読み取り専用の調査を今の probe と同じ形で返す。SSH は予備として残す（土台は 2026-10-07 に実装。上の「実装」。経路の切り替え・常駐としての登録・配布は未着手） |
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
