//! Native preview supports the same desktop targets as the existing app.
//! Linux continues to test the core and CLI without pulling in the GPUI stack.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod browser;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod input;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod model;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod panels;
mod startup;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod text;

fn main() {
    let options = match startup::parse(std::env::args_os().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!(
                "katala-tune-gpui [--config <ledger.json>] [--data <directory>] [--selftest]\nmacOS / Windows preview; data defaults to a separate gpui-preview directory. --selftest requires both paths and checks startup and saved-data routes only."
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
        drop((options.config, options.data, options.selftest));
        eprintln!("GPUI preview currently supports macOS and Windows; use tune for the CLI");
        std::process::exit(2);
    }
}
