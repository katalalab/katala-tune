mod search_view;
mod view;

use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
    let process_start = std::time::Instant::now();
    let args: Vec<_> = std::env::args_os().collect();
    let sample = args.iter().any(|arg| arg == "--sample");
    let recent_local = args.iter().any(|arg| arg == "--recent-local");
    let startup = fleet_studio::startup_selection::parse_args(args);
    let (startup_selection, startup_notice) = fleet_studio::startup_selection::initialize_at(sample, startup, process_start);
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
        cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, move |window, cx| {
            window.set_window_title(if sample { "Fleet Studio — サンプル" } else { "Fleet Studio" });
            cx.new(|cx| view::FleetView::new(sample, recent_local, startup_selection, startup_notice, window, cx))
        })
        .expect("Fleet Studio window");
        cx.activate(true);
    });
}
