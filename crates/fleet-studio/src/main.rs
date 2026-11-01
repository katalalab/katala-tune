mod view;

use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
    let sample = std::env::args().any(|arg| arg == "--sample");
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
        cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, move |window, cx| {
            window.set_window_title(if sample { "Fleet Studio — サンプル" } else { "Fleet Studio" });
            cx.new(|cx| view::FleetView::new(sample, window, cx))
        })
        .expect("Fleet Studio window");
        cx.activate(true);
    });
}
