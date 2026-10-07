//! 操作卓の側から tune-agent を使う（tune-link。docs/connectivity.md）。今の分析・ライブは SSH のまま。
//! 鍵と台帳は `<data_dir>/link`（`KATALA_TUNE_LINK_DIR` で差し替えられる。アプリと同じ場所）。
//!
//!   tune agent-pair <host[:port]>      ペアリング。コードは標準入力から読む（引数には書かない）
//!   tune agent-peers                   ペア済みの tune-agent（公開鍵の指紋だけ）
//!   tune agent-probe <名前|指紋|addr>  ペア済みの tune-agent で調査を 1 回（SSH を使わない。tune probe と同じ形の JSON）
//!   tune agent-unpair <名前|指紋>      台帳から消す

use std::io::BufRead;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tune_core::nodes;
use tune_link::client;
use tune_link::keys::DeviceKeys;
use tune_link::pair::Code;
use tune_link::peers::{Peer, PeerStore, Role};
use tune_link::proto::op;
use zeroize::Zeroizing;

fn print(v: &Value) {
    println!("{}", serde_json::to_string(v).unwrap_or_default());
}

pub async fn run(cmd: &str, rest: &[String]) -> Result<(), String> {
    let dir = tune_link::console_dir(&nodes::data_dir());
    let name = client::console_name(&nodes::local_host());
    match cmd {
        "agent-pair" => {
            let addr = rest.first().ok_or("tune-agent のアドレス（host か host:port）を指定してください")?;
            eprint!("tune-agent に表示された 6 桁のコード: ");
            let mut line = Zeroizing::new(String::new());
            std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
            let code = Code::parse(&line).ok_or("6 桁の数字ではない")?;
            let peer = client::pair(addr, &code, &dir, &name).await.map_err(|e| e.to_string())?;
            print(&json!({ "paired": peer.summary() }));
            Ok(())
        }
        "agent-peers" => {
            let me = DeviceKeys::load(&dir).ok().map(|k| k.fingerprint());
            let peers = PeerStore::new(&dir).list().map_err(|e| e.to_string())?;
            print(&json!({ "dir": dir, "me": me, "peers": peers.iter().filter(|p| p.role == Role::Agent).map(Peer::summary).collect::<Vec<_>>() }));
            Ok(())
        }
        "agent-probe" => {
            let q = rest.first().ok_or("調べる tune-agent（名前・指紋・アドレス）を指定してください")?;
            let peer = PeerStore::new(&dir).find(Role::Agent, q).map_err(|e| e.to_string())?.ok_or("ペア済みの tune-agent に無い（1 台に決まらない）")?;
            let t0 = Instant::now();
            let r = client::call(&peer, &dir, &name, op::PROBE, json!({}), Duration::from_secs(150)).await;
            let wall_s = t0.elapsed().as_secs_f64();
            let at = tune_link::now_ms();
            // collect::probe_node と同じ形（node_id は台帳の名前）
            let out = match r {
                Ok(v) => {
                    json!({ "node_id": peer.name(), "ok": true, "data": v["data"], "wall_s": wall_s, "agent_wall_s": v["wall_s"], "at": at, "via": "tune-agent" })
                }
                Err(e) => json!({ "node_id": peer.name(), "ok": false, "error": e.to_string(), "wall_s": wall_s, "at": at, "via": "tune-agent" }),
            };
            eprintln!("{}: {} {wall_s:.1}s（tune-agent）", peer.name(), if out["ok"] == true { "ok" } else { "NG" });
            print(&Value::Array(vec![out]));
            Ok(())
        }
        "agent-unpair" => {
            let q = rest.first().ok_or("消す相手（名前か指紋）を指定してください")?;
            let n = PeerStore::new(&dir).remove(q).map_err(|e| e.to_string())?;
            print(&json!({ "removed": n }));
            Ok(())
        }
        _ => Err(format!("知らないコマンド: {cmd}")),
    }
}
