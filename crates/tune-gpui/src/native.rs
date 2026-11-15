//! Katala Tune のネイティブ画面（Rust + GPUI）。WebView も Node も使わず、tune-core の Engine を直接呼ぶ。
//!
//! 実行: `katala-tune-gpui [--config 台帳.json] [--data データdir] [--selftest]`
//! `--selftest` は保存データの全ルートを読み、終了する（収集はしない）。
//! `--selftest --live <id>` だけは指定機体を一度ストリームし、3 CPU 点以上を確認する。

use crate::{
    browser::{Reader, Request, Route},
    collection::{self, Job, Kind},
    gate,
    input::{self, SearchInput, Submitted},
    model,
    panels::{LogLevel, Page, Panel, Tone},
    startup,
};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, MouseButton, UniformListScrollHandle, Window,
    WindowBounds, WindowOptions, actions, div, hsla, img, point, prelude::*, px, rgb, size, uniform_list,
};
use model::{Finding, Health, NodeView, Severity};
use tune_core::engine::{Engine, Host};
use tune_core::live::{Live, ProcessRoute, Stop};
use tune_core::nodes;

actions!(tune, [Quit, CloseWindow, GoOverview, NextPage, PreviousPage, Refresh, FocusSearch, FocusContent, ToggleFocus, NextRow, PreviousRow, CopyRecord]);

#[cfg(target_os = "macos")]
const FONT: &str = ".SystemUIFont";
#[cfg(not(target_os = "macos"))]
const FONT: &str = "Segoe UI";

mod palette {
    use gpui::{Hsla, rgb};
    pub fn bg() -> Hsla {
        rgb(0x111318).into()
    }
    pub fn panel() -> Hsla {
        rgb(0x181b22).into()
    }
    pub fn card() -> Hsla {
        rgb(0x1e222b).into()
    }
    pub fn line() -> Hsla {
        rgb(0x2a2f3a).into()
    }
    pub fn text() -> Hsla {
        rgb(0xe8eaef).into()
    }
    pub fn muted() -> Hsla {
        rgb(0x8b93a3).into()
    }
    pub fn accent() -> Hsla {
        rgb(0x4f8cff).into()
    }
    pub fn ok() -> Hsla {
        rgb(0x3fbf7f).into()
    }
    pub fn warn() -> Hsla {
        rgb(0xe5a93d).into()
    }
    pub fn crit() -> Hsla {
        rgb(0xe5584f).into()
    }
}

/// 分析・取り込みの完了などを画面側に知らせるだけの Host（描画はここでは触らない）
struct DirtyHost(Arc<AtomicBool>);

impl Host for DirtyHost {
    fn logs_synced(&self, _: &serde_json::Value) {
        self.0.store(true, Ordering::Release);
    }
    fn inventory_result(&self, _: &serde_json::Value) {
        self.0.store(true, Ordering::Release);
    }
    fn ai_synced(&self, _: &serde_json::Value) {
        self.0.store(true, Ordering::Release);
    }
    fn topology_updated(&self, _: &serde_json::Value) {
        self.0.store(true, Ordering::Release);
    }
    fn probe_result(&self, _: &serde_json::Value) {
        self.0.store(true, Ordering::Release);
    }
    fn checks_updated(&self) {
        self.0.store(true, Ordering::Release);
    }
    fn state_changed(&self) {
        self.0.store(true, Ordering::Release);
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn score_color(h: &Health) -> Hsla {
    match h {
        Health::Scored(s) if *s >= 80 => palette::ok(),
        Health::Scored(s) if *s >= 55 => palette::warn(),
        Health::Scored(_) | Health::Failed(_) => palette::crit(),
        Health::Unobserved | Health::Excluded(_) => palette::muted(),
    }
}

fn severity_color(s: Severity) -> Hsla {
    match s {
        Severity::Critical => palette::crit(),
        Severity::Warn => palette::warn(),
        Severity::Info => palette::muted(),
    }
}

/// 棒の推移グラフ。0〜`max` を高さにし、点が無ければ空の枠だけ描く
fn trend(values: &[f64], max: f64, color: Hsla, height: f32) -> gpui::Div {
    let mut bars = div().h(px(height)).flex().items_end().gap_px();
    for v in values.iter().rev().take(model::LIVE_KEEP).rev() {
        let h = ((v / max).clamp(0.0, 1.0) as f32 * height).max(1.0);
        bars = bars.child(div().w(px(2.0)).h(px(h)).rounded_sm().bg(color));
    }
    bars
}

fn chip(text: impl Into<gpui::SharedString>, color: Hsla) -> gpui::Div {
    div().px_2().py_0p5().rounded_md().text_xs().text_color(color).bg(hsla(color.h, color.s, color.l, 0.14)).child(text.into())
}

struct LiveHandles {
    live: Arc<Live>,
    store: Arc<Mutex<model::LiveStore>>,
    dirty: Arc<AtomicBool>,
}

struct LiveIntent {
    generation: u64,
    id: String,
    started: bool,
    pending: bool,
    heartbeat: Instant,
}

#[derive(Default)]
struct LiveState {
    generation: u64,
    active: bool,
    intent: Option<LiveIntent>,
    notice: Option<String>,
}

/// The short state lock serializes start/stop ownership. Ledger I/O happens before
/// taking it, so navigation can invalidate an in-flight load without blocking GPUI.
struct LiveControl {
    live: Arc<Live>,
    state: Mutex<LiveState>,
}

impl LiveControl {
    fn new(live: Arc<Live>, active: bool) -> Self {
        Self { live, state: Mutex::new(LiveState { active, ..LiveState::default() }) }
    }

    fn stop_locked(&self, state: &mut LiveState, why: Stop) {
        state.generation = state.generation.wrapping_add(1);
        if let Some(intent) = state.intent.take() {
            self.live.stop(Some(&[intent.id]), why);
        }
    }

    fn stop(&self, why: Stop) {
        self.stop_locked(&mut self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner), why);
    }

    fn set_active(&self, active: bool) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = active;
        if !active {
            self.stop_locked(&mut state, Stop::Hidden);
        }
    }

