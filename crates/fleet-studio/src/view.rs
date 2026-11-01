use std::cell::RefCell;
use std::collections::HashSet;
use std::time::Duration;

use crate::search_view::{SearchElement, SearchLayout};
use fleet_studio::Observation;
use fleet_studio::audit::{self, Audit, AuditIdentity, RepoAudit, Scope};
use fleet_studio::confirmation_keys::{ConfirmKeyGate, NavigationKeyGate};
use fleet_studio::llm::{
    self, Endpoint, ModelConfirmation, ModelEntry, ModelError, ModelGrant, ModelObservation, ModelQuery, ModelRevocation, ModelScope, RefreshQueue,
};
use fleet_studio::llm_keyboard::{self, ModelKey, ModelKeyboard, ModelViewGate};
use fleet_studio::navigation::{self, OpenResult, OpenState, OpenTarget};
use fleet_studio::pointer_id::PointerIds;
use fleet_studio::recent::{self, RecentEntry, RecentGate, RecentSlot, RecentTicket, RecentWork, SyncStatus};
use fleet_studio::refresh::{self, FullRefresh};
use fleet_studio::scope::{AuditGate, AuditTicket, Confirmation, Confirmations, Grants, Permit, RefreshGate};
use fleet_studio::search_input::{KeyAction, KeyModifiers, SearchInput, search_key};
use fleet_studio::source::{self, Coverage, Dashboard, NodeCard, Repository, SavedState};
use gpui::{Context, FocusHandle, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, Render, SharedString, Window, div, prelude::*, rgb};

#[derive(Clone, Copy)]
enum RecentOpen {
    Directory,
    PullRequest(usize),
}

#[derive(Clone, PartialEq, Eq)]
enum KeyboardConfirmation {
    Machine(Confirmation),
    Model(ModelConfirmation),
}

pub struct FleetView {
    pub(crate) focus: FocusHandle,
    _focus_out: gpui::Subscription,
    _activation: gpui::Subscription,
    sample: bool,
    recent_local: bool,
    generation: u64,
    loading: bool,
    load_timeout: bool,
    dashboard: Option<Dashboard>,
    grants: Grants,
    confirmations: Confirmations,
    machine_focus: Option<(String, String)>,
    revoking: HashSet<String>,
    dashboard_gate: RefreshGate,
    scope_check_pending: bool,
    access_notice: Option<&'static str>,
    model_scope: ModelScope,
    model_confirmation: Option<ModelConfirmation>,
    model_keyboard: ModelKeyboard,
    model_view: ModelViewGate,
    confirmation_keys: ConfirmKeyGate<KeyboardConfirmation>,
    navigation_keys: NavigationKeyGate,
    model_notice: Option<&'static str>,
    input_notice: Option<&'static str>,
    model_cancellation: Option<&'static str>,
    refresh_queue: RefreshQueue,
    sample_model: Option<ModelObservation>,
    recent: RecentSlot,
    recent_gate: RecentGate,
    audit: Option<Audit>,
    audit_gate: AuditGate,
    audit_error: Option<String>,
    auditing: bool,
    audit_seq: u64,
    topology_searching: bool,
    topology_pending: bool,
    topology_seq: u64,
    search_error: Option<String>,
    topology_filter: String,
    pub(crate) search: SearchInput,
    pub(crate) searching: bool,
    pub(crate) search_layout: Option<SearchLayout>,
    selecting: bool,
    pub(crate) paste_status: Option<&'static str>,
    pointer_ids: RefCell<PointerIds>,
    selected: navigation::Selection,
    open_state: OpenState,
}

