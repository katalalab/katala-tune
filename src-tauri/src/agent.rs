//! 「接続」の画面（ペアリングの骨組み）。tune-agent とのペアリングと、ペア済みの機体の一覧だけ。
//! 調査・ライブの経路は SSH のまま（tune-agent への切り替えは次の段階。docs/connectivity.md）。
//! 鍵と台帳は `<data_dir>/link`（tune-cli の agent-* と同じ場所）。コードはこの呼び出しの中だけで使い、ログに出さない。

use serde_json::{Value, json};
use tune_core::nodes;
use tune_link::client;
use tune_link::keys::DeviceKeys;
use tune_link::pair::Code;
use tune_link::peers::{Peer, PeerStore, Role};

type R = Result<Value, String>;

fn dir() -> std::path::PathBuf {
    tune_link::console_dir(&nodes::data_dir())
}

/// ペア済みの tune-agent（公開鍵の指紋・名前・つなぎ先・時刻だけ）と、この操作卓の指紋
#[tauri::command]
pub async fn agent_peers() -> R {
    let d = dir();
    let me = DeviceKeys::load(&d).ok().map(|k| k.fingerprint());
    let peers = PeerStore::new(&d).list().map_err(|e| e.to_string())?;
    Ok(json!({ "me": me, "peers": peers.iter().filter(|p| p.role == Role::Agent).map(Peer::summary).collect::<Vec<_>>() }))
}

/// コードでペアリングする。addr は tune-agent pair の待ち受け（`host` か `host:port`）
#[tauri::command]
pub async fn agent_pair(addr: String, code: String) -> R {
    let code = zeroize::Zeroizing::new(code);
    let Some(c) = Code::parse(&code) else {
        return Ok(json!({ "ok": false, "error": "コードは 6 桁の数字です" }));
    };
    match client::pair(&addr, &c, &dir(), &client::console_name(&nodes::local_host())).await {
        Ok(p) => Ok(json!({ "ok": true, "peer": p.summary() })),
        Err(e) => Ok(json!({ "ok": false, "error": e.to_string() })),
    }
}