    fn request(&self, id: &str) -> Option<(u64, String)> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.stop_locked(&mut state, Stop::User);
        if !state.active {
            state.notice = Some("ライブは前面のウィンドウから開始してください".into());
            return None;
        }
        state.notice = None;
        let generation = state.generation;
        state.intent = Some(LiveIntent { generation, id: id.into(), started: false, pending: true, heartbeat: Instant::now() });
        Some((generation, id.into()))
    }

    fn streaming(&self, id: &str) -> bool {
        self.live.active().iter().any(|active| active == id) && self.live.brief(id).is_some_and(|brief| brief["streaming"].as_bool() == Some(true))
    }

    fn terminal_notice(&self, id: &str) -> String {
        let brief = self.live.brief(id);
        let detail = brief.as_ref().and_then(|b| b["detail"].as_str()).unwrap_or("接続が終了しました");
        format!("ライブ停止: {detail}。続ける場合は手動で開始してください。")
    }

    /// Apply a fresh complete ledger only while this exact request still owns the
    /// selected route. An obsolete load never starts or stops a newer stream.
    fn complete(&self, generation: u64, id: &str, nodes: &[nodes::Node], config_error: Option<String>) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(intent) = state.intent.as_ref().filter(|i| i.generation == generation && i.id == id) else { return };
        if !state.active {
            return;
        }
        if let Some(error) = config_error {
            self.stop_locked(&mut state, Stop::Excluded);
            state.notice = Some(format!("ライブ停止: 台帳を読み込めません: {error}"));
            return;
        }
        let started = intent.started;
        self.live.retain_active(nodes);
        let target = nodes.iter().find(|node| node.id == id);
        if started {
            // This path may only renew an existing stream. The backend performs
            // the presence check and renewal atomically, so terminal failure
            // between observations cannot reset its retry budget.
            let refusal = target.map_or_else(|| Some("台帳に無い機体".into()), nodes::Node::refusal);
            if let Some(detail) = refusal {
                self.stop_locked(&mut state, Stop::Excluded);
                state.notice = Some(format!("ライブ停止: {detail}"));
                return;
            }
            if !self.live.heartbeat(id) {
                let notice = self.terminal_notice(id);
                self.stop_locked(&mut state, Stop::User);
                state.notice = Some(notice);
                return;
            }
        } else {
            let target: Vec<_> = target.cloned().into_iter().collect();
            let unknown = if target.is_empty() { vec![id.to_string()] } else { Vec::new() };
            let result = self.live.start(&target, &unknown);
            if let Some(refusal) = result["refused"].as_array().and_then(|r| r.first()) {
                let detail = refusal["detail"].as_str().unwrap_or("開始できません");
                let notice = format!("ライブを開始できません: {detail}");
                self.stop_locked(&mut state, Stop::User);
                state.notice = Some(notice);
                return;
            }
        }
        if let Some(intent) = state.intent.as_mut() {
            intent.started = true;
            intent.pending = false;
            intent.heartbeat = Instant::now();
        }
    }

    /// Apply only the accepted Reader generation. Excluded findings are visible
    /// immediately, so the matching owned stream must stop at that same boundary.
    fn accept_nodes(&self, nodes: &[NodeView]) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.intent.as_ref().is_some_and(|intent| nodes.iter().any(|node| node.id == intent.id && matches!(node.health, Health::Excluded(_)))) {
            self.stop_locked(&mut state, Stop::Excluded);
        }
    }

    /// Called by the small UI timer. Only in-memory backend state is read here.
    fn poll(&self) -> Option<(u64, String)> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let intent = state.intent.as_ref()?;
        if intent.started && !self.streaming(&intent.id) {
            let notice = self.terminal_notice(&intent.id);
            self.stop_locked(&mut state, Stop::User);
            state.notice = Some(notice);
            return None;
        }
        let active = state.active;
        let intent = state.intent.as_mut()?;
        if active && intent.started && !intent.pending && intent.heartbeat.elapsed() >= Duration::from_secs(30) {
            intent.pending = true;
            return Some((intent.generation, intent.id.clone()));
        }
        None
    }

    fn on(&self, id: &str) -> bool {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).intent.as_ref().is_some_and(|i| i.id == id)
    }

    fn take_notice(&self) -> Option<String> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).notice.take()
    }
}

struct TuneApp {
    brand: Arc<gpui::Image>,
    live: Arc<LiveControl>,
    live_store: Arc<Mutex<model::LiveStore>>,
    live_dirty: Arc<AtomicBool>,
    engine: Arc<Engine>,
    rt: Arc<tokio::runtime::Runtime>,
    dirty: Arc<AtomicBool>,
    nodes: Vec<NodeView>,
    selected: Option<String>,
    page: Page,
    reader: Reader,
    generation: u64,
    loading: bool,
    panel: Option<Result<Panel, String>>,
    query: String,
    log_level: LogLevel,
    search: Entity<SearchInput>,
    focus: FocusHandle,
    selected_row: Option<usize>,
    scroll: UniformListScrollHandle,
    probing: bool,
    collection: Job,
    notice: Option<String>,
    live_notice: Option<String>,
    /// 変更操作の承認の関門（確認欄）。進むのは「実行する」を押したときだけ
    gate: gate::Gate,
    /// 変更操作の結果（画面上部に一行で出す）
    action_result: Arc<Mutex<Option<String>>>,
}

