//! tune-agent: 各機体の常駐（docs/connectivity.md・docs/observability.md）。
//!
//!   tune-agent pair   [--listen IP]... [--port 47232] [--run-port 47231] [--name 名前] [--dir D]
//!       6 桁のコードを端末に表示し、操作卓からのペアリングを 5 分だけ待つ（1 回限り・3 回続けて間違えたら止める）。
//!       コードは端末（/dev/tty・Windows は CONOUT$）にだけ出し、標準出力・ログ・引数には出さない
//!   tune-agent run    [--listen IP]... [--port 47231] [--otlp-port 4318 | --no-otlp] [--name 名前] [--dir D]
//!       ペア済みの操作卓だけを受け付け、読み取り専用の調査を返す。OTLP/HTTP の受け口を 127.0.0.1 に開く
//!   tune-agent status [--port 47231] [--otlp-port 4318] [--dir D]
//!       機体鍵の指紋・ペア済みの相手・待ち受けの状態を JSON で（秘密鍵・コードは出さない）
//!   tune-agent unpair <指紋|名前> [--dir D]
//!       ペア済みの相手を台帳から消す
//!
//! 待ち受けは既定で 127.0.0.1 と Tailscale のアドレスだけ。0.0.0.0・:: では待ち受けない。
//! 置き場所（--dir）の既定は `~/.katala-tune/agent`（`KATALA_TUNE_AGENT_DIR` で差し替えられる）。
//! 常駐としての登録（launchd・タスク スケジューラ）と、Claude Code・Codex の設定の書き換えはしない（手順は docs）。

mod listen;
mod otlp;
mod probe;
mod server;

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tune_link::frame::Framed;
use tune_link::keys::{self, DeviceKeys};
use tune_link::pair::{self, Code, PairEvent, PairWindow, Refusal};
use tune_link::peers::{Peer, PeerStore, Role};
use tune_link::{PAIR_PORT, RUN_PORT};

/// 標準エラーへ 1 行（時刻つき）。秘密・コード・要求の中身は渡さない
pub fn log(msg: &str) {
    eprintln!("{} {msg}", utc_now());
}

fn utc_now() -> String {
    let secs = tune_link::now_ms().div_euclid(1000);
    let (days, s) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // 日数 → 年月日（proleptic Gregorian）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

struct Opts {
    dir: PathBuf,
    listen: Vec<IpAddr>,
    port: Option<u16>,
    run_port: u16,
    otlp_port: u16,
    no_otlp: bool,
    name: Option<String>,
    rest: Vec<String>,
}

impl Opts {
    fn name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            let h = gethostname::gethostname().to_string_lossy().into_owned();
            h.strip_suffix(".local").unwrap_or(&h).to_string()
        })
    }
}

fn default_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("KATALA_TUNE_AGENT_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    std::env::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".katala-tune").join("agent")
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        dir: default_dir(),
        listen: Vec::new(),
        port: None,
        run_port: RUN_PORT,
        otlp_port: otlp::OTLP_PORT,
        no_otlp: false,
        name: None,
        rest: Vec::new(),
    };
    let mut it = args.iter();
    let port = |v: Option<&String>, flag: &str| -> Result<u16, String> {
        v.ok_or(format!("{flag} の値が無い"))?.parse().map_err(|_| format!("{flag} の値が数ではない"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => o.dir = PathBuf::from(it.next().ok_or("--dir の値が無い")?),
            "--listen" => o.listen.push(listen::check_listen(it.next().ok_or("--listen の値が無い")?)?),
            "--port" => o.port = Some(port(it.next(), "--port")?),
            "--run-port" => o.run_port = port(it.next(), "--run-port")?,
            "--otlp-port" => o.otlp_port = port(it.next(), "--otlp-port")?,
            "--no-otlp" => o.no_otlp = true,
            "--name" => o.name = Some(keys::clean_name(it.next().ok_or("--name の値が無い")?)),
            s if s.starts_with("--") => return Err(format!("知らないオプション: {s}")),
            s => o.rest.push(s.to_string()),
        }
    }
    Ok(o)
}

