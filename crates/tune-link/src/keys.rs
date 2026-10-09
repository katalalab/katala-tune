//! 機体鍵。機体ごとに 2 つの鍵ペアを作り、秘密鍵は機体の外に出さない。
//!
//! | 鍵 | 使い道 |
//! |---|---|
//! | 機体鍵（Ed25519 の署名鍵） | 機体の身元。自分の Noise の静的鍵に署名して「この静的鍵は私のもの」を示す（名刺 [`Card`]）。署名にしか使わない |
//! | Noise の静的鍵（X25519） | 接続ごとの Noise の鍵交換で、相手に自分を確かめさせる。機体鍵の署名が付いたものだけを相手は受け付ける |
//!
//! Ed25519 の鍵を X25519 に変換して流用はしない（役割を分ける。理由は docs/connectivity.md）。
//! 通信を暗号化するセッション鍵は、接続ごとに作る使い捨ての X25519 の鍵（Noise の ephemeral）から作る。
//!
//! 置き場所は本人だけが読めるファイル（Unix は 0600・ディレクトリ 0700、Windows は継承を切って本人だけに許可）。
//! 読み込むときに他のユーザーが読める状態・自分の所有でない（Unix）なら、使わずに止める（ssh と同じ考え方）。
//! Windows は新しく作るファイルの ACL を絞るだけで、ディレクトリ自体の ACL・既存のファイルの ACL は確かめない（未検証。docs/connectivity.md）。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use curve25519_dalek::MontgomeryPoint;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{Error, Result, hex};

pub const KEY_FILE: &str = "device.key";
/// ファイルの形: 先頭 8 バイトの目印 ＋ Ed25519 の秘密鍵（32）＋ X25519 の秘密鍵（32）
const MAGIC: &[u8; 8] = b"KTLINK1\n";
const KEY_FILE_LEN: usize = 8 + 32 + 32;
/// 名刺の署名の対象の先頭（他の用途の署名と取り違えないための区切り）
const BIND_CONTEXT: &[u8] = b"katala-tune/link/v1 static-key\0";
/// 名刺の名前の長さの上限（文字数）
pub const MAX_NAME: usize = 64;

pub struct DeviceKeys {
    signing: SigningKey,
    static_secret: Zeroizing<[u8; 32]>,
    static_public: [u8; 32],
}

/// 秘密鍵を Debug に出さない（指紋だけ）
impl std::fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKeys").field("fingerprint", &self.fingerprint()).finish_non_exhaustive()
    }
}

impl DeviceKeys {
    /// OS の乱数で新しく作る
    pub fn generate() -> Result<DeviceKeys> {
        let mut seed = Zeroizing::new([0u8; 32]);
        let mut x = Zeroizing::new([0u8; 32]);
        OsRng.try_fill_bytes(&mut *seed).map_err(|e| Error::Store(format!("乱数を作れない: {e}")))?;
        OsRng.try_fill_bytes(&mut *x).map_err(|e| Error::Store(format!("乱数を作れない: {e}")))?;
        Ok(DeviceKeys::from_secrets(&seed, &x))
    }

    fn from_secrets(seed: &[u8; 32], x: &[u8; 32]) -> DeviceKeys {
        let signing = SigningKey::from_bytes(seed);
        // X25519 の公開鍵 = clamp した秘密鍵 × 基点（RFC 7748。snow の 25519 と同じ計算）
        let static_public = MontgomeryPoint::mul_base_clamped(*x).to_bytes();
        DeviceKeys { signing, static_secret: Zeroizing::new(*x), static_public }
    }

    /// 機体鍵の公開鍵（Ed25519）
    pub fn identity(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Noise の静的鍵の公開鍵（X25519）
    pub fn static_public(&self) -> [u8; 32] {
        self.static_public
    }

    /// Noise に渡すときだけ使う（クレートの外には出さない）
    pub(crate) fn static_secret(&self) -> &[u8; 32] {
        &self.static_secret
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.identity())
    }