impl TuneApp {
    fn new(
        engine: Arc<Engine>,
        rt: Arc<tokio::runtime::Runtime>,
        dirty: Arc<AtomicBool>,
        handles: LiveHandles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reader = Reader::new(engine.clone(), &rt);
        let ledger: Vec<_> = engine.config().nodes.iter().map(nodes::Node::public).collect();
        let search = cx.new(SearchInput::new);
        cx.subscribe(&search, |app, field, _: &Submitted, cx| {
            app.query = field.read(cx).text().to_owned();
            app.selected_row = None;
            app.scroll = UniformListScrollHandle::new();
            app.reload(cx);
        })
        .detach();
        let focus = cx.focus_handle();
        focus.focus(window);
        let live = Arc::new(LiveControl::new(handles.live, window.is_window_active()));
        cx.on_release(|app, _| app.live.stop(Stop::Hidden)).detach();
        cx.observe_window_activation(window, |app, window, cx| {
            app.live.set_active(window.is_window_active());
            cx.notify();
        })
        .detach();
        // 確認欄が出た・消えたときの描き直しは、live_dirty の旗（cx.notify だけ）で知らせる
        let gate_dirty = handles.dirty.clone();
        let mut app = Self {
            brand: Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, include_bytes!("../../../build/mark.png").to_vec())),
            live,
            live_store: handles.store,
            live_dirty: handles.dirty,
            engine,
            rt,
            dirty,
            nodes: model::build(&ledger, &[]),
            selected: None,
            page: Page::Overview,
            reader,
            generation: 0,
            loading: false,
            panel: None,
            query: String::new(),
            log_level: LogLevel::All,
            search,
            focus,
            selected_row: None,
            scroll: UniformListScrollHandle::new(),
            probing: false,
            collection: Job::default(),
            notice: None,
            live_notice: None,
            gate: gate::Gate::new(move || gate_dirty.store(true, Ordering::Release)),
            action_result: Arc::new(Mutex::new(None)),
        };
        app.reload(cx);
        // Only poll small notification/mailbox state. All SQLite work is in Reader.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                if this
                    .update(cx, |app, cx| {
                        if let Some(request) = app.live.poll() {
                            app.refresh_live(request);
                        }
                        if let Some(notice) = app.live.take_notice() {
                            app.live_notice = Some(notice);
                            cx.notify();
                        }
                        if app.live_dirty.swap(false, Ordering::AcqRel) {
                            cx.notify();
                        }
                        if app.collection.poll() {
                            // Receipt ownership is independent of route/query generations.
                            app.reload(cx);
                        }
                        let probing = app.engine.is_probing();
                        if app.dirty.swap(false, Ordering::AcqRel) || probing != app.probing {
                            app.probing = probing;
                            app.reload(cx);
                        }
                        if let Some(response) = app.reader.take() {
                            // A late response belongs to its original navigation/query.
                            if response.generation == app.generation {
                                app.loading = false;
                                if response.config_error.is_some() {
                                    app.live.stop(Stop::Excluded);
                                }
                                app.notice = response.config_error;
                                match response.nodes {
                                    Ok(nodes) => {
                                        app.live.accept_nodes(&nodes);
                                        app.nodes = nodes;
                                    }
                                    Err(error) => app.notice = Some(error),
                                }
                                app.panel = response.panel;
                                if app.selected.as_ref().is_some_and(|id| !app.nodes.iter().any(|n| &n.id == id)) {
                                    app.navigate(Page::Overview, None, cx);
                                }
                                if app.selected_row.is_some_and(|i| app.panel.as_ref().and_then(|p| p.as_ref().ok()).is_none_or(|p| i >= p.rows.len())) {
                                    app.selected_row = None;
                                }
                                cx.notify();
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        app
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.loading = true;
        let route = self.selected.clone().map_or(Route::Page(self.page), Route::Node);
        if let Err(error) = self.reader.request(Request { generation: self.generation, route, query: self.query.clone(), log_level: self.log_level }) {
            self.loading = false;
            self.panel = Some(Err(error));
        }
        cx.notify();
    }

    fn navigate(&mut self, page: Page, selected: Option<String>, cx: &mut Context<Self>) {
        if self.selected != selected {
            self.live.stop(Stop::User);
        }
        self.live_notice = None;
        self.live.take_notice();
        self.page = page;
        self.selected = selected;
        self.query.clear();
        self.panel = None;
        self.selected_row = None;
        self.scroll = UniformListScrollHandle::new();
        self.search.update(cx, |field, cx| field.reset(page.search_hint(), cx));
        self.reload(cx);
    }

    fn refresh_live(&self, (generation, id): (u64, String)) {
        let engine = self.engine.clone();
        let control = self.live.clone();
        self.rt.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let cfg = engine.reload_config();
                (cfg.nodes, engine.config_error())
            })
            .await;
            match result {
                Ok((nodes, error)) => control.complete(generation, &id, &nodes, error),
                Err(_) => control.complete(generation, &id, &[], Some("台帳の読み込みタスクが停止しました".into())),
            }
        });
    }

    fn live_start(&mut self, id: &str, window: &Window) {
        self.live_notice = None;
        self.live.set_active(window.is_window_active());
        if self.selected.as_deref() != Some(id) {
            self.live_notice = Some("ライブを開始する機体を先に開いてください".into());
            return;
        }
        if let Some(request) = self.live.request(id) {
            self.refresh_live(request);
        }
    }

    fn cycle_page(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
        let i = self
            .selected
            .as_ref()
            .and_then(|id| self.nodes.iter().position(|n| &n.id == id))
            .map(|i| i + Page::ALL.len())
            .unwrap_or_else(|| Page::ALL.iter().position(|p| *p == self.page).unwrap_or(0));
        let next = (i as isize + direction).rem_euclid((Page::ALL.len() + self.nodes.len()) as isize) as usize;
        self.focus.focus(window);
        if next < Page::ALL.len() {
            self.navigate(Page::ALL[next], None, cx);
        } else {
            self.navigate(Page::Overview, Some(self.nodes[next - Page::ALL.len()].id.clone()), cx);
        }
    }

    fn select_row(&mut self, direction: isize, cx: &mut Context<Self>) {
        let Some(Ok(panel)) = self.panel.as_ref() else {
            return;
        };
        if panel.rows.is_empty() {
            return;
        }
        let next = self
            .selected_row
            .map_or(if direction > 0 { 0 } else { panel.rows.len() - 1 }, |i| (i as isize + direction).clamp(0, panel.rows.len() as isize - 1) as usize);
        self.selected_row = Some(next);
        self.scroll.scroll_to_item(next, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// 提案された変更操作を実行する。確認（承認の関門）を通り、許可リスト・共用機や保護対象の拒否・
    /// 確認後の台帳の読み直し・実行記録はすべて tune-core の `confirm_and_run` が行う
    fn run_action(&mut self, node_id: String, action: serde_json::Value, label: String, cx: &mut Context<Self>) {
        let (engine, gate, result, flag, dirty) =
            (self.engine.clone(), self.gate.clone(), self.action_result.clone(), self.live_dirty.clone(), self.dirty.clone());
        self.rt.spawn(async move {
            let r = engine.confirm_and_run(&node_id, &action, &format!("{node_id}: {label}"), None, gate.confirm()).await;
            *result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(model::action_message(&node_id, r));
            dirty.store(true, Ordering::Release);
            flag.store(true, Ordering::Release);
        });
        cx.notify();
    }

    /// 実行記録から元に戻す（戻すときも確認する）
    fn undo_action(&mut self, entry_id: String, cx: &mut Context<Self>) {
        let (engine, gate, result, flag, dirty) =
            (self.engine.clone(), self.gate.clone(), self.action_result.clone(), self.live_dirty.clone(), self.dirty.clone());
        self.rt.spawn(async move {
            let r = engine.undo(&entry_id, gate.confirm()).await;
            *result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(model::action_message("", r));
            dirty.store(true, Ordering::Release);
            flag.store(true, Ordering::Release);
        });
        cx.notify();
    }

    fn collect(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if kind == Kind::Probe && self.probing {
            return;
        }
        let Some(sender) = self.collection.begin(kind) else {
            return;
        };
        let engine = self.engine.clone();
        let handle = self.rt.handle().clone();
        self.rt.spawn(async move {
            // Engine entry points also do synchronous ledger/SQLite work. Keep
            // the entire invocation off GPUI, including the immediate reread.
            let result = tokio::task::spawn_blocking(move || handle.block_on(collection::run(engine, kind, None)))
                .await
                .unwrap_or_else(|_| collection::Receipt::error(kind, "収集タスクが停止しました"));
            let _ = sender.send(result);
        });
        cx.notify();
    }

    fn collection_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut status = div().id("collection-status").flex_none().max_h(px(190.)).overflow_y_scroll().px_5().py_2().text_xs();
        if let Some(kind) = self.collection.running {
            status = status.child(div().text_color(palette::accent()).child(format!("{}を実行中…（画面を移動しても継続します）", kind.label())));
        }
        if let Some(result) = &self.collection.result {
            status =
                status
                    .child(
                        div().flex().items_center().gap_3().child(div().flex_1().child(result.summary())).child(
                            div()
                                .id("copy-collection-result")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(palette::card())
                                .cursor_pointer()
                                .child("収集結果の詳細をコピー")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|app, _, _, cx| {
                                        if let Some(result) = &app.collection.result {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(result.copy_text()));
                                        }
                                    }),
                                ),
                        ),
                    )
                    .child(div().text_color(palette::muted()).child(
                        "直近の明示収集の結果。最大24行・詳細は各2048文字。部分成功と対象外を完全成功へ含めません。保存画面の検索・再読込では消えません。",
                    ));
            for row in &result.rows {
                let color = match row.tone {
                    Tone::Error => palette::crit(),
                    Tone::Warning => palette::warn(),
                    Tone::Good => palette::ok(),
                    _ => palette::muted(),
                };
                status = status.child(
                    div()
                        .py_1()
                        .border_b_1()
                        .border_color(palette::line())
                        .child(div().text_color(color).child(format!("{} · {}", row.title, row.badge)))
                        .child(div().text_color(palette::muted()).child({
                            let mut preview: String = row.detail.chars().take(240).collect();
                            if row.detail.chars().count() > 240 {
                                preview.push_str("…（続きは詳細コピー）");
                            }
                            preview
                        })),
                );
            }
        }
        status
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let row = |id: gpui::ElementId, label: String, dot: Hsla, tail: String, active: bool, page: Page, target: Option<String>, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .when(active, |d| d.bg(palette::card()))
                .hover(|d| d.bg(palette::card()))
                .child(div().size_2().rounded_full().bg(dot))
                .child(div().flex_1().truncate().text_sm().child(label))
                .child(div().text_xs().text_color(palette::muted()).child(tail))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _, window, cx| {
                        app.focus.focus(window);
                        app.navigate(page, target.clone(), cx);
                    }),
                )
        };
        let mut list = div().flex().flex_col().gap_0p5().p_2();
        list = list.child(div().px_3().pt_2().pb_1().text_xs().text_color(palette::muted()).child("ワークスペース"));
        for (i, page) in Page::ALL.into_iter().enumerate() {
            list = list.child(row(
                ("page", i).into(),
                page.label().into(),
                palette::accent(),
                String::new(),
                self.selected.is_none() && self.page == page,
                page,
                None,
                cx,
            ));
        }
        list = list.child(div().px_3().pt_4().pb_1().text_xs().text_color(palette::muted()).child(format!("機体 · {}", self.nodes.len())));
        for (i, n) in self.nodes.iter().enumerate() {
            let tail = match &n.health {
                Health::Scored(s) => s.to_string(),
                Health::Failed(_) => "失敗".into(),
                Health::Unobserved => "未".into(),
                Health::Excluded(_) => "対象外".into(),
            };
            list = list.child(row(
                ("nav", i + 1).into(),
                n.id.clone(),
                score_color(&n.health),
                tail,
                self.selected.as_deref() == Some(&n.id),
                Page::Overview,
                Some(n.id.clone()),
                cx,
            ));
        }
        div()
            .w(px(232.0))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(palette::panel())
            .border_r_1()
            .border_color(palette::line())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(img(self.brand.clone()).size_8().flex_none())
                    .child(div().text_sm().font_weight(FontWeight::BOLD).child("Katala Tune")),
            )
            .child(div().flex_1().id("nodes").overflow_y_scroll().child(list))
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.selected.clone().unwrap_or_else(|| self.page.label().into());
        let (crit, warn): (usize, usize) = self.nodes.iter().fold((0, 0), |a, n| (a.0 + n.count(Severity::Critical), a.1 + n.count(Severity::Warn)));
        let busy = self.probing || self.collection.running.is_some();
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(palette::line())
            .child(div().flex_1().text_lg().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(chip(format!("要対応 {crit}"), if crit > 0 { palette::crit() } else { palette::muted() }))
            .child(chip(format!("注意 {warn}"), if warn > 0 { palette::warn() } else { palette::muted() }))
            .child(
                div()
                    .id("probe-all")
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .bg(if busy { palette::line() } else { palette::accent() })
                    .text_color(if busy { palette::muted() } else { rgb(0xffffff).into() })
                    .when(!busy, |d| d.cursor_pointer())
                    .child(if busy { "収集実行中…" } else { "全機を分析" })
                    .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| app.collect(Kind::Probe, cx))),
            )
    }

    fn overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut order: Vec<&NodeView> = self.nodes.iter().collect();
        order.sort_by_key(|n| n.urgency());
        let now = now_ms();
        let mut grid = div().flex().flex_wrap().gap_3().p_5();
        for (i, n) in order.into_iter().enumerate() {
            let id = n.id.clone();
            let score = match &n.health {
                Health::Scored(s) => s.to_string(),
                Health::Failed(_) => "!".into(),
                Health::Unobserved => "–".into(),
                Health::Excluded(_) => "対象外".into(),
            };
            let mut foot = div().flex().flex_wrap().gap_1p5();
            if let Health::Failed(e) = &n.health {
                foot = foot.child(
                    div()
                        .max_w(px(212.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(chip(e.lines().next().unwrap_or("失敗").to_string(), palette::crit())),
                );
            }
            if let Health::Excluded(label) = &n.health {
                foot = foot.child(chip(label.clone(), palette::muted()));
            }
            for (sev, count) in [(Severity::Critical, n.count(Severity::Critical)), (Severity::Warn, n.count(Severity::Warn))] {
                if count > 0 {
                    foot = foot.child(chip(format!("{} {count}", sev.label()), severity_color(sev)));
                }
            }
            let metric = |label: &str, v: Option<f64>, unit: &str| {
                div()
                    .flex()
                    .justify_between()
                    .text_xs()
                    .child(div().text_color(palette::muted()).child(label.to_string()))
                    .child(v.map_or("–".to_string(), |v| format!("{v:.0}{unit}")))
            };
            grid = grid.child(
                div()
                    .id(("card", i))
                    .w(px(244.0))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded_lg()
                    .bg(palette::card())
                    .border_1()
                    .border_color(palette::line())
                    .cursor_pointer()
                    .hover(|d| d.border_color(palette::accent()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().flex_1().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).child(n.id.clone()))
                            .child(div().text_xl().font_weight(FontWeight::BOLD).text_color(score_color(&n.health)).child(score)),
                    )
                    .child(div().text_xs().text_color(palette::muted()).child(format!(
                        "{}{}",
                        n.os,
                        n.at_ms.map_or(String::new(), |a| format!(" · {}", model::ago(now, a)))
                    )))
                    .child(metric("CPU", n.cpu_busy, "%"))
                    .child(metric("空きメモリ", n.mem_avail_pct, "%"))
                    .when(!n.score_trend.is_empty(), |d| d.child(trend(&n.score_trend, 100.0, score_color(&n.health), 18.0)))
                    .child(foot)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |app, _, window, cx| {
                            app.focus.focus(window);
                            app.navigate(Page::Overview, Some(id.clone()), cx);
                        }),
                    ),
            );
        }
        if self.nodes.is_empty() {
            grid = grid.child(div().text_color(palette::muted()).child("台帳に機体がありません（~/.config/katala-tune/nodes.json）"));
        }
        div().id("overview-scroll").flex_1().overflow_y_scroll().child(grid)
    }

    fn finding_row(f: &Finding, node_id: &str, idx: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let color = severity_color(f.severity);
        let proposal = f.action.clone().map(|action| {
            let (node, title) = (node_id.to_string(), format!("{}（{}）", f.action_label, f.title));
            div()
                .id(("act", idx))
                .px_3()
                .py_1()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .border_1()
                .border_color(palette::line())
                .hover(|d| d.border_color(palette::warn()))
                .child(format!("提案: {}…", f.action_label))
                .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| app.run_action(node.clone(), action.clone(), title.clone(), cx)))
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded_md()
            .bg(palette::card())
            .border_l_2()
            .border_color(color)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(chip(f.severity.label(), color))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(f.title.clone())),
            )
            .when(!f.detail.is_empty(), |d| d.child(div().text_xs().text_color(palette::muted()).child(f.detail.clone())))
            .when(!f.advice.is_empty(), |d| d.child(div().text_xs().child(f.advice.clone())))
            .when_some(proposal, |d, p| d.child(div().flex().child(p)))
    }

    fn detail(&self, id: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(n) = self.nodes.iter().find(|n| n.id == id) else {
            return div().id("detail").p_5().child("機体が見つかりません");
        };
        let mut facts = div().flex().flex_wrap().gap_2();
        facts = facts.child(chip(n.os.clone(), palette::muted()));
        if !n.role.is_empty() {
            facts = facts.child(chip(n.role.clone(), palette::muted()));
        }
        if let Some(c) = n.cpu_busy {
            facts = facts.child(chip(format!("CPU {c:.0}%"), palette::muted()));
        }
        if let Some(m) = n.mem_avail_pct {
            facts = facts.child(chip(format!("空きメモリ {m:.0}%"), palette::muted()));
        }
        if let Some(u) = n.uptime_h {
            facts = facts.child(chip(format!("稼働 {}", model::uptime(u)), palette::muted()));
        }
        let on = self.live.on(id);
        let toggle = {
            let id = id.to_string();
            div()
                .id("live-toggle")
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .bg(if on { palette::crit() } else { palette::card() })
                .border_1()
                .border_color(palette::line())
                .child(if on { "ライブを止める" } else { "ライブ（1 秒ごと）" })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _, window, cx| {
                        if app.live.on(&id) {
                            app.live.stop(Stop::User);
                        } else {
                            app.live_start(&id, window);
                        }
                        cx.notify();
                    }),
                )
        };
        facts = facts.child(toggle);
        let live_panel = if on {
            let l = {
                let store = self.live_store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                store.0.get(id).cloned().unwrap_or_default()
            };
            let now = |v: Option<f64>| v.map_or("–".to_string(), |x| format!("{x:.0}%"));
            let status = if l.cpu.is_empty() { Some(format!("接続中… {}", l.detail.clone().unwrap_or_default())) } else { None };
            Some(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .bg(palette::card())
                    .border_1()
                    .border_color(palette::accent())
                    .child(div().text_xs().text_color(palette::muted()).child(format!("ライブ（直近 {} 有効点まで）· {}", model::LIVE_KEEP, l.state)))
                    .when_some(status, |d, s| d.child(div().text_sm().text_color(palette::warn()).child(s)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_xs().text_color(palette::muted()).child("CPU"))
                                    .child(div().text_xl().font_weight(FontWeight::BOLD).child(now(l.latest_cpu))),
                            )
                            .child(trend(&l.cpu, 100.0, palette::accent(), 48.0)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_xs().text_color(palette::muted()).child("メモリ使用"))
                                    .child(div().text_xl().font_weight(FontWeight::BOLD).child(now(l.latest_mem))),
                            )
                            .child(trend(&l.mem, 100.0, palette::warn(), 48.0)),
                    ),
            )
        } else {
            None
        };
        let mut trends = div().flex().flex_wrap().gap_5();
        for (label, values, max, color) in [
            ("スコア", &n.score_trend, 100.0, palette::ok()),
            ("CPU 使用率", &n.cpu_trend, 100.0, palette::accent()),
            ("空きメモリ", &n.mem_trend, 100.0, palette::warn()),
        ] {
            if !values.is_empty() {
                trends = trends.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_3()
                        .rounded_md()
                        .bg(palette::card())
                        .child(div().text_xs().text_color(palette::muted()).child(format!("{label}（直近 {} 回）", values.len().min(60))))
                        .child(trend(values, max, color, 44.0)),
                );
            }
        }
        let mut list = div().flex().flex_col().gap_2();
        for (i, f) in n.findings.iter().enumerate() {
            list = list.child(Self::finding_row(f, id, i, cx));
        }
        let body = match &n.health {
            Health::Excluded(reason) => div().text_sm().text_color(palette::muted()).child(format!("対象外: {reason}。接続や分析は実行しません。")),
            Health::Failed(e) => div().text_sm().text_color(palette::crit()).child(format!("直近の分析に失敗: {e}")),
            Health::Unobserved => div().text_sm().text_color(palette::muted()).child("まだ分析していません。「全機を分析」を押してください。"),
            Health::Scored(_) if n.findings.is_empty() => div().text_sm().text_color(palette::ok()).child("所見はありません"),
            Health::Scored(_) => list,
        };
        div()
            .id("detail")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .child(facts)
            .when_some(live_panel, |d, p| d.child(p))
            .child(trends)
            .child(body)
    }
    fn record_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_5()
            .py_3()
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .child(div().id("search-submit").px_3().py_2().rounded_md().bg(palette::accent()).text_sm().cursor_pointer().child("検索").on_mouse_down(
                MouseButton::Left,
                cx.listener(|app, _, _, cx| {
                    if app.search.read(cx).is_composing() {
                        return;
                    }
                    app.query = app.search.read(cx).text().to_owned();
                    app.selected_row = None;
                    app.scroll = UniformListScrollHandle::new();
                    app.reload(cx);
                }),
            ))
            .child(
                div()
                    .id("refresh-view")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(palette::card())
                    .text_sm()
                    .cursor_pointer()
                    .child("再読込")
                    .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| app.reload(cx))),
            );
        if let Some(kind) = Kind::for_page(self.page) {
            let busy = self.collection.running.is_some();
            toolbar = toolbar.child(
                div()
                    .id("collect-page")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(if busy { palette::line() } else { palette::accent() })
                    .text_sm()
                    .when(!busy, |d| d.cursor_pointer())
                    .child(if busy { "収集実行中…".to_owned() } else { format!("全対象の{}", kind.label()) })
                    .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| app.collect(kind, cx))),
            );
        }
        let mut body = div().flex_1().min_h_0().flex().flex_col().child(toolbar);
        if self.page == Page::Logs {
            let mut levels = div().flex().gap_2().px_5().pb_3();
            for (i, level) in LogLevel::ALL.into_iter().enumerate() {
                levels = levels.child(
                    div()
                        .id(("log-level", i))
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .text_sm()
                        .bg(if self.log_level == level { palette::accent() } else { palette::card() })
                        .cursor_pointer()
                        .child(level.label())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |app, _, _, cx| {
                                app.log_level = level;
                                app.selected_row = None;
                                app.scroll = UniformListScrollHandle::new();
                                app.reload(cx);
                            }),
                        ),
                );
            }
            body = body.child(levels);
        }
        let Some(panel) = self.panel.as_ref() else {
            return body.child(div().p_5().text_sm().text_color(palette::muted()).child(if self.loading {
                "保存データを読み込んでいます…"
            } else {
                "保存データを表示できません"
            }));
        };
        let panel = match panel {
            Ok(panel) => panel,
            Err(error) => return body.child(div().px_5().py_3().text_sm().text_color(palette::crit()).child(format!("読み込みに失敗: {error}"))),
        };
        let mut facts = div().flex().flex_wrap().gap_2().px_5().pb_3();
        for (label, value) in &panel.facts {
            facts = facts.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(palette::card())
                    .child(div().text_xs().text_color(palette::muted()).child(label.clone()))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(value.clone())),
            );
        }
        body = body.child(div().id("summary-facts").max_h(px(128.)).flex_none().overflow_y_scroll().child(facts)).child(
            div()
                .id("scope-note")
                .max_h(px(80.))
                .flex_none()
                .overflow_y_scroll()
                .px_5()
                .pb_3()
                .text_xs()
                .text_color(palette::muted())
                .child(panel.note.clone()),
        );
        if panel.rows.is_empty() {
            body = body.child(div().flex_1().p_5().text_sm().text_color(palette::muted()).child("この表示範囲に該当する保存レコードはありません"));
        } else {
            body = body.child(
                uniform_list(
                    "records",
                    panel.rows.len(),
                    cx.processor(|app, range: std::ops::Range<usize>, _, cx| {
                        let Some(Ok(panel)) = app.panel.as_ref() else {
                            return Vec::new();
                        };
                        range
                            .filter_map(|i| {
                                panel.rows.get(i).map(|row| {
                                    let color = match row.tone {
                                        Tone::Normal => palette::text(),
                                        Tone::Muted => palette::muted(),
                                        Tone::Good => palette::ok(),
                                        Tone::Warning => palette::warn(),
                                        Tone::Error => palette::crit(),
                                    };
                                    div()
                                        .id(("record", i))
                                        .h(px(68.))
                                        .px_5()
                                        .py_2()
                                        .border_b_1()
                                        .border_color(palette::line())
                                        .when(app.selected_row == Some(i), |d| d.bg(palette::card()))
                                        .hover(|d| d.bg(palette::card()))
                                        .cursor_pointer()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_3()
                                                .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(row.title.clone()))
                                                .child(chip(row.badge.clone(), color)),
                                        )
                                        .child(div().mt_1().truncate().text_xs().text_color(palette::muted()).child(row.detail.replace(['\n', '\r'], " ")))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |app, _, window, cx| {
                                                app.selected_row = Some(i);
                                                app.focus.focus(window);
                                                cx.notify();
                                            }),
                                        )
                                })
                            })
                            .collect()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1()
                .min_h_0(),
            );
        }
        if let Some(row) = self.selected_row.and_then(|i| panel.rows.get(i)) {
            body = body.child(
                div()
                    .max_h(px(180.))
                    .flex_none()
                    .border_t_1()
                    .border_color(palette::accent())
                    .bg(palette::panel())
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_5()
                            .py_2()
                            .child(div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).child(row.title.clone()))
                            .child(
                                div()
                                    .id("copy-record")
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(palette::card())
                                    .text_xs()
                                    .cursor_pointer()
                                    .child("コピー")
                                    .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| app.copy_record(cx))),
                            )
                            .when_some(row.undo.clone(), |d, id| {
                                d.child(
                                    div()
                                        .id("undo-record")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(palette::line())
                                        .text_xs()
                                        .cursor_pointer()
                                        .hover(|d| d.border_color(palette::warn()))
                                        .child("元に戻す…")
                                        .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| app.undo_action(id.clone(), cx))),
                                )
                            }),
                    )
                    .child(div().id("record-detail").overflow_y_scroll().px_5().pb_3().text_sm().child(format!("{}\n{}", row.title, row.detail))),
            );
        }
        body.child(div().px_5().py_2().text_xs().text_color(palette::muted()).child(format!(
            "{}表示行 · {}",
            panel.rows.len(),
            if self.loading { "読み込み中（前回の表示）" } else { "保存済みデータ · ↑↓で詳細 / Tabで検索へ" }
        )))
    }

    /// 変更操作の確認。1 番目（既定）は「やめる」。「実行する」を押したときだけ進む。背景を押しても進まない
    fn confirm_modal(&self, p: gate::Pending, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("confirm-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0.0, 0.0, 0.0, 0.6))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|app, _, _, cx| {
                    app.gate.answer(false);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("confirm-card")
                    .w(px(520.0))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .bg(palette::panel())
                    .border_1()
                    .border_color(palette::warn())
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(div().text_xs().text_color(palette::warn()).child("変更を伴う操作です。確認してください"))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(p.title))
                    .child(div().text_sm().text_color(palette::muted()).child(p.detail))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("confirm-cancel")
                                    .px_4()
                                    .py_1p5()
                                    .rounded_md()
                                    .text_sm()
                                    .cursor_pointer()
                                    .bg(palette::accent())
                                    .text_color(rgb(0xffffff))
                                    .child("やめる")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|app, _, _, cx| {
                                            app.gate.answer(false);
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .id("confirm-run")
                                    .px_4()
                                    .py_1p5()
                                    .rounded_md()
                                    .text_sm()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(palette::crit())
                                    .text_color(palette::crit())
                                    .child("実行する")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|app, _, _, cx| {
                                            app.gate.answer(true);
                                            cx.notify();
                                        }),
                                    ),
                            ),
                    ),
            )
    }

    fn copy_record(&self, cx: &mut Context<Self>) {
        if let Some(row) = self.panel.as_ref().and_then(|p| p.as_ref().ok()).and_then(|p| self.selected_row.and_then(|i| p.rows.get(i))) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!("{}\n{}\n{}", row.title, row.badge, row.detail)));
        }
    }
}

