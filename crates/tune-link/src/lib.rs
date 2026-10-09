//! Katala Tune の機体どうしの接続（docs/connectivity.md）。経路（TCP・将来の WebSocket 中継・QUIC）に依存しない。
//!
//! | モジュール | 役割 |
//! |---|---|
//! | [`keys`] | 機体鍵（Ed25519 の署名鍵と、Noise 用の X25519 の静的鍵）の生成・保存（本人だけが読めるファイル）・公開の名刺（[`keys::Card`]） |
//! | [`peers`] | ペア済みの相手（公開鍵だけ）の台帳 |
//! | [`frame`] | 経路の抽象（1 フレーム = 最大 65535 バイト。[`frame::Transport`]）と、ストリーム上の実装（長さ 2 バイト＋本体） |
//! | [`pair`] | ペアリング: 6 桁のコード → SPAKE2 → その鍵を PSK にした Noise（XXpsk0）で、互いの名刺（公開鍵だけ）を交換する |
//! | [`channel`] | ペア済みの相手との端末間暗号化（Noise IK。相手の静的鍵と、それを署名した機体鍵を毎回確かめる） |
//! | [`proto`] | 暗号化した通信の上の、要求と応答（長さ付きの JSON） |
//! | [`client`] | 操作卓の側の TCP での呼び出し（ペアリング・接続・要求） |
//!
//! 暗号は自作しない。SPAKE2（`spake2`）・Noise（`snow`）・Ed25519（`ed25519-dalek`）の組み合わせだけで作る。
//! 秘密鍵・コードは Debug・Display・ログに出さない（[`keys::DeviceKeys`]・[`pair::Code`] の Debug は伏せる）。

pub mod channel;
pub mod client;
pub mod frame;
pub mod hex;
pub mod keys;
pub mod pair;
pub mod peers;
pub mod proto;

use std::fmt;
use std::path::{Path, PathBuf};

/// `tune-agent run` が待ち受ける既定のポート（ペア済みの相手だけを受け付ける）
pub const RUN_PORT: u16 = 47231;
/// `tune-agent pair` が待ち受ける既定のポート（コードの受付中だけ開く）
pub const PAIR_PORT: u16 = 47232;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Noise の処理の失敗（復号できない・形が違う）。中身は snow のエラーの種類だけで、鍵は含まない
    Noise(snow::Error),
    /// 形式・手順の違い
    Protocol(String),
    /// コードが違う（相手が続けて受け付ける残りの回数）
    WrongCode {
        remaining: u32,
    },
    /// 受付をしていない（期限切れ・使用済み・止めた）
    Refused(pair::Refusal),
    /// ペアしていない相手
    Unpaired,
    /// 時間切れ
    Timeout,
    /// 鍵・台帳のファイル
    Store(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "通信の失敗: {e}"),
            Error::Noise(e) => write!(f, "暗号の処理に失敗（相手の鍵が違うか、通信が改ざんされた）: {e}"),
            Error::Protocol(s) => write!(f, "形式が違う: {s}"),
            Error::WrongCode { remaining } => write!(f, "コードが違う（あと {remaining} 回まで。超えると受付を止める）"),
            Error::Refused(r) => write!(f, "受け付けていない: {r}"),
            Error::Unpaired => write!(f, "ペアしていない相手"),
            Error::Timeout => write!(f, "時間切れ"),
            Error::Store(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<snow::Error> for Error {
    fn from(e: snow::Error) -> Self {
        Error::Noise(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// 操作卓の側（Katala Tune・tune-cli）の鍵と台帳の置き場所。`KATALA_TUNE_LINK_DIR` で差し替えられる（検証用）
pub fn console_dir(app_data_dir: &Path) -> PathBuf {
    match std::env::var_os("KATALA_TUNE_LINK_DIR").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => app_data_dir.join("link"),
    }
}

/// いまの時刻（Unix 時刻のミリ秒）
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
