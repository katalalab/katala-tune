use crate::llm::{Endpoint, ModelConfirmation, ModelQuery, ModelScope};
use crate::llm_keyboard;
use crate::scope::{Confirmation, Confirmations, Grants};
use std::sync::Arc;

pub const MACHINE_UNAVAILABLE: &str = "確認内容の全体表示を保証できないため、この版では保存要約の新規許可を一時停止しています。";
pub const MODEL_UNAVAILABLE: &str = "確認内容の全体表示を保証できないため、この版では非ループバック LLM の新規許可を一時停止しています。";

pub fn footer_notice(sample: bool, windows: bool) -> &'static str {
    match (sample, windows) {
        (true, true) => "/ 検索 · M 機体 · L 追加先 · ↑↓ 選択 · Enter 確認 · Y 模擬許可 · X 取消 · Esc 戻る",
        (true, false) => "/ 検索 · M 機体 · ↑↓ 選択 · Enter 確認 · 模擬許可はマウス · Esc 戻る",
        (false, true) => "/ 検索 · M 機体 · L 追加先 · ↑↓ 選択 · Enter 操作 · X 取消 · Esc 戻る",
        (false, false) => "/ 検索 · M 機体 · ↑↓ 選択 · Enter 操作 · Esc 戻る",
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

pub fn preparation_available(_sample: bool) -> bool {
    true
}

pub fn can_prepare_confirmation(composing: bool) -> bool {
    !composing
}

/// Legacy entry without presentation evidence remains fail-closed for real data.
pub fn confirm_machine(sample: bool, confirmations: &mut Confirmations, grants: &mut Grants, confirmation: &Confirmation) -> MachineConfirmationResult {
    if !sample {
        return if confirmations.cancel_if(confirmation) { MachineConfirmationResult::Unavailable } else { MachineConfirmationResult::Stale };
    }
    if !confirmations.take_if(confirmation) {
        return MachineConfirmationResult::Stale;
    }
    if grants.authorize(&confirmation.id, &confirmation.fingerprint) { MachineConfirmationResult::Authorized } else { MachineConfirmationResult::Failed }
}

pub fn confirm_model(
    sample: bool,
    scope: &mut ModelScope,
    visible: &[Endpoint],
    current: &mut Option<ModelConfirmation>,
    confirmation: &ModelConfirmation,
    pending: bool,
) -> ModelConfirmationResult {
    if !sample {
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

/// Logical viewport dimensions, independent of GPUI so the gate is testable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    pub fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    pub fn usable(self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width >= 320.0 && self.height >= 200.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsentTarget {
    Machine(Confirmation),
    Model(ModelConfirmation),
}

#[derive(Clone)]
pub struct ConsentFrame {
    target: ConsentTarget,
    viewport: Viewport,
    scroll_offset: (f32, f32),
    nonce: Arc<()>,
}

/// Scroll state sampled after this frame was drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMetrics {
    pub bounds: Viewport,
    pub offset_x: f32,
    pub offset_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

const SCROLL_TOLERANCE_PX: f32 = 1.0;

impl ScrollMetrics {
    fn valid(self, viewport: Viewport) -> bool {
        let finite = [self.bounds.width, self.bounds.height, self.offset_x, self.offset_y, self.max_x, self.max_y].into_iter().all(f32::is_finite);
        finite
            && self.bounds.usable()
            && (self.bounds.width - viewport.width).abs() <= 1.0
            && (self.bounds.height - viewport.height).abs() <= 1.0
            && self.max_x >= 0.0
            && self.max_y >= 0.0
            && self.offset_x <= 0.0
            && self.offset_y <= 0.0
            && self.offset_x >= -self.max_x - 1.0
            && self.offset_y >= -self.max_y - 1.0
    }

    fn at_end(self) -> bool {
        self.max_x <= SCROLL_TOLERANCE_PX && self.offset_y <= -self.max_y + SCROLL_TOLERANCE_PX
    }
}

pub fn unavailable_notice(metrics: ScrollMetrics) -> &'static str {
    if metrics.max_x.is_finite() && metrics.max_x > SCROLL_TOLERANCE_PX {
        "横にはみ出しています。ウィンドウを拡大して対象を選び直してください。"
    } else {
        "確認内容の末尾まで移動し、表示後にもう一度操作してください。"
    }
}

/// A frame is valid only for the target and viewport that produced it.
#[derive(Default)]
pub struct ConsentScreen {
    target: Option<ConsentTarget>,
    frame: Option<ConsentFrame>,
    generation: u64,
    layout: Option<(Arc<()>, ScrollMetrics)>,
}

impl ConsentScreen {
    pub fn begin(&mut self, target: ConsentTarget) {
        let Some(generation) = self.generation.checked_add(1) else {
            self.invalidate();
            return;
        };
        self.generation = generation;
        self.target = Some(target);
        self.frame = None;
        self.layout = None;
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn target(&self) -> Option<&ConsentTarget> {
        self.target.as_ref()
    }

    pub fn current_frame(&self) -> Option<&ConsentFrame> {
        self.frame.as_ref()
    }

    pub fn present(&mut self, viewport: Viewport, scroll_offset: (f32, f32)) -> Option<ConsentFrame> {
        if let Some(frame) = &self.frame
            && viewport.usable()
            && frame.viewport == viewport
            && frame.scroll_offset == scroll_offset
            && self.target.as_ref() == Some(&frame.target)
        {
            return Some(frame.clone());
        }
        self.frame = None;
        self.layout = None;
        if !viewport.usable() || !scroll_offset.0.is_finite() || !scroll_offset.1.is_finite() {
            return None;
        }
        let frame = ConsentFrame { target: self.target.clone()?, viewport, scroll_offset, nonce: Arc::new(()) };
        self.frame = Some(frame.clone());
        Some(frame)
    }

    pub fn acknowledge_layout(&mut self, frame: &ConsentFrame, viewport: Viewport, metrics: ScrollMetrics) -> bool {
        if frame.viewport != viewport
            || !metrics.valid(viewport)
            || frame.scroll_offset != (metrics.offset_x, metrics.offset_y)
            || self.target.as_ref() != Some(&frame.target)
            || !self.frame.as_ref().is_some_and(|current| Arc::ptr_eq(&current.nonce, &frame.nonce))
        {
            return false;
        }
        self.layout = Some((frame.nonce.clone(), metrics));
        true
    }

    pub fn accepts(&self, frame: &ConsentFrame, target: &ConsentTarget, viewport: Viewport, active: bool, metrics: ScrollMetrics) -> bool {
        active
            && viewport.usable()
            && frame.viewport == viewport
            && &frame.target == target
            && self.target.as_ref() == Some(target)
            && self.frame.as_ref().is_some_and(|current| Arc::ptr_eq(&current.nonce, &frame.nonce))
            && self.layout.as_ref().is_some_and(|(nonce, rendered)| Arc::ptr_eq(nonce, &frame.nonce) && *rendered == metrics)
            && metrics.valid(viewport)
            && metrics.at_end()
    }

    pub fn invalidate(&mut self) {
        self.frame = None;
        self.target = None;
        self.layout = None;
    }
}

pub struct Presentation<'a> {
    pub screen: &'a ConsentScreen,
    pub frame: &'a ConsentFrame,
    pub viewport: Viewport,
    pub active: bool,
    pub metrics: ScrollMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerMark {
    pub x: f32,
    pub y: f32,
    pub clicks: usize,
}

#[derive(Default)]
pub struct ConsentPointer {
    pending: Option<ConsentPress>,
}

struct ConsentPress {
    target: ConsentTarget,
    frame: ConsentFrame,
    metrics: ScrollMetrics,
    mark: PointerMark,
}

impl ConsentPointer {
    pub fn down(&mut self, presentation: Presentation<'_>, target: &ConsentTarget, mark: PointerMark, primary: bool) -> bool {
        self.pending = None;
        if !primary
            || mark.clicks != 1
            || !mark.x.is_finite()
            || !mark.y.is_finite()
            || !presentation.screen.accepts(presentation.frame, target, presentation.viewport, presentation.active, presentation.metrics)
        {
            return false;
        }
        self.pending = Some(ConsentPress { target: target.clone(), frame: presentation.frame.clone(), metrics: presentation.metrics, mark });
        true
    }

    pub fn release(&mut self, presentation: Presentation<'_>, target: &ConsentTarget, mark: PointerMark, primary: bool) -> Option<ConsentFrame> {
        if !primary {
            self.pending = None;
            return None;
        }
        let press = self.pending.as_ref()?;
        if &press.target != target || press.mark != mark {
            return None;
        }
        let press = self.pending.take()?;
        if press.metrics != presentation.metrics
            || !presentation.screen.accepts(presentation.frame, target, presentation.viewport, presentation.active, presentation.metrics)
            || !presentation.screen.accepts(&press.frame, target, presentation.viewport, presentation.active, presentation.metrics)
        {
            return None;
        }
        Some(press.frame)
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }
}

pub fn confirmation_key_down(
    presentation: Presentation<'_>,
    gate: &mut crate::confirmation_keys::ConfirmKeyGate<ConsentTarget>,
    target: &ConsentTarget,
    held: bool,
    modified: bool,
) -> bool {
    if presentation.screen.accepts(presentation.frame, target, presentation.viewport, presentation.active, presentation.metrics) {
        gate.down(Some(target), held, modified)
    } else {
        gate.down(None, held, true);
        false
    }
}

pub fn confirm_machine_presented(
    presentation: Presentation<'_>,
    confirmations: &mut Confirmations,
    grants: &mut Grants,
    confirmation: &Confirmation,
) -> MachineConfirmationResult {
    let target = ConsentTarget::Machine(confirmation.clone());
    if !presentation.screen.accepts(presentation.frame, &target, presentation.viewport, presentation.active, presentation.metrics)
        || !confirmations.take_if(confirmation)
    {
        return MachineConfirmationResult::Stale;
    }
    if grants.authorize(&confirmation.id, &confirmation.fingerprint) { MachineConfirmationResult::Authorized } else { MachineConfirmationResult::Failed }
}

pub fn confirm_model_presented(
    presentation: Presentation<'_>,
    scope: &mut ModelScope,
    visible: &[Endpoint],
    current: &mut Option<ModelConfirmation>,
    confirmation: &ModelConfirmation,
    pending: bool,
) -> ModelConfirmationResult {
    let target = ConsentTarget::Model(confirmation.clone());
    if !presentation.screen.accepts(presentation.frame, &target, presentation.viewport, presentation.active, presentation.metrics) {
        return ModelConfirmationResult::Stale;
    }
    match llm_keyboard::confirm(scope, visible, current.as_ref(), confirmation, pending) {
        Some(query) => ModelConfirmationResult::Authorized(query),
        None => ModelConfirmationResult::Stale,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmationAction {
    Confirm,
    Cancel,
    Refresh,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Ignore,
}

pub fn confirmation_action(key: &str, held: bool, modified: bool, composing: bool) -> ConfirmationAction {
    if modified || composing {
        return ConfirmationAction::Ignore;
    }
    match key {
        "y" if !held => ConfirmationAction::Confirm,
        "escape" if !held => ConfirmationAction::Cancel,
        "r" if !held => ConfirmationAction::Refresh,
        "up" => ConfirmationAction::Up,
        "down" => ConfirmationAction::Down,
        "left" => ConfirmationAction::Left,
        "right" => ConfirmationAction::Right,
        "pageup" => ConfirmationAction::PageUp,
        "pagedown" => ConfirmationAction::PageDown,
        _ => ConfirmationAction::Ignore,
    }
}

/// Preserve printable characters while making invisible format characters explicit.
pub fn disclosure_text(value: &str) -> String {
    value.chars().flat_map(char::escape_debug).collect()
}
