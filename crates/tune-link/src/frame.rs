//! 経路の抽象。上（ペアリング・暗号化した通信）はフレーム（最大 65535 バイトのバイト列）の送受信だけを使う。
//!
//! - TCP・QUIC のストリーム・テスト用のパイプ: [`Framed`]（長さ 2 バイト（ビッグエンディアン）＋本体。Noise の仕様の推奨と同じ）
//! - 将来の WebSocket 中継: WebSocket の 1 メッセージ = 1 フレームとして [`Transport`] を実装する（中継は暗号文のフレームを通すだけ）

use std::future::Future;
use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 1 フレームの上限（Noise の 1 メッセージの上限と同じ）
pub const MAX_FRAME: usize = 65535;

pub trait Transport: Send {
    /// 1 フレームを送る（`MAX_FRAME` を超えたら InvalidInput）
    fn send(&mut self, frame: &[u8]) -> impl Future<Output = io::Result<()>> + Send;
    /// 1 フレームを受け取る。相手が閉じたら UnexpectedEof
    fn recv(&mut self) -> impl Future<Output = io::Result<Vec<u8>>> + Send;
}

/// バイトのストリーム（TCP など）の上のフレーム
pub struct Framed<S> {
    io: S,
}

impl<S> Framed<S> {
    pub fn new(io: S) -> Framed<S> {
        Framed { io }
    }

    pub fn get_ref(&self) -> &S {
        &self.io
    }

    pub fn into_inner(self) -> S {
        self.io
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Transport for Framed<S> {
    async fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        if frame.len() > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "フレームが大きすぎる"));
        }
        let mut buf = Vec::with_capacity(2 + frame.len());
        buf.extend_from_slice(&(frame.len() as u16).to_be_bytes());
        buf.extend_from_slice(frame);
        self.io.write_all(&buf).await?;
        self.io.flush().await
    }

    async fn recv(&mut self) -> io::Result<Vec<u8>> {
        let mut len = [0u8; 2];
        self.io.read_exact(&mut len).await?;
        let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
        self.io.read_exact(&mut buf).await?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_and_eof() {
        let (a, b) = tokio::io::duplex(1 << 20);
        let (mut a, mut b) = (Framed::new(a), Framed::new(b));
        a.send(b"").await.unwrap();
        a.send(&[7u8; MAX_FRAME]).await.unwrap();
        assert!(a.send(&vec![0u8; MAX_FRAME + 1]).await.is_err());
        assert_eq!(b.recv().await.unwrap(), b"");
        assert_eq!(b.recv().await.unwrap().len(), MAX_FRAME);
        drop(a);
        assert_eq!(b.recv().await.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}
