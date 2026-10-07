//! ペアリングと端末間暗号化の性質を確かめる（docs/connectivity.md の「確かめていること」）。
//! 通信は 127.0.0.1 の TCP か、メモリ上のパイプ（tokio::io::duplex）だけ。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use snow::Builder;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tune_link::channel::{self, Secure};
use tune_link::frame::{Framed, Transport};
use tune_link::keys::{DeviceKeys, Verified};
use tune_link::pair::{self, Code, PairWindow, Refusal};
use tune_link::peers::{Peer, PeerStore, Role};
use tune_link::proto::{Response, op};
use tune_link::{Error, client, hex};

fn tmpdir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("tune-link-it-{tag}-{}-{}-{}", std::process::id(), tune_link::now_ms(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// (送ったか, フレーム) の記録
type Log = Arc<Mutex<Vec<(bool, Vec<u8>)>>>;

/// 送ったフレーム・受け取ったフレームを記録する（盗み見の役）
struct Tap<T> {
    inner: T,
    log: Log,
}

impl<T: Transport> Transport for Tap<T> {
    async fn send(&mut self, frame: &[u8]) -> std::io::Result<()> {
        self.log.lock().unwrap().push((true, frame.to_vec()));
        self.inner.send(frame).await
    }
    async fn recv(&mut self) -> std::io::Result<Vec<u8>> {
        let f = self.inner.recv().await?;
        self.log.lock().unwrap().push((false, f.clone()));
        Ok(f)
    }
}

fn pipe() -> (Tap<Framed<tokio::io::DuplexStream>>, Log, Framed<tokio::io::DuplexStream>) {
    let (a, b) = tokio::io::duplex(1 << 20);
    let log: Log = Arc::default();
    (Tap { inner: Framed::new(a), log: log.clone() }, log, Framed::new(b))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn all_bytes(log: &Log) -> Vec<u8> {
    log.lock().unwrap().iter().flat_map(|(_, f)| f.clone()).collect()
}

fn code(s: &str) -> Code {
    Code::parse(s).unwrap()
}

/// 1 回のペアリングをメモリ上で。agent 側の送受信を記録して返す
async fn pair_in_memory(
    window: &mut PairWindow,
    agent: &DeviceKeys,
    console: &DeviceKeys,
    console_code: &str,
) -> (Result<Verified, Error>, Result<pair::Paired, Error>, Log, usize) {
    let (mut agent_side, log, mut console_side) = pipe();
    let registered = AtomicUsize::new(0);
    let c = code(console_code);
    let (a, b) = tokio::join!(
        pair::respond(&mut agent_side, window, agent, "agent-under-test", 47231, |_, _| {
            registered.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
        pair::initiate(&mut console_side, &c, console, "console-under-test"),
    );
    (a, b, log, registered.load(Ordering::Relaxed))
}

#[tokio::test]
async fn right_code_pairs_and_exchanges_public_keys_only() {
    let (agent, console) = (DeviceKeys::generate().unwrap(), DeviceKeys::generate().unwrap());
    let mut w = PairWindow::new(code("314159"), Instant::now());
    let (a, b, log, registered) = pair_in_memory(&mut w, &agent, &console, "314 159").await;
    let got_console = a.unwrap();
    let got_agent = b.unwrap();
    assert_eq!((got_console.identity, got_console.static_key), (console.identity(), console.static_public()));
    assert_eq!((got_agent.agent.identity, got_agent.agent.static_key), (agent.identity(), agent.static_public()));
    assert_eq!(got_agent.run_port, 47231);
    assert_eq!(registered, 1);
    // 名刺は暗号化して運ぶので、盗み見た通信に公開鍵も名前も平文では出ない
    let wire = all_bytes(&log);
    for needle in [&agent.identity()[..], &console.identity()[..], b"agent-under-test", b"console-under-test"] {
        assert!(!contains(&wire, needle));
    }
    assert!(!contains(&wire, b"314159"));
}

#[tokio::test]
async fn wrong_code_does_not_pair_and_reveals_nothing() {
    let (agent, console) = (DeviceKeys::generate().unwrap(), DeviceKeys::generate().unwrap());
    let mut w = PairWindow::new(code("111111"), Instant::now());
    let (a, b, log, registered) = pair_in_memory(&mut w, &agent, &console, "111112").await;
    assert!(matches!(a, Err(Error::WrongCode { remaining: 2 })), "{a:?}");
    assert!(matches!(b, Err(Error::WrongCode { remaining: 2 })), "{b:?}");
    assert_eq!((registered, w.failures()), (0, 1));
    // tune-agent は名刺（公開鍵・名前）を送っていない（psk0 なので 1 通目で止まる）。操作卓の名刺も届いていない
    let sent_by_agent: Vec<u8> = log.lock().unwrap().iter().filter(|(s, _)| *s).flat_map(|(_, f)| f.clone()).collect();
    let recv_by_agent: Vec<u8> = log.lock().unwrap().iter().filter(|(s, _)| !*s).flat_map(|(_, f)| f.clone()).collect();
    assert_eq!(sent_by_agent.len(), 1 + 33 + 2, "SPAKE2 のメッセージと「違う」の印だけ");
    for needle in [&agent.identity()[..], &agent.static_public()[..], b"agent-under-test"] {
        assert!(!contains(&sent_by_agent, needle));
        assert!(!contains(&sent_by_agent, hex::encode(needle).as_bytes()));
    }
    for needle in [&console.identity()[..], &console.static_public()[..], b"console-under-test"] {
        assert!(!contains(&recv_by_agent, needle));
        assert!(!contains(&recv_by_agent, hex::encode(needle).as_bytes()));
    }
}

#[tokio::test]
async fn code_expires_after_five_minutes() {
    let (agent, console) = (DeviceKeys::generate().unwrap(), DeviceKeys::generate().unwrap());
    // 本物の 5 分（起動して 5 分たっていない機体では、開いた時刻を過去にできないので短い期限で代える）
    let mut w = match Instant::now().checked_sub(pair::CODE_TTL + Duration::from_secs(1)) {
        Some(past) => PairWindow::new(code("271828"), past),
        None => {
            let w = PairWindow::with_limits(code("271828"), Instant::now(), Duration::from_millis(30), pair::MAX_FAILURES);
            tokio::time::sleep(Duration::from_millis(60)).await;
            w
        }
    };
    let (a, b, log, registered) = pair_in_memory(&mut w, &agent, &console, "271828").await;
    assert!(matches!(a, Err(Error::Refused(Refusal::Expired))), "{a:?}");
    assert!(matches!(b, Err(Error::Refused(Refusal::Expired))), "{b:?}");
    assert_eq!(registered, 0);
    let sent_by_agent: usize = log.lock().unwrap().iter().filter(|(s, _)| *s).map(|(_, f)| f.len()).sum();
    assert_eq!(sent_by_agent, 2, "拒否の印だけ（SPAKE2 にも進まない）");
}

#[tokio::test]
async fn code_is_single_use() {
    let (agent, console) = (DeviceKeys::generate().unwrap(), DeviceKeys::generate().unwrap());
    let mut w = PairWindow::new(code("161803"), Instant::now());
    let (a, b, _, _) = pair_in_memory(&mut w, &agent, &console, "161803").await;
    assert!(a.is_ok() && b.is_ok());
    // 同じコードの 2 回目（別の操作卓でも同じ操作卓でも）は通らない
    let other = DeviceKeys::generate().unwrap();
    let (a, b, _, registered) = pair_in_memory(&mut w, &agent, &other, "161803").await;
    assert!(matches!(a, Err(Error::Refused(Refusal::Used))), "{a:?}");
    assert!(matches!(b, Err(Error::Refused(Refusal::Used))), "{b:?}");
    assert_eq!(registered, 0);
}

/// 127.0.0.1 で待ち受け、接続を `pair::serve` へ順に渡す（tune-agent pair と同じ作り）
async fn pair_server(
    agent: Arc<DeviceKeys>,
    window: PairWindow,
    store_dir: PathBuf,
) -> (String, tokio::task::JoinHandle<(Result<Verified, Error>, PairWindow)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, mut rx) = mpsc::channel::<(Framed<TcpStream>, String)>(8);
    tokio::spawn(async move {
        while let Ok((s, from)) = listener.accept().await {
            if tx.send((Framed::new(s), from.to_string())).await.is_err() {
                break;
            }
        }
    });
    let h = tokio::spawn(async move {
        let mut window = window;
        let store = PeerStore::new(&store_dir);
        let r = pair::serve(
            &mut rx,
            &mut window,
            &agent,
            "agent-under-test",
            47231,
            |_, card| store.add(Peer { role: Role::Console, card: card.clone(), paired_at: tune_link::now_ms(), addr: None }),
            |_| {},
        )
        .await;
        (r, window)
    });
    (addr, h)
}

#[tokio::test]
async fn consecutive_wrong_codes_stop_the_window_one_attempt_at_a_time() {
    let agent = Arc::new(DeviceKeys::generate().unwrap());
    let agent_dir = tmpdir("lock-agent");
    let (addr, server) = pair_server(agent.clone(), PairWindow::new(code("424242"), Instant::now()), agent_dir.clone()).await;
    // 5 つの間違いを同時に投げても、1 つずつ順に処理され、3 回で止まる
    let mut tasks = Vec::new();
    for i in 0..5 {
        let addr = addr.clone();
        let dir = tmpdir(&format!("lock-console-{i}"));
        tasks.push(tokio::spawn(async move { client::pair(&addr, &code(&format!("00000{i}")), &dir, "c").await }));
    }
    let mut wrong = Vec::new();
    for t in tasks {
        match t.await.unwrap() {
            Err(Error::WrongCode { remaining }) => wrong.push(remaining),
            Err(_) => {}
            Ok(p) => panic!("間違ったコードで対になった: {p:?}"),
        }
    }
    wrong.sort();
    assert_eq!(wrong, vec![0, 1, 2], "1 回ずつ順に数えられ、3 回で止まる");
    let (r, window) = server.await.unwrap();
    assert!(matches!(r, Err(Error::Refused(Refusal::Locked))), "{r:?}");
    assert_eq!(window.check(Instant::now()), Err(Refusal::Locked));
    // 止まった後は、正しいコードでも対にならない（受付が閉じている）
    let late = client::pair(&addr, &code("424242"), &tmpdir("lock-late"), "c").await;
    assert!(late.is_err());
    assert!(PeerStore::new(&agent_dir).list().unwrap().is_empty());
}

/// tune-agent run と同じ受け方: ペア済みの操作卓だけに応じ、要求には応答を返す。応答した数を数える
async fn run_server(agent: Arc<DeviceKeys>, store_dir: PathBuf, handled: Arc<AtomicUsize>, unpaired: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            let (agent, store_dir, handled, unpaired) = (agent.clone(), store_dir.clone(), handled.clone(), unpaired.clone());
            tokio::spawn(async move {
                let store = PeerStore::new(&store_dir);
                let accepted =
                    channel::accept(Framed::new(s), &agent, "agent-under-test", |k| store.find_static(Role::Console, k).ok().flatten().map(|(_, v)| v)).await;
                let mut sec = match accepted {
                    Ok(s) => s,
                    Err(Error::Unpaired) => {
                        unpaired.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    Err(_) => return,
                };
                while let Ok(req) = sec.next_request().await {
                    handled.fetch_add(1, Ordering::Relaxed);
                    let res = if req.op == op::HELLO { Response::ok(req.id, json!({ "peer": sec.peer().name })) } else { Response::err(req.id, "unsupported") };
                    if sec.reply(&res).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn paired_console_can_talk_and_unpaired_keys_are_cut_before_content() {
    let agent = Arc::new(DeviceKeys::generate().unwrap());
    let agent_dir = tmpdir("run-agent");
    let console_dir = tmpdir("run-console");
    // ペアリング（TCP）
    let (pair_addr, server) = pair_server(agent.clone(), PairWindow::new(code("123123"), Instant::now()), agent_dir.clone()).await;
    let peer = client::pair(&pair_addr, &code("123123"), &console_dir, "console-under-test").await.unwrap();
    assert!(server.await.unwrap().0.is_ok());
    // 本番の待ち受け
    let (handled, unpaired) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let run_addr = run_server(agent.clone(), agent_dir.clone(), handled.clone(), unpaired.clone()).await;
    let peer = Peer { addr: Some(run_addr.clone()), ..peer };
    let r = client::call(&peer, &console_dir, "console-under-test", op::HELLO, json!({}), Duration::from_secs(5)).await.unwrap();
    assert_eq!(r["peer"], "console-under-test");
    // 大きなメッセージも分けて運べる
    let mut s = client::connect(&peer, &console_dir, "console-under-test").await.unwrap();
    let big = "x".repeat(300_000);
    assert!(s.call(2, "unknown", json!({ "pad": big })).await.is_err(), "知らない操作は ok: false");
    drop(s);
    assert_eq!(handled.load(Ordering::Relaxed), 2);

    // ペアしていない鍵（tune-agent の静的鍵を知っていても）: 1 通目で切られ、何も返ってこない・要求は処理されない
    let stranger = DeviceKeys::generate().unwrap();
    let agent_v = peer.verified().unwrap();
    let s = TcpStream::connect(&run_addr).await.unwrap();
    let log: Log = Arc::default();
    let tap = Tap { inner: Framed::new(s), log: log.clone() };
    let r = channel::connect(tap, &stranger, "stranger", &agent_v).await;
    assert!(r.is_err());
    assert_eq!(log.lock().unwrap().iter().filter(|(sent, _)| !*sent).count(), 0, "tune-agent は何も送り返さない");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(unpaired.load(Ordering::Relaxed), 1);
    assert_eq!(handled.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn replayed_first_message_executes_nothing() {
    let agent = Arc::new(DeviceKeys::generate().unwrap());
    let console = DeviceKeys::generate().unwrap();
    let agent_dir = tmpdir("replay-agent");
    PeerStore::new(&agent_dir).add(Peer { role: Role::Console, card: console.card("console"), paired_at: 0, addr: None }).unwrap();
    let (handled, unpaired) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let run_addr = run_server(agent.clone(), agent_dir, handled.clone(), unpaired).await;
    let agent_v = agent.card("agent-under-test").verify().unwrap();
    // 正規の接続を盗み見る
    let log: Log = Arc::default();
    let tap = Tap { inner: Framed::new(TcpStream::connect(&run_addr).await.unwrap()), log: log.clone() };
    let mut s: Secure<_> = channel::connect(tap, &console, "console", &agent_v).await.unwrap();
    s.call(1, op::HELLO, json!({})).await.unwrap();
    drop(s);
    let frames: Vec<Vec<u8>> = log.lock().unwrap().iter().filter(|(sent, _)| *sent).map(|(_, f)| f.clone()).collect();
    assert_eq!(handled.load(Ordering::Relaxed), 1);
    // 1 通目と要求をそのまま送り直す
    let mut replay = Framed::new(TcpStream::connect(&run_addr).await.unwrap());
    for f in &frames {
        let _ = replay.send(f).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(handled.load(Ordering::Relaxed), 1, "送り直した要求は復号できず、実行されない");
}

/// 盗み見た通信（SPAKE2 と Noise の 1 通目）から、コードの候補を手元で確かめる手段が無いこと。
/// 正しいコードでも、SPAKE2 の秘密の数を持たない人には鍵が作れず、Noise の 1 通目を開けない
#[tokio::test]
async fn eavesdropped_pairing_cannot_be_brute_forced_offline() {
    let (agent, console) = (DeviceKeys::generate().unwrap(), DeviceKeys::generate().unwrap());
    let mut w = PairWindow::new(code("987654"), Instant::now());
    let (a, b, log, _) = pair_in_memory(&mut w, &agent, &console, "987654").await;
    assert!(a.is_ok() && b.is_ok());
    let frames = log.lock().unwrap().clone();
    // agent が受け取った最初のフレーム = "KTP1" ‖ SPAKE2(A)、送った最初のフレーム = 0x00 ‖ SPAKE2(B)、受け取った 2 つ目 = Noise の 1 通目
    let recv: Vec<&Vec<u8>> = frames.iter().filter(|(s, _)| !*s).map(|(_, f)| f).collect();
    let sent: Vec<&Vec<u8>> = frames.iter().filter(|(s, _)| *s).map(|(_, f)| f).collect();
    let msg_a = &recv[0][4..];
    let msg_b = &sent[0][1..];
    let noise1 = recv[1].clone();

    let opens = |psk: &[u8; 32]| {
        let kp = Builder::new(pair::PAIR_PARAMS.parse().unwrap()).generate_keypair().unwrap();
        let mut hs = Builder::new(pair::PAIR_PARAMS.parse().unwrap())
            .local_private_key(&kp.private)
            .unwrap()
            .psk(0, psk)
            .unwrap()
            .prologue(pair::PAIR_PROLOGUE)
            .unwrap()
            .build_responder()
            .unwrap();
        let mut buf = vec![0u8; 65535];
        hs.read_message(&noise1, &mut buf).is_ok()
    };
    let key = |k: Vec<u8>| -> [u8; 32] { k.try_into().unwrap() };
    let ids = (Identity::new(b"katala-tune console"), Identity::new(b"katala-tune agent"));
    let mut tried = 0;
    for guess in std::iter::once(987654u32).chain((0..300).map(|i| i * 3331 % 1_000_000)) {
        let pw = Password::new(format!("katala-tune pairing code {guess:06}"));
        // B のふりをして A のメッセージと組む / A のふりをして B のメッセージと組む。どちらも鍵が合わない
        let (s_b, _) = Spake2::<Ed25519Group>::start_b(&pw, &ids.0, &ids.1);
        let (s_a, _) = Spake2::<Ed25519Group>::start_a(&pw, &ids.0, &ids.1);
        assert!(!opens(&key(s_b.finish(msg_a).unwrap())), "候補 {guess:06} で開けてしまった");
        assert!(!opens(&key(s_a.finish(msg_b).unwrap())), "候補 {guess:06} で開けてしまった");
        tried += 1;
    }
    assert_eq!(tried, 301);

    // 対照: 手順の当事者（SPAKE2 の秘密の数を持つ側）なら、同じ確かめ方で開ける（確かめ方そのものは正しい）
    let pw = Password::new("katala-tune pairing code 987654");
    let (s_a, m_a) = Spake2::<Ed25519Group>::start_a(&pw, &ids.0, &ids.1);
    let (s_b, m_b) = Spake2::<Ed25519Group>::start_b(&pw, &ids.0, &ids.1);
    let (ka, kb) = (key(s_a.finish(&m_b).unwrap()), key(s_b.finish(&m_a).unwrap()));
    assert_eq!(ka, kb);
    let kp = Builder::new(pair::PAIR_PARAMS.parse().unwrap()).generate_keypair().unwrap();
    let mut init = Builder::new(pair::PAIR_PARAMS.parse().unwrap())
        .local_private_key(&kp.private)
        .unwrap()
        .psk(0, &ka)
        .unwrap()
        .prologue(pair::PAIR_PROLOGUE)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut m1 = vec![0u8; 65535];
    let n = init.write_message(&[], &mut m1).unwrap();
    let kp2 = Builder::new(pair::PAIR_PARAMS.parse().unwrap()).generate_keypair().unwrap();
    let mut resp = Builder::new(pair::PAIR_PARAMS.parse().unwrap())
        .local_private_key(&kp2.private)
        .unwrap()
        .psk(0, &kb)
        .unwrap()
        .prologue(pair::PAIR_PROLOGUE)
        .unwrap()
        .build_responder()
        .unwrap();
    let mut buf = vec![0u8; 65535];
    assert!(resp.read_message(&m1[..n], &mut buf).is_ok());
}

/// 送るフレームの n 番目（0 から）の 1 バイトを変える（途中の改ざんの役）
struct Flip<T> {
    inner: T,
    sent: usize,
    at: usize,
}

impl<T: Transport> Transport for Flip<T> {
    async fn send(&mut self, frame: &[u8]) -> std::io::Result<()> {
        let mut f = frame.to_vec();
        if self.sent == self.at {
            let i = f.len() / 2;
            f[i] ^= 1;
        }
        self.sent += 1;
        self.inner.send(&f).await
    }
    async fn recv(&mut self) -> std::io::Result<Vec<u8>> {
        self.inner.recv().await
    }
}

#[tokio::test]
async fn tampered_or_wrong_agent_is_rejected() {
    let agent = DeviceKeys::generate().unwrap();
    let console = DeviceKeys::generate().unwrap();
    let agent_v = agent.card("a").verify().unwrap();
    let console_v = console.card("c").verify().unwrap();
    // ハンドシェイクの後の要求（2 つ目に送るフレーム）の 1 バイトを変えると、受け手は復号に失敗して何も処理しない
    let (x, y) = tokio::io::duplex(1 << 16);
    let (srv, cli) = tokio::join!(
        channel::accept(Framed::new(x), &agent, "a", |k| (*k == console_v.static_key).then(|| console_v.clone())),
        channel::connect(Flip { inner: Framed::new(y), sent: 0, at: 1 }, &console, "c", &agent_v),
    );
    let (mut srv, mut cli) = (srv.unwrap(), cli.unwrap());
    let (r, _) = tokio::join!(srv.next_request(), async { cli.send(br#"{"id":1,"op":"hello"}"#).await });
    assert!(matches!(r, Err(Error::Noise(_))), "{r:?}");

    // 台帳と違う tune-agent（なりすまし）には、操作卓がつながない
    let imposter = DeviceKeys::generate().unwrap();
    let (x, y) = tokio::io::duplex(1 << 16);
    let (_, cli) = tokio::join!(
        channel::accept(Framed::new(x), &imposter, "a", |k| (*k == console_v.static_key).then(|| console_v.clone())),
        channel::connect(Framed::new(y), &console, "c", &agent_v),
    );
    assert!(cli.is_err());
}
