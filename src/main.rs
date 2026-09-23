mod actions;
mod config;
mod niri;
#[cfg(test)]
mod test_helpers;
mod ui;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};

use clap::{Parser, Subcommand};
use gio::{ApplicationCommandLine, ApplicationFlags, ApplicationHoldGuard};
use gtk4::gdk;
use gtk4::prelude::*;

/// D-Bus name owned by the running instance.
const APP_ID: &str = "dev.nickolaj.niri-dynamic-workspaces";

/// Debug builds and `NDW_APP_ID` take distinct ids so a locally built overlay
/// is never forwarded over D-Bus to an installed daemon.
fn application_id() -> String {
    if let Some(id) = std::env::var("NDW_APP_ID").ok().filter(|s| !s.is_empty()) {
        if gtk4::gio::Application::id_is_valid(&id) {
            return id;
        }
        eprintln!("warning: ignoring invalid NDW_APP_ID '{id}'");
    }

    if cfg!(debug_assertions) {
        format!("{APP_ID}.Devel")
    } else {
        APP_ID.to_string()
    }
}

/// A dynamic workspace switcher for the niri Wayland compositor.
///
/// Opens a fullscreen overlay showing workspace cards.
/// Press a key to interact with workspaces, Escape to close.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Path to config file [default: ~/.config/niri-dynamic-workspaces/config.toml]
    #[arg(short, long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Switch to or create a workspace [default]
    Switch {
        /// Workspace key (a-z, 0-9) — act directly without overlay
        #[arg(value_parser = parse_key_arg)]
        key: Option<char>,
    },
    /// Delete a workspace
    Delete {
        /// Workspace key (a-z, 0-9) — act directly without overlay
        #[arg(value_parser = parse_key_arg)]
        key: Option<char>,
    },
    /// Move the focused window to a workspace
    MoveWindow {
        /// Workspace key (a-z, 0-9) — act directly without overlay
        #[arg(value_parser = parse_key_arg)]
        key: Option<char>,
    },
    /// Start as a background daemon (for spawn-at-startup)
    Daemon,
    /// Report config problems; exit non-zero if there are any
    Check,
}

/// Parse a workspace key argument, so a bad key fails in the caller's own pre-parse.
fn parse_key_arg(s: &str) -> Result<char, String> {
    config::parse_workspace_char(s).ok_or_else(|| "must be a single key, a-z or 0-9".to_string())
}

/// Resolve a relative `path` against `cwd`, the invoking process's directory.
fn absolutize(path: PathBuf, cwd: Option<&Path>) -> PathBuf {
    match cwd {
        Some(cwd) if path.is_relative() => cwd.join(path),
        _ => path,
    }
}

/// Print a line on the invoking process's stderr, also when a daemon handles the call.
fn report(cmdline: &ApplicationCommandLine, msg: &str) {
    cmdline.printerr_literal(&format!("{msg}\n"));
}

/// Exit status for a local invocation that failed after its handler returned.
static LATE_EXIT_STATUS: AtomicI32 = AtomicI32::new(0);

/// Report a failure from an async completion, after the command-line handler returned.
///
/// The completion must hold a clone of `cmdline`: a forwarded caller waits
/// for its last reference to drop, so it still gets the message and status.
/// GIO reads a local invocation's status as soon as the handler returns, so
/// `main` exits with it instead.
fn fail_later(cmdline: &ApplicationCommandLine, msg: &str) {
    report(cmdline, msg);
    cmdline.set_exit_status(1);
    if !cmdline.is_remote() {
        LATE_EXIT_STATUS.store(1, Ordering::Relaxed);
    }
}

/// Load the config at `path` (or the default location), send each problem
/// to `report`, and return the exit status: 1 if there were any.
///
/// Needs no display or bus, so it can run in a build sandbox.
fn check_config(path: Option<&Path>, report: impl Fn(&str), out: impl Fn(&str)) -> i32 {
    let cfg = config::load_config(path);
    for d in &cfg.diagnostics {
        report(&d.to_string());
    }
    if !cfg.diagnostics.is_empty() {
        return 1;
    }
    match config::config_path(path) {
        Some(file) if file.exists() => out(&format!("{}: no problems found", file.display())),
        Some(file) => out(&format!(
            "no config file at {}, defaults apply",
            file.display()
        )),
        // load_config has reported the missing config directory.
        None => {}
    }
    0
}

