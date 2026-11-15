//! Katala Tune のネイティブ画面（Rust + GPUI）。WebView も Node も使わず、tune-core の Engine を直接呼ぶ。
//!
//! 実行: `katala-tune [--config 台帳.json] [--data データdir] [--selftest]`
//! `--selftest` は画面を開いて台帳と前回結果を読み、1 フレーム描いて終了する（E2E 用。分析は走らせない）。

mod gate;
mod model;

use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    App, Application, Bounds, Context, FontWeight, Hsla, MouseButton, Window, WindowBounds, WindowOptions, div, hsla, point,
    prelude::*, px, rgb, size,
};
use gate::Gate;
use model::{Finding, Health, NodeView, Severity};
use tune_core::engine::{Engine, Host};
use tune_core::live::{Live, ProcessRoute, Stop};
use tune_core::nodes;

#[cfg(target_os = "macos")]
const FONT: &str = ".SystemUIFont";
#[cfg(not(target_os = "macos"))]
const FONT: &str = "Segoe UI";

mod palette {
    use gpui::{Hsla, rgb};
    pub fn bg() -> Hsla { rgb(0x111318).into() }
    pub fn panel() -> Hsla { rgb(0x181b22).into() }
    pub fn card() -> Hsla { rgb(0x1e222b).into() }
    pub fn line() -> Hsla { rgb(0x2a2f3a).into() }
    pub fn text() -> Hsla { rgb(0xe8eaef).into() }
    pub fn muted() -> Hsla { rgb(0x8b93a3).into() }
    pub fn accent() -> Hsla { rgb(0x4f8cff).into() }
    pub fn ok() -> Hsla { rgb(0x3fbf7f).into() }
    pub fn warn() -> Hsla { rgb(0xe5a93d).into() }
    pub fn crit() -> Hsla { rgb(0xe5584f).into() }
}

/// 分析・取り込みの完了などを画面側に知らせるだけの Host（描画はここでは触らない）
struct DirtyHost(Arc<AtomicBool>);

impl Host for DirtyHost {
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
        Health::Unobserved => palette::muted(),
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
fn trend(values: &[f64], max: f64, color: Hsla, height: f32, take: usize) -> gpui::Div {
    let mut bars = div().h(px(height)).flex().items_end().gap_px();
    for v in values.iter().rev().take(take).rev() {
        let h = ((v / max).clamp(0.0, 1.0) as f32 * height).max(1.0);
        bars = bars.child(div().w(px(3.0)).h(px(h)).rounded_sm().bg(color));
    }
    bars
}

fn chip(text: impl Into<gpui::SharedString>, color: Hsla) -> gpui::Div {
    div()
        .px_2()
        .py_0p5()
        .rounded_md()
        .text_xs()
        .text_color(color)
        .bg(hsla(color.h, color.s, color.l, 0.14))
        .child(text.into())
}

/// 機体を選んでいないときに出す画面
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Status,
    Logs,
    Topology,
    Actions,
    Schedule,
    Resources,
    Power,
    Tools,
    Ai,
    Specs,
    Processes,
    Security,
    Network,
    Link,
}

impl Page {
    /// サイドバーに並べる順
    const NAV: [Self; 14] = [Self::Status, Self::Resources, Self::Processes, Self::Security, Self::Network, Self::Link, Self::Power, Self::Specs, Self::Tools, Self::Ai, Self::Logs, Self::Topology, Self::Actions, Self::Schedule];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "概要",
            Self::Status => "状態",
            Self::Logs => "ログ",
            Self::Topology => "構成図",
            Self::Actions => "実行記録",
            Self::Schedule => "スケジュール",
            Self::Resources => "リソース",
            Self::Power => "電力・Clock",
            Self::Tools => "道具",
            Self::Ai => "AI",
            Self::Specs => "スペック比較",
            Self::Processes => "プロセス",
            Self::Security => "セキュリティ",
            Self::Network => "ネットワーク",
            Self::Link => "接続",
        }
    }

    fn from_arg(s: &str) -> Option<Self> {
        Some(match s {
            "status" => Self::Status,
            "logs" => Self::Logs,
            "topology" => Self::Topology,
            "actions" => Self::Actions,
            "schedule" => Self::Schedule,
            "resources" => Self::Resources,
            "power" => Self::Power,
            "tools" => Self::Tools,
            "ai" => Self::Ai,
            "specs" => Self::Specs,
            "processes" => Self::Processes,
            "security" => Self::Security,
            "network" => Self::Network,
            "link" => Self::Link,
            _ => return None,
        })
    }
}

struct LiveHandles {
    live: Arc<Live>,
    store: Arc<Mutex<model::LiveStore>>,
    dirty: Arc<AtomicBool>,
}

struct TuneApp {
    engine: Arc<Engine>,
    rt: Arc<tokio::runtime::Runtime>,
    dirty: Arc<AtomicBool>,
    nodes: Vec<NodeView>,
    selected: Option<String>,
    probing: bool,
    notice: Option<String>,
    live: Arc<Live>,
    live_store: Arc<Mutex<model::LiveStore>>,
    live_dirty: Arc<AtomicBool>,
    /// ライブを流している機体と、最後に合図（heartbeat）を送った時刻
    live_on: Option<(String, Instant)>,
    page: Page,
    actions: Vec<model::ActionRow>,
    power: model::PowerView,
    tools: model::InventoryView,
    ai: model::AiView,
    specs: model::SpecsView,
    security: model::SecurityView,
    peers: Vec<model::PeerRow>,
    network: Arc<Mutex<Option<Vec<model::NetRow>>>>,
    network_busy: Arc<AtomicBool>,
    gate: Gate,
    /// 変更操作の結果（画面上部に出す）
    result: Arc<Mutex<Option<String>>>,
    schedule: model::ScheduleView,
    status: model::StatusView,
    topology: model::TopologyView,
    log_level: Option<&'static str>,
    logs: Vec<model::LogRow>,
    sigs: Vec<model::Signature>,
}

