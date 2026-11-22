//! Native menus are created once on GPUI's AppKit/Win32 event-loop thread.
use crate::monitor_host::MonitorHost;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Open(Option<&'static str>),
    Quit,
}
pub struct Tray {
    _icon: TrayIcon,
    status: MenuItem,
    previous: String,
}
impl Tray {
    pub fn new() -> Result<Self, String> {
        let menu = Menu::new();
        let status = MenuItem::new("Preview · 収集未開始", false, None);
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("preview-open", "開く", true, None),
            &MenuItem::with_id("preview-dashboard", "ダッシュボード", true, None),
            &MenuItem::with_id("preview-status", "状態", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("preview-quit", "終了", true, None),
        ])
        .map_err(|e| e.to_string())?;
        // Tiny opaque K silhouette, template rendered by AppKit; no image IO.
        let mut rgba = vec![0; 20 * 20 * 4];
        for y in 3usize..17 {
            for x in 3usize..17 {
                if x < 6 || (x as isize - 7 - (y as isize - 10).abs()).abs() < 2 {
                    let i = (y * 20 + x) * 4;
                    rgba[i..i + 4].copy_from_slice(&[90, 150, 255, 255]);
                }
            }
        }
        let icon = Icon::from_rgba(rgba, 20, 20).map_err(|e| e.to_string())?;
        let _icon = TrayIconBuilder::new()
            .with_id("katala-gpui-preview")
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_icon_as_template(cfg!(target_os = "macos"))
            .with_tooltip("Katala Tune GPUI Preview")
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .with_menu_on_right_click(true)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { _icon, status, previous: String::new() })
    }
    pub fn next(&self) -> Option<Command> {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id.as_ref() {
                "preview-open" => return Some(Command::Open(None)),
                "preview-dashboard" => return Some(Command::Open(Some("dashboard"))),
                "preview-status" => return Some(Command::Open(Some("status"))),
                "preview-quit" => return Some(Command::Quit),
                _ => {}
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if cfg!(target_os = "windows")
                && matches!(event, TrayIconEvent::Click { button: tray_icon::MouseButton::Left, button_state: tray_icon::MouseButtonState::Up, .. })
            {
                return Some(Command::Open(None));
            }
        }
        None
    }
    /// Memory snapshots only; independent of UI's consumed dirty bit. Menu text
    /// changes only when these values change, never a DB read or notify loop.
    pub fn refresh(&mut self, host: &MonitorHost) {
        let monitor = host.snapshot();
        let (enabled, schedule) = host.schedule_snapshot();
        let text =
            format!("監視 {:?} · Live {} · スキャン {} {:?}", monitor.status.phase, monitor.streams, if enabled { "実行中" } else { "停止" }, schedule.phase);
        if self.previous != text {
            self.status.set_text(&text);
            self.previous = text;
        }
    }
}