impl FleetView {
    pub fn new(sample: bool, recent_local: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let focus_out = cx.on_focus_out(&focus, window, |view, _, _, cx| {
            view.lose_keyboard_focus(cx);
            view.leave_search(cx);
        });
        let activation = cx.observe_window_activation(window, |view, window, cx| {
            if !window.is_window_active() {
                view.lose_keyboard_focus(cx);
                view.leave_search(cx);
            }
        });
        let mut view = Self {
            focus,
            _focus_out: focus_out,
            _activation: activation,
            sample,
            recent_local,
            generation: 0,
            loading: false,
            load_timeout: false,
            dashboard: None,
            grants: Grants::default(),
            confirmations: Confirmations::default(),
            machine_focus: None,
            revoking: HashSet::new(),
            dashboard_gate: RefreshGate::default(),
            scope_check_pending: false,
            access_notice: None,
            model_scope: ModelScope::default(),
            model_confirmation: None,
            model_keyboard: ModelKeyboard::default(),
            model_view: ModelViewGate::default(),
            confirmation_keys: ConfirmKeyGate::default(),
            navigation_keys: NavigationKeyGate::default(),
            model_notice: None,
            input_notice: None,
            model_cancellation: None,
            refresh_queue: RefreshQueue::default(),
            sample_model: None,
            recent: RecentSlot::default(),
            recent_gate: RecentGate::default(),
            audit: None,
            audit_gate: AuditGate::default(),
            audit_error: None,
            auditing: false,
            audit_seq: 0,
            topology_searching: false,
            topology_pending: false,
            topology_seq: 0,
            search_error: None,
            topology_filter: String::new(),
            search: SearchInput::default(),
            searching: false,
            search_layout: None,
            selecting: false,
            paste_status: None,
            pointer_ids: RefCell::new(PointerIds::default()),
            selected: navigation::Selection::default(),
            open_state: OpenState::default(),
        };
        let executor = cx.background_executor().clone();
        cx.spawn(async move |view, cx| {
            loop {
                executor.timer(Duration::from_secs(30)).await;
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        view.refresh(cx);
        view
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.selected.invalidate();
        self.machine_focus = None;
        self.model_keyboard.exit();
        self.model_view.invalidate();
        self.confirmation_keys.clear();
        self.model_notice = None;
        self.pointer_ids.borrow_mut().clear();
        let decision = refresh::begin(FullRefresh {
            sample: self.sample,
            machine_confirmations: &mut self.confirmations,
            dashboard_gate: &mut self.dashboard_gate,
            scope_check_pending: &mut self.scope_check_pending,
            generation: &mut self.generation,
            audit_gate: &mut self.audit_gate,
            audit: &mut self.audit,
            recent: &mut self.recent,
            recent_gate: &mut self.recent_gate,
            models: &mut self.model_scope,
            model_confirmation: &mut self.model_confirmation,
            model_queue: &mut self.refresh_queue,
        });
        self.audit_error = if self.auditing {
            Some("更新により監査結果を非表示にしました。進行中の監査は終了待ちです。".into())
        } else if decision.had_audit {
            Some("更新により前回の監査結果を破棄しました。再監査は未実行です。".into())
        } else {
            None
        };
        if self.sample {
            self.model_scope = ModelScope::sample();
            self.scope_check_pending = false;
            self.dashboard = Some(sample_dashboard(&self.grants));
            let mut observation = ModelObservation::default();
            observation.record(Ok(vec!["fixture-model".into()]), tune_core::db::now_ms());
            self.sample_model = Some(observation);
            self.recent.data = Some(sample_recent());
            self.loading = false;
            cx.notify();
            return;
        }
        if let Some(generation) = decision.recent {
            self.start_recent(generation, cx);
        }
        if let Some(dashboard) = &mut self.dashboard {
            for node in &mut dashboard.nodes {
                if node.permit.is_some() {
                    node.observation = Observation::Unknown;
                    node.previous = node.last_at.is_some();
                }
            }
        }
        self.loading = true;
        self.start_dashboard(cx);
        if decision.models_now {
            self.start_model_refresh(cx);
        }
        cx.notify();
    }

    fn start_model_refresh(&mut self, cx: &mut Context<Self>) {
        if self.sample {
            return;
        }
        let configured = std::env::var("FLEET_STUDIO_LLM_ENDPOINTS").ok();
        self.model_confirmation = None;
        let requests = self.model_scope.refresh(configured.as_deref());
        self.reconcile_model_controls();
        for request in requests {
            self.start_model_query(request, cx);
        }
        cx.notify();
    }

    fn start_model_query(&mut self, request: ModelQuery, cx: &mut Context<Self>) {
        if self.sample || !self.model_scope.dispatch(&request) {
            return;
        }
        let queued = request.clone();
        let task =
            cx.background_executor().spawn(async move { queued.start(|endpoint| llm::models(endpoint.address, endpoint.protocol, Duration::from_secs(2))) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let at = tune_core::db::now_ms();
            let _ = view.update(cx, |view, cx| {
                let cancellation = view.model_scope.cancellation(&request);
                let refresh_cancelled = view.model_scope.cancellation_for_refresh(&request);
                view.model_scope.complete(&request, result, at);
                view.reconcile_model_controls();
                if let Some(cancellation) = cancellation.filter(|_| !refresh_cancelled) {
                    view.model_cancellation = Some(match cancellation {
                        ModelRevocation::Prevented => "取消した 1 件は未開始のまま完了しました。",
                        ModelRevocation::InProgress => "取消した 1 件が完了し、結果を破棄しました。",
                        ModelRevocation::Idle => "許可の取消が完了しました。",
                    });
                }
                view.resume_model_refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn model_status(&self, model: &ModelEntry) -> String {
        if self.sample {
            if model.authorized { "許可済み（サンプル・通信なし）" } else { "未照会（サンプル・通信なし）" }.into()
        } else {
            model_text(&model.observation, model.authorized, self.refresh_queue.pending())
        }
    }

    fn visible_model_endpoints(&self) -> Vec<Endpoint> {
        self.model_scope
            .entries()
            .iter()
            .filter(|model| llm_keyboard::matches(self.search.filter_text(), &model.endpoint.label, &self.model_status(model)))
            .map(|model| model.endpoint.clone())
            .collect()
    }

    fn dismiss_model_confirmation(&mut self) {
        llm_keyboard::dismiss(&mut self.model_scope, &mut self.model_confirmation);
    }

    fn leave_model_keyboard(&mut self) {
        self.model_keyboard.exit();
        self.model_view.invalidate();
        self.dismiss_model_confirmation();
        self.confirmation_keys.clear();
        self.model_notice = None;
    }

    fn reconcile_model_controls(&mut self) {
        let visible = self.visible_model_endpoints();
        self.model_view.observe(&visible);
        let selection_lost = self.model_keyboard.reconcile(&self.model_scope, &visible);
        let confirmation_hidden = self.model_confirmation.as_ref().is_some_and(|confirmation| !visible.contains(confirmation.endpoint()));
        if selection_lost || confirmation_hidden {
            self.model_view.invalidate();
            self.dismiss_model_confirmation();
            self.confirmation_keys.clear();
            self.pointer_ids.borrow_mut().clear();
            if selection_lost {
                self.model_notice = Some("選択先が非表示または変更されました。検索を解除し、対象を選び直してください。");
            }
        }
    }

    fn lose_keyboard_focus(&mut self, cx: &mut Context<Self>) {
        llm_keyboard::focus_lost(&mut self.model_keyboard, &mut self.model_scope, &mut self.model_confirmation, &mut self.selected);
        self.model_view.invalidate();
        self.machine_focus = None;
        self.confirmations.clear();
        self.confirmation_keys.focus_lost();
        self.navigation_keys.focus_lost();
        self.model_notice = None;
        self.input_notice = None;
        self.pointer_ids.borrow_mut().clear();
        cx.notify();
    }

    fn prepare_model_confirmation(&mut self, endpoint: &Endpoint, cx: &mut Context<Self>) {
        let visible = self.visible_model_endpoints();
        if self.refresh_queue.pending() || !visible.contains(endpoint) {
            return;
        }
        if self.model_keyboard.active() && self.model_keyboard.current(&self.model_scope, &visible) != Some(endpoint) {
            self.model_keyboard.exit();
        }
        self.machine_focus = None;
        self.confirmations.clear();
        self.dismiss_model_confirmation();
        self.confirmation_keys.clear();
        if let Some(confirmation) = llm_keyboard::prepare(&mut self.model_scope, &visible, endpoint, self.refresh_queue.pending()) {
            self.model_view.invalidate();
            if self.model_keyboard.current(&self.model_scope, &visible) == Some(endpoint) {
                self.confirmation_keys.arm(KeyboardConfirmation::Model(confirmation.clone()));
            }
            self.model_confirmation = Some(confirmation);
            self.model_cancellation = None;
            self.model_notice = None;
            self.pointer_ids.borrow_mut().clear();
            cx.notify();
        }
    }

    fn confirm_model(&mut self, confirmation: &ModelConfirmation, cx: &mut Context<Self>) {
        let visible = self.visible_model_endpoints();
        if let Some(request) =
            llm_keyboard::confirm(&mut self.model_scope, &visible, self.model_confirmation.as_ref(), confirmation, self.refresh_queue.pending())
        {
            self.model_view.invalidate();
            self.model_confirmation = None;
            self.confirmation_keys.clear();
            self.pointer_ids.borrow_mut().clear();
            let visible = self.visible_model_endpoints();
            self.model_keyboard.authorized(&self.model_scope, &visible, confirmation.endpoint());
            if self.model_keyboard.active() && self.model_keyboard.current(&self.model_scope, &visible).is_none() {
                self.model_notice = Some("選択先が非表示になりました。取消は検索を解除し、L と上下キーで対象を選び直して X を押してください。");
            }
            self.start_model_query(request, cx);
        }
        self.reconcile_model_controls();
        cx.notify();
    }

    fn cancel_model_confirmation(&mut self, confirmation: &ModelConfirmation, cx: &mut Context<Self>) {
        let visible = self.visible_model_endpoints();
        if llm_keyboard::cancel(&mut self.model_scope, &visible, self.model_confirmation.as_ref(), confirmation) {
            self.model_view.invalidate();
            self.model_confirmation = None;
            self.confirmation_keys.clear();
            self.pointer_ids.borrow_mut().clear();
            cx.notify();
        }
    }

    fn revoke_model(&mut self, grant: &ModelGrant, cx: &mut Context<Self>) {
        let visible = self.visible_model_endpoints();
        if let Some(outcome) = llm_keyboard::revoke(&mut self.model_scope, &visible, grant) {
            self.model_view.invalidate();
            self.machine_focus = None;
            self.confirmations.clear();
            self.model_confirmation = None;
            self.confirmation_keys.clear();
            self.model_cancellation = Some(match outcome {
                ModelRevocation::Prevented => "許可を取り消し、未開始の通信を阻止しました。",
                ModelRevocation::InProgress => "許可を取り消しました。通信の開始が確定しているため、停止は保証できません。",
                ModelRevocation::Idle => "許可と前回の結果を取り消しました。",
            });
            self.pointer_ids.borrow_mut().clear();
            self.reconcile_model_controls();
            self.resume_model_refresh(cx);
            cx.notify();
        }
    }

    fn resume_model_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_queue.ready(self.model_scope.querying()) {
            self.start_model_refresh(cx);
        }
    }

    /// Machine selection refreshes only saved summaries, never recent work or LLM endpoints.
    fn start_dashboard(&mut self, cx: &mut Context<Self>) {
        if self.sample {
            self.scope_check_pending = false;
            self.dashboard = Some(sample_dashboard(&self.grants));
            cx.notify();
            return;
        }
        self.scope_check_pending = true;
        let Some(ticket) = self.dashboard_gate.request() else { return };
        self.loading = true;
        self.load_timeout = false;
        let grants = self.grants.clone();
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                if view.dashboard_gate.is_current(ticket) {
                    view.loading = false;
                    view.load_timeout = true;
                    if let Some(data) = &mut view.dashboard {
                        for node in &mut data.nodes {
                            if matches!(node.observation, Observation::Current | Observation::Stale) {
                                node.observation = Observation::Unknown;
                            }
                        }
                        data.topology_coverage = Coverage::Unknown;
                        for repo in &mut data.repositories {
                            repo.previous = true;
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        let task = cx
            .background_executor()
            .spawn(async move { source::load_scoped(&tune_core::nodes::user_config_path(), &tune_core::nodes::data_dir(), tune_core::db::now_ms(), &grants) });
        cx.spawn(async move |view, cx| {
            let mut data = task.await;
            let _ = view.update(cx, |view, cx| {
                let Some(finished) = view.dashboard_gate.finish(ticket) else { return };
                if finished.accept {
                    view.scope_check_pending = false;
                    if data.config_error.is_some() {
                        view.grants.clear();
                    } else {
                        let fingerprints =
                            data.nodes.iter().filter_map(|node| node.fingerprint.as_ref().map(|fingerprint| (node.id.clone(), fingerprint.clone()))).collect();
                        view.grants.reconcile(&fingerprints);
                    }
                    source::discard_revoked(&mut data, &view.grants);
                    view.pointer_ids.borrow_mut().clear();
                    if let Some(previous) = &view.dashboard {
                        source::preserve_previous(previous, &mut data);
                    }
                    view.dashboard = Some(data);
                    view.confirmations.clear();
                    view.machine_focus = None;
                    view.selected.invalidate();
                    view.topology_filter.clear();
                    if !view.search.filter_text().is_empty() {
                        view.search_repositories(cx);
                    }
                }
                view.loading = false;
                view.load_timeout = false;
                view.revoking.clear();
                if finished.restart {
                    view.start_dashboard(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn request_machine_access(&mut self, id: String, fingerprint: String, cx: &mut Context<Self>) {
        if self.scope_check_pending || self.revoking.contains(&id) || self.grants.matching(&id, &fingerprint).is_some() {
            return;
        }
        let still_shown = self
            .dashboard
            .as_ref()
            .is_some_and(|dashboard| dashboard.nodes.iter().any(|node| node.id == id && node.fingerprint.as_deref() == Some(fingerprint.as_str())));
        if still_shown {
            self.leave_model_keyboard();
            self.machine_focus = Some((id.clone(), fingerprint.clone()));
            self.selected.invalidate();
            if let Some(confirmation) = self.confirmations.begin(&id, &fingerprint) {
                self.confirmation_keys.arm(KeyboardConfirmation::Machine(confirmation));
            } else {
                self.access_notice = Some("確認を開始できません。");
            }
            cx.notify();
        }
    }

    fn confirm_machine_access(&mut self, confirmation: Confirmation, cx: &mut Context<Self>) {
        let id = &confirmation.id;
        let fingerprint = &confirmation.fingerprint;
        if self.scope_check_pending || self.revoking.contains(id) || self.grants.matching(id, fingerprint).is_some() {
            return;
        }
        let still_shown = self
            .dashboard
            .as_ref()
            .is_some_and(|dashboard| dashboard.nodes.iter().any(|node| node.id == *id && node.fingerprint.as_deref() == Some(fingerprint.as_str())));
        if !still_shown || !self.confirmations.take_if(&confirmation) {
            return;
        }
        self.confirmation_keys.clear();
        if !self.grants.authorize(id, fingerprint) {
            self.access_notice = Some("表示許可を開始できません。");
            cx.notify();
            return;
        }
        self.dashboard_gate.invalidate();
        self.start_dashboard(cx);
        cx.notify();
    }

    fn cancel_machine_access(&mut self, confirmation: Confirmation, cx: &mut Context<Self>) {
        if self.confirmations.cancel_if(&confirmation) {
            self.confirmation_keys.clear();
            cx.notify();
        }
    }

    fn revoke_machine_access(&mut self, id: String, permit: Permit, cx: &mut Context<Self>) {
        if !self.grants.revoke_if_current(&id, &permit) {
            return;
        }
        self.leave_model_keyboard();
        self.dashboard_gate.invalidate();
        self.confirmations.clear();
        self.machine_focus = None;
        self.selected.invalidate();
        self.pointer_ids.borrow_mut().clear();
        if let Some(dashboard) = &mut self.dashboard {
            for node in dashboard.nodes.iter_mut().filter(|node| node.id == id) {
                node.clear_saved();
            }
        }
        if self.sample {
            self.dashboard = Some(sample_dashboard(&self.grants));
        } else if self.dashboard_gate.is_active() {
            self.revoking.insert(id);
            let _ = self.dashboard_gate.request();
        }
        cx.notify();
    }

    fn cycle_machine(&mut self, reverse: bool) {
        self.leave_model_keyboard();
        let choices: Vec<(String, String)> = self.dashboard.as_ref().map_or_else(Vec::new, |dashboard| {
            dashboard
                .nodes
                .iter()
                .filter(|node| self.matches(&format!("{} {} {}", node.id, node.role, node.operating_state)))
                .filter_map(|node| {
                    let fingerprint = node.fingerprint.as_ref()?;
                    if self.revoking.contains(&node.id) || (self.scope_check_pending && self.grants.matching(&node.id, fingerprint).is_none()) {
                        return None;
                    }
                    Some((node.id.clone(), fingerprint.clone()))
                })
                .collect()
        });
        let next = fleet_studio::scope::cycle_machine_confirmation(
            &mut self.confirmations,
            &mut self.confirmation_keys,
            &mut self.selected,
            &choices,
            self.machine_focus.as_ref(),
            reverse,
        );
        let Some(next) = next else {
            self.machine_focus = None;
            self.access_notice = Some("選択できる機体がありません。");
            return;
        };
        self.machine_focus = Some(next);
        self.access_notice = None;
    }

    fn machine_banner(&self) -> Option<String> {
        let (id, fingerprint) = self.machine_focus.as_ref()?;
        if self.confirmations.current().is_some_and(|pending| pending.id == *id && pending.fingerprint == *fingerprint) {
            if !cfg!(target_os = "windows") {
                Some(format!("台帳 ID {id}: キーボードでの最終許可は未対応です。確認後、画面のボタンを使用してください。"))
            } else {
                Some(format!("台帳 ID {id}: {}", fleet_studio::scope::MACHINE_CONFIRMATION_KEYBOARD))
            }
        } else if self.grants.matching(id, fingerprint).is_some() {
            Some(format!("台帳 ID {id}: Enter で表示許可を取り消す。Esc で機体選択を終了。"))
        } else {
            Some(format!("台帳 ID {id}: Enter で権限確認へ。Esc で機体選択を終了。"))
        }
    }

    fn activate_machine(&mut self, cx: &mut Context<Self>) {
        let Some((id, fingerprint)) = self.machine_focus.clone() else { return };
        if self.revoking.contains(&id) {
            return;
        }
        if let Some(permit) = self.grants.matching(&id, &fingerprint) {
            self.revoke_machine_access(id, permit, cx);
        } else if self.confirmations.current().is_some_and(|confirmation| confirmation.id == id && confirmation.fingerprint == fingerprint) {
            // A separate Y press is required to confirm.
        } else {
            self.request_machine_access(id, fingerprint, cx);
        }
    }

    fn confirm_focused_machine(&mut self, cx: &mut Context<Self>) {
        if !cfg!(target_os = "windows") {
            self.access_notice = Some("この OS のキーボード最終許可は未対応です。画面のボタンを使用してください。");
            return;
        }
        let Some((id, fingerprint)) = self.machine_focus.as_ref() else { return };
        if let Some(confirmation) = self.confirmations.current().filter(|pending| pending.id == *id && pending.fingerprint == *fingerprint).cloned() {
            self.confirm_machine_access(confirmation, cx);
        }
    }

    fn select_model(&mut self) {
        self.model_view.invalidate();
        self.machine_focus = None;
        self.confirmations.clear();
        self.dismiss_model_confirmation();
        self.confirmation_keys.clear();
        self.selected.invalidate();
        let visible = self.visible_model_endpoints();
        self.model_keyboard.begin(&self.model_scope, &visible);
        self.model_notice = if self.model_keyboard.current(&self.model_scope, &visible).is_none() {
            Some("表示中の選択可能な追加先はありません。検索条件を確認してください。")
        } else {
            None
        };
        self.pointer_ids.borrow_mut().clear();
    }

    fn cycle_model(&mut self, reverse: bool) {
        self.model_view.invalidate();
        self.dismiss_model_confirmation();
        self.confirmation_keys.clear();
        let visible = self.visible_model_endpoints();
        self.model_keyboard.cycle(&self.model_scope, &visible, reverse);
        self.model_notice = if self.model_keyboard.current(&self.model_scope, &visible).is_none() {
            Some("表示中の選択可能な追加先はありません。検索条件を確認してください。")
        } else {
            None
        };
        self.pointer_ids.borrow_mut().clear();
    }

    fn inspect_selected_model(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible_model_endpoints();
        let Some(endpoint) = self.model_keyboard.current(&self.model_scope, &visible).cloned() else { return };
        if self.model_confirmation.as_ref().is_some_and(|confirmation| confirmation.endpoint() == &endpoint) {
            return;
        }
        if self.model_scope.entries().iter().any(|entry| entry.endpoint == endpoint && entry.authorized) {
            self.model_notice = Some("許可済みです。取消は X を押してください。");
            return;
        }
        if self.refresh_queue.pending() {
            self.model_notice = Some("更新待ちの間は新たな許可を開始できません。既存の許可は取り消せます。");
            return;
        }
        self.prepare_model_confirmation(&endpoint, cx);
    }

    fn revoke_selected_model(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible_model_endpoints();
        let Some(endpoint) = self.model_keyboard.current(&self.model_scope, &visible) else { return };
        if let Some(grant) = self.model_scope.entries().iter().find(|entry| &entry.endpoint == endpoint).and_then(|entry| entry.grant()).cloned() {
            self.revoke_model(&grant, cx);
        }
    }

    fn keyboard_confirmation(&self) -> Option<KeyboardConfirmation> {
        if self.model_keyboard.active() {
            let visible = self.visible_model_endpoints();
            let endpoint = self.model_keyboard.current(&self.model_scope, &visible)?;
            let confirmation = self.model_confirmation.as_ref().filter(|confirmation| confirmation.endpoint() == endpoint)?;
            return Some(KeyboardConfirmation::Model(confirmation.clone()));
        }
        let (id, fingerprint) = self.machine_focus.as_ref()?;
        let shown = self.dashboard.as_ref().is_some_and(|dashboard| {
            dashboard.nodes.iter().any(|node| {
                node.id == *id && node.fingerprint.as_ref() == Some(fingerprint) && self.matches(&format!("{} {} {}", node.id, node.role, node.operating_state))
            })
        });
        shown.then_some(())?;
        self.confirmations
            .current()
            .filter(|confirmation| confirmation.id == *id && confirmation.fingerprint == *fingerprint)
            .cloned()
            .map(KeyboardConfirmation::Machine)
    }

    fn model_banner(&self) -> Option<String> {
        let visible = self.visible_model_endpoints();
        let endpoint = self.model_keyboard.current(&self.model_scope, &visible);
        let authorized = endpoint.is_some_and(|endpoint| self.model_scope.entries().iter().any(|entry| &entry.endpoint == endpoint && entry.authorized));
        llm_keyboard::banner(self.model_keyboard.active(), endpoint, self.model_confirmation.as_ref(), authorized, self.model_notice)
    }

    fn start_recent(&mut self, generation: u64, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                view.recent.timeout(generation);
                cx.notify();
            });
        })
        .detach();
        let local_opt_in = self.recent_local;
        let task = cx
            .background_executor()
            .spawn(async move { recent::load(&tune_core::nodes::user_config_path(), &tune_core::nodes::data_dir(), tune_core::db::now_ms(), local_opt_in) });
        cx.spawn(async move |view, cx| {
            let data = task.await;
            let _ = view.update(cx, |view, cx| {
                view.pointer_ids.borrow_mut().clear();
                if let Some(next) = view.recent.complete(generation, data) {
                    view.start_recent(next, cx);
                }
                view.selected.invalidate();
                cx.notify();
            });
        })
        .detach();
    }

    fn search_repositories(&mut self, cx: &mut Context<Self>) {
        if self.sample {
            return;
        }
        if self.topology_searching {
            self.topology_pending = true;
            return;
        }
        let query = self.search.filter_text().to_owned();
        let generation = self.generation;
        self.topology_searching = true;
        self.topology_seq = self.topology_seq.wrapping_add(1);
        let seq = self.topology_seq;
        self.search_error = None;
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                if view.topology_searching && view.topology_seq == seq {
                    view.search_error = Some("構成情報の検索が 5 秒を超過（終了待ち）".into());
                    if let Some(dashboard) = &mut view.dashboard {
                        dashboard.topology_coverage = Coverage::Unknown;
                        for repo in &mut dashboard.repositories {
                            repo.previous = true;
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        let filter = query.clone();
        let task = cx.background_executor().spawn(async move { source::search_topology(&tune_core::nodes::data_dir(), &filter, tune_core::db::now_ms()) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.topology_searching = false;
                if view.generation == generation && view.search.filter_text() == query {
                    match result {
                        Ok(result) => {
                            if let Some(dashboard) = &mut view.dashboard {
                                dashboard.repositories = result.repositories;
                                view.pointer_ids.borrow_mut().clear();
                                dashboard.topology_coverage = result.coverage;
                                dashboard.topology_oldest_at = result.oldest_at;
                                dashboard.topology_limited = result.limited;
                                dashboard.topology_error = None;
                                view.search_error = None;
                                view.topology_filter = query.clone();
                                view.selected.invalidate();
                            }
                        }
                        Err(error) => {
                            if let Some(dashboard) = &mut view.dashboard {
                                dashboard.topology_coverage = Coverage::Unknown;
                                for repo in &mut dashboard.repositories {
                                    repo.previous = true;
                                }
                            }
                            view.search_error = Some(error.into());
                        }
                    }
                }
                if std::mem::take(&mut view.topology_pending) {
                    view.search_repositories(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn audit_repositories(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.auditing {
            return;
        }
        self.audit_error = None;
        if scope == Scope::Fleet {
            self.audit_error = Some(audit::REMOTE_DISABLED.into());
            cx.notify();
            return;
        }
        self.audit_gate.invalidate_and_clear(&mut self.audit);
        self.pointer_ids.borrow_mut().clear();
        if self.sample {
            self.audit = Some(Audit {
                repos: vec![RepoAudit {
                    node: "fixture-node".into(),
                    path: "fixture/project".into(),
                    branch: Some("feature".into()),
                    dirty: Some(1),
                    unpushed: Some(2),
                    behind: Some(0),
                    local_only: None,
                    previous: false,
                    local: true,
                    at: Some(tune_core::db::now_ms()),
                    error: None,
                }],
                incomplete_nodes: Vec::new(),
                issues: Vec::new(),
                observed_nodes: vec!["fixture-node".into()],
                identity: None,
            });
            cx.notify();
            return;
        }
        self.auditing = true;
        self.audit_seq = self.audit_seq.wrapping_add(1);
        let seq = self.audit_seq;
        let ticket = self.audit_gate.ticket();
        let timer = cx.background_executor().timer(Duration::from_secs(65));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                if view.auditing && view.audit_seq == seq {
                    view.audit_error = Some("監査が時間上限を超過（終了待ち）".into());
                    cx.notify();
                }
            });
        })
        .detach();
        let task = cx.background_executor().spawn(async move { audit::collect(&tune_core::nodes::user_config_path(), scope) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.auditing = false;
                if !ticket.is_current() {
                    view.audit_error = Some("更新により進行中だった監査結果を破棄しました。再監査は未実行です。".into());
                } else {
                    match result {
                        Ok(audit) => {
                            view.pointer_ids.borrow_mut().clear();
                            view.audit = Some(audit);
                            view.audit_error = None;
                        }
                        Err(error) => view.audit_error = Some(error.into()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn search_changed(&mut self, cx: &mut Context<Self>) {
        self.leave_model_keyboard();
        self.selected.invalidate();
        cx.notify();
    }

    fn leave_search(&mut self, cx: &mut Context<Self>) {
        if self.searching {
            self.search.cancel_composition();
            self.searching = false;
            self.selecting = false;
            self.paste_status = None;
            self.search_changed(cx);
            self.search_repositories(cx);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let modified = modifiers.control || modifiers.platform || modifiers.alt || modifiers.shift || modifiers.function;
        let fresh = self.navigation_keys.down(key, event.is_held);
        let blocked = event.is_held || modified || !fresh;
        if !blocked {
            self.input_notice = None;
        }
        if !self.searching {
            self.reconcile_model_controls();
        }
        let current = if self.searching { None } else { self.keyboard_confirmation() };
        let confirmed = if key == "y" { self.confirmation_keys.down(current.as_ref(), event.is_held, modified || self.searching) } else { false };
        if self.searching {
            let m = event.keystroke.modifiers;
            let action = search_key(
                key,
                KeyModifiers { control: m.control, platform: m.platform, alt: m.alt, shift: m.shift, function: m.function },
                self.search.is_composing(),
                cfg!(target_os = "macos"),
            );
            match action {
                KeyAction::Ime | KeyAction::Text => return,
                KeyAction::Confirm | KeyAction::Leave => self.leave_search(cx),
                KeyAction::Paste => self.paste_status = Some("貼り付けは未対応です。"),
                KeyAction::SelectAll => self.search.select_all(),
                KeyAction::Left(extend) => self.search.move_left(extend),
                KeyAction::Right(extend) => self.search.move_right(extend),
                KeyAction::Home(extend) => self.search.move_home(extend),
                KeyAction::End(extend) => self.search.move_end(extend),
                KeyAction::Backspace => {
                    self.search.backspace();
                }
                KeyAction::Delete => {
                    self.search.delete();
                }
            }
            self.search_changed(cx);
            cx.stop_propagation();
            return;
        } else {
            if llm_keyboard::consume_blocked_shortcut(key, false, blocked) {
                if !fresh && !event.is_held && !modified {
                    self.input_notice = Some("キーを一度離し、もう一度押してください。");
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
            if let Some(action) = llm_keyboard::key(key, self.model_keyboard.active(), false, blocked, cfg!(target_os = "windows")) {
                match action {
                    ModelKey::Select => self.select_model(),
                    ModelKey::Next => self.cycle_model(false),
                    ModelKey::Previous => self.cycle_model(true),
                    ModelKey::Inspect => self.inspect_selected_model(cx),
                    ModelKey::Confirm => {
                        if confirmed && let Some(KeyboardConfirmation::Model(confirmation)) = current.as_ref() {
                            self.confirm_model(confirmation, cx);
                        } else if matches!(current.as_ref(), Some(KeyboardConfirmation::Model(_))) {
                            self.input_notice = Some("Y を一度離し、もう一度押してください。必要なら確認を開き直してください。");
                        }
                    }
                    ModelKey::Revoke => self.revoke_selected_model(cx),
                    ModelKey::Leave => self.leave_model_keyboard(),
                    ModelKey::Consume => {}
                    ModelKey::Unsupported => {
                        self.model_notice = Some("この OS の LLM キーボード選択は未対応です。画面のボタンを使用してください。");
                    }
                }
                self.focus.focus(window);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if key == "m" && !blocked {
                self.leave_model_keyboard();
            }
            if let Some(action) = fleet_studio::scope::machine_key(key, self.machine_focus.is_some(), false, blocked) {
                use fleet_studio::scope::MachineKey;
                match action {
                    MachineKey::Next => self.cycle_machine(false),
                    MachineKey::Previous => self.cycle_machine(true),
                    MachineKey::Activate => self.activate_machine(cx),
                    MachineKey::Confirm => {
                        if !cfg!(target_os = "windows") || confirmed {
                            self.confirm_focused_machine(cx);
                        } else if matches!(current.as_ref(), Some(KeyboardConfirmation::Machine(_))) {
                            self.input_notice = Some("Y を一度離し、もう一度押してください。必要なら確認を開き直してください。");
                        }
                    }
                    MachineKey::Leave => {
                        self.machine_focus = None;
                        self.confirmations.clear();
                        self.confirmation_keys.clear();
                    }
                    MachineKey::Consume => {}
                }
                self.focus.focus(window);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            match key {
                "/" | "slash" => {
                    self.searching = true;
                    self.leave_model_keyboard();
                    self.machine_focus = None;
                    self.confirmations.clear();
                    self.selected.invalidate();
                    cx.stop_propagation();
                }
                "r" => self.refresh(cx),
                "a" => self.audit_repositories(Scope::Local, cx),
                "down" => {
                    let count = self.repository_count()
                        + self.recent.data.as_ref().map_or(0, |data| data.entries.iter().filter(|entry| self.matches_recent(entry)).count());
                    self.selected.next(count);
                }
                "up" => {
                    let count = self.repository_count()
                        + self.recent.data.as_ref().map_or(0, |data| data.entries.iter().filter(|entry| self.matches_recent(entry)).count());
                    self.selected.previous(count);
                }
                "enter" => {
                    if let Some(entry) = self.selected_recent() {
                        let kind = if entry.local && entry.directory_url.is_some() { RecentOpen::Directory } else { RecentOpen::PullRequest(0) };
                        let ticket = self.recent_gate.ticket();
                        self.open_recent(entry, kind, ticket, cx);
                    } else if let Some(url) = self.selected_url() {
                        self.open_repository(&url, cx);
                    }
                }
                "escape" => {
                    self.leave_model_keyboard();
                    self.search.clear();
                    self.selected.invalidate();
                    self.search_repositories(cx);
                }
                _ => {}
            }
        }
        self.focus.focus(window);
        cx.notify();
    }

    fn on_key_up(&mut self, event: &KeyUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        self.navigation_keys.up(key);
        if key == "y" {
            self.confirmation_keys.up();
        }
    }

    fn matches(&self, value: &str) -> bool {
        self.search.filter_text().is_empty() || value.to_lowercase().contains(&self.search.filter_text().to_lowercase())
    }

    fn matches_recent(&self, entry: &RecentEntry) -> bool {
        self.matches(&format!("{} {} {}", entry.node, entry.tool, entry.cwd.as_deref().unwrap_or("")))
    }

    fn repository_count(&self) -> usize {
        self.dashboard.as_ref().map_or(0, |dashboard| dashboard.repositories.iter().filter(|repo| self.matches(&repo.name)).take(200).count())
    }

    fn selected_url(&self) -> Option<String> {
        let selected = self.selected.index()?;
        let repo_count = self.repository_count();
        if selected < repo_count {
            return self
                .dashboard
                .as_ref()?
                .repositories
                .iter()
                .filter(|repo| self.matches(&repo.name))
                .take(200)
                .nth(selected)?
                .origin
                .as_deref()
                .and_then(navigation::repository_url);
        }
        None
    }

    fn selected_recent(&self) -> Option<RecentEntry> {
        let index = self.selected.index()?.checked_sub(self.repository_count())?;
        self.recent.data.as_ref()?.entries.iter().filter(|entry| self.matches_recent(entry)).nth(index).cloned()
    }

    fn open_github(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Some(target) = OpenTarget::github(url) {
            self.open_target(target, None, cx);
        } else {
            self.open_failure(OpenResult::Failed, cx);
        }
    }

    fn open_repository(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Some(result) = navigation::sample_target_result(self.sample) {
            self.open_failure(result, cx);
        } else {
            self.open_github(url, cx);
        }
    }

    fn open_failure(&mut self, result: OpenResult, cx: &mut Context<Self>) {
        if let Some(id) = self.open_state.start() {
            self.open_state.finish(id, result);
            cx.notify();
        }
    }

    fn open_recent(&mut self, entry: RecentEntry, kind: RecentOpen, ticket: RecentTicket, cx: &mut Context<Self>) {
        if !ticket.is_current() {
            self.open_failure(OpenResult::SelectionChanged, cx);
            return;
        }
        let Some(data) = self.recent.data.as_ref() else {
            self.open_failure(OpenResult::SelectionChanged, cx);
            return;
        };
        if !entry.local || !data.entries.contains(&entry) {
            self.open_failure(OpenResult::SelectionChanged, cx);
            return;
        }
        if let Some(result) = navigation::sample_target_result(self.sample) {
            self.open_failure(result, cx);
            return;
        }
        let permit = if self.sample {
            None
        } else if let Some(fingerprint) = &data.selection_fingerprint {
            Some((entry.node.clone(), fingerprint.clone(), ticket))
        } else {
            self.open_failure(OpenResult::SelectionChanged, cx);
            return;
        };
        let target = match kind {
            RecentOpen::Directory if entry.directory_url.is_some() => entry.cwd.as_deref().and_then(OpenTarget::directory),
            RecentOpen::PullRequest(index) => entry.prs.get(index).and_then(|url| recent::github_pr_url(url)).and_then(|url| OpenTarget::github(&url)),
            _ => None,
        };
        if let Some(target) = target {
            self.open_target(target, permit, cx);
        } else {
            self.open_failure(OpenResult::Failed, cx);
        }
    }

    fn open_target(&mut self, target: OpenTarget, permit: Option<(String, String, RecentTicket)>, cx: &mut Context<Self>) {
        self.open_target_checked(target, permit, None, cx);
    }

    fn open_audit_target(&mut self, target: OpenTarget, ticket: AuditTicket, identity: AuditIdentity, cx: &mut Context<Self>) {
        self.open_target_checked(target, None, Some((ticket, identity)), cx);
    }

    fn open_target_checked(
        &mut self,
        target: OpenTarget,
        permit: Option<(String, String, RecentTicket)>,
        audit_permit: Option<(AuditTicket, AuditIdentity)>,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.open_state.start() else { return };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        if std::thread::Builder::new()
            .name("fleet-studio-open".into())
            .spawn(move || {
                let allowed = || {
                    audit_permit.as_ref().is_none_or(|(ticket, identity)| {
                        ticket.is_current()
                            && audit::revalidate_identity(&tune_core::nodes::user_config_path(), &tune_core::nodes::local_host(), identity)
                            && ticket.is_current()
                    }) && permit.as_ref().is_none_or(|(node, fingerprint, ticket)| {
                        ticket
                            .revalidate(|| recent::revalidate_local(&tune_core::nodes::user_config_path(), &tune_core::nodes::local_host(), node, fingerprint))
                    })
                };
                let result = target.open_guarded(allowed);
                let _ = sender.send(result);
            })
            .is_err()
        {
            self.open_state.finish(id, OpenResult::Failed);
            cx.notify();
            return;
        }
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| {
                view.open_state.timed_out(id);
                cx.notify();
            });
        })
        .detach();
        cx.spawn(async move |view, cx| {
            let result = receiver.await.unwrap_or(OpenResult::Failed);
            let _ = view.update(cx, |view, cx| {
                view.open_state.finish(id, result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Drop for FleetView {
    fn drop(&mut self) {
        self.grants.clear();
        self.audit_gate.close();
    }
}

fn sample_recent() -> RecentWork {
    let now = tune_core::db::now_ms();
    let cwd = if cfg!(windows) { r"C:\fixture\project" } else { "/fixture/project" };
    RecentWork {
        nodes: vec![SyncStatus {
            node: "fixture-node".into(),
            attempt_at: Some(now),
            last_ok_at: Some(now),
            failure: Some(false),
            pending: Some(false),
            file_errors: Some(0),
            session_error: false,
        }],
        entries: vec![RecentEntry {
            node: "fixture-node".into(),
            local: true,
            tool: "fixture-agent".into(),
            cwd: Some(cwd.into()),
            activity_at: Some(now),
            directory_url: recent::local_cwd_url(true, cwd),
            prs: vec!["https://github.com/example/fixture/pull/1".into()],
            pr_state: recent::PrState::Complete,
        }],
        max_age_ms: Some(90 * 60 * 1_000),
        validated_at: Some(now),
        ..Default::default()
    }
}

fn sample_dashboard(grants: &Grants) -> Dashboard {
    let now = tune_core::db::now_ms();
    let fingerprint = "synthetic-fingerprint";
    let permit = grants.matching("fixture-node", fingerprint);
    let approved = permit.is_some();
    Dashboard {
        nodes: vec![NodeCard {
            id: "fixture-node".into(),
            fingerprint: Some(fingerprint.into()),
            permit,
            saved_state: if approved { SavedState::Available } else { SavedState::Unapproved },
            previous: false,
            role: "research".into(),
            operating_state: "online".into(),
            local: Some(true),
            last_at: approved.then_some(now),
            observation: if approved { Observation::Current } else { Observation::Unknown },
            failure_reason: None,
            failure_at: None,
            max_age_ms: approved.then_some(180 * 60 * 1_000),
            score: approved.then_some(90),
            critical: approved.then_some(0),
            warnings: approved.then_some(1),
        }],
        config_error: None,
        database_error: None,
        repositories: vec![Repository {
            name: "fixture-repository".into(),
            origin: Some("https://github.com/example/fixture".into()),
            observed_at: Some(now),
            previous: false,
        }],
        topology_coverage: Coverage::Complete,
        topology_error: None,
        topology_oldest_at: Some(now),
        topology_limited: false,
    }
}

fn when(at: Option<i64>) -> String {
    at.and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "時刻不明".into())
}

fn observation_text(value: Observation) -> &'static str {
    match value {
        Observation::Current => "保存データが新しい",
        Observation::Stale => "古い",
        Observation::Failed => "取得失敗",
        Observation::Unknown => "不明",
        Observation::Excluded => "対象外",
    }
}

fn coverage_text(value: Coverage) -> &'static str {
    match value {
        Coverage::Complete => "取得済み",
        Coverage::Selected => "表示分のみ・全体は不明",
        Coverage::Partial => "一部・未確定",
        Coverage::Stale => "古い",
        Coverage::Unknown => "不明",
    }
}

fn model_text(observation: &ModelObservation, authorized: bool, refresh_pending: bool) -> String {
    if !authorized {
        return "未照会".into();
    }
    let text: String = match &observation.latest {
        None if refresh_pending => "更新待ち".into(),
        None => "取得中".into(),
        Some(Ok(models)) if models.is_empty() => "応答あり・モデル一覧は空".into(),
        Some(Ok(models)) => models.join(", "),
        Some(Err(ModelError::TimedOut)) => "タイムアウト".into(),
        Some(Err(ModelError::Unavailable)) => "接続できません".into(),
        Some(Err(ModelError::InvalidResponse)) => "応答を解釈できません".into(),
        Some(Err(ModelError::Http(code))) => format!("HTTP {code}"),
    };
    if !matches!(&observation.latest, Some(Ok(_)))
        && let Some((models, at)) = &observation.last_good
    {
        let age = match observation.last_good_freshness(tune_core::db::now_ms()) {
            Observation::Stale => "（古い）",
            Observation::Unknown => "（時刻不明）",
            _ => "",
        };
        return format!("{text} / 前回{age}: {} ({})", if models.is_empty() { "一覧は空".into() } else { models.join(", ") }, when(Some(*at)));
    }
    if matches!(&observation.latest, Some(Ok(_))) {
        match fleet_studio::observation(observation.checked_at, false, true, tune_core::db::now_ms(), llm::MODEL_MAX_AGE_MS) {
            Observation::Stale => return format!("古い / {text}"),
            Observation::Unknown => return format!("時刻不明 / {text}"),
            _ => {}
        }
    }
    text
}

fn number(value: Option<u64>) -> String {
    value.map_or_else(|| "不明".into(), |value| value.to_string())
}

fn recent_status(status: &SyncStatus, now: i64, max_age_ms: Option<i64>) -> String {
    let latest = match status.collection_state() {
        recent::CollectionState::Failed => "失敗",
        recent::CollectionState::Succeeded => "保存上は成功",
        recent::CollectionState::Unknown => "不明",
    };
    let (pending, partial) = if status.last_ok_at.is_some() {
        (
            match status.pending {
                Some(true) => "あり",
                Some(false) => "なし",
                None => "不明",
            },
            recent::file_error_count_text(status.file_errors),
        )
    } else {
        ("不明", "不明".into())
    };
    format!(
        "最新取込の記録: {latest} {} / 最終成功の記録: {}（{}） / 続き・再読込: {pending} / 一部失敗: {partial}{}",
        when(status.attempt_at),
        when(status.last_ok_at),
        observation_text(status.freshness(now, max_age_ms.unwrap_or(-1))),
        if status.session_error { " / 作業一覧の読取失敗" } else { "" },
    )
}

impl Render for FleetView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reconcile_model_controls();
        let visible_models = self.visible_model_endpoints();
        let model_ticket = self.model_view.ticket();
        let now = tune_core::db::now_ms();
        let mut pointer_ids = self.pointer_ids.borrow_mut();
        pointer_ids.begin_frame();
        let mut content = div().id("content").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5();
        if self.load_timeout {
            content = content.child(note("保存データの読込が 5 秒を超過（終了待ち）。前回の結果は不明として表示します。".into(), 0xffb86c));
        }
        if let Some(dashboard) = &self.dashboard {
            if let Some(error) = &dashboard.config_error {
                content = content.child(note(error.clone(), 0xffb86c));
            }
            if let Some(error) = &dashboard.database_error {
                content = content.child(note(error.clone(), 0xffb86c));
            }

            if let Some(message) = self.access_notice {
                content = content.child(note(message.into(), 0xffb86c));
            }
            content = content.child(section("機体", if self.loading { "読み込み中" } else { "台帳と、個別に許可した保存要約" }));
            content =
                content.child(note("保存情報は台帳 ID に紐づく過去の要約です。保存元の実機・本文の完全性・現在の接続や正常性は未確認です。".into(), 0x9baec4));
            content = content.child(note("台帳の照合は更新時点のものです。取消中は開始済みの読み取りを待ちます。".into(), 0x9baec4));
            for node in dashboard.nodes.iter().filter(|node| self.matches(&format!("{} {} {}", node.id, node.role, node.operating_state))) {
                let id = node.id.clone();
                let fingerprint = node.fingerprint.clone();
                let identity = node.identity_text();
                let focused = self
                    .machine_focus
                    .as_ref()
                    .is_some_and(|(focused_id, focused_fingerprint)| focused_id == &id && fingerprint.as_ref() == Some(focused_fingerprint));
                let current_permit = fingerprint.as_deref().and_then(|value| self.grants.matching(&id, value));
                let is_revoking = self.revoking.contains(&id);
                let show_saved = fleet_studio::scope::may_show_saved(&self.grants, &id, node.permit.as_ref(), self.scope_check_pending);
                let status = node.visible_status(now, show_saved, self.scope_check_pending && current_permit.is_some());
                let detail = if !show_saved {
                    format!("{status}  /  台帳: {}  /  {}", node.operating_state, identity)
                } else {
                    format!(
                        "{}{}  /  台帳: {}  /  {}  /  重大 {}・注意 {}・score {}  /  {}: {}{}{}",
                        status,
                        if node.previous { "（前回値・最新の読取は未確認）" } else { "" },
                        node.operating_state,
                        identity,
                        number(node.critical),
                        number(node.warnings),
                        node.score.map_or_else(|| "不明".into(), |value| value.to_string()),
                        if node.failure_at.is_some() { "前回保存要約の記録時刻" } else { "保存要約の記録時刻" },
                        when(node.last_at),
                        node.failure_at.map_or(String::new(), |at| format!("  /  保存上の失敗時刻: {}", when(Some(at)))),
                        node.failure_reason.map_or(String::new(), |reason| format!("  /  理由: {reason}")),
                    )
                };
                let mut item = div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(if focused { 0x233b56 } else { 0x182436 }))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(format!("{}  ·  {}", node.id, node.role))
                    .child(div().text_sm().text_color(rgb(0x9baec4)).child(detail));
                if is_revoking {
                    item = item.child(div().text_sm().text_color(rgb(0xffb86c)).child("取消処理中：開始済みの読み取りが終わるまで再許可できません。"));
                } else if let Some(permit) = current_permit {
                    let serial = permit.serial().to_string();
                    let button_id = pointer_ids.get("machine-revoke", &[&id, fingerprint.as_deref().unwrap_or(""), &serial]);
                    item = item.child(
                        div()
                            .id(SharedString::from(format!("machine-revoke-{button_id}")))
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(0x8fc9ff))
                            .child("この台帳 ID の表示許可を取り消す")
                            .on_click(cx.listener(move |view, _, _, cx| view.revoke_machine_access(id.clone(), permit.clone(), cx))),
                    );
                } else if self.scope_check_pending {
                    item = item.child(div().text_sm().child("台帳の再照合が終わるまで待機中です。"));
                } else if let Some(fingerprint) = fingerprint {
                    let pending = self.confirmations.current().filter(|pending| pending.id == id && pending.fingerprint == fingerprint).cloned();
                    if let Some(confirmation) = pending {
                        item = item.child(div().text_sm().text_color(rgb(0xffd9a1)).child(fleet_studio::scope::MACHINE_CONFIRMATION));
                        let nonce = confirmation.nonce.to_string();
                        let yes_id = pointer_ids.get("machine-confirm", &[&id, &fingerprint, &nonce]);
                        let cancel_id = pointer_ids.get("machine-cancel", &[&id, &fingerprint, &nonce]);
                        let cancel_confirmation = confirmation.clone();
                        item = item.child(
                            div()
                                .flex()
                                .gap_4()
                                .child(
                                    div()
                                        .id(SharedString::from(format!("machine-confirm-{yes_id}")))
                                        .cursor_pointer()
                                        .text_sm()
                                        .text_color(rgb(0x8fc9ff))
                                        .child("確認して表示")
                                        .on_click(cx.listener(move |view, _, _, cx| view.confirm_machine_access(confirmation.clone(), cx))),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("machine-cancel-{cancel_id}")))
                                        .cursor_pointer()
                                        .text_sm()
                                        .child("やめる")
                                        .on_click(cx.listener(move |view, _, _, cx| view.cancel_machine_access(cancel_confirmation.clone(), cx))),
                                ),
                        );
                    } else {
                        let button_id = pointer_ids.get("machine-request", &[&id, &fingerprint]);
                        item = item.child(
                            div()
                                .id(SharedString::from(format!("machine-request-{button_id}")))
                                .cursor_pointer()
                                .text_sm()
                                .text_color(rgb(0x8fc9ff))
                                .child("権限を確認して保存要約を表示")
                                .on_click(cx.listener(move |view, _, _, cx| view.request_machine_access(id.clone(), fingerprint.clone(), cx))),
                        );
                    }
                } else {
                    item = item.child(div().text_sm().text_color(rgb(0xffb86c)).child("台帳 ID の照合が不明なため選択できません。"));
                }
                content = content.child(item);
            }
            content = content.child(section("リポジトリ", &format!("構成情報: {}", coverage_text(dashboard.current_coverage(now)))));
            if let Some(error) = &dashboard.topology_error {
                content = content.child(note(error.clone(), 0xffb86c));
            }
            if let Some(error) = &self.search_error {
                content = content.child(note(error.clone(), 0xffb86c));
            }
            if self.topology_searching {
                content = content.child(note("保存済みの構成情報を検索中…".into(), 0x9baec4));
            }
            if dashboard.topology_limited {
                content = content.child(note("構成情報には取得上限または未確定候補があります。保存 DB の構成情報の検索は未対応です。".into(), 0xffb86c));
            }
            let search_unapplied = !self.sample && self.search.filter_text() != self.topology_filter;
            if search_unapplied {
                content = content.child(note("表示範囲を検索しています。保存 DB の構成情報の検索は未対応です。".into(), 0xffb86c));
            }
            let mut shown = 0;
            for (index, repo) in dashboard.repositories.iter().filter(|repo| self.matches(&repo.name)).take(200).enumerate() {
                shown += 1;
                let url = repo.origin.as_deref().and_then(navigation::repository_url);
                let mut item = div()
                    .id(SharedString::from(format!("click-{}", pointer_ids.get("repo", &[&repo.name, url.as_deref().unwrap_or("")]))))
                    .p_3()
                    .rounded_md()
                    .bg(rgb(if self.selected.index() == Some(index) { 0x233b56 } else { 0x182436 }))
                    .flex()
                    .flex_col()
                    .child(format!("{}{}", if self.selected.index() == Some(index) { "› " } else { "  " }, repo.name))
                    .child(div().text_sm().text_color(rgb(0x9baec4)).child(format!(
                        "{} / 取得: {}",
                        if repo.previous { "前回（取得失敗・未確認）" } else { observation_text(repo.freshness(now)) },
                        when(repo.observed_at)
                    )));
                if let Some(url) = url {
                    item = item.cursor_pointer().on_click(cx.listener(move |view, _, _, cx| view.open_repository(&url, cx)));
                }
                content = content.child(item);
            }
            let total = dashboard.repositories.iter().filter(|repo| self.matches(&repo.name)).count();
            if total > shown {
                content = content.child(note(format!("{total} 件中 {shown} 件を表示。検索で絞り込めます。"), 0x9baec4));
            }
            if shown == 0 {
                let has_error = dashboard.topology_error.is_some() || self.search_error.is_some();
                content = content.child(note(source::empty_repository_text(dashboard.current_coverage(now), has_error).into(), 0x9baec4));
            }
        } else {
            content = content.child(note("保存済みデータを読み込み中…".into(), 0x9baec4));
        }

        content = content.child(section("最近の作業", "保存済み・現在の稼働状態ではありません"));
        content = content
            .child(note("保存された取込上の記録です。収集の正常完了や網羅性は保証できず、台帳の機体 ID は接続先の本人確認ではありません。".into(), 0x9baec4));
        content = content.child(note(
            "既定は非表示。明示して起動した場合だけこの機体の親セッションを最大 20 件表示します。遠隔機は対象外です。検索は表示範囲の機体・ツール・作業場所です。".into(),
            0x9baec4,
        ));
        if self.recent.timed_out {
            content = content.child(note("最近の作業の読込が 5 秒を超過（終了待ち）。以前の内容は非表示です。".into(), 0xffb86c));
        } else if self.recent.in_flight.is_some() {
            content = content.child(note("最近の作業を読み込み中…".into(), 0x9baec4));
        }
        if let Some(recent) = &self.recent.data {
            content = content.child(note(format!("台帳照合: {}。以降の変更は自動反映されません。R で更新してください。", when(recent.validated_at)), 0x9baec4));
            if let Some(error) = recent.error {
                content = content.child(note(error.into(), 0xffb86c));
            }
            if recent.node_limited || recent.entries_limited || recent.selection_incomplete {
                content = content.child(note("対象または表示範囲に上限・未確認があります。表示外は未確認です。".into(), 0xffb86c));
            }
            if recent.local_ambiguous {
                content = content.child(note("この機体の照合が複数一致したため不明です。一致した機体の作業は表示しません。".into(), 0xffb86c));
            }
            for status in recent.nodes.iter().filter(|status| self.matches(&status.node)) {
                content = content.child(row(status.node.clone(), recent_status(status, now, recent.max_age_ms)));
            }
            let repo_count = self.repository_count();
            for (index, entry) in recent.entries.iter().filter(|entry| self.matches_recent(entry)).enumerate() {
                let collection = recent
                    .nodes
                    .iter()
                    .find(|status| status.node == entry.node)
                    .map(|status| recent_status(status, now, recent.max_age_ms))
                    .unwrap_or_else(|| "収集状態: 不明".into());
                let prs = match entry.pr_state {
                    recent::PrState::Unknown => "PR: 不明".to_owned(),
                    recent::PrState::Empty => "PR: なし".to_owned(),
                    recent::PrState::Complete => format!("PR: {} 件", entry.prs.len()),
                    recent::PrState::Partial => "PR: 一部未確認または上限".to_owned(),
                };
                let mut item = div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(if self.selected.index() == Some(repo_count + index) { 0x233b56 } else { 0x182436 }))
                    .flex()
                    .flex_col()
                    .child(format!("{}{} · {}", if self.selected.index() == Some(repo_count + index) { "› " } else { "  " }, entry.node, entry.tool))
                    .child(div().text_sm().text_color(rgb(0x9baec4)).child(format!(
                        "作業場所: {}{} / 最終活動: {}",
                        entry.cwd.as_deref().unwrap_or("不明"),
                        if entry.local { "" } else { "（遠隔・ローカルでは開けません）" },
                        when(entry.activity_at)
                    )))
                    .child(div().text_sm().text_color(rgb(0x9baec4)).child(collection))
                    .child(div().text_sm().text_color(rgb(0x9baec4)).child(prs));
                let mut actions = div().flex().gap_4().text_sm();
                if entry.local && entry.directory_url.is_some() {
                    let selected_entry = entry.clone();
                    let ticket = self.recent_gate.ticket();
                    actions = actions.child(
                        div()
                            .id(SharedString::from(format!(
                                "click-{}",
                                pointer_ids.get(
                                    "recent-dir",
                                    &[
                                        &entry.node,
                                        &entry.tool,
                                        entry.cwd.as_deref().unwrap_or(""),
                                        entry.directory_url.as_deref().unwrap_or(""),
                                        if entry.local { "local" } else { "remote" }
                                    ]
                                )
                            )))
                            .cursor_pointer()
                            .child("場所を表示")
                            .on_click(cx.listener(move |view, _, _, cx| view.open_recent(selected_entry.clone(), RecentOpen::Directory, ticket.clone(), cx))),
                    );
                }
                for pr_index in 0..entry.prs.len() {
                    let selected_entry = entry.clone();
                    let ticket = self.recent_gate.ticket();
                    actions = actions.child(
                        div()
                            .id(SharedString::from(format!(
                                "click-{}",
                                pointer_ids.get(
                                    "recent-pr",
                                    &[
                                        &entry.node,
                                        &entry.tool,
                                        entry.cwd.as_deref().unwrap_or(""),
                                        &entry.prs[pr_index],
                                        if entry.local { "local" } else { "remote" }
                                    ]
                                )
                            )))
                            .cursor_pointer()
                            .child(format!("PR {} を開く", pr_index + 1))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.open_recent(selected_entry.clone(), RecentOpen::PullRequest(pr_index), ticket.clone(), cx)
                            })),
                    );
                }
                item = item.child(actions);
                content = content.child(item);
            }
            if recent.entries.is_empty() && recent.error.is_none() {
                content = content.child(note(recent::empty_work_text(recent, now).into(), 0x9baec4));
            }
        }

        content = content.child(section("LLM 接続先", "モデル一覧のみ取得・推論なし"));
        if self.refresh_queue.pending() {
            content = content.child(note("更新を予約しました。実行中の読み込みと照会の完了後に再実行します。".into(), 0xffb86c));
        }
        if let Some(message) = self.model_cancellation {
            content = content.child(note(message.into(), 0xffb86c));
        }
        if self.model_scope.pending_cancellations() > 0 {
            content = content.child(note(format!("旧照会の完了待ち: {} 件。結果は表示しません。", self.model_scope.pending_cancellations()), 0xffb86c));
        }
        if let Some(observation) = &self.sample_model {
            content = content.child(row("サンプル LLM".into(), model_text(observation, true, false)));
        }
        if self.model_scope.rejected() > 0 {
            content = content.child(note(format!("不正な LLM 接続設定: {} 件", self.model_scope.rejected()), 0xffb86c));
        }
        if self.model_scope.duplicates() > 0 {
            content = content.child(note(format!("重複した LLM 接続設定: {} 件（重複分は照会しません）", self.model_scope.duplicates()), 0xffb86c));
        }
        for model in self.model_scope.entries() {
            let status = self.model_status(model);
            if !visible_models.contains(&model.endpoint) {
                continue;
            }
            let endpoint = model.endpoint.clone();
            let address = endpoint.address.to_string();
            let protocol = endpoint.protocol.label();
            let local = endpoint.address.ip().is_loopback();
            let mut item =
                div().flex().flex_col().gap_2().child(row(endpoint.label.clone(), format!("{}  /  今回: {}", status, when(model.observation.checked_at))));
            if self.model_keyboard.current(&self.model_scope, &visible_models) == Some(&endpoint) {
                item = item.child(note("キーボードで選択中".into(), 0x8fc9ff));
            }
            if !local {
                item = item.child(note(format!("追加先: {address} / {protocol} / 平文 HTTP"), 0xffb86c));
                if let Some(grant) = model.grant().cloned() {
                    item = item.child(note("取消前に開始・実行待ちの通信は、取り消せない場合があります。".into(), 0xffb86c));
                    let id = pointer_ids.get("llm-revoke", &[&address, protocol, &endpoint.label]);
                    let ticket = model_ticket.clone();
                    item = item.child(div().id(SharedString::from(format!("click-{id}"))).cursor_pointer().child("許可を取り消す").on_click(cx.listener(
                        move |view, _, _, cx| {
                            if view.model_view.is_current(&ticket) {
                                view.revoke_model(&grant, cx);
                            }
                        },
                    )));
                } else if self.refresh_queue.pending() {
                    item = item.child(note("更新待ちのため、接続内容の確認はできません。".into(), 0xffb86c));
                } else if let Some(confirmation) = self.model_confirmation.as_ref().filter(|confirmation| confirmation.endpoint() == &endpoint) {
                    item = item.child(note(format!("接続内容を確認: {}", confirmation.description()), 0xffb86c));
                    let confirm_id = pointer_ids.get("llm-confirm", &[&address, protocol, &endpoint.label]);
                    let cancel_id = pointer_ids.get("llm-cancel-confirmation", &[&address, protocol, &endpoint.label]);
                    let selected = confirmation.clone();
                    let cancel = confirmation.clone();
                    let confirm_ticket = model_ticket.clone();
                    let cancel_ticket = model_ticket.clone();
                    item = item.child(
                        div()
                            .flex()
                            .gap_3()
                            .child(div().id(SharedString::from(format!("click-{confirm_id}"))).cursor_pointer().child("この接続を有効化").on_click(
                                cx.listener(move |view, _, _, cx| {
                                    if view.model_view.is_current(&confirm_ticket) {
                                        view.confirm_model(&selected, cx);
                                    }
                                }),
                            ))
                            .child(div().id(SharedString::from(format!("click-{cancel_id}"))).cursor_pointer().child("キャンセル").on_click(cx.listener(
                                move |view, _, _, cx| {
                                    if view.model_view.is_current(&cancel_ticket) {
                                        view.cancel_model_confirmation(&cancel, cx);
                                    }
                                },
                            ))),
                    );
                } else {
                    let id = pointer_ids.get("llm-prepare", &[&address, protocol, &endpoint.label]);
                    let selected = endpoint.clone();
                    let ticket = model_ticket.clone();
                    item = item.child(div().id(SharedString::from(format!("click-{id}"))).cursor_pointer().child("接続内容を確認").on_click(cx.listener(
                        move |view, _, _, cx| {
                            if view.model_view.is_current(&ticket) {
                                view.prepare_model_confirmation(&selected, cx);
                            }
                        },
                    )));
                }
            }
            content = content.child(item);
        }
        content = content.child(section("リポジトリ衛生", if self.auditing { "取得中…" } else { "この機体のみ A / 遠隔は権限・接続先未確認のため未対応" }));
        content =
            content.child(note("最後に fetch した追跡参照との差です。探索先の一部欠落やアクセス拒否は元ツールで判別できない場合があります。".into(), 0x9baec4));
        if let Some(error) = &self.audit_error {
            content = content.child(note(error.clone(), 0xffb86c));
        }
        if let Some(audit) = &self.audit {
            for issue in &audit.issues {
                content = content
                    .child(note(format!("{}: {}{} / {}", issue.node, if issue.previous { "前回: " } else { "" }, issue.reason, when(issue.at)), 0xffb86c));
            }
            for repo in audit.repos.iter().filter(|repo| self.matches(&format!("{} {}", repo.node, repo.path))).take(200) {
                let detail = format!(
                    "{}  /  変更・未追跡 {}  /  未 push {}  /  behind {}  /  remote に無い {}  /  {}  /  {}{}",
                    repo.branch.as_deref().unwrap_or("branch 不明"),
                    number(repo.dirty),
                    number(repo.unpushed),
                    number(repo.behind),
                    number(repo.local_only),
                    if repo.previous {
                        "前回"
                    } else {
                        observation_text(fleet_studio::observation(repo.at, repo.error.is_some(), true, now, 60 * 60 * 1_000))
                    },
                    when(repo.at),
                    repo.error.map_or(String::new(), |error| format!(" / {error}"))
                );
                let local_path = repo.local.then(|| repo.path.clone());
                let mut item = div()
                    .id(SharedString::from(format!(
                        "click-{}",
                        pointer_ids.get("audit", &[&repo.node, &repo.path, if repo.local { "local" } else { "remote" }])
                    )))
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0x182436))
                    .flex()
                    .flex_col()
                    .child(format!("{}{} · {}", repo.node, if repo.local { "（この機体）" } else { "（台帳名・接続先 hostname 未照合）" }, repo.path))
                    .child(div().text_sm().text_color(rgb(0x9baec4)).child(detail));
                if let Some(path) = local_path {
                    let ticket = self.audit_gate.ticket();
                    let identity = audit.identity.clone().filter(|identity| identity.node == repo.node);
                    item = item.cursor_pointer().on_click(cx.listener(move |view, _, _, cx| {
                        if !ticket.is_current() {
                            view.open_failure(OpenResult::SelectionChanged, cx);
                        } else if let Some(result) = navigation::sample_target_result(view.sample) {
                            view.open_failure(result, cx);
                        } else if let Some(identity) = &identity {
                            if let Some(target) = OpenTarget::directory(&path) {
                                view.open_audit_target(target, ticket.clone(), identity.clone(), cx);
                            } else {
                                view.open_failure(OpenResult::Failed, cx);
                            }
                        } else {
                            view.open_failure(OpenResult::SelectionChanged, cx);
                        }
                    }));
                }
                content = content.child(item);
            }
            let total = audit.repos.iter().filter(|repo| self.matches(&format!("{} {}", repo.node, repo.path))).count();
            if total > 200 {
                content = content.child(note(format!("{total} 件中 200 件を表示。検索で絞り込めます。"), 0x9baec4));
            }
        } else if !self.auditing {
            let message = if self.audit_error.is_some() {
                "監査結果は未確認です。未確認のリポジトリは正常と数えません。"
            } else {
                "未実行。未確認のリポジトリは正常と数えません。"
            };
            content = content.child(note(message.into(), 0x9baec4));
        }

        pointer_ids.finish_frame();
        drop(pointer_ids);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x0d1725))
            .text_color(rgb(0xe7edf6))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_key_up(cx.listener(Self::on_key_up))
            .child(
                div()
                    .px_5()
                    .py_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(rgb(0x132237))
                    .child(div().flex().flex_col().child(div().text_xl().child("Fleet Studio")).child(
                        div().text_sm().text_color(rgb(0x9baec4)).child(if self.sample {
                            "サンプルデータ · 読み取り専用"
                        } else {
                            "KatalaTune のデータ · 読み取り専用"
                        }),
                    ))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("refresh")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(0x29486b))
                                    .cursor_pointer()
                                    .child("更新 R")
                                    .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                            )
                            .child(
                                div()
                                    .id("audit")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(0x29486b))
                                    .cursor_pointer()
                                    .child("この機体を監査 A")
                                    .on_click(cx.listener(|view, _, _, cx| view.audit_repositories(Scope::Local, cx))),
                            )
                            .child(
                                div()
                                    .id("audit-fleet")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(0x29486b))
                                    .cursor_pointer()
                                    .child("遠隔監査（未対応）")
                                    .on_click(cx.listener(|view, _, _, cx| view.audit_repositories(Scope::Fleet, cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .id("open-status")
                    .px_5()
                    .py_2()
                    .bg(rgb(0x192b42))
                    .text_sm()
                    .child(navigation::open_status_text(self.open_state.status()).unwrap_or("リンク操作の結果をここに表示します。")),
            )
            .when(!self.revoking.is_empty(), |root| {
                root.child(div().px_5().py_2().text_sm().text_color(rgb(0xffb86c)).child("保存情報の取消処理中：開始済みの読み取りを待っています。"))
            })
            .when_some(self.machine_banner(), |root, message| {
                root.child(div().px_5().py_2().text_sm().text_color(rgb(0xffd9a1)).bg(rgb(0x192b42)).child(message))
            })
            .when_some(self.model_banner(), |root, message| {
                root.child(div().px_5().py_2().text_sm().text_color(rgb(0xffd9a1)).bg(rgb(0x192b42)).child(message))
            })
            .when_some(self.input_notice, |root, message| root.child(div().px_5().py_2().text_sm().text_color(rgb(0xffd9a1)).bg(rgb(0x192b42)).child(message)))
            .child(
                div()
                    .id("search")
                    .px_5()
                    .py_3()
                    .bg(rgb(0x192b42))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child("検索 / :")
                    .child(div().flex_1().child(SearchElement { view: cx.entity() }))
                    .when(self.search.is_composing(), |container| container.child(div().text_sm().child("変換中: 確定前の条件で表示")))
                    .when_some(self.paste_status, |container, status| container.child(div().text_sm().child(status)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, event: &gpui::MouseDownEvent, window, cx| {
                            view.searching = true;
                            view.leave_model_keyboard();
                            view.machine_focus = None;
                            view.confirmations.clear();
                            view.focus.focus(window);
                            view.selecting = true;
                            if let Some(byte) = view.search_layout.as_ref().and_then(|layout| layout.byte_for_point(view.search.text(), event.position)) {
                                view.search.select_byte(byte, event.modifiers.shift);
                            }
                            view.search_changed(cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|view, event: &gpui::MouseMoveEvent, _, cx| {
                        if view.selecting
                            && let Some(byte) = view.search_layout.as_ref().and_then(|layout| layout.byte_for_point(view.search.text(), event.position))
                        {
                            view.search.select_byte(byte, true);
                            view.search_changed(cx);
                        }
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|view, _, _, _| view.selecting = false))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|view, _, _, _| view.selecting = false)),
            )
            .child(content)
            .child(
                div()
                    .px_5()
                    .py_3()
                    .flex()
                    .justify_between()
                    .bg(rgb(0x132237))
                    .child(if cfg!(target_os = "windows") {
                        "/ 検索 · M 機体 · L 追加先 · ↑↓ 選択 · Enter 確認 · Y 許可 · X 取消 · Esc 戻る"
                    } else {
                        "/ 検索 · M 機体 · ↑↓ 選択 · Enter 確認 · 許可はマウス · Esc 戻る"
                    })
                    .child(
                        div()
                            .flex()
                            .gap_4()
                            .child(
                                div()
                                    .id("documentation")
                                    .cursor_pointer()
                                    .child("説明")
                                    .on_click(cx.listener(|view, _, _, cx| view.open_github("https://github.com/katalalab/katala-tune", cx))),
                            )
                            .child(
                                div()
                                    .id("issues")
                                    .cursor_pointer()
                                    .child("Issues")
                                    .on_click(cx.listener(|view, _, _, cx| view.open_github("https://github.com/katalalab/katala-tune/issues", cx))),
                            ),
                    ),
            )
    }
}

fn section(title: &'static str, subtitle: &str) -> impl IntoElement {
    div()
        .pt_3()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_lg().child(title))
        .child(div().text_sm().text_color(rgb(0x9baec4)).child(subtitle.to_owned()))
}

fn row(title: String, detail: String) -> impl IntoElement {
    div().p_3().rounded_md().bg(rgb(0x182436)).flex().flex_col().child(title).child(div().text_sm().text_color(rgb(0x9baec4)).child(detail))
}

fn note(message: String, color: u32) -> impl IntoElement {
    div().p_2().text_sm().text_color(rgb(color)).child(message)
}
