//! Katala Tune のネイティブ画面（Rust + GPUI）。WebView も Node も使わず、tune-core の Engine を直接呼ぶ。
//!
//! 実行: `katala-tune-gpui [--config 台帳.json] [--data データdir] [--selftest]`
//! `--selftest` は保存データの全ルートを読み、終了する（収集はしない）。
//! `--selftest --live <id>` だけは指定機体を一度ストリームし、3 CPU 点以上を確認する。

use crate::{
    browser::{Reader, Request, Route},
    collection::{self, Job, Kind},
    dashboard::{self, DashboardView},
    gate,
    input::{self, SearchInput, Submitted},
    model,
    panels::{LogLevel, Page, Panel, Tone},
    review, startup,
};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, MouseButton, PathBuilder, UniformListScrollHandle, Window,
    WindowBounds, WindowOptions, actions, canvas, div, hsla, img, point, prelude::*, px, rgb, size, uniform_list,
};
use model::{Finding, Health, NodeView, Severity};
use tune_core::engine::{Engine, Host};
use tune_core::live::{Live, Owner, ProcessRoute, Stop};
use tune_core::nodes;

actions!(tune, [Quit, CloseWindow, GoOverview, NextPage, PreviousPage, Refresh, FocusSearch, FocusContent, ToggleFocus, NextRow, PreviousRow, CopyRecord]);

#[cfg(target_os = "macos")]
const FONT: &str = ".SystemUIFont";
#[cfg(not(target_os = "macos"))]
const FONT: &str = "Segoe UI";

pub(crate) mod palette {
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

/// スコアのリング。円弧の長さがスコア（0〜100）に比例する。値が無いときは円だけ
fn score_ring(fraction: f32, color: Hsla, label: String) -> gpui::Div {
    use std::f32::consts::{FRAC_PI_2, TAU};
    let arc = |path: &mut PathBuilder, c: gpui::Point<gpui::Pixels>, r: gpui::Pixels, f: f32| {
        let steps = ((96.0 * f) as usize).max(2);
        for i in 0..=steps {
            let a = -FRAC_PI_2 + TAU * f * (i as f32 / steps as f32);
            let p = point(c.x + r * a.cos(), c.y + r * a.sin());
            if i == 0 { path.move_to(p) } else { path.line_to(p) }
        }
    };
    div()
        .size(px(44.0))
        .flex_none()
        .relative()
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let c = bounds.center();
                    let r = bounds.size.width.min(bounds.size.height) / 2.0 - px(3.0);
                    for (f, col) in [(0.9999, palette::line()), (fraction.clamp(0.0, 1.0), color)] {
                        if f <= 0.0 {
                            continue;
                        }
                        let mut path = PathBuilder::stroke(px(4.0));
                        arc(&mut path, c, r, f);
                        if let Ok(path) = path.build() {
                            window.paint_path(path, col);
                        }
                    }
                },
            )
            .size_full(),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .child(label),
        )
}

/// ナビのアイコン。旧画面（renderer/ui.js ほか）の 20x20 の線画をそのまま使い、色だけ指定して SVG にする
fn icon_body(name: &str) -> &'static str {
    match name {
        "overview" => {
            r#"<rect x="3" y="3" width="6" height="6" rx="1.5"/><rect x="11" y="3" width="6" height="6" rx="1.5"/><rect x="3" y="11" width="6" height="6" rx="1.5"/><rect x="11" y="11" width="6" height="6" rx="1.5"/>"#
        }
        "status" => r#"<path d="M2.5 10.5h3.2l2-5.2 4 10 2.2-4.8h3.6"/>"#,
        "resources" => {
            r#"<rect x="3" y="11" width="3" height="6" rx="1"/><rect x="8.5" y="7" width="3" height="10" rx="1"/><rect x="14" y="3" width="3" height="14" rx="1"/>"#
        }
        "procs" => r#"<rect x="3" y="3" width="14" height="14" rx="2"/><path d="M7 7h6M7 10h6M7 13h4"/>"#,
        "jobs" => r#"<rect x="3" y="4" width="14" height="13" rx="2"/><path d="M3 8h14M7 2.5v3M13 2.5v3"/>"#,
        "logs" => r#"<path d="M4 5h12M4 10h12M4 15h8"/>"#,
        "actions" => r#"<circle cx="10" cy="10" r="7"/><path d="M10 6v4l2.5 2"/>"#,
        "sync" => r#"<path d="M4 10a6 6 0 0 1 10.2-4.3M16 10a6 6 0 0 1-10.2 4.3M14.5 2.5v3.5H11M5.5 17.5V14H9"/>"#,
        "link" => r#"<path d="M8.5 11.5l3-3M7 9l-1.8 1.8a2.8 2.8 0 0 0 4 4L11 13M9 7l1.8-1.8a2.8 2.8 0 0 1 4 4L13 11"/>"#,
        "tool" => r#"<path d="M12.6 3.2a4 4 0 0 0-4.9 5.3l-4.6 4.6a1.6 1.6 0 0 0 2.3 2.3l4.6-4.6a4 4 0 0 0 5.3-4.9l-2.4 2.4-2.1-.6-.6-2.1z"/>"#,
        "shield" => r#"<path d="M10 2.75l6 2.25v4.5c0 3.8-2.6 6.6-6 7.75-3.4-1.15-6-3.95-6-7.75V5z"/><path d="M7.4 10l1.9 1.9 3.4-3.6"/>"#,
        "agent" => r#"<rect x="3.5" y="5.5" width="13" height="10" rx="3"/><path d="M10 2.75v2.75M7.5 10h.01M12.5 10h.01M7.75 13h4.5"/>"#,
        "review" => r#"<path d="M5 2.75h7l3 3v11.5H5z"/><path d="M12 2.75v3h3M7.5 10h5M7.5 13h5"/>"#,
        "topology" => {
            r#"<circle cx="4" cy="10" r="1.8"/><circle cx="15.5" cy="4" r="1.8"/><circle cx="15.5" cy="16" r="1.8"/><path d="M5.7 9.1 13.8 4.9M5.7 10.9l8.1 4.2"/>"#
        }
        "mac" => r#"<rect x="3" y="4" width="14" height="9.5" rx="1.5"/><path d="M7.5 16.5h5M10 13.5v3"/>"#,
        "win" => r#"<rect x="3" y="3.5" width="14" height="13" rx="1.5"/><path d="M10 3.5v13M3 10h14"/>"#,
        _ => "",
    }
}

fn icon_image(name: &str, color: &str) -> Arc<gpui::Image> {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" width="40" height="40" fill="none" stroke="{color}" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
        icon_body(name)
    );
    Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Svg, svg.into_bytes()))
}

fn chip(text: impl Into<gpui::SharedString>, color: Hsla) -> gpui::Div {
    div().px_2().py_0p5().rounded_md().text_xs().text_color(color).bg(hsla(color.h, color.s, color.l, 0.14)).child(text.into())
}

