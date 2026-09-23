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

## Changelog & Releases

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Every user-visible change adds an entry under `## [Unreleased]` in the commit that makes it: anything a user of the binary, config file, or Home Manager module would notice (CLI, config keys and defaults, overlay behaviour, themes, HM options, packaging and minimum versions). Skip docs/test/refactor/ci/chore-only changes and fixes to things not yet released. Entries that break an existing config, CLI invocation, hook script, or environment start with `**Breaking:**` and say what to change.

```bash
nix develop --command just changelog-check                     # CI gate
nix develop --command just release <X.Y.Z|patch|minor|major>   # roll Unreleased, bump Cargo.toml/Cargo.lock, commit
```

`release` never pushes or tags. Pushing its commit to main releases once CI is green: `release.yml` publishes the crate and uses the version's changelog section as the GitHub release body. All changelog parsing lives in the `justfile`.

## Architecture

Five top-level source files, plus the `ui/` module directory:

- **main.rs** — clap CLI (`switch`/`delete`/`move-window` with optional direct key, `daemon`, `check`, `rename`) and GTK4 application bootstrap. `check` and `rename` run before GTK starts, so they need no display or D-Bus. Direct-key invocations act via IPC without an overlay; otherwise `ui::build_ui` opens the overlay. Messages for the user go through the `ApplicationCommandLine` (`report`, `fail_later`), so they and the exit status reach the invoking terminal even when a daemon handles the call. `daemon` holds the app alive and runs the cleanup thread.
- **actions.rs** — workspace action choreography shared by the CLI and the overlay: niri IPC call → on-create/on-delete hooks. Window placement (moving programs' windows onto their new workspace, then ordering their columns) and delete completion (waiting for the windows to close, then unsetting the name) run in the background under an application hold guard. `resolve_create_request` turns `switch --template/--var/--title` into the name, programs and hook context of a new workspace.
- **config.rs** — Reads `~/.config/niri-dynamic-workspaces/config.toml` (TOML via serde; `serde_ignored` reports unknown keys), validates keybinds, workspaces, templates, and template variables, returns `ResolvedConfig`. Templates and variables keep their declaration order (toml `preserve_order` into `IndexMap`). All fields have defaults; missing/broken config is non-fatal: problems are collected in `ResolvedConfig::diagnostics` for the caller to print or show. Also owns keyboard layout tables and `{{variable}}` substitution.
- **niri.rs** — Niri IPC layer over the `NiriClient` transport trait (real `SocketClient` per request; scripted mock in tests). Key functions: `list_workspaces`, `list_windows`, `switch_workspace`, `begin_delete`/`finish_delete`, `move_window_to_workspace`, `run_event_cleanup` (daemon event stream), `place_spawned_windows`, `run_hooks`.
- **ui/** — Builds the full-screen layer-shell overlay with `gtk4-layer-shell`. `mod.rs` (overlay construction, modes, key handling), `keys.rs` (layout-independent key resolution, Alt variants, held-key guard), `metrics.rs` (sizing + scaled CSS), `theme.rs` (stylesheet + theme providers), `cards.rs` (workspace info + card widgets), `picker.rs` (template picker), `variables.rs` (variable form + fuzzy select).
- **test_helpers.rs** — constructors for `niri_ipc::Workspace`/`Window` test fixtures.
- **e2e/** — end-to-end harness: `harness.sh` boots a nested headless niri (cage) and drives the overlay with pointer clicks, key presses and screenshots, `test.sh` is the suite, `render-themes.sh` regenerates the theme gallery (`docs/themes.md` + `docs/themes/*.png`) and `render-readme.sh` the README screenshot (`docs/readme.png`) — rerun them when `style.css` or `themes/` change; `render-lib.sh` holds what they share. See `e2e/README.md`.

Data flow: `main` → `config::load_config()` → `ui::build_ui(app, config, mode)` → `niri::*` IPC calls on user interaction.

## Key Conventions

- **Error handling**: `anyhow` with `.context()` throughout niri.rs; UI handlers show errors in a label rather than panicking.
- **Workspace naming**: all dynamic workspaces are prefixed (default `dyn-`) followed by a single workspace key character (a–z or 0–9), optionally followed by a space and a title (`dyn-a My Project`). The prefix is configurable.
- **Nix-first**: the project is built and developed via Nix flakes. `nix/package.nix` is the build derivation, `nix/devshell.nix` provides the dev environment, `nix/hm-module.nix` is a Home Manager integration module, and `nix/hm-module-checks.nix` holds its checks: eval-only assertions (`checks.hm-module`) and builds of generated config files, which the module validates with `check` (`checks.hm-module-config`). `nix/checks.nix` adds `checks.package`, the package build including `cargo test`; CI runs both through `nix flake check`.
- **Crate contents**: Cargo.toml `include` lists the files published to crates.io. Add any new build-time input (an `include_str!` target, a `build.rs` input) there and to the source filter in `nix/package.nix`; CI's `cargo package --locked` step fails when one is missing.
- **niri-ipc version pinned**: `niri-ipc = "=26.4.0"` in Cargo.toml — exact version match to the compositor IPC protocol.
- **GTK4 CSS**: `style.css` is structural and uses only custom properties for colours and sizes. Palettes live in `themes/`: `build.rs` turns every `themes/<name>.css` (except `gtk-*.css` support files) into a built-in `general.theme` name, and named GTK colours (`@foo`) stay inside `themes/gtk*.css`. `ui/theme.rs` registers the embedded CSS and applies `general.theme` on every overlay open; sizes are generated per monitor by `ui/metrics.rs`.
- **Linting**: clippy `all` + `pedantic` warnings are enabled in `Cargo.toml [lints.clippy]`. A few noisy pedantic lints (`module_name_repetitions`, `wildcard_imports`, `cast_possible_truncation`) are suppressed. Fix warnings rather than suppressing them, unless the lint is truly inapplicable (e.g. `too_many_lines` on UI builder functions).
- **Testing**: unit tests live in `#[cfg(test)] mod tests` at the bottom of `main.rs`, `actions.rs`, `config.rs`, `niri.rs`, and the `ui/` modules. Tests cover pure functions (parsing, config resolution, string transforms) and IPC logic via the mocked `NiriClient`. Add tests when adding new pure logic or IPC sequences. End-to-end tests against a real nested compositor live in `e2e/test.sh` (`nix develop --command ./e2e/test.sh`); add a case there when changing overlay modes or the action choreography.
- **Formatting**: `rustfmt.toml` pins `edition = "2021"`. Run `cargo fmt` before committing.
- **Commit messages**: use [Conventional Commits](https://www.conventionalcommits.org/) style (e.g. `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`). Follow the 50/72 rule: subject line max 50 characters, wrap body text at 72 characters. Be as concise as possible when describing the change.
- **README sync**: when modifying config options (defaults, field names, sections in `config.rs`), update the Configuration section in `README.md` to match. If you notice a discrepancy at any point, fix it. A `config.rs` test loads every `toml` block in `README.md` and fails on any diagnostic.
