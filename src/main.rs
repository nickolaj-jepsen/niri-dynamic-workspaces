mod config;
mod niri;
#[cfg(test)]
mod test_helpers;
mod ui;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use clap::{Parser, Subcommand};
use gtk4::gio::{ApplicationFlags, ApplicationHoldGuard};
use gtk4::prelude::*;
use gtk4::{gdk, CssProvider};

/// A dynamic workspace switcher for the niri Wayland compositor.
///
/// Opens a fullscreen overlay showing workspace cards.
/// Press a key to interact with workspaces, Escape to close.
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
    Switch {
        /// Workspace key (a-z, 0-9) — act directly without overlay
        key: Option<String>,
    },
    /// Delete a workspace
    Delete {
        /// Workspace key (a-z, 0-9) — act directly without overlay
        key: Option<String>,
    },
    /// Move the focused window to a workspace
    MoveWindow {
        /// Workspace key (a-z, 0-9) — act directly without overlay
        key: Option<String>,
    },
    /// Start as a background daemon (for spawn-at-startup)
    Daemon,
}

fn handle_direct_action(cli: &Cli, mode: ui::Mode, key: &str) -> i32 {
    let Some(ch) = config::parse_workspace_char(key) else {
        eprintln!("error: invalid workspace key '{key}' (must be a-z or 0-9)");
        return 1;
    };

    let cfg = config::load_config(cli.config.as_deref());
    let prefix = &cfg.workspace_prefix;
    let ws_name = config::workspace_name(prefix, ch);
    let empty_vars = std::collections::HashMap::new();

    let result = match mode {
        ui::Mode::Normal => {
            let programs = cfg.programs_for(ch);
            niri::switch_workspace(prefix, ch, &ws_name, programs).map(|(created, req)| {
                if let Some(r) = req {
                    niri::reorder_workspace_columns(&r);
                }
                if created {
                    let hooks = config::collect_create_hooks(&cfg, None);
                    let env = config::build_hook_env(&ws_name, ch, None, &empty_vars);
                    niri::run_hooks(&hooks, &env);
                }
            })
        }
        ui::Mode::Delete => {
            let result = niri::delete_workspace(prefix, ch);
            if result.is_ok() {
                let env = config::build_hook_env(&ws_name, ch, None, &empty_vars);
                niri::run_hooks(&cfg.hooks.on_delete, &env);
            }
            result
        }
        ui::Mode::MoveWindow => niri::move_window_to_workspace(prefix, ch, &ws_name),
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        return 1;
    }

    0
}

fn handle_overlay(app: &gtk4::Application, cli: &Cli, mode: ui::Mode) -> i32 {
    let cfg = Rc::new(config::load_config(cli.config.as_deref()));

    if let Some(window) = app.active_window() {
        let same_mode = ui::Mode::from_window(&window) == Some(mode);
        window.close();
        if same_mode {
            return 0;
        }
    }

    ui::build_ui(app, &cfg, mode);
    0
}

fn main() {
    // Pre-parse so --help / --version print to the caller's stdout and exit
    // before GTK starts (important when a daemon is already running).
    if let Err(e) = Cli::try_parse() {
        e.exit();
    }

    let app = gtk4::Application::builder()
        .application_id("dev.nickolaj.niri-dynamic-workspaces")
        .flags(ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|_| {
        let provider = CssProvider::new();
        provider.load_from_data(include_str!("../style.css"));
        gtk4::style_context_add_provider_for_display(
            &gdk::Display::default().expect("Could not get default display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });

    let hold_guard: RefCell<Option<ApplicationHoldGuard>> = RefCell::default();

    app.connect_command_line(move |app, cmdline| {
        let args: Vec<std::ffi::OsString> = cmdline.arguments();
        let cli = match Cli::try_parse_from(args) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };

        let (mode, key) = match cli.command {
            Some(Command::Daemon) => {
                if hold_guard.borrow().is_some() {
                    return 0;
                }
                let cfg = config::load_config(cli.config.as_deref());
                if cfg.auto_delete_empty {
                    let prefix = cfg.workspace_prefix.clone();
                    std::thread::Builder::new()
                        .name("cleanup".into())
                        .spawn(move || niri::run_event_cleanup(&prefix))
                        .ok();
                }
                *hold_guard.borrow_mut() = Some(app.hold());
                return 0;
            }
            None => (ui::Mode::Normal, None),
            Some(Command::Switch { ref key }) => (ui::Mode::Normal, key.as_deref()),
            Some(Command::Delete { ref key }) => (ui::Mode::Delete, key.as_deref()),
            Some(Command::MoveWindow { ref key }) => (ui::Mode::MoveWindow, key.as_deref()),
        };

        if let Some(key) = key {
            return handle_direct_action(&cli, mode, key);
        }

        handle_overlay(app, &cli, mode)
    });

    app.run();
}