/// 旧画面と同じアイコン名
fn page_icon(page: Page) -> &'static str {
    match page {
        Page::Dashboard => "status",
        Page::Overview => "overview",
        Page::Status => "status",
        Page::Logs => "logs",
        Page::Topology => "topology",
        Page::Actions => "actions",
        Page::Jobs => "jobs",
        Page::Resources | Page::Power | Page::Specs => "resources",
        Page::Tools => "tool",
        Page::Ai => "agent",
        Page::Processes => "procs",
        Page::Security => "shield",
        Page::Network => "sync",
        Page::Link => "link",
        Page::Review => "review",
    }
}

/// 見出しの下の説明。どの画面も保存済みの値を読むだけで、押したときだけ収集や実行をする
fn page_desc(page: Page) -> &'static str {
    match page {
        Page::Dashboard => "保存済みの監視データ。収集済みの Live があれば最新の値を表示します。表示だけでは収集を開始しません。",
        Page::Overview => "全機体の状態と、優先して見るもの。直近の分析の結果です。",
        Page::Status => "保存済みの現在状態と、最近の変化。ここで状態を計算し直すことはしません。",
        Page::Resources => "全機体の CPU・メモリ・ディスク・swap を、直近の分析の値で並べます。",
        Page::Processes => "直近の分析時点の、アプリ別の使用量。メモリの多い上位を機体ごとに出します。",
        Page::Security => "ネットワークとセキュリティの監視の状態。読み取りだけで、遮断や設定の変更はしません。",
        Page::Network => "設定確認と外部到達診断。選んだボタンを押したときだけ実行します。",
        Page::Link => "ペア済みの tune-agent の一覧。ペアリングと解除は、鍵を扱うためここではしません。",
        Page::Review => "task-completion.v1 の申告を一時表示します。検証・受入・完了は判定しません。",
        Page::Power => "機体ごとの電力。計測できていない値は 0 ではなく「–」と不足項目で示します。",
        Page::Specs => "保存済みのスペック報告を、部品の区分ごとに機体を並べて比べます。",
        Page::Tools => "保存済みの道具の棚卸し。版差のあるものを先に並べます。",
        Page::Ai => "AI の利用の集計。料金は料金表から求めた概算で、請求額ではありません。",
        Page::Logs => "各機体のイベントログ。同じ形のものを「同種ログ」にまとめて数えます。",
        Page::Topology => "観測時点の構成図（読み取り専用）。表示は上限付きで、全件ではありません。",
        Page::Actions => "承認して実行した操作の記録。元に戻せるものは「元に戻す…」から戻せます。",
        Page::Jobs => "自動スキャンの設定と、前回の実行。表示だけです。",
    }
}

