//! RustyBar for Windows: a status bar at the top of every screen that costs a gaming PC nothing.
//! One process, data read in-process and on events, drawn only when it changes, animated by the
//! compositor. CLAUDE.md has the rules to work on it, README.md how to use it.

#![windows_subsystem = "windows"]

mod app;
mod bar;
mod config;
mod crash;
mod fit;
mod gfx;
mod i18n;
mod ical;
mod install;
mod ipc;
mod json;
mod log;
mod lua;
mod mock;
mod net;
mod palette;
mod popup;
mod process;
mod secret;
mod settings;
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
        Some("msg") => {
            use std::io::Write;
            process::attach_console();
            // Not println!: on a closed pipe (`| select -First 1`) it would panic
            let mut words = args[1..].to_vec();
            // The bar runs elsewhere: the file of a shot goes where this terminal is
            if words.first().is_some_and(|w| w == "shot")
                && let Some(file) = words.get_mut(2)
                && let Ok(absolute) = std::path::absolute(&*file)
            {
                *file = absolute.display().to_string();
            }
            let words = &words[..];
            if words.is_empty() {
                let _ = writeln!(std::io::stdout(), "{}", app::USAGE);
                std::process::exit(2);
            }
            match ipc::send(words) {
                Ok(answer) => {
                    let _ = writeln!(std::io::stdout(), "{}", answer.trim_end());
                }
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "{e}");
                    std::process::exit(1);
                }
            }
        }
        Some("settings") => std::process::exit(settings::run(&args[1..])),
        Some("secret") => {
            process::attach_console();
            std::process::exit(secret::command(&args[1..]));
        }
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
