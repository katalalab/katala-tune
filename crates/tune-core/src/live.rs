//! ライブ表示（1〜2 秒ごとの CPU・メモリ・ディスク・ネットワーク・GPU と、5 秒ごとの上位プロセス）。
//!
//! 画面が見ているあいだだけ、各機体で軽いサンプラー（`probes/live_mac.py`・`probes/live_win.ps1`）を動かし、
//! 1 行 1 JSON を読み続ける。機体に常駐はさせない。サンプラーは読み取り専用で、読み手が居なくなると
//! （SSH の切断・アプリの終了）次の書き込みで終わり、長くても `sampler_max_age` で終わる（必要ならつなぎ直す）。
//!
//! 将来は常駐（tune-agent）の暗号化通信に載せ替えるので、取得の経路と、解析・保持・管理を分けている:
//!
//! | 層 | 中身 |
//! |---|---|
//! | 取得の経路 [`Route`] | 行を流す元（[`Source`]）を開く。今は [`ProcessRoute`]（この機体はローカル実行、ほかは SSH。collect.rs と同じ作法） |
//! | 解析 [`parse_line`] | サンプラーの 1 行を [`Msg`] に。壊れた行・知らない行は捨てる。値は範囲に収める |
//! | 保持 [`Ring`] | 機体ごとの直近 `ring` 点（既定 300 点 = 1 秒ごとで 5 分）と、最新の上位プロセス |
//! | 管理 [`Live`] | 開始・停止・画面からの合図（heartbeat）が途切れたときの自動停止・同時に流す台数の上限・つなぎ直しの上限・後始末 |
//!
//! 画面へは [`Live::tick`] が `flush` ごとに、変わった機体の新しい点だけをまとめて 1 つのイベントで送る
//! （元データは Rust 側。画面にはスパークラインに要る値だけ。docs/architecture.md の決まり）。

use crate::collect;
use crate::db::now_ms;
use crate::monitor_journal::{
    Category, CloseTrigger, Data, InputEnd, Journal, Kind, PipeEnd, Policy, Reason, StatePhase, StreamBoundary, StreamIo, WaitOrigin, elapsed_ms,
};
use crate::nodes::Node;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, ReadBuf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

mod part_01;
mod part_02;
pub use part_01::*;
pub use part_02::*;

pub const MAC_LIVE: &str = include_str!("../../../probes/live_mac.py");

/// PS 5.1 のため BOM 付き ASCII。バイトのまま渡す
pub const WIN_LIVE: &[u8] = include_bytes!("../../../probes/live_win.ps1");

#[cfg(test)]
mod tests;