fn handle_direct_action(
    app: &gtk4::Application,
    cmdline: &ApplicationCommandLine,
    cli: &Cli,
    mode: ui::Mode,
    ch: char,
) -> anyhow::Result<()> {
    let cfg = config::load_config(cli.config.as_deref());
    for d in &cfg.diagnostics {
        report(cmdline, &d.to_string());
    }

    // Statically mapped key: act on the pinned workspace directly.
    if let Some(target) = cfg.static_workspaces.get(&ch) {
        // Resolved first: niri ignores an action on a missing workspace.
        return match mode {
            ui::Mode::Normal => {
                niri::workspace_id_by_name(target).and_then(niri::focus_workspace_by_id)
            }
            ui::Mode::MoveWindow => niri::workspace_id_by_name(target)
                .and_then(|id| niri::move_window_to_workspace_by_id(id, None)),
            ui::Mode::Delete => anyhow::bail!(
                "key '{ch}' is pinned to static workspace '{target}', which cannot be deleted"
            ),
        };
    }

    let ws_name = config::workspace_name(&cfg.workspace_prefix, ch);
    match mode {
        ui::Mode::Normal => actions::switch_workspace(
            app,
            &cfg,
            ch,
            &ws_name,
            cfg.programs_for(ch),
            &actions::HookInfo::default(),
        ),
        ui::Mode::Delete => {
            let cmdline = cmdline.clone();
            actions::delete_workspace(app, &cfg, ch, move |result| {
                if let Err(e) = result {
                    fail_later(&cmdline, &format!("error: {e:#}"));
                }
            })
        }
        ui::Mode::MoveWindow => actions::move_window(&cfg, ch, &ws_name, None),
    }
}

fn handle_overlay(
    app: &gtk4::Application,
    cmdline: &ApplicationCommandLine,
    cli: &Cli,
    mode: ui::Mode,
) {
    if let Some(window) = app.active_window() {
        let same_mode = ui::Mode::from_window(&window) == Some(mode);
        window.close();
        if same_mode {
            return;
        }
    }

    let cfg = Rc::new(config::load_config(cli.config.as_deref()));
    for d in &cfg.diagnostics {
        report(cmdline, &d.to_string());
    }
    ui::build_ui(app, &cfg, mode);
}

