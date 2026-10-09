//! ペアリング。6 桁のコードから SPAKE2 で共有鍵を作り、その鍵を PSK にした Noise（`Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s`）で
//! 互いの名刺（公開鍵だけ）を交換する。
//!
//! ```text
//! 操作卓（A）                                  tune-agent（B。コードを表示して待つ）
//!   "KTP1" ‖ SPAKE2 の A のメッセージ  ─────▶  受付中か確かめる（期限・使用済み・止めた → 拒否を返して終わり）
//!                                     ◀─────  0x00 ‖ SPAKE2 の B のメッセージ        ← ここから 1 回の試行として数える
//!   K = SPAKE2 の鍵                             K = SPAKE2 の鍵
//!   Noise 1 通目（psk, e）             ─────▶  復号できない = コードが違う → 0x02 ‖ 残りの回数 を返して終わり
//!                                     ◀─────  Noise 2 通目（e, ee, s, es）＋ tune-agent の名刺
//!   Noise 3 通目（s, se）＋ 操作卓の名刺 ─────▶  名刺の署名と静的鍵を確かめて登録
//!                                     ◀─────  暗号化した {"ok": true, "run_port": …}
//! ```
//!
//! - 盗み見た人は SPAKE2 の秘密の数を知らないので K を計算できず、コードの候補を手元で確かめる手段が無い
//!   （総当たりはオンラインで 1 回ずつ。tune-agent は 1 回ずつ順に処理し、[`MAX_FAILURES`] 回で受付を止める）
//! - 間に入った人は、1 回の接続で 1 つの候補しか試せない（SPAKE2 の性質）。外れると Noise の 1 通目が復号できない
//! - コードが違うとき、tune-agent は名刺を送らない（PSK を最初に混ぜる psk0 なので、1 通目で分かる）
//! - コードは 5 分で失効し、1 回成功したら使えない（[`PairWindow`]）

use std::time::{Duration, Instant};

use rand_core::{OsRng, RngCore};
use serde_json::{Value, json};
use snow::Builder;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::sync::mpsc;
use tokio::time::timeout;
use zeroize::{Zeroize, Zeroizing};

use crate::channel::{params, remote_static};
use crate::frame::{MAX_FRAME, Transport};
use crate::keys::{Card, DeviceKeys, Verified};
use crate::{Error, Result};

pub const CODE_TTL: Duration = Duration::from_secs(300);
/// 続けて間違えたら受付を止める回数（当たる確率は最大でも 3 / 1,000,000）
pub const MAX_FAILURES: u32 = 3;
/// 1 回の試行の上限（相手が途中で黙っても、受付をふさぎ続けない）
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(20);

pub const PAIR_PARAMS: &str = "Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s";
pub const PAIR_PROLOGUE: &[u8] = b"katala-tune/pair/1";
const MAGIC: &[u8; 4] = b"KTP1";
const ID_CONSOLE: &[u8] = b"katala-tune console";
const ID_AGENT: &[u8] = b"katala-tune agent";

const ST_OK: u8 = 0;
const ST_REFUSED: u8 = 1;
const ST_WRONG: u8 = 2;

/// 6 桁のコード。Debug では伏せる。表示は端末・画面だけに（ログ・標準出力・引数に出さない）
pub struct Code(Zeroizing<String>);

impl std::fmt::Debug for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Code(******)")
    }
}

impl Code {
    /// OS の乱数で一様に作る（000000〜999999）
    pub fn generate() -> Result<Code> {
        // 4,294,000,000 = 1,000,000 の倍数。これ以上は捨てて引き直す（剰余の偏りを出さない）
        const LIMIT: u32 = 4_294_000_000;
        loop {
            let mut b = [0u8; 4];
            OsRng.try_fill_bytes(&mut b).map_err(|e| Error::Store(format!("乱数を作れない: {e}")))?;
            let x = u32::from_le_bytes(b);
            b.zeroize();
            if x < LIMIT {
                return Ok(Code(Zeroizing::new(format!("{:06}", x % 1_000_000))));
            }
        }
    }

    /// 入力を読む（空白とハイフンは無視。ちょうど 6 桁の数字だけ）
    pub fn parse(s: &str) -> Option<Code> {
        let digits: Zeroizing<String> = Zeroizing::new(s.chars().filter(|c| !c.is_whitespace() && *c != '-').collect());
        (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| Code(digits))
    }

    /// 端末・画面に出す形（`123 456`）
    pub fn grouped(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{} {}", &self.0[..3], &self.0[3..]))
    }

    /// SPAKE2 に渡す形（spake2 の Password は内部の写しを消さない。docs/connectivity.md の「残る危険」）
    fn password(&self) -> Password {
        let s = Zeroizing::new(format!("katala-tune pairing code {}", self.0.as_str()));
        Password::new(s.as_bytes())
    }
}

