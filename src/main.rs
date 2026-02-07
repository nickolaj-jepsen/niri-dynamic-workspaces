mod config;
mod niri;
mod ui;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use clap::{Parser, Subcommand};
use gtk4::prelude::*;
use gtk4::{gdk, CssProvider};

/// A dynamic workspace switcher for the niri Wayland compositor.
///
/// Opens a fullscreen overlay showing workspace cards.
/// Press a–z to interact with workspaces, Escape to close.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to config file [default: ~/.config/niri-dynamic-workspaces/config.toml]
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Switch to or create a workspace [default]
    Switch,
    /// Delete a workspace
    Delete,
    /// Move the focused window to a workspace
    MoveWindow,
}

fn main() {
    let cli = Cli::parse();

    let mode = match cli.command {
        None | Some(Command::Switch) => ui::Mode::Normal,
        Some(Command::Delete) => ui::Mode::Delete,
        Some(Command::MoveWindow) => ui::Mode::MoveWindow,
    };
    let cfg = config::load_config(cli.config.as_deref());

    // Use a separate application ID per mode so each can toggle independently
    let app_id = match mode {
        ui::Mode::Delete => "dev.nickolaj.niri-dynamic-workspaces.delete",
        ui::Mode::MoveWindow => "dev.nickolaj.niri-dynamic-workspaces.move-window",
        ui::Mode::Normal => "dev.nickolaj.niri-dynamic-workspaces",
    };

    let app = gtk4::Application::builder().application_id(app_id).build();

    app.connect_startup(|_| {
        let provider = CssProvider::new();
        provider.load_from_data(include_str!("../style.css"));
        gtk4::style_context_add_provider_for_display(
            &gdk::Display::default().expect("Could not get default display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });

    let reorder_request: Rc<RefCell<Option<niri::ReorderRequest>>> = Rc::default();
    let reorder_ref = Rc::clone(&reorder_request);

    app.connect_activate(move |app| {
        // Toggle: if already showing, close the existing window
        if let Some(window) = app.active_window() {
            window.close();
            return;
        }
        ui::build_ui(app, &cfg, mode, &reorder_ref);
    });

    app.run_with_args::<&str>(&[]);

    if let Some(request) = reorder_request.take() {
        niri::reorder_workspace_columns(&request);
    }
}
