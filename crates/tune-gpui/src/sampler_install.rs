//! Window-local source/confirmation flow. The app host owns every accepted job.
use crate::{
    gate::Gate,
    monitor_host::{MonitorHost, NativeInstallResult},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tune_core::helpers::native_install::{self, InstallError, NativeTargetSnapshot, PreparedNativeSampler};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Phase {
    #[default]
    Idle,
    Choosing,
    Preparing,
    Confirming,
    Repreparing,
}
impl Phase {
    pub(super) fn label(self) -> Option<&'static str> {
        match self {
            Self::Idle => None,
            Self::Choosing => Some("配布フォルダを選択しています"),
            Self::Preparing => Some("配布ファイルと導入先を確認しています"),
            Self::Confirming => Some("導入内容の確認を待っています"),
            Self::Repreparing => Some("承認したファイルと導入先を再確認しています"),
        }
    }
}
#[derive(Default, Clone)]
pub(super) struct UiState {
    pub(super) phase: Phase,
    pub(super) notice: Option<String>,
}
impl UiState {
    pub(super) fn choose(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Choosing;
        true
    }
}
#[derive(PartialEq, Eq)]
struct SourceFingerprint {
    source: PathBuf,
    manifest_sha256: String,
    sampler_sha256: String,
    sampler_bytes: u64,
}
impl SourceFingerprint {
    fn of(prepared: &PreparedNativeSampler) -> Self {
        Self {
            source: prepared.source().into(),
            manifest_sha256: prepared.manifest_sha256().into(),
            sampler_sha256: prepared.sampler_sha256().into(),
            sampler_bytes: prepared.sampler_bytes(),
        }
    }
}
struct Confirmation {
    fingerprint: SourceFingerprint,
    target: NativeTargetSnapshot,
    detail: String,
}
fn prepare(source: PathBuf) -> Result<Confirmation, String> {
    let prepared = native_install::prepare_native_sampler(&source).map_err(|error| error_detail(&error))?;
    let target = native_install::inspect_bound_target().map_err(|error| error_detail(&error))?;
    let detail = format!(
        "この Mac / PC の収集ヘルパー（sampler）1 ファイルを導入します。\n導入先: {}\n現在の SHA-256: {}\n新しい SHA-256: {}\n\n配布元フォルダ: {}\n版: {}\n対象: {}\nサイズ: {} bytes\nRevision: {}\nSource tree: {}\nCI: {} / run {} / attempt {}\nWorkflow: {}\nWorkflow SHA: {}\nEvent: {}\nManifest SHA-256: {}\n\nGUI・診断 CLI・他の機体への配布は含みません。ハッシュと CI 情報は配布物の申告であり、発行者の証明ではありません。",
        target.target().display(),
        target.sha256().unwrap_or("未配置"),
        prepared.sampler_sha256(),
        prepared.source().display(),
        prepared.version(),
        prepared.build_target(),
        prepared.sampler_bytes(),
        prepared.revision(),
        prepared.source_tree(),
        prepared.repository(),
        prepared.run_id(),
        prepared.run_attempt(),
        prepared.workflow_ref(),
        prepared.workflow_sha(),
        prepared.event_name(),
        prepared.manifest_sha256()
    );
    // No prepared bytes are retained during the human confirmation wait.
    Ok(Confirmation { fingerprint: SourceFingerprint::of(&prepared), target, detail })
}
fn reprepare(confirmation: Confirmation) -> Result<(PreparedNativeSampler, NativeTargetSnapshot), String> {
    let prepared = native_install::prepare_native_sampler(&confirmation.fingerprint.source).map_err(|error| error_detail(&error))?;
    if SourceFingerprint::of(&prepared) != confirmation.fingerprint {
        return Err("確認後に配布ファイルが変わりました。フォルダを選び直し、内容を再確認してください".into());
    }
    native_install::revalidate_bound_target(&confirmation.target)
        .map_err(|error| format!("導入先を再確認できません。最初から確認し直してください。\n{}", error_detail(&error)))?;
    Ok((prepared, confirmation.target))
}
fn phase(state: &Mutex<UiState>, dirty: &AtomicBool, phase: Phase) {
    state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).phase = phase;
    dirty.store(true, Ordering::Release);
}
pub(super) fn finish(state: &Mutex<UiState>, dirty: &AtomicBool, result: &Result<(), String>) {
    let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.phase = Phase::Idle;
    if let Err(error) = result {
        state.notice = Some(error.clone());
    }
    dirty.store(true, Ordering::Release);
}
pub(super) async fn run_selection(
    paths: Option<Vec<PathBuf>>,
    host: Arc<MonitorHost>,
    gate: Gate,
    state: Arc<Mutex<UiState>>,
    dirty: Arc<AtomicBool>,
) -> Result<(), String> {
    let result = async {
        let Some(mut paths) = paths else {
            return Ok(());
        };
        if paths.len() != 1 {
            return Err("展開済みの配布フォルダを一つ選択してください".into());
        }
        if gate.is_closed() {
            return Ok(());
        }
        phase(&state, &dirty, Phase::Preparing);
        let source = paths.pop().unwrap();
        let confirmation = tokio::task::spawn_blocking(move || prepare(source)).await.map_err(|_| "配布ファイルの確認処理が停止しました".to_owned())??;
        phase(&state, &dirty, Phase::Confirming);
        if !(gate.confirm())("収集ヘルパーを導入".into(), confirmation.detail.clone()).await {
            return Ok(());
        }
        phase(&state, &dirty, Phase::Repreparing);
        let (prepared, target) =
            tokio::task::spawn_blocking(move || reprepare(confirmation)).await.map_err(|_| "導入直前の確認処理が停止しました".to_owned())??;
        host.admit_native_install(prepared, target, &gate)?;
        state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).notice = None;
        Ok(())
    }
    .await;
    finish(&state, &dirty, &result);
    result
}
fn error_detail(error: &InstallError) -> String {
    crate::monitor_host::NativeInstallFailure::from(error).detail()
}
pub(super) fn receipt_text(result: &NativeInstallResult) -> String {
    match result {
        NativeInstallResult::WorkerFailed => "導入処理の完了を確認できません。収集と再導入を停止しています。導入先の状態を確認してください。".into(),
        NativeInstallResult::Completed(Err(error)) => error.detail(),
        NativeInstallResult::Completed(Ok(receipt)) => {
            let mut detail = format!(
                "{}\n導入先: {}\n旧 SHA-256: {}\nSHA-256: {}\nManifest SHA-256: {}",
                if receipt.changed() {
                    "収集ヘルパーを配置し、検証しました"
                } else {
                    "同じ収集ヘルパーが配置済みです（変更なし）"
                },
                receipt.target().display(),
                receipt.old_sha256().unwrap_or("未配置"),
                receipt.sampler_sha256(),
                receipt.manifest_sha256()
            );
            if let Some(backup) = receipt.retained_backup() {
                detail.push_str(&format!("\n配置は完了しています。旧ファイルを保持: {}", backup.display()));
            }
            detail
        }
    }
}

#[cfg(test)]
#[path = "sampler_install_tests.rs"]
mod tests;
