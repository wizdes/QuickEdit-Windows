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
mod panels;
mod platform;
mod render;
mod settings_host;
mod spelling;
mod timing;
mod ui;

fn main() {
    std::process::exit(app::run());
}