/// 受け付けない理由
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Expired,
    Used,
    Locked,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refusal::Expired => "コードの期限が切れた（5 分）。機体で tune-agent pair をやり直してください",
            Refusal::Used => "このコードは使用済み（1 回限り）",
            Refusal::Locked => "続けて間違えたので受付を止めた。機体で tune-agent pair をやり直してください",
        })
    }
}

impl Refusal {
    fn byte(self) -> u8 {
        match self {
            Refusal::Expired => 1,
            Refusal::Used => 2,
            Refusal::Locked => 3,
        }
    }

    fn from_byte(b: u8) -> Option<Refusal> {
        match b {
            1 => Some(Refusal::Expired),
            2 => Some(Refusal::Used),
            3 => Some(Refusal::Locked),
            _ => None,
        }
    }
}

/// コードの受付（期限・1 回限り・続けて間違えたら止める）
pub struct PairWindow {
    code: Code,
    opened: Instant,
    ttl: Duration,
    max_failures: u32,
    failures: u32,
    used: bool,
}

impl PairWindow {
    pub fn new(code: Code, now: Instant) -> PairWindow {
        PairWindow::with_limits(code, now, CODE_TTL, MAX_FAILURES)
    }

    pub fn with_limits(code: Code, now: Instant, ttl: Duration, max_failures: u32) -> PairWindow {
        PairWindow { code, opened: now, ttl, max_failures: max_failures.max(1), failures: 0, used: false }
    }

    /// 端末に表示するためだけに使う
    pub fn code(&self) -> &Code {
        &self.code
    }

