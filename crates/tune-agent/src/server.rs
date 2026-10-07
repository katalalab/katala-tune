//! `tune-agent run` の接続の受け方。ペア済みの操作卓（台帳の Role::Console）だけに応じ、読み取り専用の操作だけを返す。
//!
//! 1. Noise IK の 1 通目で相手の静的鍵を確かめ、台帳に無ければ名刺も読まずに切る（何も送らない）
//! 2. ハンドシェイクの後の要求だけを処理する（1 通目の再送では何も動かない）
//! 3. ログには相手の名前・指紋・操作の名前・所要時間だけを書く（要求の中身・結果・鍵は書かない）

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::net::TcpStream;
use tune_link::frame::Framed;
use tune_link::keys::{DeviceKeys, fingerprint};
use tune_link::peers::{PeerStore, Role};
use tune_link::proto::{Request, Response, op};
use tune_link::{Error, channel};

use crate::log;

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 要求の来ない接続を閉じるまで
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Agent {
    pub keys: DeviceKeys,
    pub name: String,
    pub dir: PathBuf,
    /// 調査は同時に 1 つだけ（機体に負荷を重ねない）
    probe_lock: tokio::sync::Mutex<()>,
}

impl Agent {
    pub fn new(keys: DeviceKeys, name: String, dir: PathBuf) -> Agent {
        Agent { keys, name, dir, probe_lock: tokio::sync::Mutex::new(()) }
    }
}

pub async fn handle(agent: Arc<Agent>, stream: TcpStream, from: SocketAddr) {
    let _ = stream.set_nodelay(true);
    let store = PeerStore::new(&agent.dir);
    let accepted = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        channel::accept(Framed::new(stream), &agent.keys, &agent.name, |k| store.find_static(Role::Console, k).ok().flatten().map(|(_, v)| v)),
    )
    .await;
    let mut sec = match accepted {
        Ok(Ok(s)) => s,
        Ok(Err(Error::Unpaired)) => {
            log(&format!("ペアしていない鍵からの接続を切った from={from}"));
            return;
        }
        // 何も送らずに閉じた接続（status の確認など）は記録しない
        Ok(Err(Error::Io(e))) if e.kind() == std::io::ErrorKind::UnexpectedEof => return,
        Ok(Err(e)) => {
            log(&format!("接続を断った from={from}: {e}"));
            return;
        }
        Err(_) => {
            log(&format!("接続を断った from={from}: ハンドシェイクが時間切れ"));
            return;
        }
    };
    let who = format!("{}（{}）", sec.peer().name, fingerprint(&sec.peer().identity));
    log(&format!("接続 from={from} 相手={who}"));
    loop {
        let req = match tokio::time::timeout(IDLE_TIMEOUT, sec.next_request()).await {
            Ok(Ok(r)) => r,
            Ok(Err(Error::Io(_))) | Err(_) => break,
            Ok(Err(e)) => {
                log(&format!("要求を読めないので切った 相手={who}: {e}"));
                break;
            }
        };
        let t = Instant::now();
        // ログには決まった操作の名前だけ（相手が送った文字列をそのまま書かない）
        let opname = [op::HELLO, op::PROBE, op::LIVE_START, op::LIVE_STOP, op::OTLP_READ].into_iter().find(|o| *o == req.op).unwrap_or("(知らない操作)");
        let res = dispatch(&agent, req).await;
        log(&format!("要求 op={opname} 相手={who} {}ms {}", t.elapsed().as_millis(), if res.ok { "ok" } else { "失敗" }));
        if sec.reply(&res).await.is_err() {
            break;
        }
    }
}

/// 読み取り専用の操作だけ。args は使わない（外から差し込める値を持たない）
pub async fn dispatch(agent: &Agent, req: Request) -> Response {
    match req.op.as_str() {
        op::HELLO => Response::ok(
            req.id,
            json!({
                "agent": "tune-agent",
                "version": env!("CARGO_PKG_VERSION"),
                "name": agent.name,
                "os": std::env::consts::OS,
                "ops": [op::HELLO, op::PROBE],
            }),
        ),
        op::PROBE => {
            let _g = agent.probe_lock.lock().await;
            match crate::probe::probe().await {
                Ok((data, wall_s)) => Response::ok(req.id, json!({ "data": data, "wall_s": wall_s })),
                Err(e) => Response::err(req.id, e),
            }
        }
        op::LIVE_START | op::LIVE_STOP | op::OTLP_READ => Response::err(req.id, "まだ無い操作（次の段階で足す）"),
        _ => Response::err(req.id, "知らない操作"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_read_only_ops_exist() {
        let dir = std::env::temp_dir().join(format!("tune-agent-dispatch-{}", std::process::id()));
        let a = Agent::new(DeviceKeys::generate().unwrap(), "t".into(), dir);
        let r = dispatch(&a, Request { id: 1, op: op::HELLO.into(), args: json!({}) }).await;
        assert!(r.ok);
        assert_eq!(r.result["ops"], json!(["hello", "probe"]));
        for o in [op::LIVE_START, op::OTLP_READ, "exec", "shell", "write"] {
            let r = dispatch(&a, Request { id: 2, op: o.into(), args: json!({ "cmd": "rm -rf /" }) }).await;
            assert!(!r.ok, "{o}");
        }
    }
}