async fn bind_all(addrs: &[IpAddr], port: u16) -> Vec<(SocketAddr, TcpListener)> {
    let mut out = Vec::new();
    for ip in addrs {
        let sa = SocketAddr::new(*ip, port);
        match TcpListener::bind(sa).await {
            Ok(l) => out.push((sa, l)),
            Err(e) => log(&format!("{sa} で待ち受けられない: {e}")),
        }
    }
    out
}

/// 端末にだけ書く（標準出力・標準エラーはログに流れうるので使わない）
fn terminal() -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    let p = "CONOUT$";
    #[cfg(not(windows))]
    let p = "/dev/tty";
    std::fs::OpenOptions::new().write(true).open(p)
}

/// 続けて間違えて受付が止まった時刻を残すファイル。止まった直後に `pair` をやり直して、また試せるのを遅らせる
const LOCKOUT_FILE: &str = "pair.lockout";
/// 止まってから次の受付を始められるまで
const LOCKOUT_COOLDOWN_MS: i64 = 60_000;

/// 止まってからまだ待つ必要があれば、残りのミリ秒
fn cooldown_remaining(dir: &std::path::Path, now: i64) -> Option<i64> {
    let at: i64 = std::fs::read_to_string(dir.join(LOCKOUT_FILE)).ok()?.trim().parse().ok()?;
    let until = at.saturating_add(LOCKOUT_COOLDOWN_MS);
    (now < until).then(|| until - now)
}

fn record_lockout(dir: &std::path::Path, now: i64) {
    let _ = std::fs::write(dir.join(LOCKOUT_FILE), now.to_string());
}

