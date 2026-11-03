//! QuickEdit for Windows. `app` holds the state and the event loop. Each other module holds
//! one kind of work.
#![windows_subsystem = "windows"]

mod a11y;
mod alerts;
mod app;
mod args;
mod bench;
mod channel;
mod commands;
mod docs;
mod events;
mod ime;
mod notices;
mod panels;
mod platform;
mod render;
mod settings_host;
mod spelling;
mod timing;
mod ui;

fn main() {
    let started = std::time::Instant::now();
    let dll_rule = platform::keep_dll_search_to_system32();
    std::process::exit(app::run(dll_rule, started.elapsed()));
}
