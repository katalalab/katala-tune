//! Thin OS boundary. Completion handlers must never require a GPUI response.
use std::path::Path;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(any(target_os = "windows", test))]
mod windows_login;
#[cfg(any(target_os = "windows", test))]
mod windows_notifications;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Unknown,
    Enabled,
    Disabled,
    Denied,
    Unsupported(String),
    Failed(String),
}
impl State {
    pub fn observed(&self) -> Option<bool> {
        match self {
            Self::Enabled => Some(true),
            Self::Disabled => Some(false),
            _ => None,
        }
    }
    pub fn label(&self) -> String {
        match self {
            Self::Unknown => "未確認 / 未許可".into(),
            Self::Enabled => "有効（OS 観測）".into(),
            Self::Disabled => "無効（OS 観測）".into(),
            Self::Denied => "OS が拒否 / 承認待ち".into(),
            Self::Unsupported(reason) => format!("未対応: {reason}"),
            Self::Failed(reason) => format!("照会失敗: {reason}"),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Observation {
    pub source: String,
    pub notification: State,
    pub login: State,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Accepted,
    Denied,
    Unsupported(String),
    Failed(String),
}
impl Effect {
    pub fn label(&self) -> String {
        match self {
            Self::Accepted => "OS が要求を受理（通知の表示は未確認）".into(),
            Self::Denied => "OS が拒否 / 承認待ち".into(),
            Self::Unsupported(reason) => format!("未対応: {reason}"),
            Self::Failed(reason) => format!("操作失敗: {reason}"),
        }
    }
}
pub type Completion<T> = Box<dyn FnOnce(T) + Send>;
pub trait Service: Send + Sync + 'static {
    fn query(&self, done: Completion<Observation>);
    fn permission(&self, done: Completion<Effect>);
    fn settings(&self, done: Completion<Effect>);
    fn login(&self, enabled: bool, done: Completion<Effect>);
    fn notify(&self, title: String, body: String, done: Completion<Effect>);
}
pub fn platform(config: &Path, data: &Path, diagnostic: bool) -> std::sync::Arc<dyn Service> {
    #[cfg(target_os = "macos")]
    return std::sync::Arc::new(macos::Mac::new(config, data, diagnostic));
    #[cfg(target_os = "windows")]
    return std::sync::Arc::new(windows::Windows::new(config, data, diagnostic));
}
