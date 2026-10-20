//! ペア済みの相手の台帳（`peers.json`）。持つのは相手の公開鍵と名刺の署名だけ（秘密は無い）。
//! 本人だけが読み書きできるファイルに置く（書き換えられると、知らない鍵を受け付けてしまうため）。
//! 使うたびに名刺の署名を確かめ直し、壊れた項目は使わない。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::keys::{self, Card, Verified};
use crate::{Error, Result};

pub const PEERS_FILE: &str = "peers.json";
const LOCK_FILE: &str = "peers.lock";
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

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
        plain_metadata(&self.dir, true)?;
        let p = self.path();
        if plain_metadata(&p, false)?.is_none() {
            return Ok(Vec::new());
        }
        keys::check_private(&p)?;
        let text = std::fs::read(&p).map_err(|e| Error::Store(format!("台帳を読めない（{}）: {e}", p.display())))?;
        let f: File = serde_json::from_slice(&text).map_err(|e| Error::Store(format!("台帳の形が違う（{}）: {e}", p.display())))?;
        let peers = f.peers.into_iter().filter(|x| x.verified().is_ok()).collect();
        #[cfg(test)]
        tests::after_read(&self.dir);
        Ok(peers)
    }

    /// 足す。同じ機体鍵の相手は置き換える（ペアリングのやり直し）
    pub fn add(&self, peer: Peer) -> Result<()> {
        let v = peer.verified()?;
        plain_metadata(&self.dir, true)?;
        keys::ensure_private_dir(&self.dir)?;
        let _lock = self.write_lock(LOCK_TIMEOUT)?;
        let mut peers = self.list()?;
        peers.retain(|x| x.verified().map(|w| w.identity != v.identity).unwrap_or(false));
        peers.push(peer);
        let body = serde_json::to_vec_pretty(&File { peers }).map_err(|e| Error::Store(e.to_string()))?;
        keys::write_private(&self.path(), &body, false)
    }

    /// 名前・指紋・つなぎ先が 1 件に決まるときだけ消す。曖昧なら変更せずエラー。消した数
    pub fn remove(&self, query: &str) -> Result<usize> {
        // A no-op/ambiguous request must not even create a sidecar. This first
        // read is only a preflight; the decision is repeated under the lock.
        if removal_index(&self.list()?, query)?.is_none() {
            return Ok(0);
        }
        let _lock = self.write_lock(LOCK_TIMEOUT)?;
        let mut peers = self.list()?;
        let Some(index) = removal_index(&peers, query)? else { return Ok(0) };
        peers.remove(index);
        let body = serde_json::to_vec_pretty(&File { peers }).map_err(|e| Error::Store(e.to_string()))?;
        keys::write_private(&self.path(), &body, false)?;
        Ok(1)
    }

    /// Never unlink/replace this sidecar: locking peers.json itself would split
    /// the lock when write_private atomically replaces that file. Closing this
    /// un-cloned File releases the OS lock, including on errors/unwinding.
    fn write_lock(&self, timeout: Duration) -> Result<std::fs::File> {
        plain_metadata(&self.dir, true)?;
        keys::ensure_private_dir(&self.dir)?;
        let path = self.dir.join(LOCK_FILE);
        if plain_metadata(&path, false)?.is_none() {
            // Create once with the existing 0600/Windows ACL helper. Its
            // create_new hard link cannot replace another creator's sidecar.
            match keys::write_private(&path, b"", true) {
                Ok(()) => (),
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e),
            }
        }
        keys::check_private(&path)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Open the reparse point itself; keep the stable file from being
            // deleted/renamed while either a holder or a waiter has it open.
            options.custom_flags(0x0020_0000).share_mode(0x1 | 0x2);
        }
        let file = options.open(&path)?;
        let opened = file.metadata()?;
        check_plain(&opened, &path, false)?;
        if opened.len() != 0 {
            return Err(Error::Store("台帳のロックファイルが空ではないので使わない".into()));
        }
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    #[cfg(test)]
                    tests::lock_contended(&self.dir);
                    if start.elapsed() >= timeout {
                        return Err(Error::Store("台帳が別の処理で使用中（ロック取得が時間切れ）".into()));
                    }
                    std::thread::sleep(Duration::from_millis(10).min(timeout.saturating_sub(start.elapsed())));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(Error::Store(format!("台帳のロックを取得できない: {e}"))),
            }
        }
        let current = plain_metadata(&path, false)?.ok_or_else(|| Error::Store("台帳のロックファイルが消えた".into()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (opened.dev(), opened.ino()) != (current.dev(), current.ino()) {
                return Err(Error::Store("台帳のロックファイルが差し替わったので使わない".into()));
            }
        }
        let _ = current;
        keys::check_private(&path)?;
        Ok(file)
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

fn check_plain(meta: &std::fs::Metadata, path: &Path, directory: bool) -> Result<()> {
    let mut plain = if directory { meta.is_dir() } else { meta.is_file() };
    plain &= !meta.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        plain &= meta.file_attributes() & 0x0000_0400 == 0; // FILE_ATTRIBUTE_REPARSE_POINT
    }
    if !plain {
        return Err(Error::Store(format!("{} は通常の{}ではないので使わない", path.display(), if directory { "ディレクトリ" } else { "ファイル" })));
    }
    Ok(())
}