    /// 名刺（公開鍵 2 つと、静的鍵への機体鍵の署名）。秘密は含まない
    pub fn card(&self, name: &str) -> Card {
        let name = clean_name(name);
        let sig: Signature = self.signing.sign(&binding(&self.static_public, &name));
        Card { name, identity: hex::encode(&self.identity()), static_key: hex::encode(&self.static_public), sig: hex::encode(&sig.to_bytes()) }
    }

    /// `dir/device.key` を読む。無ければ作る（作ったら true）
    pub fn load_or_create(dir: &Path) -> Result<(DeviceKeys, bool)> {
        let path = dir.join(KEY_FILE);
        if path.exists() {
            return Ok((DeviceKeys::load(dir)?, false));
        }
        ensure_private_dir(dir)?;
        let keys = DeviceKeys::generate()?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(KEY_FILE_LEN));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(keys.signing.as_bytes());
        bytes.extend_from_slice(&*keys.static_secret);
        match write_private(&path, &bytes, true) {
            Ok(()) => Ok((keys, true)),
            // 同時に作られた（pair と run を同時に起動したなど）。先にできた方を使う
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok((DeviceKeys::load(dir)?, false)),
            Err(e) => Err(e),
        }
    }

    /// `dir/device.key` を読む。他のユーザーが読める・形が違うなら使わない
    pub fn load(dir: &Path) -> Result<DeviceKeys> {
        let path = dir.join(KEY_FILE);
        check_private(&path)?;
        let bytes = Zeroizing::new(fs::read(&path).map_err(|e| Error::Store(format!("機体鍵を読めない（{}）: {e}", path.display())))?);
        if bytes.len() != KEY_FILE_LEN || &bytes[..8] != MAGIC {
            return Err(Error::Store(format!("機体鍵のファイルの形が違う: {}", path.display())));
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        let mut x = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(&bytes[8..40]);
        x.copy_from_slice(&bytes[40..72]);
        Ok(DeviceKeys::from_secrets(&seed, &x))
    }
}

/// 公開鍵の指紋（SHA-256 の先頭 10 バイト）。画面とログで機体を見分けるためだけに使う
pub fn fingerprint(identity: &[u8; 32]) -> String {
    let d = Sha256::digest(identity);
    let h = hex::encode(&d[..10]);
    h.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join("-")
}

fn binding(static_public: &[u8; 32], name: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(BIND_CONTEXT.len() + 32 + name.len());
    m.extend_from_slice(BIND_CONTEXT);
    m.extend_from_slice(static_public);
    m.extend_from_slice(name.as_bytes());
    m
}

/// 名前は表示にだけ使う。制御文字を除き、長さを抑える
pub fn clean_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect::<String>().trim().to_string()
}

/// 名刺: 機体鍵の公開鍵（identity）・Noise の静的鍵の公開鍵（static_key）・静的鍵と名前への機体鍵の署名（sig）。どれも 16 進
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub name: String,
    pub identity: String,
    pub static_key: String,
    pub sig: String,
}

/// 署名を確かめた名刺
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub name: String,
    pub identity: [u8; 32],
    pub static_key: [u8; 32],
}

impl Card {
    /// 署名を確かめる（Ed25519 の strict な検証。小さい位数の鍵などを拒む）
    pub fn verify(&self) -> Result<Verified> {
        let bad = |w: &str| Error::Protocol(format!("名刺の{w}が正しくない"));
        if self.name != clean_name(&self.name) {
            return Err(bad("名前"));
        }
        let identity = hex::decode_array::<32>(&self.identity).ok_or_else(|| bad("機体鍵"))?;
        let static_key = hex::decode_array::<32>(&self.static_key).ok_or_else(|| bad("静的鍵"))?;
        let sig = hex::decode_array::<64>(&self.sig).ok_or_else(|| bad("署名"))?;
        let vk = VerifyingKey::from_bytes(&identity).map_err(|_| bad("機体鍵"))?;
        vk.verify_strict(&binding(&static_key, &self.name), &Signature::from_bytes(&sig)).map_err(|_| bad("署名"))?;
        Ok(Verified { name: self.name.clone(), identity, static_key })
    }