impl Render for TuneApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match self.selected.clone() {
            Some(id) => self.detail(&id, cx).into_any_element(),
            None if self.page == Page::Overview => self.overview(cx).into_any_element(),
            None => self.record_panel(cx).into_any_element(),
        };
        div()
            .key_context("Browser")
            .track_focus(&self.focus)
            .on_action(|_: &Quit, _, cx| cx.quit())
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(cx.listener(|app, _: &GoOverview, window, cx| {
                app.focus.focus(window);
                app.navigate(Page::Overview, None, cx);
            }))
            .on_action(cx.listener(|app, _: &NextPage, window, cx| app.cycle_page(1, window, cx)))
            .on_action(cx.listener(|app, _: &PreviousPage, window, cx| app.cycle_page(-1, window, cx)))
            .on_action(cx.listener(|app, _: &Refresh, _, cx| app.reload(cx)))
            .on_action(cx.listener(|app, _: &FocusSearch, window, cx| {
                if app.selected.is_none() && app.page != Page::Overview {
                    app.search.focus_handle(cx).focus(window);
                }
            }))
            .on_action(cx.listener(|app, _: &FocusContent, window, _| app.focus.focus(window)))
            .on_action(cx.listener(|app, _: &ToggleFocus, window, cx| {
                if app.selected.is_none() && app.page != Page::Overview && !app.search.focus_handle(cx).is_focused(window) {
                    app.search.focus_handle(cx).focus(window);
                } else {
                    app.focus.focus(window);
                }
            }))
            .on_action(cx.listener(|app, _: &NextRow, _, cx| app.select_row(1, cx)))
            .on_action(cx.listener(|app, _: &PreviousRow, _, cx| app.select_row(-1, cx)))
            .on_action(cx.listener(|app, _: &CopyRecord, _, cx| app.copy_record(cx)))
            .size_full()
            .flex()
            .font_family(FONT)
            .bg(palette::bg())
            .text_color(palette::text())
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.header(cx))
                    .child(self.collection_status(cx))
                    .when_some(self.notice.clone(), |d, n| d.child(div().px_5().py_2().text_xs().text_color(palette::warn()).child(n)))
                    .when_some(self.live_notice.clone(), |d, n| d.child(div().px_5().py_2().text_xs().text_color(palette::warn()).child(n)))
                    .when_some(self.action_result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(), |d, r| {
                        d.child(div().flex().items_center().gap_3().px_5().py_2().bg(palette::card()).child(div().flex_1().text_sm().child(r)).child(
                            div().id("result-close").px_2().text_xs().text_color(palette::muted()).cursor_pointer().child("閉じる").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|app, _, _, cx| {
                                    *app.action_result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                                    cx.notify();
                                }),
                            ),
                        ))
                    })
                    .child(main),
            )
            .when_some(self.gate.pending(), |d, p| d.child(self.confirm_modal(p, cx)))
    }
}