impl TuneApp {
    fn new(engine: Arc<Engine>, rt: Arc<tokio::runtime::Runtime>, dirty: Arc<AtomicBool>, live: LiveHandles, cx: &mut Context<Self>) -> Self {
        let live_dirty_for_gate = live.dirty.clone();
        let mut app = Self { engine, rt, dirty, nodes: Vec::new(), selected: None, probing: false, notice: None, live: live.live, live_store: live.store, live_dirty: live.dirty, live_on: None, page: Page::Overview, actions: Vec::new(), power: Default::default(), tools: Default::default(), ai: Default::default(), specs: Default::default(), security: Default::default(), peers: Vec::new(), network: Arc::new(Mutex::new(None)), network_busy: Arc::new(AtomicBool::new(false)), gate: { let f = live_dirty_for_gate; Gate::new(move || f.store(true, Ordering::Release)) }, result: Arc::new(Mutex::new(None)), schedule: Default::default(), status: Default::default(), topology: Default::default(), log_level: None, logs: Vec::new(), sigs: Vec::new() };
        app.reload();
        app.load_status();
        // 通知を待つ代わりに 0.4 秒ごとに旗を見る。変わったときだけ読み直して描き直す
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let alive = this
                    .update(cx, |app, cx| {
                        let probing = app.engine.is_probing();
                        app.live_heartbeat();
                        if app.live_dirty.swap(false, Ordering::AcqRel) {
                            cx.notify();
                        }
                        if app.dirty.swap(false, Ordering::AcqRel) || probing != app.probing {
                            app.probing = probing;
                            app.reload();
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        app
    }

    fn reload(&mut self) {
        let cfg = self.engine.config();
        let ledger: Vec<_> = cfg.nodes.iter().map(nodes::Node::public).collect();
        match self.engine.last() {
            Ok(last) => {
                self.nodes = model::build(&ledger, &last);
                for n in &mut self.nodes {
                    if let Ok(rows) = self.engine.with_db(|d| d.history(&n.id, 60)) {
                        n.score_trend = model::series(&rows, "score");
                        n.cpu_trend = model::series(&rows, "cpu");
                        n.mem_trend = model::series(&rows, "mem_avail");
                    }
                }
                self.notice = self.engine.config_error();
            }
            Err(e) => self.notice = Some(e),
        }
        if self.selected.as_ref().is_some_and(|id| !self.nodes.iter().any(|n| &n.id == id)) {
            self.selected = None;
        }
        self.load_page();
    }

    /// 開いている画面の分だけ、保存済みを読み直す（収集・更新・書き込みはしない）
    fn load_page(&mut self) {
        match self.page {
            Page::Logs => self.load_logs(),
            Page::Status => self.load_status(),
            Page::Topology => {
                if let Ok(v) = self.engine.topology_ui_view(&Default::default()) {
                    self.topology = model::topology(&v);
                }
            }
            Page::Actions => {
                if let Ok(rows) = self.engine.with_db(|d| d.actions(100)) {
                    self.actions = model::actions(&rows);
                }
            }
            Page::Schedule => {
                if let Ok(v) = self.engine.status_snapshot() {
                    self.schedule = model::schedule(&v);
                }
            }
            Page::Power => {
                if let Ok(v) = self.engine.power_report() {
                    self.power = model::power(&v);
                }
            }
            Page::Tools => {
                if let Ok(v) = self.engine.inventory_view(false) {
                    self.tools = model::inventory(&v);
                }
            }
            Page::Ai => {
                if let Ok(v) = self.engine.ai_summary(&serde_json::json!({})) {
                    self.ai = model::ai(&v);
                }
            }
            Page::Specs => {
                if let Ok(v) = self.engine.specs_view() {
                    self.specs = model::specs(&v);
                }
            }
            Page::Security => {
                if let Ok(v) = self.engine.netsec_view(&serde_json::json!({})) {
                    self.security = model::security(&v);
                }
            }
            Page::Link => {
                let dir = tune_link::console_dir(self.engine.data_dir());
                if let Ok(list) = tune_link::peers::PeerStore::new(&dir).list() {
                    let rows: Vec<_> = list.iter().filter(|p| p.role == tune_link::peers::Role::Agent).map(tune_link::peers::Peer::summary).collect();
                    self.peers = model::peers(&rows);
                }
            }
            Page::Overview | Page::Resources | Page::Processes | Page::Network => {}
        }
    }

    /// 直近 7 日。重い取り込みはせず、保存済みを読むだけ
    fn load_logs(&mut self) {
        let since = now_ms() - 7 * 86_400_000;
        let mut f = serde_json::json!({ "since": since, "limit": 200 });
        if let Some(l) = self.log_level {
            f["level"] = l.into();
        }
        if let Ok(rows) = self.engine.logs_query(&f) {
            self.logs = model::log_rows(&rows);
        }
        if let Ok(sigs) = self.engine.logs_signatures(&serde_json::json!({ "since": since, "limit": 15 })) {
            self.sigs = model::signatures(&sigs);
        }
    }

    /// 合図は 30 秒ごと。Engine 側は 2 分途切れると自動で止める
    fn live_heartbeat(&mut self) {
        if self.live_on.as_ref().is_some_and(|(_, at)| at.elapsed() >= Duration::from_secs(30)) {
            let id = self.live_on.as_ref().map(|(id, _)| id.clone()).unwrap_or_default();
            self.live_start(&id);
        }
    }

    fn live_start(&mut self, id: &str) {
        let cfg = self.engine.reload_config();
        let nodes: Vec<_> = cfg.nodes.iter().filter(|n| n.id == id).cloned().collect();
        let _g = self.rt.enter();
        self.live.start(&nodes, &[]);
        self.live_on = Some((id.to_string(), Instant::now()));
    }

    fn live_stop(&mut self) {
        if let Some((id, _)) = self.live_on.take() {
            self.live.stop(Some(&[id]), Stop::User);
        }
    }

    fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        // 画面を移ったらライブは止める（見ていない機体に流し続けない）
        if self.live_on.as_ref().map(|(i, _)| i) != id.as_ref() {
            self.live_stop();
        }
        self.selected = id;
        self.page = Page::Overview;
        cx.notify();
    }

    fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.live_stop();
        self.selected = None;
        self.page = page;
        self.load_page();
        cx.notify();
    }

    /// 保存済みの現在状態を読むだけ（状態の再計算・通知はしない）
    fn load_status(&mut self) {
        if let Ok(v) = self.engine.status_snapshot() {
            self.status = model::status(&v);
        }
    }

    /// 受動の到達性確認（読み取りだけ）。各機体へ接続するので時間がかかることがある。ボタンを押したときだけ走らせる
    fn network_check(&mut self, cx: &mut Context<Self>) {
        if self.network_busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let (engine, slot, busy, dirty) = (self.engine.clone(), self.network.clone(), self.network_busy.clone(), self.live_dirty.clone());
        self.rt.spawn(async move {
            let rows = match engine.network_check(&[], false).await {
                Ok(v) => model::network(&v),
                Err(e) => vec![model::NetRow { node: "（確認できません）".into(), ok: false, skipped: false, error: e, at_ms: None, flags: Vec::new() }],
            };
            *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rows);
            busy.store(false, Ordering::Release);
            dirty.store(true, Ordering::Release);
        });
        cx.notify();
    }

    /// 提案された変更操作を実行する。確認（承認の関門）を通り、許可リスト・共用機や保護対象の拒否・
    /// 確認後の台帳の読み直し・実行記録はすべて tune-core の `confirm_and_run` が行う
    fn run_action(&mut self, node_id: String, action: serde_json::Value, label: String, cx: &mut Context<Self>) {
        let (engine, gate, result, dirty) = (self.engine.clone(), self.gate.clone(), self.result.clone(), self.live_dirty.clone());
        self.rt.spawn(async move {
            let r = engine.confirm_and_run(&node_id, &action, &format!("{node_id}: {label}"), None, gate.confirm()).await;
            *result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(model::action_message(&node_id, r));
            dirty.store(true, Ordering::Release);
        });
        cx.notify();
    }

    /// 実行記録から元に戻す（戻すときも確認する）
    fn undo_action(&mut self, entry_id: String, cx: &mut Context<Self>) {
        let (engine, gate, result, dirty, page_dirty) = (self.engine.clone(), self.gate.clone(), self.result.clone(), self.live_dirty.clone(), self.dirty.clone());
        self.rt.spawn(async move {
            let r = engine.undo(&entry_id, gate.confirm()).await;
            *result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(model::action_message("", r));
            page_dirty.store(true, Ordering::Release);
            dirty.store(true, Ordering::Release);
        });
        cx.notify();
    }

