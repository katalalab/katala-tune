//! Native preview supports the same desktop targets as the existing app.
//! Linux continues to test the core and CLI without pulling in the GPUI stack.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod browser;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod collection;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod dashboard;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod gate;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod input;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod instance;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod model;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod monitor_host;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native_host;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod network;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod os;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod panels;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod resident;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod review;
mod startup;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod text;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod tray;

fn main() {
    let options = match startup::parse(std::env::args_os().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!(
                "katala-tune-gpui [--config <ledger.json>] [--data <directory>] [--open <view|node-id>] [--review <file.json>] [--resident [--hidden]] [--selftest [--live <node-id>] [--logs] [--confirm-demo] [--shutdown-check close|quit|native-quit]]\nmacOS / Windows preview; data defaults to a separate gpui-preview directory. --selftest requires both paths; without explicit diagnostics it checks startup and saved-data routes only. --shutdown-check starts a Probe job and expects an injected synthetic transport plus parent markers under <data>/shutdown-check."
            );
            return;
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    native::run(options);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        drop((
            options.config,
            options.data,
            options.selftest,
            options.resident,
            options.hidden,
            options.open,
            options.live_id,
            options.logs,
            options.confirm_demo,
            options.review,
            options.shutdown_check,
        ));
        eprintln!("GPUI preview currently supports macOS and Windows; use tune for the CLI");
        std::process::exit(2);
    }
}
