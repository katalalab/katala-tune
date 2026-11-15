//! Native preview supports the same desktop targets as the existing app.
//! Linux continues to test the core and CLI without pulling in the GPUI stack.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod gate;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod model;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod review;
mod startup;

fn main() {
    let options = match startup::parse(std::env::args_os().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!(
                "katala-tune-gpui [--config <ledger.json>] [--data <directory>] [--open <page|node-id>] [--review <file.json>] [--selftest [--live <node-id>] [--logs] [--network <node-id>] [--confirm-demo]]\nmacOS / Windows preview; data defaults to a separate gpui-preview directory. --selftest requires both paths and checks startup only."
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
        drop(options);
        eprintln!("GPUI preview currently supports macOS and Windows; use tune for the CLI");
        std::process::exit(2);
    }
}
