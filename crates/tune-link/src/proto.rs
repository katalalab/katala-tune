//! 暗号化した通信の上の、要求と応答。1 メッセージ = 1 つの JSON（長さは [`crate::channel::Secure`] が持つ）。
//!
//! 要求: `{"id": 1, "op": "probe", "args": {...}}`
//! 応答: `{"id": 1, "ok": true, "result": ...}` か `{"id": 1, "ok": false, "error": "..."}`
//!
//! 今ある操作は読み取り専用の [`op::HELLO`]・[`op::PROBE`] だけ。ライブの開始・停止、OTLP の受け渡しは名前だけ決めておき、
//! 次の段階で同じ形に載せる（知らない操作には `ok: false` を返す）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::channel::Secure;
use crate::frame::Transport;
use crate::{Error, Result};

pub mod op {
    /// 相手の版と、使える操作の一覧
    pub const HELLO: &str = "hello";
    /// 読み取り専用の調査（今の probe と同じ JSON）
    pub const PROBE: &str = "probe";
    /// （次の段階）ライブ表示の開始・停止
    pub const LIVE_START: &str = "live.start";
    pub const LIVE_STOP: &str = "live.stop";
    /// （次の段階）OTLP の受け口が書いたファイルの続きを読む
    pub const OTLP_READ: &str = "otlp.read";
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub op: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Response {
        Response { id, ok: true, result, error: None }
    }

    pub fn err(id: u64, msg: impl Into<String>) -> Response {
        Response { id, ok: false, result: Value::Null, error: Some(msg.into()) }
    }
}

impl<T: Transport> Secure<T> {
    /// 操作卓の側: 要求を送り、応答の result を返す（`ok: false` は Err）
    pub async fn call(&mut self, id: u64, op: &str, args: Value) -> Result<Value> {
        let req = serde_json::to_vec(&Request { id, op: op.into(), args }).map_err(|e| Error::Protocol(e.to_string()))?;
        self.send(&req).await?;
        let res: Response = serde_json::from_slice(&self.recv().await?).map_err(|e| Error::Protocol(format!("応答を読めない: {e}")))?;
        if res.id != id {
            return Err(Error::Protocol("応答の id が違う".into()));
        }
        if res.ok { Ok(res.result) } else { Err(Error::Protocol(res.error.unwrap_or_else(|| "失敗".into()))) }
    }

    /// tune-agent の側: 次の要求
    pub async fn next_request(&mut self) -> Result<Request> {
        serde_json::from_slice(&self.recv().await?).map_err(|e| Error::Protocol(format!("要求を読めない: {e}")))
    }

    pub async fn reply(&mut self, r: &Response) -> Result<()> {
        let b = serde_json::to_vec(r).map_err(|e| Error::Protocol(e.to_string()))?;
        self.send(&b).await
    }
}
