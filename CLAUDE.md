# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

A GTK4 Wayland overlay for managing named workspaces on the Niri compositor. The user presses a keybind (default Mod+D), sees a grid of workspace cards, and presses a–z to switch/create or Shift+a–z to delete.

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

Four source files, ~750 lines total:

- **main.rs** — GTK4 application bootstrap: loads config, loads `style.css`, calls `ui::build_ui` on activate.
- **config.rs** — Reads `~/.config/niri-dynamic-workspaces/config.toml` (TOML via serde), validates keybinds and modifiers, returns `ResolvedConfig`. All fields have defaults; missing/broken config is non-fatal.
- **niri.rs** — Niri IPC layer. Talks to the compositor over a Unix socket using `niri_ipc::Socket`. Key functions: `list_workspaces`, `list_windows`, `focus_or_create_workspace`, `delete_workspace`.
- **ui.rs** — Builds the full-screen layer-shell overlay with `gtk4-layer-shell`. Gathers workspace/window state from niri IPC, renders a `FlowBox` grid of cards, handles keyboard (a–z, Shift+key, close binds) and click events.

Data flow: `main` → `config::load_config()` → `ui::build_ui(app, config)` → `niri::*` IPC calls on user interaction.

## Key Conventions

- **Error handling**: `anyhow` with `.context()` throughout niri.rs; UI handlers show errors in a label rather than panicking.
- **Workspace naming**: all dynamic workspaces are prefixed (default `dyn-`) followed by a single lowercase letter. The prefix is configurable.
- **Nix-first**: the project is built and developed via Nix flakes. `nix/package.nix` is the build derivation, `nix/devshell.nix` provides the dev environment, `nix/hm-module.nix` is a Home Manager integration module.
- **niri-ipc version pinned**: `niri-ipc = "=25.11.0"` in Cargo.toml — exact version match to the compositor IPC protocol.
- **GTK4 CSS**: all visual styling lives in `style.css` at the repo root, loaded at runtime.
- **Linting**: clippy `all` + `pedantic` warnings are enabled in `Cargo.toml [lints.clippy]`. A few noisy pedantic lints (`module_name_repetitions`, `wildcard_imports`, `cast_possible_truncation`) are suppressed. Fix warnings rather than suppressing them, unless the lint is truly inapplicable (e.g. `too_many_lines` on UI builder functions).
- **Testing**: unit tests live in `#[cfg(test)] mod tests` at the bottom of `config.rs` and `ui.rs`. Tests cover pure functions only (parsing, config resolution, string transforms). Add tests when adding new pure logic.
- **Formatting**: `rustfmt.toml` pins `edition = "2021"`. Run `cargo fmt` before committing.
- **Commit messages**: use [Conventional Commits](https://www.conventionalcommits.org/) style (e.g. `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`). Follow the 50/72 rule: subject line max 50 characters, wrap body text at 72 characters.
- **README sync**: when modifying config options (defaults, field names, sections in `config.rs`), update the Configuration section in `README.md` to match. If you notice a discrepancy at any point, fix it.