    fn probe_all(&mut self, cx: &mut Context<Self>) {
        if self.probing {
            return;
        }
        self.probing = true;
        let engine = self.engine.clone();
        self.rt.spawn(async move {
            engine.run_probe(None, false).await;
        });
        cx.notify();
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let row = |id: gpui::ElementId, label: String, dot: Hsla, tail: String, active: bool, target: Option<String>, cx: &mut Context<Self>| {
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
                .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| {
                    app.select(target.clone(), cx);
                }))
        };
        let mut list = div().flex().flex_col().gap_0p5().p_2();
        list = list.child(row(("nav", 0usize).into(), "概要".into(), palette::accent(), String::new(), self.selected.is_none() && self.page == Page::Overview, None, cx));
        for (i, page) in Page::NAV.into_iter().enumerate() {
            let fails = if page == Page::Status { self.status.count(model::CheckStatus::Fail) } else { 0 };
            list = list.child(
                div().id(("page", i)).flex().items_center().gap_2().px_3().py_1p5().rounded_md().cursor_pointer()
                    .when(self.selected.is_none() && self.page == page, |d| d.bg(palette::card()))
                    .hover(|d| d.bg(palette::card()))
                    .child(div().size_2().rounded_full().bg(if fails > 0 { palette::crit() } else { palette::muted() }))
                    .child(div().flex_1().text_sm().child(page.title()))
                    .child(div().text_xs().text_color(palette::muted()).child(if fails > 0 { fails.to_string() } else { String::new() }))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| app.open_page(page, cx))),
            );
        }
        for (i, n) in self.nodes.iter().enumerate() {
            let tail = match &n.health {
                Health::Scored(s) => s.to_string(),
                Health::Failed(_) => "失敗".into(),
                Health::Unobserved => "未".into(),
            };
            list = list.child(row(("nav", i + 1).into(), n.id.clone(), score_color(&n.health), tail, self.selected.as_deref() == Some(&n.id), Some(n.id.clone()), cx));
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
            .child(div().px_4().pt_4().pb_2().text_sm().font_weight(FontWeight::BOLD).child("Katala Tune"))
            .child(div().flex_1().id("nodes").overflow_y_scroll().child(list))
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.selected.clone().unwrap_or_else(|| self.page.title().to_string());
        let (crit, warn): (usize, usize) = self.nodes.iter().fold((0, 0), |a, n| (a.0 + n.count(Severity::Critical), a.1 + n.count(Severity::Warn)));
        let busy = self.probing;
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
                    .child(if busy { "分析中…" } else { "全機を分析" })
                    .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| app.probe_all(cx))),
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
            };
            let mut foot = div().flex().flex_wrap().gap_1p5();
            if let Health::Failed(e) = &n.health {
                foot = foot.child(div().max_w(px(212.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(chip(e.lines().next().unwrap_or("失敗").to_string(), palette::crit())));
            }
            for (sev, count) in [(Severity::Critical, n.count(Severity::Critical)), (Severity::Warn, n.count(Severity::Warn))] {
                if count > 0 {
                    foot = foot.child(chip(format!("{} {count}", sev.label()), severity_color(sev)));
                }
            }
            let metric = |label: &str, v: Option<f64>, unit: &str| {
                div().flex().justify_between().text_xs().child(div().text_color(palette::muted()).child(label.to_string())).child(v.map_or("–".to_string(), |v| format!("{v:.0}{unit}")))
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
                    .when(!n.score_trend.is_empty(), |d| d.child(trend(&n.score_trend, 100.0, score_color(&n.health), 18.0, 60)))
                    .child(foot)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| {
                        app.select(Some(id.clone()), cx);
                    })),
            );
        }
        if self.nodes.is_empty() {
            grid = grid.child(div().text_color(palette::muted()).child("台帳に機体がありません（~/.config/katala-tune/nodes.json）"));
        }
        div().id("overview-scroll").flex_1().overflow_y_scroll().child(grid)
    }

    fn logs_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = now_ms();
        let level_color = |l: &str| match l {
            "critical" | "error" => palette::crit(),
            "warn" => palette::warn(),
            _ => palette::muted(),
        };
        let mut filters = div().flex().gap_2();
        for (i, (label, level)) in [("すべて", None), ("重大", Some("critical")), ("エラー", Some("error")), ("警告", Some("warn"))].into_iter().enumerate() {
            let active = self.log_level == level;
            filters = filters.child(
                div().id(("lvl", i)).px_3().py_1().rounded_md().text_sm().cursor_pointer()
                    .bg(if active { palette::accent() } else { palette::card() })
                    .child(label)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| {
                        app.log_level = level;
                        app.load_logs();
                        cx.notify();
                    })),
            );
        }
        let mut sigs = div().flex().flex_col().gap_1p5().w(px(360.0)).flex_none();
        sigs = sigs.child(div().text_xs().text_color(palette::muted()).child("同種ログ（7 日）"));
        for s in &self.sigs {
            sigs = sigs.child(
                div().flex().flex_col().gap_0p5().p_2().rounded_md().bg(palette::card())
                    .child(div().flex().items_center().gap_2().child(chip(model::level_label(&s.level), level_color(&s.level))).child(div().text_xs().text_color(palette::muted()).child(format!("{} 件 · {} 台", s.count, s.nodes))))
                    .child(div().text_xs().child(s.sample.clone())),
            );
        }
        if self.sigs.is_empty() {
            sigs = sigs.child(div().text_sm().text_color(palette::muted()).child("まだログを取り込んでいません"));
        }
        let mut rows = div().flex().flex_col().gap_1().flex_1().min_w_0();
        rows = rows.child(div().text_xs().text_color(palette::muted()).child(format!("新しい順（{} 件）", self.logs.len().min(200))));
        for l in &self.logs {
            rows = rows.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(56.0)).flex_none().child(chip(model::level_label(&l.level), level_color(&l.level))))
                    .child(div().text_xs().text_color(palette::muted()).w(px(84.0)).flex_none().truncate().child(l.node.clone()))
                    .child(div().text_xs().text_color(palette::muted()).w(px(64.0)).flex_none().child(model::ago(now, l.ts_ms)))
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(l.message.clone())),
            );
        }
        div().id("logs").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(filters)
            .child(div().flex().gap_4().child(sigs).child(rows))
    }

    fn topology_page(&self) -> impl IntoElement {
        let t = &self.topology;
        let now = now_ms();
        let note = |title: &str, body: String| {
            div().flex().flex_col().gap_1().p_3().rounded_md().bg(palette::card())
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
                .child(div().text_xs().text_color(palette::muted()).child(body))
        };
        let stat = |label: &str, v: String| div().flex().flex_col().gap_0p5().child(div().text_xs().text_color(palette::muted()).child(label.to_string())).child(div().text_lg().font_weight(FontWeight::BOLD).child(v));
        let mut scopes = div().flex().flex_col().gap_1();
        for s in &t.scopes {
            let color = match s.status.as_str() { "complete" => palette::ok(), "failed" => palette::crit(), "partial" => palette::warn(), _ => palette::muted() };
            scopes = scopes.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(72.0)).flex_none().child(chip(model::scope_status_label(&s.status), color)))
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(s.id.clone()))
                    .child(div().text_xs().text_color(palette::muted()).child(s.observed_ms.map_or("未観測".to_string(), |a| model::ago(now, a)))),
            );
        }
        let mut kinds = div().flex().flex_wrap().gap_2();
        for (k, n) in &t.kind_counts {
            kinds = kinds.child(chip(format!("{k} {n}"), palette::muted()));
        }
        let mut ents = div().flex().flex_col().gap_1();
        for (k, label) in t.entities.iter().take(200) {
            ents = ents.child(
                div().flex().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(140.0)).flex_none().truncate().text_xs().text_color(palette::muted()).child(k.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(label.clone())),
            );
        }
        div().id("topology").flex_1().overflow_y_scroll().flex().flex_col().gap_4().p_5()
            .child(note(
                "表示範囲と検査について",
                format!(
                    "表示中の項目だけを検査しています。表示外の項目と保存原本全体は未検査です。保存項目数: 実体 {} 件 / 関係 {} 件（全件を検査した件数ではありません）。表示は上限付きで、属性・履歴の一部を省略しています。",
                    t.stored_entities, t.stored_edges
                ),
            ))
            .child(note("観測時点の構成図です", "完了範囲でも、表示は最後に観測した時点の記録です。リアルタイムの存在判定ではありません。更新は read-only です。".into()))
            .child(div().flex().gap_8().child(stat("範囲", t.scopes.len().to_string())).child(stat("履歴", t.history.to_string())).child(stat("実体（表示 / 保存）", format!("{} / {}", t.entities.len().min(200), t.stored_entities))).child(stat("関係（表示 / 保存）", format!("{} / {}", t.edges_shown, t.stored_edges))))
            .child(div().text_xs().text_color(palette::muted()).child("収集の範囲"))
            .child(scopes)
            .child(div().text_xs().text_color(palette::muted()).child("実体の種類（表示中）"))
            .child(kinds)
            .child(div().text_xs().text_color(palette::muted()).child(format!("実体（表示 {} 件まで）", t.entities.len().min(200))))
            .child(ents)
    }

    fn status_page(&self) -> impl IntoElement {
        let st = &self.status;
        let now = now_ms();
        let color = |s: model::CheckStatus| match s {
            model::CheckStatus::Fail => palette::crit(),
            model::CheckStatus::Warn => palette::warn(),
            model::CheckStatus::Ok => palette::ok(),
            model::CheckStatus::Unknown => palette::muted(),
        };
        let mut counts = div().flex().gap_2();
        for s in [model::CheckStatus::Fail, model::CheckStatus::Warn, model::CheckStatus::Unknown, model::CheckStatus::Ok] {
            counts = counts.child(chip(format!("{} {}", s.label(), st.count(s)), color(s)));
        }
        let when = |label: &str, t: Option<i64>| format!("{label} {}", t.map_or("未実施".to_string(), |t| model::ago(now, t)));
        let mut rows = div().flex().flex_col().gap_1();
        for c in &st.checks {
            rows = rows.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(56.0)).flex_none().child(chip(c.status.label(), color(c.status))))
                    .child(div().w(px(110.0)).flex_none().truncate().text_xs().text_color(palette::muted()).child(c.scope.clone()))
                    .child(div().w(px(150.0)).flex_none().truncate().text_xs().font_weight(FontWeight::SEMIBOLD).child(c.name.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(palette::muted()).child(c.detail.clone())),
            );
        }
        let mut events = div().flex().flex_col().gap_1();
        for e in st.events.iter().take(30) {
            events = events.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(64.0)).flex_none().text_xs().text_color(palette::muted()).child(model::ago(now, e.ts_ms)))
                    .child(div().w(px(110.0)).flex_none().truncate().text_xs().text_color(palette::muted()).child(e.scope.clone()))
                    .child(div().w(px(120.0)).flex_none().truncate().text_xs().child(e.name.clone()))
                    .child(div().flex().gap_1().flex_none().child(chip(e.from.label(), color(e.from))).child(div().text_xs().child("→")).child(chip(e.to.label(), color(e.to))))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(palette::muted()).child(e.detail.clone())),
            );
        }
        div().id("status").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().flex().items_center().gap_4().child(counts).child(div().text_xs().text_color(palette::muted()).child(format!("{} · {}", when("分析", st.last_probe_ms), when("ログ取り込み", st.last_logs_ms)))))
            .child(rows)
            .child(div().text_xs().text_color(palette::muted()).child(format!("最近の変化（{} 件まで）", st.events.len().min(30))))
            .child(events)
    }

    /// 全機のリソースを並べる（直近の分析の値。ライブは機体の詳細で見る）
    fn resources_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = now_ms();
        let meter = |label: &str, pct: Option<f64>, bad_high: bool| {
            let color = match pct {
                None => palette::muted(),
                Some(p) => {
                    let load = if bad_high { p } else { 100.0 - p };
                    if load >= 90.0 { palette::crit() } else if load >= 70.0 { palette::warn() } else { palette::ok() }
                }
            };
            div().w(px(150.0)).flex_none().flex().flex_col().gap_1()
                .child(div().flex().justify_between().text_xs().child(div().text_color(palette::muted()).child(label.to_string())).child(pct.map_or("–".to_string(), |p| format!("{p:.0}%"))))
                .child(div().h(px(5.0)).rounded_full().bg(palette::line()).child(div().h(px(5.0)).rounded_full().bg(color).w(px(pct.map_or(0.0, |p| p.clamp(0.0, 100.0) as f32 * 1.5)))))
        };
        let mut rows = div().flex().flex_col().gap_2();
        for (i, n) in self.nodes.iter().enumerate() {
            let id = n.id.clone();
            let mem_used = n.mem_avail_pct.map(|a| 100.0 - a);
            let disk = n.disks.iter().min_by(|a, b| a.free_pct.total_cmp(&b.free_pct));
            let hw = format!("{}{}{}", n.cpu_name, n.cores.map_or(String::new(), |c| format!(" · {c} コア")), n.uptime_h.map_or(String::new(), |u| format!(" · 稼働 {}", model::uptime(u))));
            rows = rows.child(
                div().id(("res", i)).flex().items_center().gap_4().px_3().py_2().rounded_md().bg(palette::card()).cursor_pointer()
                    .hover(|d| d.bg(palette::line()))
                    .child(div().w(px(120.0)).flex_none().flex().flex_col().child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(n.id.clone())).child(div().text_xs().text_color(palette::muted()).child(n.at_ms.map_or("未分析".to_string(), |a| model::ago(now, a)))))
                    .child(meter("CPU", n.cpu_busy, true))
                    .child(meter("メモリ使用", mem_used, true))
                    .child(meter(&format!("ディスク使用{}", disk.map_or(String::new(), |d| format!(" {}", d.mount))), disk.map(|d| 100.0 - d.free_pct), true))
                    .child(div().w(px(104.0)).flex_none().text_xs().text_color(palette::muted()).child(n.swap_gb.map_or("swap –".to_string(), |s| format!("swap {s:.1}GB"))))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(palette::muted()).child(hw))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| app.select(Some(id.clone()), cx))),
            );
        }
        div().id("resources").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child("直近の分析の値です。1 秒ごとの様子は機体の詳細の「ライブ」で見られます。行を押すと機体の詳細に移ります。"))
            .child(rows)
    }

    /// 電力と Clock。計測できていない値は 0 ではなく「–」と理由で出す
    fn power_page(&self) -> impl IntoElement {
        let p = &self.power;
        let w = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{v:.1} W"));
        let mut rows = div().flex().flex_col().gap_1();
        for r in &p.rows {
            let color = match r.kind.as_str() { "measured" => palette::ok(), "estimated" => palette::warn(), _ => palette::muted() };
            let clock = self.nodes.iter().find(|n| n.id == r.node).map(|n| n.cpu_name.clone()).unwrap_or_default();
            rows = rows.child(
                div().flex().items_center().gap_2().px_3().py_2().rounded_md().bg(palette::card())
                    .child(div().w(px(120.0)).flex_none().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).child(r.node.clone()))
                    .child(div().w(px(72.0)).flex_none().child(chip(model::power_kind_label(&r.kind), color)))
                    .child(div().w(px(90.0)).flex_none().text_xs().child(format!("合計 {}", w(r.total_w.or(r.wall_w)))))
                    .child(div().w(px(90.0)).flex_none().text_xs().text_color(palette::muted()).child(format!("CPU {}", w(r.cpu_w))))
                    .child(div().w(px(90.0)).flex_none().text_xs().text_color(palette::muted()).child(format!("GPU {}", w(r.gpu_w))))
                    .child(div().w(px(120.0)).flex_none().text_xs().text_color(palette::muted()).child(r.projected_kwh.map_or("月間 –".to_string(), |k| format!("予測 {k:.1} kWh"))))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(palette::muted()).child(if r.missing.is_empty() { clock } else { format!("不足: {}", r.missing.join(", ")) })),
            );
        }
        div().id("power").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child(format!("電力を計測できている機体: {} / {} 台（計測できない値は 0 ではなく「–」と不足項目で示します）。合計: {}", p.available, p.expected, w(p.fleet_total_w))))
            .child(rows)
    }

    /// 道具の棚卸し（保存済み）。棚卸しの実行・公開・除外はこの画面からはしない
    fn tools_page(&self) -> impl IntoElement {
        let now = now_ms();
        let t = &self.tools;
        let mut nodes = div().flex().flex_wrap().gap_2();
        for (id, count, ok, err) in &t.nodes {
            nodes = nodes.child(chip(format!("{id} {count} 件{}", if err.is_some() { " ・失敗" } else { "" }), if err.is_some() { palette::crit() } else { palette::muted() }).when(ok.is_none(), |d| d.opacity(0.6)));
        }
        let mut groups = div().flex().flex_col().gap_1();
        for g in t.groups.iter().take(150) {
            groups = groups.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(64.0)).flex_none().child(if g.drift { chip("版差", palette::warn()) } else { chip("同一", palette::muted()) }))
                    .child(div().w(px(220.0)).flex_none().truncate().text_xs().font_weight(FontWeight::SEMIBOLD).child(g.name.clone()))
                    .child(div().w(px(56.0)).flex_none().text_xs().text_color(palette::muted()).child(format!("{} 台", g.node_count)))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(palette::muted()).child(g.versions.join(" / "))),
            );
        }
        let mut events = div().flex().flex_col().gap_1();
        for e in t.events.iter().take(20) {
            events = events.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(64.0)).flex_none().text_xs().text_color(palette::muted()).child(model::ago(now, e.ts_ms)))
                    .child(div().w(px(100.0)).flex_none().truncate().text_xs().text_color(palette::muted()).child(e.node.clone()))
                    .child(div().w(px(180.0)).flex_none().truncate().text_xs().child(e.name.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(palette::muted()).child(format!("{} {} → {}", e.kind, e.from, e.to))),
            );
        }
        div().id("tools").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child(format!("保存済みの棚卸し（前回 {}）。版差のあるものを先に並べます。実行や公開は旧画面から行います。", t.last_at.map_or("未実施".to_string(), |a| model::ago(now, a)))))
            .child(nodes)
            .child(div().text_xs().text_color(palette::muted()).child(format!("道具（表示 {} / {} 件）", t.groups.len().min(150), t.groups.len())))
            .child(groups)
            .child(div().text_xs().text_color(palette::muted()).child("最近の更新"))
            .child(events)
    }

    /// AI の利用（保存済みの集計）。料金は料金表から求めた概算で、請求額ではない
    fn ai_page(&self) -> impl IntoElement {
        let a = &self.ai;
        let now = now_ms();
        let stat = |label: &str, v: String| div().flex().flex_col().gap_0p5().child(div().text_xs().text_color(palette::muted()).child(label.to_string())).child(div().text_lg().font_weight(FontWeight::BOLD).child(v));
        let mil = |n: i64| format!("{:.1}M", n as f64 / 1e6);
        let mut models = div().flex().flex_col().gap_1();
        let max_cost = a.models.first().map_or(1.0, |m| m.1.max(0.01));
        for (name, cost) in a.models.iter().take(12) {
            models = models.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(200.0)).flex_none().truncate().text_xs().child(name.clone()))
                    .child(div().w(px(90.0)).flex_none().text_xs().text_color(palette::muted()).child(format!("${cost:.2}")))
                    .child(div().h(px(5.0)).rounded_full().bg(palette::accent()).w(px((cost / max_cost * 300.0) as f32))),
            );
        }
        let mut tools = div().flex().flex_col().gap_1();
        for (name, calls, errors) in a.tools.iter().take(12) {
            tools = tools.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(240.0)).flex_none().truncate().text_xs().child(name.clone()))
                    .child(div().w(px(90.0)).flex_none().text_xs().text_color(palette::muted()).child(format!("{calls} 回")))
                    .child(div().text_xs().text_color(if *errors > 0 { palette::warn() } else { palette::muted() }).child(format!("失敗 {errors}"))),
            );
        }
        div().id("ai").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child(format!("直近 {} 日の保存済みの集計（取り込み {}）。料金は料金表から求めた概算で、請求額ではありません。", a.days, a.last_at.map_or("未実施".to_string(), |t| model::ago(now, t)))))
            .child(div().flex().gap_8().child(stat("セッション", a.sessions.to_string())).child(stat("プロンプト", a.prompts.to_string())).child(stat("概算料金", format!("${:.0}", a.cost_usd))).child(stat("入力 / 出力トークン", format!("{} / {}", mil(a.tok_in), mil(a.tok_out)))).child(stat("PR", a.prs.to_string())))
            .child(div().text_xs().text_color(palette::muted()).child("モデル別（概算料金の多い順）"))
            .child(models)
            .child(div().text_xs().text_color(palette::muted()).child("ツール別（呼び出しの多い順）"))
            .child(tools)
    }

    /// 部品の一覧を機体ごとに並べる（保存済みの報告）。取得に失敗した機体は前回の報告を出し、その旨を示す
    fn specs_page(&self) -> impl IntoElement {
        let now = now_ms();
        let sp = &self.specs;
        let mut meta = div().flex().flex_wrap().gap_2();
        for n in &sp.nodes {
            let (text, color) = if !n.eligible {
                (format!("{} 対象外", n.node), palette::muted())
            } else if n.last_failed {
                (format!("{} 取得失敗（前回 {}）", n.node, n.at_ms.map_or("なし".to_string(), |a| model::ago(now, a))), palette::warn())
            } else {
                (format!("{} {} · {}", n.node, n.collector, n.at_ms.map_or("未取得".to_string(), |a| model::ago(now, a))), palette::muted())
            };
            meta = meta.child(chip(text, color));
        }
        let mut body = div().flex().flex_col().gap_4();
        for sec in &sp.sections {
            let mut rows = div().flex().flex_col().gap_1();
            for (node, names) in &sec.rows {
                rows = rows.child(
                    div().flex().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                        .child(div().w(px(110.0)).flex_none().truncate().text_xs().text_color(palette::muted()).child(node.clone()))
                        .child(div().flex_1().min_w_0().text_xs().child(names.join(" / "))),
                );
            }
            body = body.child(
                div().flex().flex_col().gap_1()
                    .child(div().flex().items_center().gap_2().child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(sec.name.clone())).child(div().text_xs().text_color(palette::muted()).child(format!("{} 部品 · 公式情報で未確認 {}", sec.components, sec.unverified))))
                    .child(rows),
            );
        }
        div().id("specs").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child("保存済みのスペック報告。収集は旧画面か CLI で行います。公式情報と照合できていない部品は「未確認」として数えます。"))
            .child(meta)
            .child(body)
    }

    /// 直近の分析時点のアプリ別の使用量（メモリの多い順の上位）。常時の監視ではなく、スナップショット
    fn processes_page(&self) -> impl IntoElement {
        let now = now_ms();
        let mut body = div().flex().flex_col().gap_4();
        for n in &self.nodes {
            if n.apps.is_empty() {
                continue;
            }
            let mut rows = div().flex().flex_col().gap_0p5();
            for a in n.apps.iter().take(8) {
                rows = rows.child(
                    div().flex().items_center().gap_2().px_2().py_0p5().rounded_md().bg(palette::card())
                        .child(div().flex_1().min_w_0().truncate().text_xs().child(a.app.clone()))
                        .child(div().w(px(56.0)).flex_none().text_xs().text_color(palette::muted()).child(format!("{} 個", a.count)))
                        .child(div().w(px(64.0)).flex_none().text_xs().text_color(if a.cpu >= 25.0 { palette::warn() } else { palette::muted() }).child(format!("CPU {:.1}%", a.cpu)))
                        .child(div().w(px(84.0)).flex_none().text_xs().child(format!("{:.1} GB", a.mem_mb / 1024.0))),
                );
            }
            body = body.child(
                div().flex().flex_col().gap_1().w(px(480.0))
                    .child(div().flex().items_center().gap_2().child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(n.id.clone())).child(div().text_xs().text_color(palette::muted()).child(format!("{} · AI エージェント {} 個", n.at_ms.map_or("未分析".to_string(), |a| model::ago(now, a)), n.agent_processes.map_or("–".to_string(), |c| c.to_string())))))
                    .child(rows),
            );
        }
        div().id("processes").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child("直近の分析時点のアプリ別の使用量（メモリの多い上位 8 件）。1 秒ごとの様子は機体の詳細のライブで見られます。"))
            .child(div().flex().flex_wrap().gap_6().child(body))
    }

    /// ネットワークとセキュリティの監視状態。読み取りだけで、遮断や設定変更はしない
    fn security_page(&self) -> impl IntoElement {
        let s = &self.security;
        let mut rows = div().flex().flex_col().gap_1();
        for n in &s.nodes {
            rows = rows.child(
                div().flex().items_center().gap_2().px_3().py_2().rounded_md().bg(palette::card())
                    .child(div().w(px(120.0)).flex_none().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).child(n.node.clone()))
                    .child(div().w(px(80.0)).flex_none().child(if n.enabled { chip("監視中", palette::ok()) } else { chip("無効", palette::muted()) }))
                    .child(div().text_xs().text_color(palette::muted()).child(if n.shared { "共用機（変更操作は出ない）".to_string() } else { format!("確認項目 {} 件", n.checks) })),
            );
        }
        let enabled = s.nodes.iter().filter(|n| n.enabled).count();
        div().id("security").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child(format!("監視は読み取りだけで、遮断や設定の変更は行いません。有効 {enabled} / {} 台。学習 {} 日・保存 {} 日。", s.nodes.len(), s.learn_days, s.keep_days)))
            .child(div().flex().gap_2().child(chip(format!("通知対象のイベント {}", s.events), palette::muted())).child(chip(format!("接続先 {}", s.peers), palette::muted())).child(chip(format!("ログイン {}", s.logins), palette::muted())))
            .child(rows)
            .when(enabled == 0, |d| d.child(div().text_sm().text_color(palette::warn()).child("どの機体でも有効になっていません。台帳の設定で機体ごとに有効にします。")))
    }

    fn network_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.network_busy.load(Ordering::Acquire);
        let rows_data = self.network.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let now = now_ms();
        let mut rows = div().flex().flex_col().gap_1();
        for r in rows_data.iter().flatten() {
            let mut flags = div().flex().gap_2().flex_1().min_w_0();
            for (label, v) in &r.flags {
                let (text, color) = match v { Some(true) => (format!("{label} ○"), palette::ok()), Some(false) => (format!("{label} ×"), palette::crit()), None => (format!("{label} –"), palette::muted()) };
                flags = flags.child(chip(text, color));
            }
            rows = rows.child(
                div().flex().items_center().gap_2().px_3().py_2().rounded_md().bg(palette::card())
                    .child(div().w(px(120.0)).flex_none().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).child(r.node.clone()))
                    .child(div().w(px(72.0)).flex_none().child(if r.skipped { chip("対象外", palette::muted()) } else if r.ok { chip("確認済", palette::ok()) } else { chip("失敗", palette::crit()) }))
                    .child(div().w(px(64.0)).flex_none().text_xs().text_color(palette::muted()).child(r.at_ms.map_or(String::new(), |a| model::ago(now, a))))
                    .child(if r.ok { flags.into_any_element() } else { div().flex_1().min_w_0().truncate().text_xs().text_color(palette::muted()).child(r.error.clone()).into_any_element() }),
            );
        }
        div().id("network").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().flex().items_center().gap_3()
                .child(div().flex_1().text_xs().text_color(palette::muted()).child("各機体のネットワークの到達性を、受動的に（外へ探りを入れずに）確認します。結果は真偽だけで、IP アドレスは含みません。押したときだけ各機体へ接続します。"))
                .child(
                    div().id("net-check").px_3().py_1p5().rounded_md().text_sm()
                        .bg(if busy { palette::line() } else { palette::accent() })
                        .text_color(if busy { palette::muted() } else { rgb(0xffffff).into() })
                        .when(!busy, |d| d.cursor_pointer())
                        .child(if busy { "確認中…" } else { "全機を確認" })
                        .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| app.network_check(cx))),
                ))
            .child(rows)
            .when(rows_data.is_none() && !busy, |d| d.child(div().text_sm().text_color(palette::muted()).child("まだ確認していません。「全機を確認」を押してください。")))
    }

    /// ペア済みの tune-agent（公開鍵の指紋・名前・つなぎ先・時刻）。ペアリングと解除は鍵とコードを扱うので、この画面ではしない
    fn link_page(&self) -> impl IntoElement {
        let now = now_ms();
        let mut rows = div().flex().flex_col().gap_1();
        for p in &self.peers {
            rows = rows.child(
                div().flex().items_center().gap_2().px_3().py_2().rounded_md().bg(palette::card())
                    .child(div().w(px(140.0)).flex_none().truncate().text_sm().font_weight(FontWeight::SEMIBOLD).child(p.name.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(p.fingerprint.clone()))
                    .child(div().w(px(160.0)).flex_none().truncate().text_xs().text_color(palette::muted()).child(if p.addr.is_empty() { "–".to_string() } else { p.addr.clone() }))
                    .child(div().w(px(80.0)).flex_none().text_xs().text_color(palette::muted()).child(p.paired_ms.map_or("–".to_string(), |t| model::ago(now, t)))),
            );
        }
        div().id("link").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child("ペア済みの tune-agent の一覧（公開鍵の指紋・つなぎ先・ペアリングした時刻）。ペアリングと解除は鍵とコードを扱うため、この画面ではできません。旧画面か CLI（tune agent-pair / agent-unpair）で行います。調べる経路は今は SSH のままです。"))
            .child(rows)
            .when(self.peers.is_empty(), |d| d.child(div().text_sm().text_color(palette::muted()).child("ペア済みの機体はありません")))
    }

    fn actions_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = now_ms();
        let mut rows = div().flex().flex_col().gap_1();
        for (i, a) in self.actions.iter().enumerate() {
            let color = if a.ok { palette::ok() } else { palette::crit() };
            let undo = a.can_undo.then(|| {
                let id = a.id.clone();
                div().id(("undo", i)).px_2().py_0p5().rounded_md().text_xs().cursor_pointer().border_1().border_color(palette::line())
                    .hover(|d| d.border_color(palette::warn()))
                    .child("元に戻す…")
                    .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| app.undo_action(id.clone(), cx)))
            });
            rows = rows.child(
                div().flex().items_center().gap_2().px_2().py_1().rounded_md().bg(palette::card())
                    .child(div().w(px(64.0)).flex_none().text_xs().text_color(palette::muted()).child(model::ago(now, a.at_ms)))
                    .child(div().w(px(72.0)).flex_none().child(chip(if a.ok { "成功" } else { "失敗" }, color)))
                    .child(div().w(px(110.0)).flex_none().truncate().text_xs().text_color(palette::muted()).child(a.node.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_xs().child(a.label.clone()))
                    .child(div().w(px(150.0)).flex_none().truncate().text_xs().text_color(palette::muted()).child(a.kind.clone()))
                    .child(div().w(px(84.0)).flex_none().when_some(undo, |d, u| d.child(u))),
            );
        }
        if self.actions.is_empty() {
            rows = rows.child(div().text_sm().text_color(palette::muted()).child("実行記録はありません"));
        }
        div().id("actions").flex_1().overflow_y_scroll().flex().flex_col().gap_3().p_5()
            .child(div().text_xs().text_color(palette::muted()).child(format!("承認して実行した操作の記録（新しい順・{} 件まで）。元に戻せる操作は「元に戻す…」から戻せます（戻すときも確認します）。", self.actions.len().min(100))))
            .child(rows)
    }

    fn schedule_page(&self) -> impl IntoElement {
        let now = now_ms();
        let s = &self.schedule;
        let every = |m: Option<i64>| m.map_or("–".to_string(), |m| if m % 60 == 0 { format!("{} 時間ごと", m / 60) } else { format!("{m} 分ごと") });
        let row = |label: &str, v: String| {
            div().flex().items_center().gap_3().px_3().py_2().rounded_md().bg(palette::card())
                .child(div().w(px(180.0)).flex_none().text_sm().text_color(palette::muted()).child(label.to_string()))
                .child(div().text_sm().child(v))
        };
        div().id("schedule").flex_1().overflow_y_scroll().flex().flex_col().gap_2().p_5()
            .child(div().text_xs().text_color(palette::muted()).child("自動スキャンの設定と前回の実行（表示だけ。変更は旧画面か設定で行います）"))
            .child(row("自動スキャン", if s.enabled { "有効".into() } else { "無効".into() }))
            .child(row("分析", every(s.probe_minutes)))
            .child(row("ログ取り込み", every(s.logs_minutes)))
            .child(row("道具の棚卸し", s.inventory_hours.map_or("–".to_string(), |h| format!("{h} 時間ごと"))))
            .child(row("AI セッション", every(s.ai_minutes)))
            .child(row("前回の分析", s.last_probe_ms.map_or("未実施".to_string(), |t| model::ago(now, t))))
            .child(row("前回のログ取り込み", s.last_logs_ms.map_or("未実施".to_string(), |t| model::ago(now, t))))
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
            .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| {
                app.gate.answer(false);
                cx.notify();
            }))
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
                        div().flex().justify_end().gap_2()
                            .child(
                                div().id("confirm-cancel").px_4().py_1p5().rounded_md().text_sm().cursor_pointer().bg(palette::accent()).text_color(rgb(0xffffff))
                                    .child("やめる")
                                    .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| {
                                        app.gate.answer(false);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div().id("confirm-run").px_4().py_1p5().rounded_md().text_sm().cursor_pointer().border_1().border_color(palette::crit()).text_color(palette::crit())
                                    .child("実行する")
                                    .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| {
                                        app.gate.answer(true);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
    }

    fn finding_row(f: &Finding, node_id: &str, idx: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let color = severity_color(f.severity);
        let proposal = f.action.clone().map(|action| {
            let (node, label, title) = (node_id.to_string(), f.action_label.clone(), format!("{}（{}）", f.action_label, f.title));
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
                .child(format!("提案: {label}…"))
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
            .child(div().flex().items_center().gap_2().child(chip(f.severity.label(), color)).child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD).child(f.title.clone())))
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
        let on = self.live_on.as_ref().is_some_and(|(i, _)| i == id);
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
                .on_mouse_down(MouseButton::Left, cx.listener(move |app, _, _, cx| {
                    if app.live_on.is_some() {
                        app.live_stop();
                    } else {
                        app.live_start(&id);
                    }
                    cx.notify();
                }))
        };
        let live_panel = if on {
            let store = self.live_store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let l = store.0.get(id).cloned().unwrap_or_default();
            let now = |v: &[f64]| v.last().map_or("–".to_string(), |x| format!("{x:.0}%"));
            let status = if l.cpu.is_empty() { Some(format!("接続中… {}", l.detail.clone().unwrap_or_default())) } else { None };
            Some(
                div().flex().flex_col().gap_2().p_3().rounded_md().bg(palette::card()).border_1().border_color(palette::accent())
                    .child(div().text_xs().text_color(palette::muted()).child(format!("ライブ（直近 {} 秒）· {}", l.cpu.len(), l.state)))
                    .when_some(status, |d, s| d.child(div().text_sm().text_color(palette::warn()).child(s)))
                    .child(div().flex().items_end().gap_4()
                        .child(div().flex().flex_col().gap_1().child(div().text_xs().text_color(palette::muted()).child("CPU")).child(div().text_xl().font_weight(FontWeight::BOLD).child(now(&l.cpu))))
                        .child(trend(&l.cpu, 100.0, palette::accent(), 48.0, model::LIVE_KEEP)))
                    .child(div().flex().items_end().gap_4()
                        .child(div().flex().flex_col().gap_1().child(div().text_xs().text_color(palette::muted()).child("メモリ使用")).child(div().text_xl().font_weight(FontWeight::BOLD).child(now(&l.mem))))
                        .child(trend(&l.mem, 100.0, palette::warn(), 48.0, model::LIVE_KEEP))),
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
                    div().flex().flex_col().gap_1().p_3().rounded_md().bg(palette::card())
                        .child(div().text_xs().text_color(palette::muted()).child(format!("{label}（直近 {} 回）", values.len().min(60))))
                        .child(trend(values, max, color, 44.0, 60)),
                );
            }
        }
        let mut list = div().flex().flex_col().gap_2();
        for (i, f) in n.findings.iter().enumerate() {
            list = list.child(Self::finding_row(f, id, i, cx));
        }
        let body = match &n.health {
            Health::Failed(e) => div().text_sm().text_color(palette::crit()).child(format!("直近の分析に失敗: {e}")),
            Health::Unobserved => div().text_sm().text_color(palette::muted()).child("まだ分析していません。「全機を分析」を押してください。"),
            Health::Scored(_) if n.findings.is_empty() => div().text_sm().text_color(palette::ok()).child("所見はありません"),
            Health::Scored(_) => list,
        };
        div().id("detail").flex_1().overflow_y_scroll().flex().flex_col().gap_4().p_5().child(div().flex().items_center().gap_3().child(div().flex_1().child(facts)).child(toggle)).when_some(live_panel, |d, p| d.child(p)).child(trends).child(body)
    }
}

