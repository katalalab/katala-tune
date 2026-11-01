use crate::llm::{Endpoint, ModelConfirmation, ModelQuery, ModelScope};
use crate::llm_keyboard;
use crate::scope::{Confirmation, Confirmations, Grants};

pub const MACHINE_UNAVAILABLE: &str =
    "確認内容の全体表示を保証できないため、この版では保存要約の新規許可を一時停止しています。";
pub const MODEL_UNAVAILABLE: &str =
    "確認内容の全体表示を保証できないため、この版では非ループバック LLM の新規許可を一時停止しています。";

pub fn footer_notice(sample: bool, windows: bool) -> &'static str {
    match (sample, windows) {
        (true, true) => "/ 検索 · M 機体 · L 追加先 · ↑↓ 選択 · Enter 確認 · Y 模擬許可 · X 取消 · Esc 戻る",
        (true, false) => "/ 検索 · M 機体 · ↑↓ 選択 · Enter 確認 · 模擬許可はマウス · Esc 戻る",
        (false, true) => "/ 検索 · M 機体 · L 追加先 · ↑↓ 選択 · Enter 操作 · X 取消 · 新規許可停止 · Esc 戻る",
        (false, false) => "/ 検索 · M 機体 · ↑↓ 選択 · Enter 操作 · 新規許可停止 · Esc 戻る",
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum MachineConfirmationResult {
    Authorized,
    Unavailable,
    Stale,
    Failed,
}

pub enum ModelConfirmationResult {
    Authorized(ModelQuery),
    Unavailable,
    Stale,
}

pub fn preparation_available(sample: bool) -> bool {
    sample
}

/// UI approval is temporarily restricted to sample mode until full disclosure
/// visibility can be established. A stale callback must not clear newer state.
pub fn confirm_machine(
    sample: bool,
    confirmations: &mut Confirmations,
    grants: &mut Grants,
    confirmation: &Confirmation,
) -> MachineConfirmationResult {
    if !preparation_available(sample) {
        return if confirmations.cancel_if(confirmation) {
            MachineConfirmationResult::Unavailable
        } else {
            MachineConfirmationResult::Stale
        };
    }
    if !confirmations.take_if(confirmation) {
        return MachineConfirmationResult::Stale;
    }
    if grants.authorize(&confirmation.id, &confirmation.fingerprint) {
        MachineConfirmationResult::Authorized
    } else {
        MachineConfirmationResult::Failed
    }
}

pub fn confirm_model(
    sample: bool,
    scope: &mut ModelScope,
    visible: &[Endpoint],
    current: &mut Option<ModelConfirmation>,
    confirmation: &ModelConfirmation,
    pending: bool,
) -> ModelConfirmationResult {
    if !preparation_available(sample) {
        if current.as_ref() != Some(confirmation) {
            return ModelConfirmationResult::Stale;
        }
        llm_keyboard::dismiss(scope, current);
        return ModelConfirmationResult::Unavailable;
    }
    match llm_keyboard::confirm(scope, visible, current.as_ref(), confirmation, pending) {
        Some(query) => ModelConfirmationResult::Authorized(query),
        None => ModelConfirmationResult::Stale,
    }
}