//! Cooperative admission for participating GUI hosts, distinct from Engine data ownership.
//! Successful admission lasts until process exit, including Pause and shutdown return.
//! Old hosts, standalone CLI/core callers and remote producers do not participate.
use std::{fmt, sync::Mutex};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

/// Bounded diagnostics deliberately contain no private filesystem paths or identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Busy,
    BusyOrDenied,
    UnsafeStorage,
    Unavailable,
}
impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Busy => "別の参加GUIが収集を使用中です。そのアプリを終了してから明示的に再試行してください",
            Self::BusyOrDenied => "別の収集所有者またはアクセス拒否により収集を開始できません",
            Self::UnsafeStorage => "収集予約の保存先が安全な所有者・種類・権限ではないため開始できません",
            Self::Unavailable => "収集予約を取得できません。状態を確認してから明示的に再試行してください",
        })
    }
}
impl std::error::Error for AdmissionError {}

static OWNER: Mutex<Option<platform::Claim>> = Mutex::new(None);

/// Reserve exactly one kernel handle for this effective OS user and process.
/// First acquisition is serialized, only success is cached, and there is no release,
/// automatic retry, ownership worker, PID, heartbeat or stale-file deletion.
pub fn ensure_gui_collection_owner() -> Result<(), AdmissionError> {
    let mut owner = OWNER.lock().map_err(|_| AdmissionError::Unavailable)?;
    if owner.is_none() {
        *owner = Some(platform::claim()?);
    }
    Ok(())
}