impl Render for TuneApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match self.selected.clone() {
            Some(id) => self.detail(&id, cx).into_any_element(),
            None => match self.page {
                Page::Overview => self.overview(cx).into_any_element(),
                Page::Status => self.status_page().into_any_element(),
                Page::Logs => self.logs_page(cx).into_any_element(),
                Page::Topology => self.topology_page().into_any_element(),
                Page::Actions => self.actions_page(cx).into_any_element(),
                Page::Schedule => self.schedule_page().into_any_element(),
                Page::Resources => self.resources_page(cx).into_any_element(),
                Page::Power => self.power_page().into_any_element(),
                Page::Tools => self.tools_page().into_any_element(),
                Page::Ai => self.ai_page().into_any_element(),
                Page::Specs => self.specs_page().into_any_element(),
                Page::Processes => self.processes_page().into_any_element(),
                Page::Security => self.security_page().into_any_element(),
                Page::Network => self.network_page(cx).into_any_element(),
                Page::Link => self.link_page().into_any_element(),
            },
        };
        div()
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
                    .when_some(self.notice.clone(), |d, n| d.child(div().px_5().py_2().text_xs().text_color(palette::warn()).child(n)))
                    .when_some(self.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone(), |d, r| {
                        d.child(
                            div().flex().items_center().gap_3().px_5().py_2().bg(palette::card())
                                .child(div().flex_1().text_sm().child(r))
                                .child(div().id("result-close").px_2().text_xs().text_color(palette::muted()).cursor_pointer().child("閉じる")
                                    .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| {
                                        *app.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                                        cx.notify();
                                    }))),
                        )
                    })
                    .child(main),
            )
            .when_some(self.gate.pending(), |d, p| d.child(self.confirm_modal(p, cx)))
    }
}