struct LiveHandles {
    monitor: Arc<crate::monitor_host::MonitorHost>,
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
    closing: bool,
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
            self.live.release_owned(Owner::Ui, &[intent.id], why);
        }
    }

    fn stop(&self, why: Stop) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // A failed current ledger read remains an administrative stop of the
        // selected target; normal navigation/focus/manual stops only release UI.
        if why == Stop::Excluded
            && let Some(intent) = state.intent.as_ref()
        {
            self.live.stop(Some(std::slice::from_ref(&intent.id)), why.clone());
        }
        self.stop_locked(&mut state, why);
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closing = true;
        self.stop_locked(&mut state, Stop::AppExit);
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
        if state.closing || !state.active {
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
        if state.closing || !state.active {
            return;
        }
        if let Some(error) = config_error {
            self.live.stop(Some(&[id.to_string()]), Stop::Excluded);
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
            if !self.live.heartbeat_owned(Owner::Ui, id) {
                let notice = self.terminal_notice(id);
                self.stop_locked(&mut state, Stop::User);
                state.notice = Some(notice);
                return;
            }
        } else {
            let target: Vec<_> = target.cloned().into_iter().collect();
            let unknown = if target.is_empty() { vec![id.to_string()] } else { Vec::new() };
            let result = self.live.start_owned(Owner::Ui, &target, &unknown);
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

    /// Apply the complete ledger only for an accepted Reader generation. Removal
    /// and explicit exclusion administratively stop every owner under Live's lock.
    fn accept_nodes(&self, nodes: &[nodes::Node]) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.live.retain_active(nodes);
        if state.intent.as_ref().is_some_and(|intent| !nodes.iter().any(|node| node.id == intent.id && node.is_active())) {
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
    monitor: Arc<crate::monitor_host::MonitorHost>,
    monitor_command_tail: Option<tokio::task::JoinHandle<()>>,
    monitor_records: Arc<Mutex<Option<Result<serde_json::Value, String>>>>,
    brand: Arc<gpui::Image>,
    icons: Vec<(&'static str, Arc<gpui::Image>)>,
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
    dashboard: Option<Result<DashboardView, String>>,
    dashboard_minutes: i64,
    dashboard_details: bool,
    dashboard_clock: Instant,
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
    /// 読み込んだ完了台帳（画面へのドラッグか --review）。保存しない
    review: Option<(review::Source, review::Document)>,
    review_error: Option<String>,
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
        cx.on_app_quit(|app, _| {
            app.live.close();
            app.monitor.request_exit();
            async {}
        })
        .detach();
        cx.observe_window_activation(window, |app, window, cx| {
            app.live.set_active(window.is_window_active());
            cx.notify();
        })
        .detach();
        // 確認欄が出た・消えたときの描き直しは、live_dirty の旗（cx.notify だけ）で知らせる
        let gate_dirty = handles.dirty.clone();
        let mut app = Self {
            monitor: handles.monitor,
            monitor_command_tail: None,
            monitor_records: Arc::new(Mutex::new(None)),
            brand: Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, include_bytes!("../../../build/mark.png").to_vec())),
            icons: Page::ALL
                .into_iter()
                .map(page_icon)
                .chain(["mac", "win"])
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .map(|n| (n, icon_image(n, "#8b93a3")))
                .collect(),
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
            dashboard: None,
            dashboard_minutes: 60,
            dashboard_details: false,
            dashboard_clock: Instant::now(),
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
            review: None,
            review_error: None,
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
                        // Age/source labels and brief() are memory-only. SQLite
                        // reloads are explicit or follow saved-data dirty events.
                        if app.page == Page::Dashboard && app.selected.is_none() && app.dashboard_clock.elapsed() >= Duration::from_secs(1) {
                            app.dashboard_clock = Instant::now();
                            cx.notify();
                        }
                        if app.collection.poll() {
                            // Session diagnostics survive route/query changes and other jobs.
                            if app.collection.running.is_none() {
                                app.reload(cx);
                            } else if app.page == Page::Network && app.selected.is_none() {
                                app.panel = app.collection.network.clone().map(Ok);
                                cx.notify();
                            }
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
                                } else if let Some(ledger) = response.ledger {
                                    app.live.accept_nodes(&ledger);
                                }
                                app.notice = response.config_error;
                                match response.nodes {
                                    Ok(nodes) => {
                                        app.nodes = nodes;
                                    }
                                    Err(error) => app.notice = Some(error),
                                }
                                // Keep the previous diagnostic visible even if a read
                                // fails; the current ledger error remains in notice.
                                app.panel = app.collection.restore_network(app.page, response.panel);
                                app.dashboard = response.dashboard;
                                if app.page == Page::Review && app.selected.is_none() {
                                    app.panel = Some(Ok(app.review_panel()));
                                }
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
        self.panel = self.collection.restore_network(self.page, self.panel.take());
        let route = self.selected.clone().map_or(Route::Page(self.page), Route::Node);
        if let Err(error) = self.reader.request(Request {
            generation: self.generation,
            route,
            query: self.query.clone(),
            log_level: self.log_level,
            minutes: self.dashboard_minutes,
        }) {
            self.loading = false;
            if self.page == Page::Dashboard && self.selected.is_none() {
                self.dashboard = Some(Err(error));
            } else {
                self.panel = Some(Err(error));
            }
        }
        cx.notify();
    }

    /// 完了台帳 JSON を読む（保存しない・外部参照を取得しない）。読めなければ理由を出し、前の表示は消す
    fn load_review(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        match review::load(path, review::now_ms()) {
            Ok(loaded) => {
                self.review = Some(loaded);
                self.review_error = None;
            }
            Err(error) => {
                self.review = None;
                self.review_error = Some(error);
            }
        }
        if self.page == Page::Review && self.selected.is_none() {
            self.panel = Some(Ok(self.review_panel()));
            self.selected_row = None;
            self.scroll = UniformListScrollHandle::new();
        }
        cx.notify();
    }

    fn review_panel(&self) -> Panel {
        match &self.review {
            Some((source, doc)) => review::panel(source, doc),
            None => review::empty_panel(self.review_error.as_deref()),
        }
    }

    fn navigate(&mut self, page: Page, selected: Option<String>, cx: &mut Context<Self>) {
        if self.selected != selected || self.page != page {
            self.live.stop(Stop::User);
        }
        self.live_notice = None;
        self.live.take_notice();
        self.page = page;
        self.selected = selected;
        self.query.clear();
        self.panel = None;
        self.dashboard = None;
        self.selected_row = None;
        self.scroll = UniformListScrollHandle::new();
        self.search.update(cx, |field, cx| field.reset(page.search_hint(), cx));
        self.reload(cx);
    }

    fn refresh_live(&self, (generation, id): (u64, String)) {
        if !self.monitor.ensure_ticker() {
            return;
        }
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
        if self.selected.as_deref() != Some(id) && !(self.selected.is_none() && self.page == Page::Resources && self.nodes.iter().any(|n| n.id == id)) {
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
        if self.probing || self.collection.running.is_some() {
            return;
        }
        if let Err(error) = self.monitor.start_manual(&mut self.collection, kind) {
            self.collection.result = Some(collection::Receipt::error(kind, &error));
        }
        cx.notify();
    }

    fn collection_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut status = div()
            .id("collection-status")
            .flex_none()
            .max_h(px(if self.page == Page::Network { 80. } else { 190. }))
            .overflow_y_scroll()
            .px_5()
            .py_2()
            .text_xs();
        if let Some(kind) = self.collection.running {
            status = status.child(div().text_color(palette::accent()).child(format!("{}を実行中…（画面を移動しても継続します）", kind.label())));
        }
        if let Some(result) = &self.collection.result {
            status = status
                .child(
                    div().flex().flex_wrap().items_center().gap_3().child(div().flex_1().min_w(px(200.)).child(result.summary())).child(
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
                .child(
                    div()
                        .text_color(palette::muted())
                        .child("直近に実行した収集の結果です。部分成功・失敗・対象外を区別して表示します。画面を移動・再読込しても結果は残ります。"),
                );
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
                            let reason = row.detail.lines().next().unwrap_or_default();
                            let mut preview: String = reason.chars().take(240).collect();
                            if reason.chars().count() > 240 {
                                preview.push_str("…（続きは詳細コピー）");
                            }
                            preview
                        })),
                );
            }
        }
        status
    }

    fn icon_view(&self, name: &str, alert: bool) -> gpui::Div {
        let found = self.icons.iter().find(|(n, _)| *n == name).map(|(_, i)| i.clone());
        div().size(px(16.0)).flex_none().when_some(found, |d, i| d.child(img(i).size_full())).when(alert, |d| d.border_b_2().border_color(palette::crit()))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let row = |id: gpui::ElementId,
                   label: String,
                   lead: gpui::AnyElement,
                   tail: String,
                   active: bool,
                   page: Page,
                   target: Option<String>,
                   cx: &mut Context<Self>| {
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
                .child(lead)
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
                self.icon_view(page_icon(page), false).into_any_element(),
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
                div().size_2().rounded_full().bg(score_color(&n.health)).into_any_element(),
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
        let (icon_name, desc) = match self.selected.as_ref().and_then(|id| self.nodes.iter().find(|n| &n.id == id)) {
            Some(n) => (
                if n.os.eq_ignore_ascii_case("macos") { "mac" } else { "win" },
                format!("{} · {}", n.os, if n.role.is_empty() { "機体の詳細" } else { n.role.as_str() }),
            ),
            None => (page_icon(self.page), page_desc(self.page).to_string()),
        };
        let (crit, warn): (usize, usize) = self.nodes.iter().fold((0, 0), |a, n| (a.0 + n.count(Severity::Critical), a.1 + n.count(Severity::Warn)));
        let busy = self.probing || self.collection.running.is_some();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(palette::line())
            .child(
                div().flex_1().min_w(px(150.)).flex().items_center().gap_3().child(self.icon_view(icon_name, false).size(px(22.0))).child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_xs().text_color(palette::muted()).truncate().child(desc)),
                ),
            )
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
                            .gap_2()
                            .child(self.icon_view(if n.os.eq_ignore_ascii_case("macos") { "mac" } else { "win" }, false))
                            .child(div().flex_1().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).child(n.id.clone()))
                            .child(score_ring(
                                match &n.health {
                                    Health::Scored(sc) => *sc as f32 / 100.0,
                                    _ => 0.0,
                                },
                                score_color(&n.health),
                                score,
                            )),
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
        let live_panel = if on { Some(self.live_panel(id)) } else { None };
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
    fn monitor_command(&mut self, command: &'static str, patch: Option<serde_json::Value>) {
        let (host, engine, result, records, dirty) =
            (self.monitor.clone(), self.engine.clone(), self.action_result.clone(), self.monitor_records.clone(), self.live_dirty.clone());
        // Chain this UI's submissions before moving blocking settings IO off
        // the main thread. A fast Start→Pause keeps click order on Tokio too.
        let previous = self.monitor_command_tail.take();
        self.monitor_command_tail = Some(self.rt.spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let _ = tokio::task::spawn_blocking(move || {
                let outcome = match command {
                    "start" => host.start_monitor(),
                    "pause" => host.pause_monitor(),
                    "pull" => host.request_pull(),
                    "save" => host.save_monitor_settings(patch.unwrap_or_default()),
                    "scan-start" => host.start_schedule(),
                    "scan-stop" => host.stop_schedule(),
                    "scan-save" => host.save_schedule_settings(patch.unwrap_or_default()),
                    "records" => {
                        *records.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(engine.monitor_events(&tune_core::monitor_journal::Filter {
                            limit: Some(50),
                            before_id: patch.as_ref().and_then(|p| p["before_id"].as_i64()),
                            ..Default::default()
                        }));
                        Ok(())
                    }
                    _ => unreachable!(),
                };
                *result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(match outcome {
                    Ok(()) => "操作を受け付けました。状態欄で完了を確認できます".into(),
                    Err(error) => error,
                });
                dirty.store(true, Ordering::Release);
            })
            .await;
        }));
    }

    /// 機体のライブ値（CPU・メモリ）の枠。詳細画面とリソース画面で共有する。
    fn live_panel(&self, id: &str) -> gpui::Div {
        let l = {
            let store = self.live_store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            store.0.get(id).cloned().unwrap_or_default()
        };
        let now = |v: Option<f64>| v.map_or("–".to_string(), |x| format!("{x:.0}%"));
        let status = if l.cpu.is_empty() { Some(format!("接続中… {}", l.detail.clone().unwrap_or_default())) } else { None };
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
            )
            .when(!l.gpu.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_xs().text_color(palette::muted()).child("GPU 使用率"))
                                .child(div().text_xl().font_weight(FontWeight::BOLD).child(now(l.latest_gpu))),
                        )
                        .child(trend(&l.gpu, 100.0, palette::ok(), 48.0)),
                )
            })
    }

    /// リソース画面の上部。台帳で観測できる機体を選んでライブ（1 秒ごと）を一つだけ開始する。
    /// 画面を開く・再読込するだけでは開始せず、別画面への移動や非アクティブ化で止まる。
    fn resources_live(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut chips = div().flex().flex_wrap().gap_2();
        for node in self.nodes.iter().filter(|n| !matches!(n.health, Health::Excluded(_))) {
            let id = node.id.clone();
            let on = self.live.on(&id);
            chips = chips.child(
                div()
                    .id(gpui::SharedString::from(format!("resources-live:{id}")))
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .bg(if on { palette::crit() } else { palette::card() })
                    .border_1()
                    .border_color(palette::line())
                    .child(if on { format!("{id} を止める") } else { format!("{id} のライブ") })
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
                    ),
            );
        }
        let active = self.nodes.iter().find(|n| self.live.on(&n.id)).map(|n| n.id.clone());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_5()
            .py_3()
            .child(div().text_xs().text_color(palette::muted()).child("ライブは選んだ機体を一つだけ、押したときに開始します。"))
            .child(chips)
            .when_some(active, |d, id| d.child(self.live_panel(&id)))
    }
    fn dashboard_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut toolbar = div().flex().flex_wrap().gap_2().px_5().py_3();
        for minutes in [60, 360, 1440] {
            toolbar = toolbar.child(
                div()
                    .id(("dashboard-range", minutes as usize))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .bg(if self.dashboard_minutes == minutes { palette::accent() } else { palette::card() })
                    .child(format!("{}時間", minutes / 60))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |app, _, _, cx| {
                            app.dashboard_minutes = minutes;
                            app.reload(cx);
                        }),
                    ),
            );
        }
        toolbar = toolbar.child(
            div()
                .id("dashboard-reload")
                .px_3()
                .py_2()
                .rounded_md()
                .bg(palette::card())
                .text_sm()
                .cursor_pointer()
                .child("再読込")
                .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| app.reload(cx))),
        );
        for (command, label) in [("start", "監視開始"), ("pause", "監視停止"), ("scan-start", "自動スキャン開始"), ("scan-stop", "スキャン停止")]
        {
            toolbar = toolbar.child(
                div()
                    .id(command)
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(palette::card())
                    .text_sm()
                    .cursor_pointer()
                    .child(label)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, _| app.monitor_command(command, None))),
            );
        }
        toolbar = toolbar.child(
            div()
                .id("dashboard-details")
                .px_3()
                .py_2()
                .rounded_md()
                .bg(palette::card())
                .text_sm()
                .cursor_pointer()
                .child(if self.dashboard_details { "詳細を閉じる" } else { "詳細" })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|app, _, _, cx| {
                        app.dashboard_details = !app.dashboard_details;
                        cx.notify();
                    }),
                ),
        );
        let observed = self.monitor.snapshot();
        let active = self.live.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active;
        let (scan_enabled, scan) = self.monitor.schedule_snapshot();
        use crate::monitor_host::SchedulePhase;
        use tune_core::monitor::{RunMode, RunPhase};
        let phase = match observed.status.phase {
            RunPhase::Starting => "開始待ち",
            RunPhase::Running => "監視中",
            RunPhase::Paused => "停止中",
            RunPhase::Stopping => "停止待ち",
            RunPhase::Failed => "失敗",
            RunPhase::Exited => "終了",
        };
        let scan_phase = match scan.phase {
            SchedulePhase::Idle => "停止中",
            SchedulePhase::Waiting => "次回待ち",
            SchedulePhase::Running => "実行中",
            SchedulePhase::ManualBusy => "手動収集のため見送り",
            SchedulePhase::Stopping => "完了後に停止",
            SchedulePhase::Failed => "失敗",
            SchedulePhase::Exited => "終了",
        };
        let status = div().px_5().py_1().text_xs().child(format!(
            "監視 {phase} · 観測中 {}台 ｜ 自動スキャン {scan_phase}{}",
            observed.streams,
            scan.error.as_ref().map(|e| format!(" · {e}")).unwrap_or_default()
        ));
        let mut details = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().px_5().text_xs().child(format!(
                "監視の開始許可 {} · 処理 {} · 採取方針 {} · ローカル採取 {} · 最新サンプル {}（最大200台で確認） · ウィンドウ表示 未観測 / 操作中 {}",
                match observed.mode {
                    RunMode::Run => "あり",
                    RunMode::Paused => "なし",
                    RunMode::Exit => "終了",
                },
                if observed.started { "開始済" } else { "未開始" },
                match observed.status.policy {
                    Some(tune_core::monitor_journal::Policy::Local) => "この操作卓で採取",
                    Some(tune_core::monitor_journal::Policy::Hub) => "ハブから取得",
                    Some(tune_core::monitor_journal::Policy::Disabled) => "無効",
                    Some(tune_core::monitor_journal::Policy::StreamBackoff) => "再接続待ち",
                    Some(tune_core::monitor_journal::Policy::PlannedRecycle) => "定期再接続",
                    Some(tune_core::monitor_journal::Policy::MonitorDisconnected) => "切断",
                    Some(tune_core::monitor_journal::Policy::SettingsUnknown) | None => "未確認",
                },
                observed.status.local_collecting.map(|b| if b { "はい" } else { "いいえ" }).unwrap_or("未観測"),
                observed.latest_sample.map_or_else(|| "未確認".into(), |t| crate::panels::time(&serde_json::json!(t))),
                if active { "はい" } else { "いいえ" }
            )))
            .child(div().px_5().text_xs().child(format!(
                "自動スキャンの開始許可 {} · 完了 {}回 · 最終完了 {}",
                if scan_enabled { "あり" } else { "なし" },
                scan.cycles,
                scan.last_completed.map_or_else(|| "未実行".into(), |t| crate::panels::time(&serde_json::json!(t)))
            )));
        let mut tools = div().px_5().flex().flex_wrap().gap_2();
        for (command, label) in [("pull", "ハブ取得"), ("records", "記録50件")] {
            tools = tools.child(
                div()
                    .id(command)
                    .px_3()
                    .py_2()
                    .bg(palette::card())
                    .text_xs()
                    .cursor_pointer()
                    .child(label)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, _| app.monitor_command(command, None))),
            );
        }
        let before_id = self
            .monitor_records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|v| v["next_before_id"].as_i64());
        if let Some(before_id) = before_id {
            tools =
                tools.child(div().id("monitor-older-records").px_3().py_2().bg(palette::card()).text_sm().cursor_pointer().child("前の50件").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _, _, _| app.monitor_command("records", Some(serde_json::json!({"before_id":before_id})))),
                ));
        }
        details = details.child(tools);
        let mut settings = div().id("monitor-settings").px_5().py_2().flex().flex_wrap().gap_2();
        if let Some(Ok(view)) = &self.dashboard {
            let enabled = view.saved["monitor"]["enabled"].as_bool().unwrap_or(false);
            settings = settings.child(
                div()
                    .id("monitor-enabled")
                    .px_3()
                    .py_2()
                    .bg(palette::card())
                    .text_xs()
                    .cursor_pointer()
                    .child(if enabled { "監視設定を無効にして保存" } else { "監視設定を有効にして保存" })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |app, _, _, _| app.monitor_command("save", Some(serde_json::json!({"enabled":!enabled})))),
                    ),
            );
            let scan_saved = view.saved["schedule"]["enabled"].as_bool();
            if let Some(enabled) = scan_saved {
                settings = settings.child(
                    div()
                        .id("schedule-enabled")
                        .px_3()
                        .py_2()
                        .bg(palette::card())
                        .text_xs()
                        .cursor_pointer()
                        .child(if enabled { "自動スキャン設定を無効にして保存" } else { "自動スキャン設定を有効にして保存" })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |app, _, _, _| app.monitor_command("scan-save", Some(serde_json::json!({"enabled":!enabled})))),
                        ),
                );
            }
            for (key, label, choices) in [
                ("probe_minutes", "分析（分）", [15, 60, 240]),
                ("logs_minutes", "ログ（分）", [15, 60, 240]),
                ("inventory_hours", "道具（時間）", [6, 24, 168]),
                ("ai_minutes", "AI（分）", [15, 30, 60]),
            ] {
                let mut row = div().flex().flex_wrap().gap_2().child(div().text_xs().child(format!("{label} · 保存 {}", view.saved["schedule"][key])));
                for value in choices {
                    row = row.child(
                        div().id((key, value as usize)).px_2().bg(palette::card()).text_xs().cursor_pointer().child(format!("{value}に保存")).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |app, _, _, _| app.monitor_command("scan-save", Some(serde_json::json!({key:value})))),
                        ),
                    );
                }
                settings = settings.child(row);
            }
            let hubs = std::iter::once(None).chain(
                self.engine.config().nodes.into_iter().filter(|n| n.is_active() && !n.local && (n.is_mac() || n.is_windows())).take(200).map(|n| Some(n.id)),
            );
            for (i, hub) in hubs.enumerate() {
                let label = hub.as_ref().map_or_else(|| "ハブなしを保存".into(), |id| format!("ハブ {id} を保存"));
                settings =
                    settings.child(
                        div().id(("monitor-hub", i)).px_3().py_2().bg(palette::card()).text_xs().cursor_pointer().child(label).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |app, _, _, _| app.monitor_command("save", Some(serde_json::json!({"hub":hub})))),
                        ),
                    );
            }
        }
        details = details
            .child(settings)
            .child(div().px_5().text_xs().text_color(palette::muted()).child("保存だけでは開始しません。停止は実行中の処理の完了を待ちます。"));
        if let Some(Ok(view)) = &self.dashboard {
            details = details.child(dashboard::render_details(view));
        }
        let mut body = div().flex_1().min_h_0().flex().flex_col().child(toolbar).child(status);
        if let Some(records) = &*self.monitor_records.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            let text = match records {
                Err(error) => error.clone(),
                Ok(value) => {
                    let rows = value["events"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .take(50)
                        .map(|e| {
                            format!("{} {} commit={} category={}", crate::panels::time(&e["ts"]), e["kind"], e["data"]["committed"], e["data"]["category"])
                        })
                        .collect::<Vec<_>>()
                        .join(" · ");
                    format!(
                        "明示照会のみ · health {} · 未確認ACKは成功を意味しません · {}",
                        value["health"],
                        if rows.is_empty() { "記録なし".into() } else { rows }
                    )
                }
            };
            details = details.child(div().id("monitor-records").max_h(px(100.)).overflow_y_scroll().px_5().text_xs().child(text));
        }
        if self.loading {
            body = body.child(div().px_5().text_xs().text_color(palette::muted()).child("保存集計を読込中…"));
        }
        match &self.dashboard {
            Some(Ok(view)) => body.child(
                div().id("dashboard-saved").flex_1().overflow_y_scroll().when(self.dashboard_details, |d| d.child(details)).child(dashboard::render_dashboard(
                    view,
                    &self.live.live,
                    now_ms(),
                    self.dashboard_details,
                )),
            ),
            Some(Err(error)) => body.child(div().p_5().text_sm().text_color(palette::crit()).child(format!("保存Dashboardの読み込みに失敗: {error}"))),
            None => body.child(div().p_5().text_sm().text_color(palette::muted()).child("保存Dashboardを読み込んでいます…")),
        }
    }

    fn record_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut toolbar = div().flex().flex_wrap().flex_none().items_center().gap_2().px_5().py_3();
        if self.page != Page::Network {
            toolbar = toolbar.child(div().flex_1().min_w(px(180.)).child(self.search.clone())).child(
                div().id("search-submit").px_3().py_2().rounded_md().bg(palette::accent()).text_sm().cursor_pointer().child("検索").on_mouse_down(
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
                ),
            );
        } else {
            toolbar = toolbar.child(
                div()
                    .id("network-explanation")
                    .w_full()
                    .max_h(px(80.))
                    .overflow_y_scroll()
                    .text_xs()
                    .text_color(palette::muted())
                    .child(crate::network::EXPLANATION),
            );
            for active in [false, true] {
                let busy = self.probing || self.collection.running.is_some();
                toolbar = toolbar.child(
                    div()
                        .id(if active { "network-active" } else { "network-passive" })
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .text_sm()
                        .bg(if busy { palette::line() } else { palette::accent() })
                        .when(!busy, |d| d.cursor_pointer())
                        .child(if busy {
                            "収集実行中…"
                        } else if active {
                            "全対象の外部到達診断"
                        } else {
                            "全対象の設定確認（HTTPSなし）"
                        })
                        .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| app.collect(Kind::Network(active), cx))),
                );
            }
        }
        toolbar = toolbar.child(
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
            let busy = self.probing || self.collection.running.is_some();
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
            if self.loading {
                "読み込み中（前回の表示）"
            } else if self.page == Page::Network {
                "セッション内の診断 · ↑↓で詳細"
            } else {
                "保存済みデータ · ↑↓で詳細 / Tabで検索へ"
            }
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
            None if self.page == Page::Dashboard => self.dashboard_panel(cx).into_any_element(),
            None if self.page == Page::Review => self
                .record_panel(cx)
                .on_drop(cx.listener(|app, paths: &gpui::ExternalPaths, _, cx| {
                    if let Some(path) = paths.paths().first() {
                        app.load_review(path, cx);
                    }
                }))
                .into_any_element(),
            None if self.page == Page::Resources => {
                div().flex_1().min_h_0().flex().flex_col().child(self.resources_live(cx)).child(self.record_panel(cx)).into_any_element()
            }
            None => self.record_panel(cx).into_any_element(),
        };
        div()
            .key_context("Browser")
            .track_focus(&self.focus)
            .on_action(cx.listener(|app, _: &Quit, _, cx| {
                app.live.close();
                app.monitor.request_exit();
                cx.quit();
            }))
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(cx.listener(|app, _: &GoOverview, window, cx| {
                app.focus.focus(window);
                app.navigate(Page::Overview, None, cx);
            }))
            .on_action(cx.listener(|app, _: &NextPage, window, cx| app.cycle_page(1, window, cx)))
            .on_action(cx.listener(|app, _: &PreviousPage, window, cx| app.cycle_page(-1, window, cx)))
            .on_action(cx.listener(|app, _: &Refresh, _, cx| app.reload(cx)))
            .on_action(cx.listener(|app, _: &FocusSearch, window, cx| {
                if app.selected.is_none() && !matches!(app.page, Page::Overview | Page::Dashboard | Page::Network) {
                    app.search.focus_handle(cx).focus(window);
                }
            }))
            .on_action(cx.listener(|app, _: &FocusContent, window, _| app.focus.focus(window)))
            .on_action(cx.listener(|app, _: &ToggleFocus, window, cx| {
                if app.selected.is_none()
                    && !matches!(app.page, Page::Overview | Page::Dashboard | Page::Network)
                    && !app.search.focus_handle(cx).is_focused(window)
                {
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
    let keys = [
        "dashboard",
        "overview",
        "status",
        "resources",
        "processes",
        "jobs",
        "logs",
        "tools",
        "ai",
        "security",
        "network",
        "specs",
        "power",
        "topology",
        "actions",
        "link",
        "review",
    ];
    if let Some((_, page)) = keys.into_iter().zip(Page::ALL).find(|(key, page)| *key == target || page.label() == target) {
        return Ok(Route::Page(page));
    }
    if ledger.iter().any(|node| node.id == target) {
        return Ok(Route::Node(target.into()));
    }
    Err(format!("--open の表示または機体が見つかりません: {target}"))
}

/// GPUI's native termination (notably NSApplication::terminate:) need not
/// return from Application::run. Drain in the synchronous quit observer, before
/// GPUI applies its 100ms allowance to returned futures. Workers use Tokio,
/// atomic dirty flags and channels; none waits for GPUI or a confirmation UI.
fn finish_shutdown(rt: &tokio::runtime::Runtime, host: &crate::monitor_host::MonitorHost, failed: &AtomicBool) -> crate::monitor_host::ShutdownReport {
    let report = rt.block_on(host.shutdown());
    if !report.confirmed() {
        eprintln!("katala-tune-gpui: 終了または収集記録の保存ACKを確認できませんでした: {report:?}");
        failed.store(true, Ordering::Release);
    }
    // Native termination can bypass the outer exit-status check as well.
    if failed.load(Ordering::Acquire) {
        std::process::exit(1);
    }
    report
}

pub fn run(options: startup::Options) {
    let startup::Options { config, data, selftest, open, live_id, logs, confirm_demo, review: review_path, shutdown_check } = options;
    let shutdown_gate = data.join("shutdown-check");
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
        Live::new(
            Arc::new(ProcessRoute),
            tune_core::monitor::live_opts(engine.config().nodes.len()),
            Box::new(move |event| {
                store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).merge(&event);
                flag.store(true, Ordering::Release);
            }),
        )
    };
    let monitor = Arc::new(crate::monitor_host::MonitorHost::new(engine.clone(), live.clone(), rt.handle().clone(), dirty.clone()));
    let monitor_exit = monitor.clone();
    let handles = LiveHandles { monitor, live: live.clone(), store: live_store, dirty: live_dirty };
    let rt_exit = rt.clone();
    let failed = Arc::new(AtomicBool::new(false));
    let failed_exit = failed.clone();

    let quit_host = monitor_exit.clone();
    let quit_rt = rt_exit.clone();
    let quit_failed = failed_exit.clone();
    let shutdown_diagnostic = shutdown_check.is_some();
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
        let host = quit_host.clone();
        cx.on_app_quit(move |cx| {
            // Close the UI admission gate before blocking, independently of
            // entity observer order. Last-window release already invalidates
            // pending UI intent when the handle can no longer be updated.
            window.update(cx, |app, _, _| app.live.close()).ok();
            let report = finish_shutdown(&quit_rt, &host, &quit_failed);
            if shutdown_diagnostic {
                println!("selftest shutdown: {report:?}");
            }
            async {}
        })
        .detach();
        cx.on_window_closed(move |cx| {
            if cx.windows().is_empty() {
                quit_host.request_exit();
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
        if let Some(path) = &review_path {
            window
                .update(cx, |app, _, cx| {
                    app.load_review(path, cx);
                    app.navigate(Page::Review, None, cx);
                })
                .ok();
        }
        if let Some(route) = initial_route {
            window
                .update(cx, |app, _, cx| match route {
                    Route::Page(page) => app.navigate(page, None, cx),
                    Route::Node(id) => app.navigate(Page::Overview, Some(id), cx),
                })
                .ok();
        }
        if let Some(mode) = shutdown_check {
            // Explicit synthetic desktop diagnostic only. The parent fixture
            // injects fake SSH and controls readiness/release outside GPUI.
            cx.spawn(async move |cx| {
                let result: Result<(), String> = async {
                    window
                        .update(cx, |app, _, cx| {
                            {
                                let _runtime = app.rt.enter();
                                app.engine.start_monitor_journal();
                            }
                            app.collect(Kind::Probe, cx);
                        })
                        .map_err(|e| e.to_string())?;
                    let deadline = Instant::now() + Duration::from_secs(15);
                    while !shutdown_gate.join("child-ready").exists() {
                        if Instant::now() >= deadline {
                            return Err("shutdown fixture child was not ready".into());
                        }
                        cx.background_executor().timer(Duration::from_millis(25)).await;
                    }
                    std::fs::write(shutdown_gate.join("closing"), mode.as_bytes()).map_err(|e| e.to_string())?;
                    match mode.as_str() {
                        "close" => window.update(cx, |_, window, _| window.remove_window()).map_err(|e| e.to_string())?,
                        "quit" => window.update(cx, |_, window, cx| window.dispatch_action(Box::new(Quit), cx)).map_err(|e| e.to_string())?,
                        "native-quit" => cx.update(|cx| cx.quit()).map_err(|e| e.to_string())?,
                        _ => unreachable!(),
                    }
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    failed.store(true, Ordering::Release);
                    eprintln!("selftest shutdown: {error}");
                    cx.update(|cx| cx.quit()).ok();
                }
            })
            .detach();
        } else if selftest {
            cx.spawn(async move |cx| {
                let result: Result<(), String> = async {
                    let mut count = 0;
                    let mut nodes = 0;
                    let mut log_counts = None;
                    let mut review_rows = None;
                    // All saved-data routes run before any explicit live diagnostic.
                    for (visit, page) in Page::ALL.into_iter().flat_map(|p| [p, p]).enumerate() {
                        window
                            .update(cx, |app, _, cx| {
                                if page == Page::Dashboard {
                                    app.dashboard_details = visit % 2 != 0;
                                }
                                if visit % 2 == 0 { app.navigate(page, None, cx) } else { app.reload(cx) }
                            })
                            .map_err(|_| "selftest window closed".to_string())?;
                        let mut loaded = false;
                        for _ in 0..100 {
                            cx.background_executor().timer(Duration::from_millis(100)).await;
                            let state = window
                                .update(cx, |app, _, _| {
                                    if app.collection.running.is_some()
                                        || app.collection.result.is_some()
                                        || !app.live.live.active().is_empty()
                                        || app.engine.monitor_journal_health()["running"] == true
                                        || app.monitor.schedule_snapshot().0
                                        || app.monitor.schedule_snapshot().1.cycles != 0
                                    {
                                        return Some((app.nodes.len(), Some("保存画面の描画が収集を開始しました".into())));
                                    }
                                    if app.loading {
                                        return None;
                                    }
                                    if review_path.is_some() && page == Page::Review {
                                        review_rows = app.panel.as_ref().and_then(|p| p.as_ref().ok()).map(|p| p.rows.len());
                                    }
                                    if logs && page == Page::Logs {
                                        log_counts = app.panel.as_ref().and_then(|p| p.as_ref().ok()).map(|p| {
                                            (
                                                p.rows.iter().filter(|r| r.title.starts_with("ログ · ")).count(),
                                                p.rows.iter().filter(|r| r.title.starts_with("署名 · ")).count(),
                                            )
                                        });
                                    }
                                    let view_error = if page == Page::Dashboard {
                                        match &app.dashboard {
                                            Some(Ok(_)) => None,
                                            Some(Err(error)) => Some(error.clone()),
                                            None => Some("専用Dashboard結果がありません".into()),
                                        }
                                    } else {
                                        app.panel.as_ref().and_then(|p| p.as_ref().err().cloned())
                                    };
                                    Some((app.nodes.len(), app.notice.clone().or(view_error)))
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
                        if visit % 2 == 1 {
                            count += 1;
                        }
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
                    if review_path.is_some() {
                        println!("selftest review rows={}", review_rows.ok_or_else(|| "selftest review did not load".to_string())?);
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
    // Some platforms return from run; the same idempotent report also protects
    // that fallback path. macOS normally terminates inside the native callback.
    finish_shutdown(&rt_exit, &monitor_exit, &failed_exit);
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

    /// Synthetic transport only; Live parses samples and owns all lifecycle state.
    #[derive(Default)]
    struct SteadyRoute(AtomicUsize);
    impl LiveRoute for SteadyRoute {
        fn open(&self, _: &nodes::Node, _: &Opts) -> Result<Source, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let (lines, receive) = tokio::sync::mpsc::channel(4);
            let (stop, mut stopped) = tokio::sync::oneshot::channel();
            let (done, finished) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(10));
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        _ = interval.tick() => {
                            if lines.send(r#"{"type":"s","cpu":5}"#.into()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                let _ = done.send(tune_core::live::Ended::default());
            });
            Ok(Source::new(receive, stop, finished))
        }
    }

    fn shared_controller() -> (LiveControl, Arc<SteadyRoute>) {
        let route = Arc::new(SteadyRoute::default());
        let live = Live::new(route.clone(), Opts::default(), Box::new(|_| {}));
        (LiveControl::new(live, true), route)
    }

    async fn samples_after(live: &Live, before: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while live.samples("a").len() <= before {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the existing shared sampler must continue delivering samples");
    }

    #[tokio::test]
    async fn quit_rejects_pending_live_completion_and_reactivation() {
        let (control, route) = controller();
        let (generation, id) = control.request("a").unwrap();
        control.close();
        control.set_active(true);
        control.complete(generation, &id, &[node("a")], None);
        assert!(control.request("a").is_none());
        assert!(control.live.active().is_empty());
        assert_eq!(route.0.load(Ordering::SeqCst), 0);
        control.live.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn live_ui_stop_navigation_inactivity_and_release_preserve_monitor_samples() {
        for action in ["manual", "navigation", "inactive", "release", "pending_hidden"] {
            let (control, route) = shared_controller();
            control.live.start_owned(Owner::Monitor, &[node("a")], &[]);
            let (generation, id) = control.request("a").unwrap();
            if action != "pending_hidden" {
                control.complete(generation, &id, &[node("a")], None);
            }
            samples_after(&control.live, 0).await;
            match action {
                "manual" | "navigation" => control.stop(Stop::User),
                "inactive" | "pending_hidden" => control.set_active(false),
                "release" => control.stop(Stop::Hidden),
                _ => unreachable!(),
            }
            let active = control.live.active();
            if active == ["a"] {
                samples_after(&control.live, control.live.samples("a").len()).await;
            }
            let monitor_renewed = control.live.heartbeat_owned(Owner::Monitor, "a");
            let released = control.live.release_owned(Owner::Monitor, &["a".into()], Stop::User);
            control.live.shutdown(Duration::from_secs(1)).await;
            assert!(!control.on("a"));
            assert_eq!(active, ["a"], "{action} may release only UI intent");
            assert!(monitor_renewed, "{action} must preserve the monitor claim");
            assert_eq!(released["stopped"], json!(["a"]));
            assert_eq!(route.0.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn live_accepted_reader_removal_stops_both_owners_before_navigation() {
        let dir = std::env::temp_dir().join(format!("tune-owner-removal-{}-{}", std::process::id(), tune_core::db::now_ms()));
        std::fs::create_dir(&dir).unwrap();
        let config = dir.join("nodes.json");
        std::fs::write(&config, json!({"nodes":[{"id":"a","os":"macos"},{"id":"b","os":"macos"}]}).to_string()).unwrap();
        let engine = Engine::open(config.clone(), dir.join("data"), Arc::new(tune_core::engine::NoHost)).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reader = Reader::new(engine.clone(), &rt);
        rt.block_on(async {
            let (control, route) = shared_controller();
            let (generation, id) = control.request("a").unwrap();
            control.complete(generation, &id, &[node("a"), node("b")], None);
            control.live.start_owned(Owner::Monitor, &[node("a"), node("b")], &[]);
            samples_after(&control.live, 0).await;
            std::fs::write(&config, json!({"nodes":[{"id":"b","os":"macos"}]}).to_string()).unwrap();
            reader.request(Request { generation: 1, route: Route::Node("a".into()), query: "a".into(), log_level: LogLevel::All, minutes: 60 }).unwrap();
            let response = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(response) = reader.take() {
                        break response;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(response.generation, 1);
            assert!(response.config_error.is_none());
            let views = response.nodes.unwrap();
            assert_eq!(views.iter().map(|node| node.id.as_str()).collect::<Vec<_>>(), ["b"]);
            control.accept_nodes(&response.ledger.unwrap());
            control.stop(Stop::User); // The selected removed route navigates away.
            let active = control.live.active();
            let ui_renewed = control.live.heartbeat_owned(Owner::Ui, "a");
            let monitor_renewed = control.live.heartbeat_owned(Owner::Monitor, "a");
            let retained = control.live.heartbeat_owned(Owner::Monitor, "b");
            if active == ["b"] {
                tokio::time::timeout(Duration::from_secs(1), async {
                    while control.live.brief("a").unwrap()["state"] != "stopped" {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            let stopped = control.live.brief("a").unwrap()["state"] == "stopped";
            control.live.shutdown(Duration::from_secs(1)).await;
            assert_eq!(active, ["b"], "accepted full-ledger removal is an administrative stop before UI navigation");
            assert!(!ui_renewed && !monitor_renewed, "neither owner may renew a removed node");
            assert!(stopped, "the removed sampler must finish");
            assert!(retained, "the complete Reader ledger preserves the remaining monitor stream");
            assert_eq!(route.0.load(Ordering::SeqCst), 2);
        });
        drop(reader);
        drop(rt);
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn live_accepted_exclusion_stops_monitor_owned_nodes_without_ui_intent() {
        let (control, _) = shared_controller();
        control.live.start_owned(Owner::Monitor, &[node("a")], &[]);
        let ledger = [nodes::Node::from_value(&json!({"id":"a","status":"paused"}), "synthetic-host")];
        control.accept_nodes(&ledger);
        let active = control.live.active();
        let renewed = control.live.heartbeat_owned(Owner::Monitor, "a");
        control.live.shutdown(Duration::from_secs(1)).await;
        assert!(active.is_empty(), "accepted exclusion is administrative, regardless of UI intent");
        assert!(!renewed);
    }

    #[tokio::test]
    async fn live_current_ledger_read_failure_stops_both_owners() {
        for from_reader in [false, true] {
            let (control, _) = shared_controller();
            let (generation, id) = control.request("a").unwrap();
            control.complete(generation, &id, &[node("a")], None);
            control.live.start_owned(Owner::Monitor, &[node("a")], &[]);
            if from_reader {
                control.stop(Stop::Excluded);
            } else {
                control.complete(generation, &id, &[], Some("invalid synthetic ledger".into()));
            }
            assert!(control.live.active().is_empty());
            assert!(!control.on("a"));
            assert!(!control.live.heartbeat_owned(Owner::Monitor, "a"));
            control.live.shutdown(Duration::from_secs(1)).await;
        }
    }

    #[tokio::test]
    async fn live_monitor_release_preserves_the_ui_claim_and_samples() {
        let (control, route) = shared_controller();
        let (generation, id) = control.request("a").unwrap();
        control.complete(generation, &id, &[node("a")], None);
        control.live.start_owned(Owner::Monitor, &[node("a")], &[]);
        samples_after(&control.live, 0).await;
        let stopped = control.live.release_owned(Owner::Monitor, &["a".into()], Stop::User);
        let active = control.live.active();
        if active == ["a"] {
            samples_after(&control.live, control.live.samples("a").len()).await;
        }
        let renewed = control.live.heartbeat_owned(Owner::Ui, "a");
        control.stop(Stop::User);
        control.live.shutdown(Duration::from_secs(1)).await;
        assert_eq!(stopped["stopped"], json!([]));
        assert_eq!(active, ["a"]);
        assert!(renewed);
        assert_eq!(route.0.load(Ordering::SeqCst), 1);
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
            let ledger = [nodes::Node::from_value(&json!({"id":"a","status":status}), "synthetic-host")];
            control.accept_nodes(&ledger);
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
        let ledger = [node("a"), nodes::Node::from_value(&json!({"id":"b","status":"paused"}), "synthetic-host")];
        control.accept_nodes(&ledger);
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
        assert_eq!(open_route("dashboard", &[]).unwrap(), Route::Page(Page::Dashboard));
        assert_eq!(open_route("network", &[]).unwrap(), Route::Page(Page::Network));
        assert_eq!(open_route("logs", &[]).unwrap(), Route::Page(Page::Logs));
        assert_eq!(open_route("resources", &[]).unwrap(), Route::Page(Page::Resources));
        assert_eq!(open_route("構成図", &[]).unwrap(), Route::Page(Page::Topology));
        assert_eq!(open_route("a", &[node("a")]).unwrap(), Route::Node("a".into()));
        assert!(open_route("missing", &[node("a")]).is_err());
    }
}