fn plain_metadata(path: &Path, directory: bool) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            check_plain(&m, path, directory)?;
            Ok(Some(m))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn removal_index(peers: &[Peer], query: &str) -> Result<Option<usize>> {
    let Some(index) = peers.iter().position(|p| matches(p, query)) else { return Ok(None) };
    if peers[index + 1..].iter().any(|p| matches(p, query)) {
        return Err(Error::Store("消す相手が 1 件に決まらない（名前・指紋・つなぎ先が複数の相手に一致）".into()));
    }
    Ok(Some(index))
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
            s.add(Peer { role: Role::Console, card: k.card(&format!("console-{i}")), paired_at: i as i64, addr: None }).unwrap();
            assert_eq!(s.list().unwrap().len(), i + 1, "2 回目以降の add も通る");
        }
        assert_eq!(s.remove("console-1").unwrap(), 1);
        assert_eq!(s.remove("console-0").unwrap(), 1);
        assert_eq!(s.list().unwrap().len(), 1);
        let mut left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        left.sort();
        assert_eq!(left, vec![std::ffi::OsString::from(PEERS_FILE), std::ffi::OsString::from(LOCK_FILE)], "恒久sidecar以外の一時ファイルを残さない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn ledger_snapshot(s: &PeerStore) -> (Vec<u8>, std::time::SystemTime) {
        // A fixed old timestamp detects even a rewrite of identical bytes.
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        std::fs::OpenOptions::new().write(true).open(s.path()).unwrap().set_modified(old).unwrap();
        (std::fs::read(s.path()).unwrap(), std::fs::metadata(s.path()).unwrap().modified().unwrap())
    }

    fn assert_ambiguous_without_write(s: &PeerStore, query: &str) {
        let before = ledger_snapshot(s);
        let peers = s.list().unwrap();
        let result = s.remove(query);
        assert!(matches!(result, Err(Error::Store(_))), "ambiguous removal must fail: {result:?}");
        assert_eq!(s.list().unwrap(), peers);
        assert_eq!(std::fs::read(s.path()).unwrap(), before.0);
        assert_eq!(std::fs::metadata(s.path()).unwrap().modified().unwrap(), before.1);
        assert_eq!(std::fs::read_dir(&s.dir).unwrap().count(), 2, "only ledger and persistent sidecar");
    }

    #[test]
    fn remove_rejects_name_fingerprint_collision_and_full_fingerprint_removes_one() {
        let dir = std::env::temp_dir().join(format!("tune-link-peer-collision-{}-{}", std::process::id(), crate::now_ms()));
        let s = PeerStore::new(&dir);
        let a = DeviceKeys::generate().unwrap();
        let b = DeviceKeys::generate().unwrap();
        let query = b.fingerprint()[..4].to_string();
        let first = Peer { role: Role::Console, card: a.card(&query), paired_at: 1, addr: None };
        let second = Peer { role: Role::Agent, card: b.card("agent-other"), paired_at: 2, addr: None };
        s.add(first.clone()).unwrap();
        s.add(second.clone()).unwrap();
        assert_eq!(s.list().unwrap().len(), 2);
        // find remains role-filtered; remove has no role selector and must reject
        // both signed peers matching the same name/fingerprint-prefix query.
        assert_eq!(s.find(Role::Console, &query).unwrap(), Some(first.clone()));
        assert_eq!(s.find(Role::Agent, &query).unwrap(), Some(second.clone()));
        assert_ambiguous_without_write(&s, &query);
        assert_ambiguous_without_write(&s, &format!("  {query}  "));
        assert_eq!(s.remove(&format!("  {}  ", b.fingerprint())).unwrap(), 1);
        assert_eq!(s.list().unwrap(), vec![first]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remove_rejects_duplicate_names_and_name_address_collisions() {
        let a = DeviceKeys::generate().unwrap();
        let b = DeviceKeys::generate().unwrap();
        for (case, name_a, name_b, addr_a, addr_b, query) in [
            ("names", "shared-console", "shared-console", None, None, "shared-console"),
            ("name-address", "127.0.0.1:1", "agent-other", None, Some("127.0.0.1:1"), "127.0.0.1:1"),
            ("addresses", "console-a", "console-b", Some("127.0.0.1:1"), Some("127.0.0.1:1"), "127.0.0.1:1"),
        ] {
            let dir = std::env::temp_dir().join(format!("tune-link-peer-{case}-{}-{}", std::process::id(), crate::now_ms()));
            let s = PeerStore::new(&dir);
            s.add(Peer { role: Role::Agent, card: a.card(name_a), paired_at: 1, addr: addr_a.map(String::from) }).unwrap();
            s.add(Peer { role: Role::Agent, card: b.card(name_b), paired_at: 2, addr: addr_b.map(String::from) }).unwrap();
            assert!(s.find(Role::Agent, query).unwrap().is_none());
            assert_ambiguous_without_write(&s, query);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn remove_rejects_shared_fingerprint_prefix_without_rewriting() {
        // Seventeen keys guarantee two fingerprints with the same first hex
        // character, without depending on a random collision or an unbounded loop.
        let keys: Vec<_> = (0..17).map(|_| DeviceKeys::generate().unwrap()).collect();
        let (i, j) =
            (0..keys.len()).find_map(|i| (i + 1..keys.len()).find(|&j| keys[i].fingerprint()[..1] == keys[j].fingerprint()[..1]).map(|j| (i, j))).unwrap();
        let dir = std::env::temp_dir().join(format!("tune-link-peer-prefix-{}-{}", std::process::id(), crate::now_ms()));
        let s = PeerStore::new(&dir);
        for (n, k) in [i, j].iter().enumerate() {
            s.add(Peer { role: Role::Console, card: keys[*k].card(&format!("console-{n}")), paired_at: n as i64, addr: None }).unwrap();
        }
        let query = keys[i].fingerprint()[..1].to_string();
        assert_eq!(s.list().unwrap().len(), 2);
        assert!(s.find(Role::Console, &query).unwrap().is_none());
        assert_ambiguous_without_write(&s, &query);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remove_empty_or_unknown_query_never_rewrites_or_creates_a_ledger() {
        let dir = std::env::temp_dir().join(format!("tune-link-peer-no-match-{}-{}", std::process::id(), crate::now_ms()));
        let s = PeerStore::new(&dir);
        for query in ["", "  ", "missing-console"] {
            assert_eq!(s.remove(query).unwrap(), 0);
            assert!(!dir.exists());
        }
        let key = DeviceKeys::generate().unwrap();
        s.add(Peer { role: Role::Console, card: key.card("console-only"), paired_at: 1, addr: None }).unwrap();
        let before = ledger_snapshot(&s);
        for query in ["", " \t\n ", "missing-console"] {
            assert_eq!(s.remove(query).unwrap(), 0);
            assert_eq!(std::fs::read(s.path()).unwrap(), before.0);
            assert_eq!(std::fs::metadata(s.path()).unwrap().modified().unwrap(), before.1);
            assert_eq!(s.list().unwrap().len(), 1);
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    struct ReadHook {
        dir: PathBuf,
        reads: usize,
        action: Box<dyn FnOnce()>,
    }
    std::thread_local! {
        static READ_HOOK: std::cell::RefCell<Option<ReadHook>> = const { std::cell::RefCell::new(None) };
        static CONTENDED: std::cell::RefCell<Option<(PathBuf, std::sync::mpsc::Sender<()>)>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn after_read(dir: &Path) {
        let action = READ_HOOK.with(|slot| {
            let mut slot = slot.borrow_mut();
            let hook = slot.as_mut()?;
            if hook.dir != dir {
                return None;
            }
            hook.reads -= 1;
            if hook.reads == 0 { slot.take().map(|h| h.action) } else { None }
        });
        if let Some(action) = action {
            action();
        }
    }

    fn set_read_hook(dir: &Path, reads: usize, action: impl FnOnce() + 'static) {
        READ_HOOK.with(|slot| *slot.borrow_mut() = Some(ReadHook { dir: dir.into(), reads, action: Box::new(action) }));
    }

    #[test]
    fn remove_reloads_after_another_store_adds_before_it_writes() {
        let dir = std::env::temp_dir().join(format!("tune-link-peer-lost-update-{}-{}", std::process::id(), crate::now_ms()));
        let store = PeerStore::new(&dir);
        for name in ["console-a", "console-b"] {
            store.add(Peer { role: Role::Console, card: DeviceKeys::generate().unwrap().card(name), paired_at: 1, addr: None }).unwrap();
        }
        let (read_tx, read_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let other_dir = dir.clone();
        let remove = std::thread::spawn(move || {
            set_read_hook(&other_dir, 1, move || {
                read_tx.send(()).unwrap();
                resume_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            });
            PeerStore::new(&other_dir).remove("console-a")
        });
        read_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let added = Peer { role: Role::Console, card: DeviceKeys::generate().unwrap().card("console-c"), paired_at: 2, addr: None };
        PeerStore::new(&dir).add(added.clone()).unwrap();
        resume_tx.send(()).unwrap();
        assert_eq!(remove.join().unwrap().unwrap(), 1);
        let peers = store.list().unwrap();
        assert_eq!(peers.len(), 2, "a successful concurrent add must not be lost by stale removal");
        assert!(peers.contains(&added));
        std::fs::remove_dir_all(dir).unwrap();
    }

    pub(super) fn lock_contended(dir: &Path) {
        CONTENDED.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(path, _)| path == dir) {
                slot.take().unwrap().1.send(()).unwrap();
            }
        });
        if std::env::var_os("TUNE_PEER_LOCK_TEST_DIR").as_deref() == Some(dir.as_os_str()) {
            let marker = dir.join("contended");
            if !marker.exists() {
                std::fs::write(marker, b"").unwrap();
            }
        }
    }

    fn temp_store(tag: &str) -> (PathBuf, PeerStore) {
        let dir = std::env::temp_dir().join(format!("tune-link-peer-concurrent-{tag}-{}-{}", std::process::id(), crate::now_ms()));
        let store = PeerStore::new(&dir);
        (dir, store)
    }

    fn peer(name: &str) -> Peer {
        Peer { role: Role::Console, card: DeviceKeys::generate().unwrap().card(name), paired_at: 1, addr: None }
    }

    fn mutate(store: &PeerStore, op: &str) -> Result<usize> {
        if let Some(name) = op.strip_prefix("add:") { store.add(peer(name)).map(|_| 1) } else { store.remove(op) }
    }

    #[test]
    fn first_concurrent_additions_create_one_private_stable_sidecar() {
        let (dir, store) = temp_store("first-add");
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let writers: Vec<_> = ["console-a", "console-b"]
            .into_iter()
            .map(|name| {
                let dir = dir.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    PeerStore::new(&dir).add(peer(name))
                })
            })
            .collect();
        start.wait();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        assert_eq!(store.list().unwrap().len(), 2);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        let lock = store.write_lock(LOCK_TIMEOUT).unwrap();
        assert_eq!(lock.metadata().unwrap().len(), 0);
        drop(lock);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn locked_writers_preserve_add_add_add_remove_and_remove_remove() {
        for (tag, first, second, expected) in [
            ("add-add", "add:console-c", "add:console-d", vec!["console-a", "console-b", "console-c", "console-d", "console-keep"]),
            ("add-remove", "add:console-c", "console-b", vec!["console-a", "console-c", "console-keep"]),
            ("remove-remove", "console-a", "console-b", vec!["console-keep"]),
        ] {
            let (dir, store) = temp_store(tag);
            for name in ["console-a", "console-b", "console-keep"] {
                store.add(peer(name)).unwrap();
            }
            let (read_tx, read_rx) = std::sync::mpsc::channel();
            let (resume_tx, resume_rx) = std::sync::mpsc::channel();
            let (busy_tx, busy_rx) = std::sync::mpsc::channel();
            let first_dir = dir.clone();
            let a = std::thread::spawn(move || {
                set_read_hook(&first_dir, if first.starts_with("add:") { 1 } else { 2 }, move || {
                    read_tx.send(()).unwrap();
                    resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                });
                mutate(&PeerStore::new(&first_dir), first)
            });
            read_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let second_dir = dir.clone();
            let b = std::thread::spawn(move || {
                CONTENDED.with(|slot| *slot.borrow_mut() = Some((second_dir.clone(), busy_tx)));
                mutate(&PeerStore::new(&second_dir), second)
            });
            busy_rx.recv_timeout(Duration::from_secs(5)).unwrap(); // actually reached WouldBlock
            resume_tx.send(()).unwrap();
            assert_eq!(a.join().unwrap().unwrap(), 1);
            assert_eq!(b.join().unwrap().unwrap(), 1);
            let mut names: Vec<_> = store.list().unwrap().iter().map(|p| p.name().to_string()).collect();
            names.sort();
            assert_eq!(names, expected, "{tag}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn removal_rechecks_zero_ambiguity_and_signatures_after_locking() {
        for case in ["zero", "ambiguous", "signature"] {
            let (dir, store) = temp_store(case);
            store.add(peer("console-target")).unwrap();
            let (read_tx, read_rx) = std::sync::mpsc::channel();
            let (resume_tx, resume_rx) = std::sync::mpsc::channel();
            let actor_dir = dir.clone();
            let actor = std::thread::spawn(move || {
                set_read_hook(&actor_dir, 1, move || {
                    read_tx.send(()).unwrap();
                    resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                });
                PeerStore::new(&actor_dir).remove("console-target")
            });
            read_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            match case {
                "zero" => {
                    assert_eq!(store.remove("console-target").unwrap(), 1);
                }
                "ambiguous" => {
                    store.add(peer("console-target")).unwrap();
                }
                _ => {
                    let _lock = store.write_lock(LOCK_TIMEOUT).unwrap();
                    let mut peers = store.list().unwrap();
                    peers[0].card.name = "bad-signature".into();
                    keys::write_private(&store.path(), &serde_json::to_vec(&File { peers }).unwrap(), false).unwrap();
                }
            }
            let before = ledger_snapshot(&store);
            resume_tx.send(()).unwrap();
            let result = actor.join().unwrap();
            if case == "ambiguous" {
                assert!(matches!(result, Err(Error::Store(_))));
            } else {
                assert_eq!(result.unwrap(), 0);
            }
            assert_eq!(std::fs::read(store.path()).unwrap(), before.0);
            assert_eq!(std::fs::metadata(store.path()).unwrap().modified().unwrap(), before.1);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn reads_noops_and_ambiguous_removal_do_not_create_sidecar() {
        let (dir, store) = temp_store("read-only");
        keys::ensure_private_dir(&dir).unwrap();
        let peers = vec![peer("console-shared"), peer("console-shared")];
        keys::write_private(&store.path(), &serde_json::to_vec(&File { peers }).unwrap(), false).unwrap();
        let before = ledger_snapshot(&store);
        assert_eq!(store.list().unwrap().len(), 2);
        assert!(store.find(Role::Console, "console-shared").unwrap().is_none());
        assert!(store.find_static(Role::Console, &[0; 32]).unwrap().is_none());
        for q in ["", "  ", "unknown-console"] {
            assert_eq!(store.remove(q).unwrap(), 0);
        }
        assert!(store.remove("console-shared").is_err());
        assert!(!dir.join(LOCK_FILE).exists());
        assert_eq!(std::fs::read(store.path()).unwrap(), before.0);
        assert_eq!(std::fs::metadata(store.path()).unwrap().modified().unwrap(), before.1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn wait_marker(dir: &Path, marker: &str) {
        let start = Instant::now();
        while !dir.join(marker).exists() {
            assert!(start.elapsed() < Duration::from_secs(10), "marker {marker} did not arrive");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn child(dir: &Path, mode: &str) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "peers::tests::lock_process_child", "--ignored"])
            .env("TUNE_PEER_LOCK_TEST_DIR", dir)
            .env("TUNE_PEER_LOCK_TEST_MODE", mode)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    fn join_child(child: &mut std::process::Child) {
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            if start.elapsed() >= Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned test child timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "worker invoked by cross-process lock tests"]
    fn lock_process_child() {
        let dir = PathBuf::from(std::env::var_os("TUNE_PEER_LOCK_TEST_DIR").unwrap());
        assert!(dir.file_name().unwrap().to_string_lossy().starts_with("tune-link-peer-concurrent-"));
        let store = PeerStore::new(&dir);
        if std::env::var("TUNE_PEER_LOCK_TEST_MODE").unwrap() == "hold" {
            let _lock = store.write_lock(LOCK_TIMEOUT).unwrap();
            std::fs::write(dir.join("ready"), b"").unwrap();
            wait_marker(&dir, "release");
        } else {
            store.add(peer("console-child")).unwrap();
        }
    }

    #[test]
    fn cross_process_lock_times_out_without_writing_and_releases_on_drop() {
        let (dir, store) = temp_store("process-hold");
        store.add(peer("console-a")).unwrap();
        let before = ledger_snapshot(&store);
        let mut child = child(&dir, "hold");
        wait_marker(&dir, "ready");
        for op in ["add:console-b", "console-a"] {
            let start = Instant::now();
            let result = mutate(&store, op);
            let elapsed = start.elapsed();
            assert!(matches!(&result, Err(Error::Store(s)) if s.contains("時間切れ")), "{result:?}");
            assert!(elapsed >= LOCK_TIMEOUT && elapsed < Duration::from_secs(5));
            eprintln!("lock timeout {op}: {} ms", elapsed.as_millis());
            assert_eq!(std::fs::read(store.path()).unwrap(), before.0);
            assert_eq!(std::fs::metadata(store.path()).unwrap().modified().unwrap(), before.1);
        }
        std::fs::write(dir.join("release"), b"").unwrap();
        join_child(&mut child);
        drop(store.write_lock(Duration::from_millis(30)).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cross_process_add_waits_and_preserves_both_successful_additions() {
        let (dir, store) = temp_store("process-add");
        store.add(peer("console-a")).unwrap();
        let lock = store.write_lock(LOCK_TIMEOUT).unwrap();
        let mut child = child(&dir, "add");
        wait_marker(&dir, "contended");
        assert!(child.try_wait().unwrap().is_none());
        drop(lock);
        store.add(peer("console-parent")).unwrap();
        join_child(&mut child);
        let mut names: Vec<_> = store.list().unwrap().iter().map(|p| p.name().to_string()).collect();
        names.sort();
        assert_eq!(names, ["console-a", "console-child", "console-parent"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lock_drops_on_error_and_unwinding_and_keeps_its_inode() {
        let (dir, store) = temp_store("drop");
        store.add(peer("console-a")).unwrap();
        let path = dir.join(LOCK_FILE);
        let before = std::fs::metadata(&path).unwrap();
        store.add(peer("console-b")).unwrap();
        assert_eq!(store.remove("console-b").unwrap(), 1);
        keys::write_private(&store.path(), b"{invalid", false).unwrap();
        assert!(store.add(peer("console-b")).is_err());
        drop(store.write_lock(Duration::from_millis(30)).unwrap());
        let result = std::panic::catch_unwind(|| {
            let _lock = store.write_lock(LOCK_TIMEOUT).unwrap();
            panic!("test unwind");
        });
        assert!(result.is_err());
        drop(store.write_lock(Duration::from_millis(30)).unwrap());
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(after.len(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
            assert_eq!(after.permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let _ = before;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_private_paths_are_rejected_without_touching_targets() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for case in ["lock-symlink", "ledger-symlink", "directory-symlink", "lock-directory", "lock-permissions", "dir-permissions", "lock-content"] {
            let (dir, store) = temp_store(case);
            keys::ensure_private_dir(&dir).unwrap();
            let target = dir.join("target");
            let content: &[u8] = if case == "ledger-symlink" { b"{\"peers\":[]}" } else { b"" };
            keys::write_private(&target, content, true).unwrap();
            let before = std::fs::read(&target).unwrap();
            let modified = std::fs::metadata(&target).unwrap().modified().unwrap();
            match case {
                "lock-symlink" => symlink(&target, dir.join(LOCK_FILE)).unwrap(),
                "ledger-symlink" => symlink(&target, store.path()).unwrap(),
                "lock-directory" => std::fs::create_dir(dir.join(LOCK_FILE)).unwrap(),
                "lock-permissions" => {
                    std::fs::write(dir.join(LOCK_FILE), b"").unwrap();
                    std::fs::set_permissions(dir.join(LOCK_FILE), std::fs::Permissions::from_mode(0o644)).unwrap();
                }
                "dir-permissions" => std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap(),
                "lock-content" => keys::write_private(&dir.join(LOCK_FILE), b"foreign", true).unwrap(),
                _ => {
                    let alias = dir.with_extension("alias");
                    symlink(&dir, &alias).unwrap();
                    assert!(PeerStore::new(&alias).add(peer("console-b")).is_err());
                    std::fs::remove_file(alias).unwrap();
                    assert!(!store.path().exists());
                    std::fs::remove_dir_all(dir).unwrap();
                    continue;
                }
            }
            assert!(store.add(peer("console-b")).is_err(), "{case}");
            assert_eq!(std::fs::read(&target).unwrap(), before, "{case}");
            assert_eq!(std::fs::metadata(&target).unwrap().modified().unwrap(), modified, "{case}");
            if case != "ledger-symlink" {
                assert!(!store.path().exists(), "{case}");
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    fn hex_of(b: &[u8]) -> String {
        crate::hex::encode(b)
    }
}