    pub fn fingerprint(&self) -> String {
        hex::decode_array::<32>(&self.identity).map(|k| fingerprint(&k)).unwrap_or_default()
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_json(b: &[u8]) -> Result<Card> {
        serde_json::from_slice(b).map_err(|e| Error::Protocol(format!("名刺を読めない: {e}")))
    }
}

// ---------------------------------------------------------------------------
// 本人だけが読めるファイル
// ---------------------------------------------------------------------------

/// ディレクトリを作る（Unix は 0700）。既にあって他のユーザーが読めるなら止める（勝手に権限を変えない）
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !dir.exists() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| Error::Store(format!("{} を作れない: {e}", dir.display())))?;
        }
        let meta = fs::metadata(dir).map_err(|e| Error::Store(format!("{} を読めない: {e}", dir.display())))?;
        if !meta.is_dir() {
            return Err(Error::Store(format!("{} はディレクトリではない", dir.display())));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(Error::Store(format!("{} を他のユーザーも開ける（chmod 700 にしてください）", dir.display())));
        }
        check_owner(&meta, dir)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir).map_err(|e| Error::Store(format!("{} を作れない: {e}", dir.display())))?;
    Ok(())
}

/// 本人だけが読めるファイルとして書く。一時ファイルに書いてから置き換える（途中で止まっても壊れたファイルを残さない）。
/// `create_new` なら、既にあるときは AlreadyExists で失敗する（上書きしない）
pub fn write_private(path: &Path, bytes: &[u8], create_new: bool) -> Result<()> {
    let tmp = tmp_path(path);
    let r = (|| -> Result<()> {
        let mut o = fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        #[cfg(windows)]
        restrict_acl(&tmp)?;
        if create_new {
            // hard_link は行き先があれば失敗する（上書きしない）
            fs::hard_link(&tmp, path)?;
        } else {
            fs::rename(&tmp, path)?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&tmp);
    r
}

fn tmp_path(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.{nanos}.tmp", std::process::id()))
}

/// 他のユーザーが読める・自分の所有でないファイルは使わない（Unix。通常のファイルだけ）。
/// Windows は作るときに ACL を絞るだけで、既存のファイルの ACL は確かめない（docs/connectivity.md の既知の制約）
pub fn check_private(path: &Path) -> Result<()> {
    let meta = fs::metadata(path).map_err(|e| Error::Store(format!("{} を読めない: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !meta.is_file() {
            return Err(Error::Store(format!("{} は通常のファイルではない", path.display())));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(Error::Store(format!("{} を他のユーザーも読める状態なので使わない（chmod 600 にしてください）", path.display())));
        }
        check_owner(&meta, path)?;
    }
    let _ = meta;
    Ok(())
}

/// 追記で開いた既存のファイル（ログなど）を本人だけのものにする（Unix）。自分の所有でなければ断り、
/// 他のユーザーが読める状態なら 0600 に直す（ログは自分が作ったものなので、鍵のように止めずに直す）。Windows では何もしない
pub fn secure_existing_file(file: &fs::File, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = file.metadata().map_err(|e| Error::Store(format!("{} を読めない: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(Error::Store(format!("{} は通常のファイルではない", path.display())));
        }
        check_owner(&meta, path)?;
        if meta.permissions().mode() & 0o077 != 0 {
            file.set_permissions(fs::Permissions::from_mode(0o600)).map_err(|e| Error::Store(format!("{} の権限を 0600 に直せない: {e}", path.display())))?;
        }
    }
    let _ = (file, path);
    Ok(())
}

/// 自分の所有か（Unix）。std に euid を返す関数が無いので、作ったばかりの一時ファイルの所有者と比べる
#[cfg(unix)]
fn check_owner(meta: &fs::Metadata, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    match current_uid() {
        Some(me) if meta.uid() != me => Err(Error::Store(format!("{} は別のユーザーの所有なので使わない", path.display()))),
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    static UID: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *UID.get_or_init(|| {
        let probe = tmp_path(&std::env::temp_dir().join("tune-link-uid"));
        let f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&probe).ok()?;
        let uid = f.metadata().ok().map(|m| m.uid());
        drop(f);
        let _ = fs::remove_file(&probe);
        uid
    })
}

/// Windows: 継承した権限を外し、いまのユーザーだけに許可する（icacls。引数はパスとユーザー名だけ）
#[cfg(windows)]
fn restrict_acl(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) if !d.is_empty() && !u.is_empty() => format!("{d}\\{u}"),
        (_, Ok(u)) if !u.is_empty() => u,
        _ => return Err(Error::Store("Windows のユーザー名が分からないので、ファイルの権限を絞れない".into())),
    };
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    let st = std::process::Command::new(root.join("System32").join("icacls.exe"))
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:F"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()?;
    if !st.success() {
        return Err(Error::Store(format!("ファイルの権限を本人だけに絞れない（icacls が失敗）: {}", path.display())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tune-link-keys-{tag}-{}-{}", std::process::id(), crate::now_ms()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn card_binds_static_key_and_name() {
        let k = DeviceKeys::generate().unwrap();
        let c = k.card("node\u{7}-a");
        assert_eq!(c.name, "node-a", "制御文字は除く");
        let v = c.verify().unwrap();
        assert_eq!((v.identity, v.static_key), (k.identity(), k.static_public()));
        // 別の静的鍵・別の名前に差し替えると署名が合わない
        let other = DeviceKeys::generate().unwrap();
        let swapped = Card { static_key: hex::encode(&other.static_public()), ..c.clone() };
        assert!(swapped.verify().is_err());
        let renamed = Card { name: "node-b".into(), ..c.clone() };
        assert!(renamed.verify().is_err());
        let wrong_id = Card { identity: hex::encode(&other.identity()), ..c };
        assert!(wrong_id.verify().is_err());
    }

    #[test]
    fn debug_never_shows_secrets() {
        let k = DeviceKeys::generate().unwrap();
        let d = format!("{k:?}");
        assert!(d.contains(&k.fingerprint()));
        assert!(!d.contains(&hex::encode(k.signing.as_bytes())));
        assert!(!d.contains(&hex::encode(&*k.static_secret)));
        let card = serde_json::to_string(&k.card("x")).unwrap();
        assert!(!card.contains(&hex::encode(k.signing.as_bytes())));
        assert!(!card.contains(&hex::encode(&*k.static_secret)));
    }

    #[test]
    fn key_file_is_private_and_stable() {
        let dir = tmpdir("file");
        let (a, created) = DeviceKeys::load_or_create(&dir).unwrap();
        assert!(created);
        let (b, created2) = DeviceKeys::load_or_create(&dir).unwrap();
        assert!(!created2);
        assert_eq!((a.identity(), a.static_public()), (b.identity(), b.static_public()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dir.join(KEY_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            // 他のユーザーが読める状態にしたら使わない
            fs::set_permissions(dir.join(KEY_FILE), fs::Permissions::from_mode(0o644)).unwrap();
            assert!(DeviceKeys::load(&dir).is_err());
            fs::set_permissions(dir.join(KEY_FILE), fs::Permissions::from_mode(0o600)).unwrap();
            assert!(DeviceKeys::load(&dir).is_ok());
            // 緩いディレクトリも使わない（勝手に直さない）
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(ensure_private_dir(&dir).is_err());
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        // 一時ファイルを残さない
        let left: Vec<_> = fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from(KEY_FILE)]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn existing_files_are_checked_and_logs_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir("existing");
        ensure_private_dir(&dir).unwrap();
        let p = dir.join("log.jsonl");
        // 既存の緩いファイルを追記で開いても、0600 に直す
        fs::write(&p, b"old\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        let f = fs::OpenOptions::new().append(true).open(&p).unwrap();
        assert!(check_private(&p).is_err());
        secure_existing_file(&f, &p).unwrap();
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(check_private(&p).is_ok());
        // ファイルでないもの（ディレクトリ）は鍵・台帳として使わない
        fs::create_dir(dir.join("sub")).unwrap();
        assert!(check_private(&dir.join("sub")).is_err());
        // 自分の所有であること（uid が取れて、いまの所有者と同じ）
        assert!(current_uid().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_is_short_and_stable() {
        let f = fingerprint(&[7u8; 32]);
        assert_eq!(f.len(), 24);
        assert_eq!(f, fingerprint(&[7u8; 32]));
        assert_ne!(f, fingerprint(&[8u8; 32]));
    }
}