fn arg(name: &str) -> Option<String> {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == name {
            return it.next();
        }
    }
    None
}

fn main() {
    let selftest = std::env::args().any(|a| a == "--selftest");
    let config = arg("--config").map_or_else(nodes::user_config_path, Into::into);
    let data = arg("--data").map_or_else(nodes::data_dir, Into::into);
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

    let live_store = Arc::new(Mutex::new(model::LiveStore::default()));
    let live_dirty = Arc::new(AtomicBool::new(false));
    let live = {
        let (store, flag) = (live_store.clone(), live_dirty.clone());
        let _g = rt.enter();
        let live = Live::new(
            Arc::new(ProcessRoute),
            tune_core::monitor::live_opts(engine.config().nodes.len()),
            Box::new(move |ev| {
                store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).merge(&ev);
                flag.store(true, Ordering::Release);
            }),
        );
        rt.spawn(live.clone().run_ticker());
        live
    };
    let handles = LiveHandles { live: live.clone(), store: live_store, dirty: live_dirty };
    let rt_exit = rt.clone();

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        let window = cx
            .open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), titlebar: Some(gpui::TitlebarOptions { title: Some("Katala Tune".into()), appears_transparent: false, traffic_light_position: Some(point(px(12.0), px(12.0))) }), ..Default::default() }, move |_, cx| {
                cx.new(|cx| TuneApp::new(engine, rt, dirty, handles, cx))
            })
            .expect("window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
        // --open logs|<機体 id>: 起動時に開く画面（画面確認・スクリーンショット用）
        match arg("--open").as_deref() {
            Some(p) => match Page::from_arg(p) {
                Some(page) => drop(window.update(cx, |v, _, cx| v.open_page(page, cx))),
                None => drop(window.update(cx, |v, _, cx| v.select(Some(p.to_string()), cx))),
            },
            None => {}
        }
        if selftest {
            let view = window.update(cx, |v, _, _| v.nodes.len()).unwrap_or(0);
            // --live <id>: その機体のライブを流し、点が届くまで確かめる（E2E 用）
            let live_id = arg("--live");
            if let Some(id) = &live_id {
                window.update(cx, |v, _, _| v.live_start(id)).ok();
            }
            let logs = std::env::args().any(|a| a == "--logs");
            // --confirm-demo: 確認欄を出して 1 フレーム描いてから「やめる」で終える（描画が落ちないことと、やめるで進まないことを確かめる。何も実行しない）
            let confirm_demo = std::env::args().any(|a| a == "--confirm-demo");
            let demo_result: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
            let demo_gate = window.update(cx, |v, _, _| (v.gate.clone(), v.rt.clone())).ok();
            if let (true, Some((gate, rt))) = (confirm_demo, demo_gate.clone()) {
                let slot = demo_result.clone();
                rt.spawn(async move {
                    let ok = (gate.confirm())("確認欄の表示テスト".into(), "これは表示の確認です。何も実行しません。".into()).await;
                    *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ok);
                });
            }
            // --network <id>: 受動の到達性確認をその機体だけに走らせ、結果が届くまで確かめる（E2E 用）
            let net_id = arg("--network");
            let net_engine = window.update(cx, |v, _, _| (v.engine.clone(), v.rt.clone())).ok();
            let net_result: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
            if let (Some(id), Some((engine, rt))) = (net_id.clone(), net_engine) {
                let slot = net_result.clone();
                rt.spawn(async move {
                    let n = engine.network_check(&[id], false).await.map(|v| model::network(&v).iter().filter(|r| r.ok).count()).unwrap_or(0);
                    *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(n);
                });
            }
            if logs {
                window.update(cx, |v, _, cx| v.open_page(Page::Logs, cx)).ok();
            }
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(if live_id.is_some() { 8000 } else if net_id.is_some() { 4000 } else { 1500 })).await;
                println!("selftest ok nodes={view}");
                if let (true, Some((gate, _))) = (confirm_demo, demo_gate) {
                    let shown = gate.pending().is_some();
                    gate.answer(false);
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                    println!("selftest confirm shown={shown} approved={:?}", demo_result.lock().map(|r| *r).unwrap_or(None));
                }
                // 開いた画面のデータが読めたか（画面ごとの件数）。--open <page> と併用する
                let counts = window.update(cx, |v, _, _| format!("power={} actions={} status={} topology={} tools={} ai={} specs={} security={} procs={} peers={}", v.power.rows.len(), v.actions.len(), v.status.checks.len(), v.topology.entities.len(), v.tools.groups.len(), v.ai.sessions, v.specs.sections.len(), v.security.nodes.len(), v.nodes.iter().filter(|n| !n.apps.is_empty()).count(), v.peers.len())).unwrap_or_default();
                println!("selftest page {counts}");
                if net_id.is_some() {
                    println!("selftest network ok_rows={}", net_result.lock().map_or(0, |n| n.unwrap_or(0)));
                }
                if logs {
                    let (r, g) = window.update(cx, |v, _, _| (v.logs.len(), v.sigs.len())).unwrap_or((0, 0));
                    println!("selftest logs rows={r} sigs={g}");
                }
                if let Some(id) = live_id {
                    let n = window
                        .update(cx, |v, _, _| v.live_store.lock().map_or(0, |s| s.0.get(&id).map_or(0, |l| l.cpu.len())))
                        .unwrap_or(0);
                    println!("selftest live points={n}");
                }
                cx.update(|cx| cx.quit()).ok();
            })
            .detach();
        }
    });
    // 画面を閉じたら、流している調査の子プロセスを止めてから終わる
    rt_exit.block_on(live.shutdown(Duration::from_secs(3)));
}
