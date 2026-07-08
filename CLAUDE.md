# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

A GTK4 Wayland overlay for managing named workspaces on the Niri compositor. The user presses a keybind (default Mod+D), sees a keyboard-shaped grid of workspace cards, and presses a key (a–z or 0–9) to switch/create workspaces. Additional modes delete workspaces and move windows; templates let a new workspace spawn a predefined program set with variable substitution.

## Build Commands

```bash
nix develop --command cargo build      # development build (preferred)
nix develop --command cargo build --release
nix build                              # full Nix package build
```

## Lint & Test Commands

```bash
nix develop --command cargo fmt -- --check   # check formatting
nix develop --command cargo clippy           # lint (all + pedantic)
nix develop --command cargo test             # run unit tests
```

All three must pass clean before committing.

## Architecture

Five source files:

- **main.rs** — clap CLI (`switch`/`delete`/`move-window` with optional direct key, `daemon`) and GTK4 application bootstrap. Direct-key invocations act via IPC without an overlay; otherwise `ui::build_ui` opens the overlay. `daemon` holds the app alive and runs the cleanup thread.
- **config.rs** — Reads `~/.config/niri-dynamic-workspaces/config.toml` (TOML via serde), validates keybinds, workspaces, templates, and template variables, returns `ResolvedConfig`. All fields have defaults; missing/broken config is non-fatal (warnings to stderr). Also owns keyboard layout tables and `{{variable}}` substitution.
- **niri.rs** — Niri IPC layer over the `NiriClient` transport trait (real `SocketClient` per request; scripted mock in tests). Key functions: `list_workspaces`, `list_windows`, `focus_or_create_workspace`, `delete_workspace`, `move_window_to_workspace`, `run_event_cleanup` (daemon event stream), `reorder_workspace_columns`, `run_hooks`.
- **ui.rs** — Builds the full-screen layer-shell overlay with `gtk4-layer-shell`. Renders the keyboard grid plus a static-workspace row, handles keyboard/click/hover events, and hosts the template picker and variable-input sub-views.
- **test_helpers.rs** — constructors for `niri_ipc::Workspace`/`Window` test fixtures.

Data flow: `main` → `config::load_config()` → `ui::build_ui(app, config, mode)` → `niri::*` IPC calls on user interaction.

## Key Conventions

- **Error handling**: `anyhow` with `.context()` throughout niri.rs; UI handlers show errors in a label rather than panicking.
- **Workspace naming**: all dynamic workspaces are prefixed (default `dyn-`) followed by a single workspace key character (a–z or 0–9), optionally followed by a space and a title (`dyn-a My Project`). The prefix is configurable.
- **Nix-first**: the project is built and developed via Nix flakes. `nix/package.nix` is the build derivation, `nix/devshell.nix` provides the dev environment, `nix/hm-module.nix` is a Home Manager integration module.
- **niri-ipc version pinned**: `niri-ipc = "=25.11.0"` in Cargo.toml — exact version match to the compositor IPC protocol.
- **GTK4 CSS**: all visual styling lives in `style.css` at the repo root, loaded at runtime.
- **Linting**: clippy `all` + `pedantic` warnings are enabled in `Cargo.toml [lints.clippy]`. A few noisy pedantic lints (`module_name_repetitions`, `wildcard_imports`, `cast_possible_truncation`) are suppressed. Fix warnings rather than suppressing them, unless the lint is truly inapplicable (e.g. `too_many_lines` on UI builder functions).
- **Testing**: unit tests live in `#[cfg(test)] mod tests` at the bottom of `config.rs`, `ui.rs`, and `niri.rs`. Tests cover pure functions (parsing, config resolution, string transforms) and IPC logic via the mocked `NiriClient`. Add tests when adding new pure logic or IPC sequences.
- **Formatting**: `rustfmt.toml` pins `edition = "2021"`. Run `cargo fmt` before committing.
- **Commit messages**: use [Conventional Commits](https://www.conventionalcommits.org/) style (e.g. `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`). Follow the 50/72 rule: subject line max 50 characters, wrap body text at 72 characters. Be as concise as possible when describing the change.
- **README sync**: when modifying config options (defaults, field names, sections in `config.rs`), update the Configuration section in `README.md` to match. If you notice a discrepancy at any point, fix it.