fn open_route(target: &str, ledger: &[nodes::Node]) -> Result<Route, String> {
    let keys = ["overview", "status", "resources", "processes", "jobs", "logs", "tools", "ai", "security", "specs", "power", "topology", "actions"];
    if let Some((_, page)) = keys.into_iter().zip(Page::ALL).find(|(key, page)| *key == target || page.label() == target) {
        return Ok(Route::Page(page));
    }
    if ledger.iter().any(|node| node.id == target) {
        return Ok(Route::Node(target.into()));
    }
    Err(format!("--open の表示または機体が見つかりません: {target}"))
}

pub fn run(options: startup::Options) {
    let startup::Options { config, data, selftest, open, live_id, logs, confirm_demo } = options;
    if (selftest || config.exists())
        && let Err(error) = nodes::load_config(&config)
    {
        eprintln!("katala-tune-gpui: {error}");
        std::process::exit(1);
    }
    let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio"));
    // Engine の中の tokio 呼び出し（接続・子プロセス）が動くよう、ランタイムの文脈で開く
    let dirty = Arc::new(AtomicBool::new(true));
    let engine = {
        let _g = rt.enter();
        match Engine::open(config, data, Arc::new(DirtyHost(dirty.clone()))) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("katala-tune: {e}");
                std::process::exit(1);
            }
        }
    };
    if let Some(error) = engine.config_error().or_else(|| engine.last().err()) {
        eprintln!("katala-tune-gpui: {error}");
        std::process::exit(1);
    }

    let initial_route = match open.as_deref().map(|target| open_route(target, &engine.config().nodes)).transpose() {
        Ok(route) => route,
        Err(error) => {
            eprintln!("katala-tune-gpui: {error}");
            std::process::exit(1);
        }
    };
    let live_store = Arc::new(Mutex::new(model::LiveStore::default()));
    let live_dirty = Arc::new(AtomicBool::new(false));
    let live = {
        let (store, flag) = (live_store.clone(), live_dirty.clone());
        let live = Live::new(
            Arc::new(ProcessRoute),
            tune_core::monitor::live_opts(engine.config().nodes.len()),
            Box::new(move |event| {
                store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).merge(&event);
                flag.store(true, Ordering::Release);
            }),
        );
        rt.spawn(live.clone().run_ticker());
        live
    };
    let handles = LiveHandles { live: live.clone(), store: live_store, dirty: live_dirty };
    let rt_exit = rt.clone();
    let failed = Arc::new(AtomicBool::new(false));
    let failed_exit = failed.clone();

    Application::new().run(move |cx: &mut App| {
        input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("alt-down", NextPage, Some("Browser")),
            KeyBinding::new("alt-up", PreviousPage, Some("Browser")),
            KeyBinding::new("f5", Refresh, Some("Browser")),
            KeyBinding::new("escape", FocusContent, Some("Browser")),
            KeyBinding::new("tab", ToggleFocus, Some("Browser")),
            KeyBinding::new("shift-tab", ToggleFocus, Some("Browser")),
            KeyBinding::new("down", NextRow, Some("Browser && !SearchInput")),
            KeyBinding::new("up", PreviousRow, Some("Browser && !SearchInput")),
        ]);
        for modifier in ["cmd", "ctrl"] {
            cx.bind_keys([
                KeyBinding::new(format!("{modifier}-q").as_str(), Quit, None),
                KeyBinding::new(format!("{modifier}-w").as_str(), CloseWindow, Some("Browser")),
                KeyBinding::new(format!("{modifier}-1").as_str(), GoOverview, Some("Browser")),
                KeyBinding::new(format!("{modifier}-r").as_str(), Refresh, Some("Browser")),
                KeyBinding::new(format!("{modifier}-f").as_str(), FocusSearch, Some("Browser")),
                KeyBinding::new(format!("{modifier}-c").as_str(), CopyRecord, Some("Browser && !SearchInput")),
            ]);
        }
        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(900.), px(640.))),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Katala Tune GPUI Preview".into()),
                        appears_transparent: false,
                        traffic_light_position: Some(point(px(12.0), px(12.0))),
                    }),
                    ..Default::default()
                },
                move |window, cx| cx.new(|cx| TuneApp::new(engine, rt, dirty, handles, window, cx)),
            )
            .expect("window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
        if let Some(route) = initial_route {
            window
                .update(cx, |app, _, cx| match route {
                    Route::Page(page) => app.navigate(page, None, cx),
                    Route::Node(id) => app.navigate(Page::Overview, Some(id), cx),
                })
                .ok();
        }
        if selftest {
            cx.spawn(async move |cx| {
                let result: Result<(), String> = async {
                    let mut count = 0;
                    let mut nodes = 0;
                    let mut log_counts = None;
                    // All saved-data routes run before any explicit live diagnostic.
                    for page in Page::ALL {
                        window.update(cx, |app, _, cx| app.navigate(page, None, cx)).map_err(|_| "selftest window closed".to_string())?;
                        let mut loaded = false;
                        for _ in 0..100 {
                            cx.background_executor().timer(Duration::from_millis(100)).await;
                            let state = window
                                .update(cx, |app, _, _| {
                                    if app.loading {
                                        return None;
                                    }
                                    if logs && page == Page::Logs {
                                        log_counts = app.panel.as_ref().and_then(|p| p.as_ref().ok()).map(|p| {
                                            (
                                                p.rows.iter().filter(|r| r.title.starts_with("ログ · ")).count(),
                                                p.rows.iter().filter(|r| r.title.starts_with("署名 · ")).count(),
                                            )
                                        });
                                    }
                                    Some((app.nodes.len(), app.notice.clone().or_else(|| app.panel.as_ref().and_then(|p| p.as_ref().err().cloned()))))
                                })
                                .map_err(|_| "selftest window closed".to_string())?;
                            match state {
                                Some((n, None)) => {
                                    nodes = n;
                                    loaded = true;
                                    break;
                                }
                                Some((_, Some(error))) => return Err(format!("selftest failed view={page:?}: {error}")),
                                None => {}
                            }
                        }
                        if !loaded {
                            return Err(format!("selftest timed out view={page:?}"));
                        }
                        count += 1;
                    }
                    if let Some(id) = live_id {
                        // Select and start once. Subsequent iterations only inspect state.
                        window
                            .update(cx, |app, window, cx| {
                                app.navigate(Page::Overview, Some(id.clone()), cx);
                                app.live_start(&id, window);
                                cx.notify();
                            })
                            .map_err(|_| "selftest window closed".to_string())?;
                        let mut valid = 0;
                        for _ in 0..80 {
                            cx.background_executor().timer(Duration::from_millis(100)).await;
                            let (on, points, notice) = window
                                .update(cx, |app, _, _| {
                                    (
                                        app.live.on(&id),
                                        app.live_store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0.get(&id).map_or(0, |node| node.cpu.len()),
                                        app.live_notice.clone().or_else(|| app.notice.clone()),
                                    )
                                })
                                .map_err(|_| "selftest window closed".to_string())?;
                            valid = points;
                            if !on {
                                return Err(format!("selftest live failed: {}", notice.unwrap_or_else(|| "stream stopped or refused".into())));
                            }
                            if valid >= 3 {
                                break;
                            }
                        }
                        if valid < 3 {
                            return Err(format!("selftest live failed: only {valid} valid CPU points within 8s (need 3)"));
                        }
                        println!("selftest live points={valid}");
                    }
                    if logs {
                        let (rows, sigs) = log_counts.ok_or_else(|| "selftest logs did not load".to_string())?;
                        println!("selftest logs rows={rows} sigs={sigs}");
                    }
                    if confirm_demo {
                        // 確認欄が出て描かれ、「やめる」で進まないこと（何も実行しない）
                        let (gate, rt) = window.update(cx, |app, _, _| (app.gate.clone(), app.rt.clone())).map_err(|_| "selftest window closed".to_string())?;
                        let g = gate.clone();
                        let task = rt.spawn(async move {
                            (g.confirm())("確認欄の表示テスト".into(), "これは表示の確認です。何も実行しません。".into()).await
                        });
                        cx.background_executor().timer(Duration::from_millis(1000)).await;
                        let shown = gate.pending().is_some();
                        gate.answer(false);
                        let approved = task.await.map_err(|e| format!("selftest confirm task: {e}"))?;
                        if !shown || approved {
                            return Err(format!("selftest confirm failed: shown={shown} approved={approved}"));
                        }
                        println!("selftest confirm shown={shown} approved={approved}");
                    }
                    println!("selftest ok nodes={nodes} views={count}");
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    failed.store(true, Ordering::Release);
                    eprintln!("{error}");
                }
                cx.update(|cx| cx.quit()).ok();
            })
            .detach();
        }
    });
    rt_exit.block_on(live.shutdown(Duration::from_secs(3)));
    if failed_exit.load(Ordering::Acquire) {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    use tune_core::live::{Opts, Route as LiveRoute, Source};

    /// Only transport is replaced: the real Live owns retries, stream state and stop.
    #[derive(Default)]
    struct FailingRoute(AtomicUsize);
    impl LiveRoute for FailingRoute {
        fn open(&self, _: &nodes::Node, _: &Opts) -> Result<Source, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err("synthetic transport unavailable".into())
        }
    }

    fn controller() -> (LiveControl, Arc<FailingRoute>) {
        let route = Arc::new(FailingRoute::default());
        let live = Live::new(route.clone(), Opts { max_retries: 0, ..Opts::default() }, Box::new(|_| {}));
        (LiveControl::new(live, true), route)
    }

    fn node(id: &str) -> nodes::Node {
        nodes::Node::from_value(&json!({"id":id,"os":"macos"}), "synthetic-host")
    }

    #[tokio::test]
    async fn live_hidden_invalidates_pending_start_and_focus_return_requires_manual_start() {
        let (control, route) = controller();
        let (generation, id) = control.request("a").unwrap();
        control.set_active(false);
        assert!(control.request("a").is_none());
        control.set_active(true);
        control.complete(generation, &id, &[node("a")], None);
        assert!(!control.on("a"));
        assert!(control.live.active().is_empty());
        assert_eq!(route.0.load(Ordering::SeqCst), 0);
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn live_stale_start_cannot_stop_newer_intent_for_the_same_node() {
        let (control, _) = controller();
        let (old, id) = control.request("a").unwrap();
        control.stop(Stop::User);
        let (new, _) = control.request("a").unwrap();
        control.complete(new, &id, &[node("a")], None);
        // A stale disk load fails after the newer request has started.
        control.complete(old, &id, &[], Some("stale invalid ledger".into()));
        assert!(control.on("a"));
        assert_eq!(control.live.active(), ["a"]);
        assert!(control.take_notice().is_none());
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn live_invalid_ledger_stops_last_good_stream_and_clears_intent() {
        let (control, _) = controller();
        let (generation, id) = control.request("a").unwrap();
        let last_good = [node("a")];
        control.complete(generation, &id, &last_good, None);
        assert_eq!(control.live.active(), ["a"]);
        control.complete(generation, &id, &last_good, Some("invalid synthetic ledger".into()));
        assert!(!control.on("a"));
        assert!(control.live.active().is_empty());
        assert!(control.take_notice().unwrap().contains("invalid synthetic ledger"));
        control.complete(generation, &id, &last_good, None);
        assert!(control.live.active().is_empty());
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn live_unknown_or_excluded_node_refuses_and_clears_intent() {
        let (control, route) = controller();
        for ledger in [Vec::new(), vec![nodes::Node::from_value(&json!({"id":"a","status":"paused"}), "synthetic-host")]] {
            let (generation, id) = control.request("a").unwrap();
            control.complete(generation, &id, &ledger, None);
            assert!(!control.on("a"));
            assert!(control.live.active().is_empty());
            assert!(control.take_notice().is_some());
        }
        assert_eq!(route.0.load(Ordering::SeqCst), 0);
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn live_retains_against_the_full_ledger_and_stops_removed_nodes() {
        let (control, _) = controller();
        let a = node("a");
        let b = node("b");
        control.live.start(std::slice::from_ref(&b), &[]);
        let (generation, id) = control.request("a").unwrap();
        control.complete(generation, &id, &[a.clone(), b], None);
        assert_eq!(control.live.active(), ["a", "b"]);
        control.complete(generation, &id, &[a], None);
        assert_eq!(control.live.active(), ["a"]);
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn live_terminal_backend_failure_never_resets_its_retry_budget() {
        let (control, route) = controller();
        let (generation, id) = control.request("a").unwrap();
        control.complete(generation, &id, &[node("a")], None);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !control.live.active().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(route.0.load(Ordering::SeqCst), 1);
        assert!(control.poll().is_none());
        assert!(!control.on("a"));
        assert!(control.take_notice().unwrap().contains("手動"));
        // A delayed heartbeat cannot create a new backend stream after failure.
        control.complete(generation, &id, &[node("a")], None);
        assert!(control.live.active().is_empty());
        assert_eq!(route.0.load(Ordering::SeqCst), 1);
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn live_accepted_exclusion_stops_owned_stream_before_the_next_heartbeat() {
        for status in ["paused", "retired"] {
            let (control, _) = controller();
            let (generation, id) = control.request("a").unwrap();
            control.complete(generation, &id, &[node("a")], None);
            assert_eq!(control.live.active(), ["a"]);
            let views = model::build(&[json!({"id":"a","status":status})], &[]);
            control.accept_nodes(&views);
            assert!(!control.on("a"), "accepted {status} ledger must clear intent immediately");
            assert!(control.live.active().is_empty());
            // In-flight renewal for the old online ledger cannot revive it.
            control.complete(generation, &id, &[node("a")], None);
            assert!(control.live.active().is_empty());
            control.live.shutdown(Duration::from_secs(1)).await;
        }
    }

    #[tokio::test]
    async fn live_accepted_exclusion_for_another_node_preserves_the_owned_stream() {
        let (control, _) = controller();
        let (generation, id) = control.request("a").unwrap();
        control.complete(generation, &id, &[node("a")], None);
        let views = model::build(&[json!({"id":"a"}), json!({"id":"b","status":"paused"})], &[]);
        control.accept_nodes(&views);
        assert!(control.on("a"));
        assert_eq!(control.live.active(), ["a"]);
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn live_delayed_heartbeat_after_terminal_failure_does_not_start_a_new_stream() {
        let (control, route) = controller();
        let (generation, id) = control.request("a").unwrap();
        control.complete(generation, &id, &[node("a")], None);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !control.live.active().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(route.0.load(Ordering::SeqCst), 1);
        // complete receives the already-issued heartbeat before the UI can poll
        // the terminal state. Renewal must never become an initial start.
        control.complete(generation, &id, &[node("a")], None);
        assert!(!control.on("a"));
        assert!(control.live.active().is_empty());
        assert!(control.take_notice().unwrap().contains("手動"));
        assert_eq!(route.0.load(Ordering::SeqCst), 1);
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[test]
    fn open_route_resolves_saved_views_and_rejects_unknown_nodes() {
        assert_eq!(open_route("logs", &[]).unwrap(), Route::Page(Page::Logs));
        assert_eq!(open_route("resources", &[]).unwrap(), Route::Page(Page::Resources));
        assert_eq!(open_route("構成図", &[]).unwrap(), Route::Page(Page::Topology));
        assert_eq!(open_route("a", &[node("a")]).unwrap(), Route::Node("a".into()));
        assert!(open_route("missing", &[node("a")]).is_err());
    }
}
