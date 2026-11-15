//! Katala Tune のネイティブ画面（Rust + GPUI）。WebView も Node も使わず、tune-core の Engine を直接呼ぶ。
//!
//! 実行: `katala-tune-gpui [--config 台帳.json] [--data データdir] [--selftest]`
//! `--selftest` は画面を開いて台帳と前回結果を読み、一定時間後に終了する起動smoke（分析は走らせない）。

use crate::{model, startup};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    App, Application, Bounds, Context, FontWeight, Hsla, MouseButton, Window, WindowBounds, WindowOptions, div, hsla, point, prelude::*, px, rgb, size,
};
use model::{Finding, Health, NodeView, Severity};
use tune_core::engine::{Engine, Host};
use tune_core::nodes;

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
    for v in values.iter().rev().take(60).rev() {
        let h = ((v / max).clamp(0.0, 1.0) as f32 * height).max(1.0);
        bars = bars.child(div().w(px(3.0)).h(px(h)).rounded_sm().bg(color));
    }
    bars
}

fn chip(text: impl Into<gpui::SharedString>, color: Hsla) -> gpui::Div {
    div().px_2().py_0p5().rounded_md().text_xs().text_color(color).bg(hsla(color.h, color.s, color.l, 0.14)).child(text.into())
}

struct TuneApp {
    engine: Arc<Engine>,
    rt: Arc<tokio::runtime::Runtime>,
    dirty: Arc<AtomicBool>,
    nodes: Vec<NodeView>,
    selected: Option<String>,
    probing: bool,
    notice: Option<String>,
}

impl TuneApp {
    fn new(engine: Arc<Engine>, rt: Arc<tokio::runtime::Runtime>, dirty: Arc<AtomicBool>, cx: &mut Context<Self>) -> Self {
        let mut app = Self { engine, rt, dirty, nodes: Vec::new(), selected: None, probing: false, notice: None };
        app.reload();
        // 通知を待つ代わりに 0.4 秒ごとに旗を見る。変わったときだけ読み直して描き直す
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let alive = this
                    .update(cx, |app, cx| {
                        let probing = app.engine.is_probing();
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
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _, _, cx| {
                        app.selected = target.clone();
                        cx.notify();
                    }),
                )
        };
        let mut list = div().flex().flex_col().gap_0p5().p_2();
        list = list.child(row(("nav", 0usize).into(), "概要".into(), palette::accent(), String::new(), self.selected.is_none(), None, cx));
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
            .child(div().px_4().pt_4().pb_2().text_sm().font_weight(FontWeight::BOLD).child("Katala Tune"))
            .child(div().flex_1().id("nodes").overflow_y_scroll().child(list))
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.selected.clone().unwrap_or_else(|| "概要".into());
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
                        cx.listener(move |app, _, _, cx| {
                            app.selected = Some(id.clone());
                            cx.notify();
                        }),
                    ),
            );
        }
        if self.nodes.is_empty() {
            grid = grid.child(div().text_color(palette::muted()).child("台帳に機体がありません（~/.config/katala-tune/nodes.json）"));
        }
        div().id("overview-scroll").flex_1().overflow_y_scroll().child(grid)
    }

    fn finding_row(f: &Finding) -> impl IntoElement {
        let color = severity_color(f.severity);
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
    }

    fn detail(&self, id: &str) -> impl IntoElement {
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
        for f in &n.findings {
            list = list.child(Self::finding_row(f));
        }
        let body = match &n.health {
            Health::Excluded(reason) => div().text_sm().text_color(palette::muted()).child(format!("対象外: {reason}。接続や分析は実行しません。")),
            Health::Failed(e) => div().text_sm().text_color(palette::crit()).child(format!("直近の分析に失敗: {e}")),
            Health::Unobserved => div().text_sm().text_color(palette::muted()).child("まだ分析していません。「全機を分析」を押してください。"),
            Health::Scored(_) if n.findings.is_empty() => div().text_sm().text_color(palette::ok()).child("所見はありません"),
            Health::Scored(_) => list,
        };
        div().id("detail").flex_1().overflow_y_scroll().flex().flex_col().gap_4().p_5().child(facts).child(trends).child(body)
    }
}

impl Render for TuneApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match self.selected.clone() {
            Some(id) => self.detail(&id).into_any_element(),
            None => self.overview(cx).into_any_element(),
        };
        div().size_full().flex().font_family(FONT).bg(palette::bg()).text_color(palette::text()).child(self.sidebar(cx)).child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(self.header(cx))
                .when_some(self.notice.clone(), |d, n| d.child(div().px_5().py_2().text_xs().text_color(palette::warn()).child(n)))
                .child(main),
        )
    }
}

pub fn run(options: startup::Options) {
    let startup::Options { config, data, selftest } = options;
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

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1120.0), px(720.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Katala Tune".into()),
                        appears_transparent: false,
                        traffic_light_position: Some(point(px(12.0), px(12.0))),
                    }),
                    ..Default::default()
                },
                move |_, cx| cx.new(|cx| TuneApp::new(engine, rt, dirty, cx)),
            )
            .expect("window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
        if selftest {
            let view = window.update(cx, |v, _, _| v.nodes.len()).unwrap_or(0);
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                println!("selftest ok nodes={view}");
                cx.update(|cx| cx.quit()).ok();
            })
            .detach();
        }
    });
}
