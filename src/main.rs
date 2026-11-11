//! RustyBar for Windows: a status bar at the top of every screen that costs a gaming PC nothing.
//! One process, data read in-process and on events, drawn only when it changes, animated by the
//! compositor. CLAUDE.md has the rules to work on it, README.md how to use it.

#![windows_subsystem = "windows"]

mod app;
mod bar;
mod config;
mod gfx;
mod i18n;
mod install;
mod json;
mod log;
mod popup;
mod process;
mod stats;
mod theme;
mod ui;
mod wake;
mod widgets;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => {
            process::attach_console();
            std::process::exit(config::check());
        }
        Some("quit") => process::quit_running(),
        Some(command @ ("install" | "uninstall")) => {
            process::attach_console();
            std::process::exit(install::run(command, &args[1..]));
        }
        Some("--version") | Some("version") => {
            process::attach_console();
            println!("rustybar {}", env!("CARGO_PKG_VERSION"));
        }
        _ => app::run(),
    }
}