    /// いま受け付けるか
    pub fn check(&self, now: Instant) -> std::result::Result<(), Refusal> {
        if self.used {
            Err(Refusal::Used)
        } else if self.failures >= self.max_failures {
            Err(Refusal::Locked)
        } else if now.saturating_duration_since(self.opened) >= self.ttl {
            Err(Refusal::Expired)
        } else {
            Ok(())
        }
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    pub fn remaining_attempts(&self) -> u32 {
        self.max_failures.saturating_sub(self.failures)
    }

    pub fn expires_in(&self, now: Instant) -> Duration {
        self.ttl.saturating_sub(now.saturating_duration_since(self.opened))
    }

    fn fail(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    fn succeed(&mut self) {
        self.used = true;
    }
}

fn psk_from(mut key: Vec<u8>) -> Result<Zeroizing<[u8; 32]>> {
    let mut psk = Zeroizing::new([0u8; 32]);
    if key.len() != 32 {
        key.zeroize();
        return Err(Error::Protocol("SPAKE2 の鍵の長さが違う".into()));
    }
    psk.copy_from_slice(&key);
    key.zeroize();
    Ok(psk)
}

/// tune-agent の側の 1 回分。成功したら `register` で相手（操作卓）を台帳に足してから確認を返す。
/// SPAKE2 のメッセージを返した時点で 1 回の試行として数え、成功以外（間違い・切断・時間切れ）はすべて失敗に数える
pub async fn respond<T, R>(t: &mut T, window: &mut PairWindow, me: &DeviceKeys, my_name: &str, run_port: u16, register: R) -> Result<Verified>
where
    T: Transport,
    R: FnOnce(&Verified, &Card) -> Result<()>,
{
    let first = timeout(ATTEMPT_TIMEOUT, t.recv()).await.map_err(|_| Error::Timeout)??;
    let Some(msg_a) = first.strip_prefix(MAGIC.as_slice()).filter(|m| m.len() <= 64) else {
        return Err(Error::Protocol("ペアリングの要求ではない".into()));
    };
    if let Err(r) = window.check(Instant::now()) {
        let _ = timeout(Duration::from_secs(2), t.send(&[ST_REFUSED, r.byte()])).await;
        return Err(Error::Refused(r));
    }
    let password = window.code.password();
    // register が成功したら台帳は変わっている。確認（ACK）が届かなくても、時間切れで途中で打ち切られても、
    // 1 回限りのコードは使用済みにする（同じコードで別の操作卓が登録できてはいけない）
    let mut committed: Option<Verified> = None;
    let result = timeout(ATTEMPT_TIMEOUT, attempt(t, password, msg_a.to_vec(), me, my_name, run_port, register, &mut committed)).await.unwrap_or(Err(Error::Timeout));
    if let Some(v) = committed {
        window.succeed();
        return Ok(v);
    }
    match result {
        Ok(v) => {
            window.succeed();
            Ok(v)
        }
        Err(e) => {
            window.fail();
            if let Error::WrongCode { .. } = e {
                let remaining = window.remaining_attempts();
                let _ = timeout(Duration::from_secs(2), t.send(&[ST_WRONG, remaining.min(255) as u8])).await;
                return Err(Error::WrongCode { remaining });
            }
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn attempt<T, R>(t: &mut T, password: Password, msg_a: Vec<u8>, me: &DeviceKeys, my_name: &str, run_port: u16, register: R, committed: &mut Option<Verified>) -> Result<Verified>
where
    T: Transport,
    R: FnOnce(&Verified, &Card) -> Result<()>,
{
    let (spake, msg_b) = Spake2::<Ed25519Group>::start_b(&password, &Identity::new(ID_CONSOLE), &Identity::new(ID_AGENT));
    drop(password);
    let mut out = Vec::with_capacity(1 + msg_b.len());
    out.push(ST_OK);
    out.extend_from_slice(&msg_b);
    t.send(&out).await?;
    let psk = psk_from(spake.finish(&msg_a).map_err(|_| Error::WrongCode { remaining: 0 })?)?;
    let mut hs = Builder::new(params(PAIR_PARAMS)).local_private_key(me.static_secret())?.psk(0, &psk)?.prologue(PAIR_PROLOGUE)?.build_responder()?;
    let mut buf = vec![0u8; MAX_FRAME];
    let m1 = t.recv().await?;
    // 復号できない = コードが違う（PSK は SPAKE2 の鍵。コードが違えば鍵が違う）
    hs.read_message(&m1, &mut buf).map_err(|_| Error::WrongCode { remaining: 0 })?;
    let n = hs.write_message(&me.card(my_name).to_json(), &mut buf)?;
    t.send(&buf[..n]).await?;
    let m3 = t.recv().await?;
    let n = hs.read_message(&m3, &mut buf)?;
    let card = Card::from_json(&buf[..n])?;
    let v = card.verify()?;
    if v.static_key != remote_static(&hs)? {
        return Err(Error::Protocol("名刺の静的鍵が、Noise で確かめた鍵と違う".into()));
    }
    let mut tr = hs.into_transport_mode()?;
    let registered = register(&v, &card);
    if registered.is_ok() {
        // この先（確認の送信）で失敗・打ち切りになっても、登録したことは呼び出し側に残す
        *committed = Some(v.clone());
    }
    let ack = match &registered {
        Ok(()) => json!({ "ok": true, "run_port": run_port }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    };
    let n = tr.write_message(&serde_json::to_vec(&ack).unwrap_or_default(), &mut buf)?;
    t.send(&buf[..n]).await?;
    registered.map(|()| v)
}

/// 操作卓の側の結果
#[derive(Clone, Debug)]
pub struct Paired {
    pub agent: Verified,
    pub card: Card,
    /// tune-agent run の待ち受けのポート
    pub run_port: u16,
}

/// 操作卓の側: コードでペアリングする
pub async fn initiate<T: Transport>(t: &mut T, code: &Code, me: &DeviceKeys, my_name: &str) -> Result<Paired> {
    timeout(ATTEMPT_TIMEOUT, initiate_inner(t, code, me, my_name)).await.unwrap_or(Err(Error::Timeout))
}

async fn initiate_inner<T: Transport>(t: &mut T, code: &Code, me: &DeviceKeys, my_name: &str) -> Result<Paired> {
    let (spake, msg_a) = Spake2::<Ed25519Group>::start_a(&code.password(), &Identity::new(ID_CONSOLE), &Identity::new(ID_AGENT));
    let mut first = MAGIC.to_vec();
    first.extend_from_slice(&msg_a);
    t.send(&first).await?;
    let reply = t.recv().await?;
    let msg_b = match reply.split_first() {
        Some((&ST_OK, rest)) => rest,
        Some((&ST_REFUSED, [r])) => return Err(Refusal::from_byte(*r).map_or_else(|| Error::Protocol("拒否の理由が読めない".into()), Error::Refused)),
        _ => return Err(Error::Protocol("ペアリングの応答ではない".into())),
    };
    let psk = psk_from(spake.finish(msg_b).map_err(|_| Error::Protocol("相手の SPAKE2 のメッセージが壊れている".into()))?)?;
    let mut hs = Builder::new(params(PAIR_PARAMS)).local_private_key(me.static_secret())?.psk(0, &psk)?.prologue(PAIR_PROLOGUE)?.build_initiator()?;
    let mut buf = vec![0u8; MAX_FRAME];
    let n = hs.write_message(&[], &mut buf)?;
    t.send(&buf[..n]).await?;
    let m2 = t.recv().await?;
    if let [ST_WRONG, remaining] = m2.as_slice() {
        return Err(Error::WrongCode { remaining: u32::from(*remaining) });
    }
    let n = hs.read_message(&m2, &mut buf)?;
    let card = Card::from_json(&buf[..n])?;
    let agent = card.verify()?;
    if agent.static_key != remote_static(&hs)? {
        return Err(Error::Protocol("名刺の静的鍵が、Noise で確かめた鍵と違う".into()));
    }
    let n = hs.write_message(&me.card(my_name).to_json(), &mut buf)?;
    t.send(&buf[..n]).await?;
    let mut tr = hs.into_transport_mode()?;
    let ack = t.recv().await?;
    let n = tr.read_message(&ack, &mut buf)?;
    let ack: Value = serde_json::from_slice(&buf[..n]).map_err(|e| Error::Protocol(format!("確認を読めない: {e}")))?;
    if ack["ok"] != json!(true) {
        return Err(Error::Protocol(format!("相手が登録できなかった: {}", ack["error"].as_str().unwrap_or("?"))));
    }
    let run_port = ack["run_port"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(crate::RUN_PORT);
    Ok(Paired { agent, card, run_port })
}

/// 受付の途中経過（ログ用。コード・鍵は含まない）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairEvent {
    Attempt { from: String },
    WrongCode { remaining: u32 },
    Refused(Refusal),
    Failed(String),
    Paired { name: String, fingerprint: String },
}

/// tune-agent の側の受付。接続は `incoming` から 1 つずつ順に処理する（同時に試させない）。
/// 成功・期限切れ・受付停止のどれかで終わる
pub async fn serve<T, R, E>(
    incoming: &mut mpsc::Receiver<(T, String)>,
    window: &mut PairWindow,
    me: &DeviceKeys,
    my_name: &str,
    run_port: u16,
    mut register: R,
    mut event: E,
) -> Result<Verified>
where
    T: Transport,
    R: FnMut(&Verified, &Card) -> Result<()>,
    E: FnMut(PairEvent),
{
    loop {
        let now = Instant::now();
        if let Err(r) = window.check(now) {
            return Err(Error::Refused(r));
        }
        let (mut t, from) = match timeout(window.expires_in(now), incoming.recv()).await {
            Ok(Some(x)) => x,
            Ok(None) => return Err(Error::Protocol("待ち受けが止まった".into())),
            Err(_) => continue,
        };
        event(PairEvent::Attempt { from });
        match respond(&mut t, window, me, my_name, run_port, |v, c| register(v, c)).await {
            Ok(v) => {
                event(PairEvent::Paired { name: v.name.clone(), fingerprint: crate::keys::fingerprint(&v.identity) });
                return Ok(v);
            }
            Err(Error::WrongCode { remaining }) => event(PairEvent::WrongCode { remaining }),
            Err(Error::Refused(r)) => event(PairEvent::Refused(r)),
            Err(e) => event(PairEvent::Failed(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_six_digits_and_hidden_in_debug() {
        for _ in 0..200 {
            let c = Code::generate().unwrap();
            assert_eq!(c.0.len(), 6);
            assert!(c.0.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(format!("{c:?}"), "Code(******)");
            assert!(!format!("{c:?}").contains(c.0.as_str()));
        }
        assert_eq!(Code::parse(" 123-456 ").map(|c| c.0.to_string()), Some("123456".into()));
        assert!(Code::parse("12345").is_none());
        assert!(Code::parse("1234567").is_none());
        assert!(Code::parse("12a456").is_none());
        assert_eq!(Code::parse("123456").unwrap().grouped().as_str(), "123 456");
    }

    #[test]
    fn window_expires_is_single_use_and_locks() {
        let t0 = Instant::now();
        let mut w = PairWindow::new(Code::parse("123456").unwrap(), t0);
        assert_eq!(w.check(t0), Ok(()));
        assert_eq!(w.check(t0 + Duration::from_secs(299)), Ok(()));
        assert_eq!(w.check(t0 + CODE_TTL), Err(Refusal::Expired), "5 分で失効");
        w.fail();
        w.fail();
        assert_eq!((w.check(t0), w.remaining_attempts()), (Ok(()), 1));
        w.fail();
        assert_eq!(w.check(t0), Err(Refusal::Locked), "3 回続けて間違えたら止める");
        let mut w = PairWindow::new(Code::parse("123456").unwrap(), t0);
        w.succeed();
        assert_eq!(w.check(t0), Err(Refusal::Used), "1 回限り");
    }

    #[test]
    fn codes_are_roughly_uniform() {
        // 先頭の桁の偏りが大きくないこと（剰余の偏りを出していない目安）
        let mut c = [0u32; 10];
        for _ in 0..5000 {
            let d = Code::generate().unwrap().0.as_bytes()[0] - b'0';
            c[d as usize] += 1;
        }
        assert!(c.iter().all(|&n| (300..700).contains(&n)), "{c:?}");
    }
}
