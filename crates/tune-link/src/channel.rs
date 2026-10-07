//! ペア済みの相手との端末間暗号化（`Noise_IK_25519_ChaChaPoly_BLAKE2s`）。
//!
//! - 操作卓（始める側）は、ペアリングで登録した tune-agent の静的鍵を知っているので IK を使う（1 往復）
//! - 1 通目には操作卓の静的鍵と名刺が、tune-agent の静的鍵に向けて暗号化して入る。tune-agent は
//!   静的鍵が台帳に無ければ、名刺も読まず、何も送り返さずに切る（内容を見る前に切る）
//! - 名刺の機体鍵（Ed25519）の署名が、Noise で確かめた静的鍵と一致することを毎回確かめる（libp2p の Noise と同じ考え方）
//! - セッション鍵は、接続ごとの使い捨ての X25519（Noise の ephemeral）から作る（前方秘匿）
//! - 1 通目は盗み見た人が再送できる（Noise の IK の性質）。tune-agent は 1 通目では何も実行せず、
//!   ハンドシェイクの後の要求（操作卓の ephemeral 鍵と静的鍵の両方が無いと作れない）だけを処理する
//!
//! 1 つのメッセージは Noise の上限（65535 バイト）を超えうるので、平文を `[続きの印 1 バイト][本体]` に分けて送る。

use snow::params::NoiseParams;
use snow::{Builder, HandshakeState, TransportState};

use crate::frame::{MAX_FRAME, Transport};
use crate::keys::{Card, DeviceKeys, Verified};
use crate::{Error, Result};

pub const LINK_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
/// 版と用途。両方の Noise の状態に混ぜるので、違う版・違う用途の相手とは手順の途中で必ず失敗する
pub const LINK_PROLOGUE: &[u8] = b"katala-tune/link/1";
/// 1 つのメッセージ（要求・応答）の上限
pub const MAX_MESSAGE: usize = 16 << 20;

const TAG: usize = 16;
const CHUNK: usize = MAX_FRAME - TAG - 1;
const MORE: u8 = 0;
const LAST: u8 = 1;

pub(crate) fn params(name: &str) -> NoiseParams {
    name.parse().expect("固定の Noise の名前")
}

pub(crate) fn remote_static(hs: &HandshakeState) -> Result<[u8; 32]> {
    hs.get_remote_static().and_then(|r| r.try_into().ok()).ok_or_else(|| Error::Protocol("相手の静的鍵が無い".into()))
}

/// 暗号化した通信路（ハンドシェイクが終わったもの）
pub struct Secure<T> {
    t: T,
    noise: TransportState,
    peer: Verified,
}

impl<T: Transport> Secure<T> {
    pub(crate) fn new(t: T, noise: TransportState, peer: Verified) -> Secure<T> {
        Secure { t, noise, peer }
    }

    /// 相手（名刺の署名と Noise の静的鍵の両方を確かめたもの）
    pub fn peer(&self) -> &Verified {
        &self.peer
    }

    pub fn transport(&self) -> &T {
        &self.t
    }

    /// 1 つのメッセージを送る（大きければ分けて送る）
    pub async fn send(&mut self, msg: &[u8]) -> Result<()> {
        if msg.len() > MAX_MESSAGE {
            return Err(Error::Protocol(format!("メッセージが大きすぎる（{} バイト）", msg.len())));
        }
        let mut out = vec![0u8; MAX_FRAME];
        let mut plain = Vec::with_capacity(CHUNK + 1);
        let n_chunks = msg.len().div_ceil(CHUNK).max(1);
        for i in 0..n_chunks {
            let part = &msg[(i * CHUNK).min(msg.len())..((i + 1) * CHUNK).min(msg.len())];
            plain.clear();
            plain.push(if i + 1 == n_chunks { LAST } else { MORE });
            plain.extend_from_slice(part);
            let n = self.noise.write_message(&plain, &mut out)?;
            self.t.send(&out[..n]).await?;
        }
        Ok(())
    }

    /// 1 つのメッセージを受け取る（改ざん・順番の入れ替え・再送は復号に失敗して Err）
    pub async fn recv(&mut self) -> Result<Vec<u8>> {
        let mut msg = Vec::new();
        let mut plain = vec![0u8; MAX_FRAME];
        loop {
            let f = self.t.recv().await?;
            let n = self.noise.read_message(&f, &mut plain)?;
            let Some((&flag, data)) = plain[..n].split_first() else {
                return Err(Error::Protocol("空のフレーム".into()));
            };
            if msg.len() + data.len() > MAX_MESSAGE {
                return Err(Error::Protocol("メッセージが大きすぎる".into()));
            }
            msg.extend_from_slice(data);
            match flag {
                LAST => return Ok(msg),
                MORE => continue,
                _ => return Err(Error::Protocol("続きの印が違う".into())),
            }
        }
    }
}

/// 操作卓の側: ペア済みの tune-agent（`agent`）につなぐ
pub async fn connect<T: Transport>(mut t: T, me: &DeviceKeys, my_name: &str, agent: &Verified) -> Result<Secure<T>> {
    let mut hs = Builder::new(params(LINK_PARAMS))
        .local_private_key(me.static_secret())?
        .remote_public_key(&agent.static_key)?
        .prologue(LINK_PROLOGUE)?
        .build_initiator()?;
    let mut buf = vec![0u8; MAX_FRAME];
    let n = hs.write_message(&me.card(my_name).to_json(), &mut buf)?;
    t.send(&buf[..n]).await?;
    let msg2 = t.recv().await?;
    let n = hs.read_message(&msg2, &mut buf)?;
    let their = Card::from_json(&buf[..n])?.verify()?;
    let remote = remote_static(&hs)?;
    if their.identity != agent.identity || their.static_key != agent.static_key || remote != agent.static_key {
        return Err(Error::Protocol("相手の機体鍵が台帳と違う".into()));
    }
    let noise = hs.into_transport_mode()?;
    Ok(Secure::new(t, noise, their))
}

/// tune-agent の側: 1 通目を受け、相手の静的鍵が `lookup` で見つかった（ペア済みの）ときだけ応じる。
/// 見つからなければ `Error::Unpaired`（名刺も読まず、何も送らない。呼び出し側は接続を閉じる）
pub async fn accept<T, F>(mut t: T, me: &DeviceKeys, my_name: &str, lookup: F) -> Result<Secure<T>>
where
    T: Transport,
    F: FnOnce(&[u8; 32]) -> Option<Verified>,
{
    let mut hs = Builder::new(params(LINK_PARAMS)).local_private_key(me.static_secret())?.prologue(LINK_PROLOGUE)?.build_responder()?;
    let msg1 = t.recv().await?;
    let mut buf = vec![0u8; MAX_FRAME];
    let n = hs.read_message(&msg1, &mut buf)?;
    let remote = remote_static(&hs)?;
    let Some(peer) = lookup(&remote) else {
        return Err(Error::Unpaired);
    };
    let their = Card::from_json(&buf[..n])?.verify()?;
    if their.identity != peer.identity || their.static_key != remote {
        return Err(Error::Unpaired);
    }
    let n = hs.write_message(&me.card(my_name).to_json(), &mut buf)?;
    t.send(&buf[..n]).await?;
    let noise = hs.into_transport_mode()?;
    Ok(Secure::new(t, noise, their))
}
