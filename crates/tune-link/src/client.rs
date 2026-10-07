//! 操作卓の側（Katala Tune・tune-cli）の TCP での呼び出し。鍵と台帳は `dir`（[`crate::console_dir`]）に置く。

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::channel::{self, Secure};
use crate::frame::Framed;
use crate::keys::DeviceKeys;
use crate::pair::{self, Code};
use crate::peers::{Peer, PeerStore, Role};
use crate::{Error, Result};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 操作卓の名刺の名前（この機体のホスト名は呼び出し側が渡す）
pub fn console_name(host: &str) -> String {
    format!("Katala Tune ({host})")
}

/// `host`・`host:port`・`[v6]:port` を `host:port` に。ホストの部分も返す
pub fn split_addr(s: &str, default_port: u16) -> Result<(String, u16)> {
    let s = s.trim();
    let bad = || Error::Protocol(format!("アドレスの形が違う: {s}"));
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == '/') {
        return Err(bad());
    }
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or_else(bad)?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| bad())?,
            None if tail.is_empty() => default_port,
            None => return Err(bad()),
        };
        return Ok((format!("[{host}]"), port));
    }
    match s.rsplit_once(':') {
        // コロンが 1 つだけなら host:port。2 つ以上は角括弧なしの IPv6
        Some((h, p)) if !h.contains(':') => Ok((h.to_string(), p.parse().map_err(|_| bad())?)),
        Some(_) => Ok((format!("[{s}]"), default_port)),
        None => Ok((s.to_string(), default_port)),
    }
}

async fn tcp(addr: &str) -> Result<Framed<TcpStream>> {
    let s = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await.map_err(|_| Error::Timeout)??;
    let _ = s.set_nodelay(true);
    Ok(Framed::new(s))
}

/// コードでペアリングし、相手（tune-agent）を台帳に足す。`addr` は `tune-agent pair` の待ち受け
pub async fn pair(addr: &str, code: &Code, dir: &Path, my_name: &str) -> Result<Peer> {
    let (host, port) = split_addr(addr, crate::PAIR_PORT)?;
    let mut t = tcp(&format!("{host}:{port}")).await?;
    let (me, _) = DeviceKeys::load_or_create(dir)?;
    let p = pair::initiate(&mut t, code, &me, my_name).await?;
    let peer = Peer { role: Role::Agent, card: p.card, paired_at: crate::now_ms(), addr: Some(format!("{host}:{}", p.run_port)) };
    PeerStore::new(dir).add(peer.clone())?;
    Ok(peer)
}

/// ペア済みの tune-agent につなぐ
pub async fn connect(peer: &Peer, dir: &Path, my_name: &str) -> Result<Secure<Framed<TcpStream>>> {
    let addr = peer.addr.as_deref().ok_or_else(|| Error::Protocol("つなぎ先が台帳に無い".into()))?;
    let agent = peer.verified()?;
    let me = DeviceKeys::load(dir)?;
    let t = tcp(addr).await?;
    timeout(CONNECT_TIMEOUT, channel::connect(t, &me, my_name, &agent)).await.map_err(|_| Error::Timeout)?
}

/// 1 回だけ要求して閉じる
pub async fn call(peer: &Peer, dir: &Path, my_name: &str, op: &str, args: Value, limit: Duration) -> Result<Value> {
    let mut s = connect(peer, dir, my_name).await?;
    timeout(limit, s.call(1, op, args)).await.map_err(|_| Error::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(split_addr("127.0.0.1", 1).unwrap(), ("127.0.0.1".into(), 1));
        assert_eq!(split_addr("127.0.0.1:2", 1).unwrap(), ("127.0.0.1".into(), 2));
        assert_eq!(split_addr("[::1]:3", 1).unwrap(), ("[::1]".into(), 3));
        assert_eq!(split_addr("[::1]", 1).unwrap(), ("[::1]".into(), 1));
        assert_eq!(split_addr("::1", 1).unwrap(), ("[::1]".into(), 1));
        assert_eq!(split_addr("host.example:4", 1).unwrap(), ("host.example".into(), 4));
        assert!(split_addr("", 1).is_err());
        assert!(split_addr("a b", 1).is_err());
        assert!(split_addr("h:x", 1).is_err());
    }
}