fn main() -> glib::ExitCode {
    // Pre-parse so --help / --version and bad arguments are handled in the
    // caller before GTK starts (important when a daemon is already running).
    let cli = Cli::try_parse().unwrap_or_else(|e| e.exit());
    // Subcommands that never start GTK, so they need no display or D-Bus.
    if matches!(cli.command, Some(Command::Check)) {
        return check_config(
            cli.config.as_deref(),
            |msg| eprintln!("{msg}"),
            |msg| println!("{msg}"),
        )
        .into();
    }

    let app = gtk4::Application::builder()
        .application_id(application_id())
        .flags(ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_startup(|_| {
        ui::install_base_styles(&gdk::Display::default().expect("Could not get default display"));
    });

    let hold_guard: RefCell<Option<ApplicationHoldGuard>> = RefCell::default();

    // The return value is the caller's exit status, forwarded ones included.
    app.connect_command_line(move |app, cmdline| {
        let mut cli = match Cli::try_parse_from(cmdline.arguments()) {
            Ok(c) => c,
            Err(e) => {
                let text = e.render().to_string();
                if e.use_stderr() {
                    cmdline.printerr_literal(&text);
                } else {
                    cmdline.print_literal(&text);
                }
                return e.exit_code();
            }
        };
        cli.config = cli
            .config
            .map(|path| absolutize(path, cmdline.cwd().as_deref()));

        let (mode, key) = match cli.command {
            // main() runs it before GTK starts; only here for exhaustiveness.
            Some(Command::Check) => {
                return check_config(
                    cli.config.as_deref(),
                    |msg| report(cmdline, msg),
                    |msg| cmdline.print_literal(&format!("{msg}\n")),
                );
            }
            Some(Command::Daemon) => {
                if hold_guard.borrow().is_some() {
                    return 0;
                }
                // Always spawn: the source re-reads the config on change, so
                // auto_delete_empty and on_delete change without a daemon
                // restart.
                let cleanup_source = config::cleanup_source(cli.config.as_deref());
                std::thread::Builder::new()
                    .name("cleanup".into())
                    .spawn(move || niri::run_event_cleanup(cleanup_source))
                    .ok();
                *hold_guard.borrow_mut() = Some(app.hold());
                return 0;
            }
            None => (ui::Mode::Normal, None),
            Some(Command::Switch { key }) => (ui::Mode::Normal, key),
            Some(Command::Delete { key }) => (ui::Mode::Delete, key),
            Some(Command::MoveWindow { key }) => (ui::Mode::MoveWindow, key),
        };

        if let Some(ch) = key {
            return match handle_direct_action(app, cmdline, &cli, mode, ch) {
                Ok(()) => 0,
                Err(e) => {
                    report(cmdline, &format!("error: {e:#}"));
                    1
                }
            };
        }

        handle_overlay(app, cmdline, &cli, mode);
        0
    });

    let status = app.run();
    // See fail_later: only main sees a local invocation's late failure.
    if status == glib::ExitCode::SUCCESS {
        LATE_EXIT_STATUS.load(Ordering::Relaxed).into()
    } else {
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolutize_relative_uses_caller_cwd() {
        assert_eq!(
            absolutize("cfg.toml".into(), Some(Path::new("/home/u"))),
            PathBuf::from("/home/u/cfg.toml")
        );
    }

    #[test]
    fn absolutize_keeps_absolute() {
        assert_eq!(
            absolutize("/etc/cfg.toml".into(), Some(Path::new("/home/u"))),
            PathBuf::from("/etc/cfg.toml")
        );
    }

    #[test]
    fn absolutize_without_cwd_is_unchanged() {
        assert_eq!(
            absolutize("cfg.toml".into(), None),
            PathBuf::from("cfg.toml")
        );
    }

    #[test]
    fn cli_rejects_invalid_key() {
        let Err(e) = Cli::try_parse_from(["ndw", "switch", "Q"]) else {
            panic!("an uppercase key must not parse");
        };
        assert_eq!(e.exit_code(), 2);
        assert!(e.to_string().contains("invalid value 'Q'"), "{e}");

        let cli = Cli::try_parse_from(["ndw", "switch", "a"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Switch { key: Some('a') })
        ));
    }

    /// Run `check_config` on `path`: its status, and what it reported and printed.
    fn run_check(path: &Path) -> (i32, Vec<String>, Vec<String>) {
        let (reported, printed) = (RefCell::new(Vec::new()), RefCell::new(Vec::new()));
        let status = check_config(
            Some(path),
            |msg| reported.borrow_mut().push(msg.to_string()),
            |msg| printed.borrow_mut().push(msg.to_string()),
        );
        (status, reported.into_inner(), printed.into_inner())
    }

    #[test]
    fn check_config_exit_status_follows_diagnostics() {
        let path = std::env::temp_dir().join(format!("ndw-check-test-{}.toml", std::process::id()));
        let check = |contents: &str| {
            std::fs::write(&path, contents).unwrap();
            run_check(&path)
        };

        let (status, reported, printed) = check("[general]\nworkspace_prefix = \"ws-\"\n");
        assert_eq!((status, reported.len()), (0, 0));
        assert!(printed[0].ends_with("no problems found"), "{printed:?}");

        let (status, reported, printed) = check("[general\n");
        assert_eq!((status, printed.len()), (1, 0));
        assert!(reported[0].starts_with("config error:"), "{reported:?}");

        let (status, reported, _) = check("[general]\nlayout = \"workman\"\n");
        assert_eq!(status, 1);
        assert!(reported[0].starts_with("config warning:"), "{reported:?}");

        std::fs::remove_file(&path).unwrap();
        let (status, reported, _) = run_check(&path);
        assert_eq!(status, 1, "a missing --config file is an error");
        assert!(reported[0].starts_with("config error:"), "{reported:?}");
    }

    #[test]
    fn cli_config_is_global() {
        let cli = Cli::try_parse_from(["ndw", "check", "--config", "x.toml"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Check)));
        assert_eq!(cli.config, Some(PathBuf::from("x.toml")));
    }

    #[test]
    fn fail_later_records_a_local_failure() {
        let cmdline: ApplicationCommandLine = glib::Object::builder()
            .property("arguments", vec![b"ndw\0".to_vec()].to_variant())
            .build();
        assert!(!cmdline.is_remote());
        fail_later(&cmdline, "error: late");
        assert_eq!(cmdline.exit_status(), 1);
        assert_eq!(LATE_EXIT_STATUS.load(Ordering::Relaxed), 1);
    }
}
