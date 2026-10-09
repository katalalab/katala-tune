//! ペア済みの相手の台帳（`peers.json`）。持つのは相手の公開鍵と名刺の署名だけ（秘密は無い）。
//! 本人だけが読み書きできるファイルに置く（書き換えられると、知らない鍵を受け付けてしまうため）。
//! 使うたびに名刺の署名を確かめ直し、壊れた項目は使わない。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::keys::{self, Card, Verified};
use crate::{Error, Result};

pub const PEERS_FILE: &str = "peers.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 操作卓（Katala Tune・tune-cli）。tune-agent が受け付ける相手
    Console,
    /// 各機体の tune-agent。操作卓がつなぎに行く相手
    Agent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub role: Role,
    pub card: Card,
    /// ペアリングした時刻（Unix 時刻のミリ秒）
    pub paired_at: i64,
    /// つなぎ先（操作卓の台帳だけ。`host:port`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
}

impl Peer {
    pub fn verified(&self) -> Result<Verified> {
        self.card.verify()
    }

    pub fn name(&self) -> &str {
        &self.card.name
    }

    pub fn fingerprint(&self) -> String {
        self.card.fingerprint()
    }

    /// 画面・status に出す形（公開鍵の指紋だけ）
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.card.name,
            "role": self.role,
            "fingerprint": self.fingerprint(),
            "paired_at": self.paired_at,
            "addr": self.addr,
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    peers: Vec<Peer>,
}

pub struct PeerStore {
    dir: PathBuf,
}

impl PeerStore {
    pub fn new(dir: &Path) -> PeerStore {
        PeerStore { dir: dir.to_path_buf() }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(PEERS_FILE)
    }

    /// 全部（無ければ空）。署名が合わない項目は除く
    pub fn list(&self) -> Result<Vec<Peer>> {
        let p = self.path();
        if !p.exists() {
            return Ok(Vec::new());
        }
        keys::check_private(&p)?;
        let text = std::fs::read(&p).map_err(|e| Error::Store(format!("台帳を読めない（{}）: {e}", p.display())))?;
        let f: File = serde_json::from_slice(&text).map_err(|e| Error::Store(format!("台帳の形が違う（{}）: {e}", p.display())))?;
        Ok(f.peers.into_iter().filter(|x| x.verified().is_ok()).collect())
    }

    /// 足す。同じ機体鍵の相手は置き換える（ペアリングのやり直し）
    pub fn add(&self, peer: Peer) -> Result<()> {
        let v = peer.verified()?;
        keys::ensure_private_dir(&self.dir)?;
        let mut peers = self.list()?;
        peers.retain(|x| x.verified().map(|w| w.identity != v.identity).unwrap_or(false));
        peers.push(peer);
        let body = serde_json::to_vec_pretty(&File { peers }).map_err(|e| Error::Store(e.to_string()))?;
        keys::write_private(&self.path(), &body, false)
    }

    /// 消す（機体鍵の指紋か名前で）。消した数
    pub fn remove(&self, query: &str) -> Result<usize> {
        let peers = self.list()?;
        let n = peers.len();
        let keep: Vec<Peer> = peers.into_iter().filter(|p| !matches(p, query)).collect();
        let removed = n - keep.len();
        if removed > 0 {
            let body = serde_json::to_vec_pretty(&File { peers: keep }).map_err(|e| Error::Store(e.to_string()))?;
            keys::write_private(&self.path(), &body, false)?;
        }
        Ok(removed)
    }

    /// Noise の静的鍵で探す（tune-agent が接続を受けたとき）
    pub fn find_static(&self, role: Role, static_key: &[u8; 32]) -> Result<Option<(Peer, Verified)>> {
        Ok(self.list()?.into_iter().filter(|p| p.role == role).find_map(|p| {
            let v = p.verified().ok()?;
            (v.static_key == *static_key).then_some((p, v))
        }))
    }

    /// 名前・指紋（先頭だけでもよい）・つなぎ先で探す。1 つに決まらなければ None
    pub fn find(&self, role: Role, query: &str) -> Result<Option<Peer>> {
        let hits: Vec<Peer> = self.list()?.into_iter().filter(|p| p.role == role && matches(p, query)).collect();
        Ok(if hits.len() == 1 { hits.into_iter().next() } else { None })
    }
}

fn matches(p: &Peer, q: &str) -> bool {
    let q = q.trim();
    !q.is_empty() && (p.card.name == q || p.fingerprint().starts_with(q) || p.addr.as_deref() == Some(q))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::DeviceKeys;

    #[test]
    fn add_replace_find_remove() {
        let dir = std::env::temp_dir().join(format!("tune-link-peers-{}-{}", std::process::id(), crate::now_ms()));
        let s = PeerStore::new(&dir);
        assert!(s.list().unwrap().is_empty());
        let a = DeviceKeys::generate().unwrap();
        let b = DeviceKeys::generate().unwrap();
        s.add(Peer { role: Role::Console, card: a.card("a"), paired_at: 1, addr: None }).unwrap();
        s.add(Peer { role: Role::Agent, card: b.card("b"), paired_at: 2, addr: Some("127.0.0.1:1".into()) }).unwrap();
        // 同じ機体鍵は置き換え
        s.add(Peer { role: Role::Console, card: a.card("a2"), paired_at: 3, addr: None }).unwrap();
        let all = s.list().unwrap();
        assert_eq!(all.len(), 2);
        assert!(s.find_static(Role::Console, &a.static_public()).unwrap().is_some());
        assert!(s.find_static(Role::Agent, &a.static_public()).unwrap().is_none(), "役割が違えば使わない");
        assert_eq!(s.find(Role::Agent, "127.0.0.1:1").unwrap().map(|p| p.paired_at), Some(2));
        assert_eq!(s.find(Role::Console, &a.fingerprint()[..9]).unwrap().map(|p| p.card.name), Some("a2".into()));
        // 署名の合わない項目は使わない
        let raw = std::fs::read_to_string(dir.join(PEERS_FILE)).unwrap();
        let tampered = raw.replacen(&hex_of(&b.static_public()), &hex_of(&a.static_public()), 1);
        crate::keys::write_private(&dir.join(PEERS_FILE), tampered.as_bytes(), false).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
        assert_eq!(s.remove("a2").unwrap(), 1);
        assert!(s.list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 台帳を作ったあとの add・remove は、既にあるファイルを置き換える（Windows の CI でも通る。`std::fs::rename` は
    /// Windows でも既存の行き先を置き換える）。途中の一時ファイルを残さない
    #[test]
    fn repeated_add_and_remove_replace_the_existing_ledger() {
        let dir = std::env::temp_dir().join(format!("tune-link-peers-replace-{}-{}", std::process::id(), crate::now_ms()));
        let s = PeerStore::new(&dir);
        let keys: Vec<DeviceKeys> = (0..3).map(|_| DeviceKeys::generate().unwrap()).collect();
        for (i, k) in keys.iter().enumerate() {
            s.add(Peer { role: Role::Console, card: k.card(&format!("c{i}")), paired_at: i as i64, addr: None }).unwrap();
            assert_eq!(s.list().unwrap().len(), i + 1, "2 回目以降の add も通る");
        }
        assert_eq!(s.remove("c1").unwrap(), 1);
        assert_eq!(s.remove("c0").unwrap(), 1);
        assert_eq!(s.list().unwrap().len(), 1);
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from(PEERS_FILE)], "一時ファイルを残さない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn hex_of(b: &[u8]) -> String {
        crate::hex::encode(b)
    }
}
