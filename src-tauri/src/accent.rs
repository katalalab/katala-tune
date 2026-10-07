//! システムのアクセントカラー（main.js の systemPreferences.getAccentColor と同じく `#rrggbb`）。取れなければ None（画面の既定の青）

#[cfg(target_os = "macos")]
pub fn get(app: &tauri::AppHandle) -> Option<String> {
    use objc2_app_kit::{NSColor, NSColorSpace};
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let c = NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace());
        let _ = tx.send(c.map(|c| [c.redComponent(), c.greenComponent(), c.blueComponent()]));
    })
    .ok()?;
    let rgb = rx.recv_timeout(std::time::Duration::from_secs(2)).ok()??;
    Some(hex(rgb.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8)))
}

/// Windows: HKCU\Software\Microsoft\Windows\DWM の AccentColor（ABGR）
#[cfg(windows)]
pub fn get(_app: &tauri::AppHandle) -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            use std::os::windows::process::CommandExt;
            let out = std::process::Command::new("reg")
                .args(["query", r"HKCU\Software\Microsoft\Windows\DWM", "/v", "AccentColor"])
                .creation_flags(0x0800_0000)
                .output()
                .ok()?;
            parse_reg_abgr(&String::from_utf8_lossy(&out.stdout))
        })
        .clone()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn get(_app: &tauri::AppHandle) -> Option<String> {
    None
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// `AccentColor    REG_DWORD    0xffd77800` → #0078d7
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_reg_abgr(text: &str) -> Option<String> {
    let v = text.lines().find(|l| l.contains("AccentColor"))?.split_whitespace().last()?;
    let n = u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()?;
    Some(hex([(n & 0xff) as u8, ((n >> 8) & 0xff) as u8, ((n >> 16) & 0xff) as u8]))
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_windows_accent() {
        assert_eq!(super::parse_reg_abgr("\r\nHKEY_CURRENT_USER\\x\r\n    AccentColor    REG_DWORD    0xffd77800\r\n").as_deref(), Some("#0078d7"));
        assert_eq!(super::parse_reg_abgr("nothing"), None);
    }
}