async fn pair_cmd(o: &Opts) -> Result<(), String> {
    if let Some(ms) = cooldown_remaining(&o.dir, tune_link::now_ms()) {
        return Err(format!("続けて間違えて受付が止まった直後なので、あと {} 秒待ってからやり直してください", (ms + 999) / 1000));
    }
    let mut tty = terminal().map_err(|_| "端末が無いのでコードを表示できない（端末から実行してください）".to_string())?;
    let (keys, created) = DeviceKeys::load_or_create(&o.dir).map_err(|e| e.to_string())?;
    if created {
        log(&format!("機体鍵を作った（{}）", o.dir.display()));
    }
    let addrs = if o.listen.is_empty() { listen::defaults().await } else { o.listen.clone() };
    let port = o.port.unwrap_or(PAIR_PORT);
    let listeners = bind_all(&addrs, port).await;
    if listeners.is_empty() {
        return Err("どのアドレスでも待ち受けられない".into());
    }
    let shown: Vec<String> = listeners.iter().map(|(a, _)| a.to_string()).collect();
    let mut window = PairWindow::new(Code::generate().map_err(|e| e.to_string())?, Instant::now());
    let name = o.name();
    let text = zeroize::Zeroizing::new(format!(
        "\n  ペアリングのコード:  {}\n  （5 分で失効・1 回限り・{} 回続けて間違えると止まる）\n  この機体: {name}（指紋 {}）\n  待ち受け: {}\n  操作卓（Katala Tune の「接続」か tune agent-pair）に、アドレスとこのコードを入れてください。\n\n",
        window.code().grouped().as_str(),
        pair::MAX_FAILURES,
        keys.fingerprint(),
        shown.join("、"),
    ));
    tty.write_all(text.as_bytes()).and_then(|()| tty.flush()).map_err(|e| format!("端末に書けない: {e}"))?;
    drop(text);
    log(&format!("ペアリングの受付を始めた（{}・5 分）", shown.join("、")));

    // 接続は 1 つずつ順に試させる（待ちきれない分は閉じる）
    let (tx, mut rx) = mpsc::channel::<(Framed<TcpStream>, String)>(4);
    for (_, l) in listeners {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Ok((s, from)) = l.accept().await {
                let _ = s.set_nodelay(true);
                if tx.try_send((Framed::new(s), from.to_string())).is_err() && tx.is_closed() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let store = PeerStore::new(&o.dir);
    let r = pair::serve(
        &mut rx,
        &mut window,
        &keys,
        &name,
        o.run_port,
        |_, card| store.add(Peer { role: Role::Console, card: card.clone(), paired_at: tune_link::now_ms(), addr: None }),
        |ev| match ev {
            PairEvent::Attempt { from } => log(&format!("ペアリングの試行 from={from}")),
            PairEvent::WrongCode { remaining } => log(&format!("コードが違った（あと {remaining} 回）")),
            PairEvent::Refused(r) => {
                if matches!(r, Refusal::Locked) {
                    record_lockout(&o.dir, tune_link::now_ms());
                }
                log(&format!("断った: {r}"))
            }
            PairEvent::Failed(e) => log(&format!("試行が失敗: {e}")),
            PairEvent::Paired { name, fingerprint } => log(&format!("ペアリングした: {name}（指紋 {fingerprint}）")),
        },
    )
    .await;
    let _ = tty.write_all("  ペアリングの受付を閉じた（コードは無効）\n".as_bytes());
    match r {
        Ok(v) => {
            println!("{}", json!({ "paired": { "name": v.name, "fingerprint": keys::fingerprint(&v.identity), "role": "console" } }));
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

async fn run_cmd(o: &Opts) -> Result<(), String> {
    let (keys, created) = DeviceKeys::load_or_create(&o.dir).map_err(|e| e.to_string())?;
    if created {
        log(&format!("機体鍵を作った（{}）", o.dir.display()));
    }
    let port = o.port.unwrap_or(RUN_PORT);
    let explicit = !o.listen.is_empty();
    let addrs = if explicit { o.listen.clone() } else { listen::defaults().await };
    let listeners = bind_all(&addrs, port).await;
    if listeners.is_empty() {
        return Err("どのアドレスでも待ち受けられない".into());
    }
    let agent = Arc::new(server::Agent::new(keys, o.name(), o.dir.clone()));
    let peers = PeerStore::new(&o.dir).list().map(|p| p.into_iter().filter(|x| x.role == Role::Console).count()).unwrap_or(0);
    log(&format!(
        "待ち受け: {}（指紋 {}・ペア済みの操作卓 {peers} 台）",
        listeners.iter().map(|(a, _)| a.to_string()).collect::<Vec<_>>().join("、"),
        agent.keys.fingerprint()
    ));
    if peers == 0 {
        log("ペア済みの操作卓がまだ無い。tune-agent pair でペアリングしてください（run は止めずに使える）");
    }
    let mut bound: Vec<IpAddr> = Vec::new();
    for (sa, l) in listeners {
        bound.push(sa.ip());
        tokio::spawn(accept_loop(l, agent.clone()));
    }
    if !o.no_otlp {
        let otlp_addr = SocketAddr::new(IpAddr::from([127, 0, 0, 1]), o.otlp_port);
        match (TcpListener::bind(otlp_addr).await, otlp::Sink::open(&o.dir.join("otel"), otlp::MAX_BYTES, otlp::KEEP)) {
            (Ok(l), Ok(sink)) => {
                log(&format!("OTLP の受け口: http://{otlp_addr}（/v1/logs・/v1/metrics、http/json だけ）→ {}", sink.path().display()));
                tokio::spawn(otlp::serve(l, Arc::new(Mutex::new(sink))));
            }
            (Err(e), _) => log(&format!("OTLP の受け口を開けない（{otlp_addr}）: {e}")),
            (_, Err(e)) => log(&format!("OTLP のファイルを開けない: {e}")),
        }
    }
    // Tailscale が後から上がったら、そのアドレスでも待ち受ける（既定のときだけ）
    if !explicit {
        let agent = agent.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let fresh: Vec<IpAddr> = listen::tailscale_addrs().await.into_iter().filter(|ip| !bound.contains(ip)).collect();
                for (sa, l) in bind_all(&fresh, port).await {
                    log(&format!("待ち受けを足した: {sa}"));
                    bound.push(sa.ip());
                    tokio::spawn(accept_loop(l, agent.clone()));
                }
            }
        });
    }
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    log("止めた");
    Ok(())
}

async fn accept_loop(l: TcpListener, agent: Arc<server::Agent>) {
    let limit = Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        match l.accept().await {
            Ok((s, from)) => {
                let Ok(permit) = limit.clone().try_acquire_owned() else {
                    log(&format!("同時の接続が多いので断った from={from}"));
                    continue;
                };
                let agent = agent.clone();
                tokio::spawn(async move {
                    server::handle(agent, s, from).await;
                    drop(permit);
                });
            }
            Err(e) => {
                log(&format!("接続を受けられない: {e}"));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn listening(port: u16) -> bool {
    matches!(tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(("127.0.0.1", port))).await, Ok(Ok(_)))
}

async fn status_cmd(o: &Opts) -> Result<(), String> {
    let device = if o.dir.join(keys::KEY_FILE).exists() {
        match DeviceKeys::load(&o.dir) {
            Ok(k) => json!({ "name": o.name(), "fingerprint": k.fingerprint() }),
            Err(e) => json!({ "error": e.to_string() }),
        }
    } else {
        json!(null)
    };
    let peers = match PeerStore::new(&o.dir).list() {
        Ok(p) => json!(p.iter().map(Peer::summary).collect::<Vec<_>>()),
        Err(e) => json!({ "error": e.to_string() }),
    };
    let port = o.port.unwrap_or(RUN_PORT);
    let mut otlp = otlp::Sink::stat(&o.dir.join("otel"));
    otlp["port"] = json!(o.otlp_port);
    otlp["listening"] = json!(listening(o.otlp_port).await);
    let v = json!({
        "dir": o.dir,
        "device": device,
        "peers": peers,
        "run": { "port": port, "listening_on_loopback": listening(port).await },
        "otlp": otlp,
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}

fn unpair_cmd(o: &Opts) -> Result<(), String> {
    let q = o.rest.first().ok_or("消す相手（指紋か名前）を指定してください")?;
    let n = PeerStore::new(&o.dir).remove(q).map_err(|e| e.to_string())?;
    println!("{}", json!({ "removed": n }));
    if n == 0 { Err("該当する相手が無い".into()) } else { Ok(()) }
}

fn usage() -> ExitCode {
    eprintln!("使い方: tune-agent <pair|run|status|unpair> [...]（詳しくは crates/tune-agent/src/main.rs の先頭）");
    ExitCode::from(2)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else { return usage() };
    let o = match parse(&args[1..]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return usage();
        }
    };
    let r = match cmd.as_str() {
        "pair" => pair_cmd(&o).await,
        "run" => run_cmd(&o).await,
        "status" => status_cmd(&o).await,
        "unpair" => unpair_cmd(&o),
        _ => return usage(),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(&e);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_waits_a_minute_after_lockout() {
        let dir = std::env::temp_dir().join(format!("tune-agent-lockout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(cooldown_remaining(&dir, 1_000_000), None, "止まっていなければ待たない");
        record_lockout(&dir, 1_000_000);
        assert_eq!(cooldown_remaining(&dir, 1_000_000), Some(60_000));
        assert_eq!(cooldown_remaining(&dir, 1_059_999), Some(1));
        assert_eq!(cooldown_remaining(&dir, 1_060_000), None, "1 分たてばやり直せる");
        std::fs::write(dir.join(LOCKOUT_FILE), "壊れた中身").unwrap();
        assert_eq!(cooldown_remaining(&dir, 1_000_000), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn options() {
        let a = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        let o = parse(&a("--listen 127.0.0.1 --port 5 --otlp-port 6 --no-otlp --name x --dir /tmp/kt")).unwrap();
        assert_eq!((o.listen.len(), o.port, o.otlp_port, o.no_otlp, o.name.as_deref()), (1, Some(5), 6, true, Some("x")));
        assert!(parse(&a("--listen 0.0.0.0")).is_err(), "全部のアドレスでは待ち受けない");
        assert!(parse(&a("--port x")).is_err());
        assert!(parse(&a("--what")).is_err());
    }

    #[test]
    fn utc_format() {
        let s = utc_now();
        assert_eq!(s.len(), 20);
        assert!(s.starts_with("20") && s.ends_with('Z'));
    }
}
