# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Releases before 0.12.0 are listed on [GitHub Releases](https://github.com/nickolaj-jepsen/niri-dynamic-workspaces/releases).

## [Unreleased]

### Added

- `switch KEY` takes `--template NAME`, `--var NAME=VALUE` and `--title TITLE` to create the workspace from a template and set its title (`--title ""` for none).
- `template = "NAME"` in a `[workspace.KEY]` table binds a template to that key, so the overlay skips the picker and `switch KEY` uses the template too.
- `rename <KEY|focused> [TITLE]` command to set or clear the title of a dynamic workspace without changing its key.
- `check` command that lists every config problem and exits non-zero if there are any; it needs no display, so it can validate a config during a build.
- `switch KEY --here` pulls a workspace from another monitor onto the focused one, and `move-window KEY --no-follow` moves the focused window without following it.
- `general.alt_variants` (default `false`) makes Alt+key or Alt+click in the overlay pull the workspace onto the focused monitor in Switch mode, or move the window without following it in Move Window mode.
- The overlay shows config and theme problems on a line under its hints, with the full list in a tooltip.
- Unknown or misspelled config keys produce a warning instead of being ignored silently; the rest of the file still loads.
- `{{name|basename}}` placeholder filter inserts the last path component of a variable's value, for example in a template `title`.
- The overlay uses the layer-shell namespace `niri-dynamic-workspaces`, so niri `layer-rule`s can target it, for example to hide it from screencasts.
- The Home Manager `keybind`, `deleteKeybind` and `moveWindowKeybind` options accept `null` to leave that bind out.
- `keybinds.close` accepts `Mod` for Super, as niri's config does.
- `fireproof` built-in theme, a warm dark palette with a terracotta accent.

### Changed

- **Breaking:** GTK 4.16 and GLib 2.80 or newer are required; with an older GTK the build now fails instead of producing an unstyled overlay.
- **Breaking:** The Home Manager module runs `niri-dynamic-workspaces check` on the generated config, so any config error or warning now fails `home-manager switch`; fix the reported problem or set `programs.niri-dynamic-workspaces.checkConfig = false`.
- **Breaking:** In `NDW_VAR_*` hook variable names, every character other than an ASCII letter or digit becomes `_`; a hook that read `NDW_VAR_PROJECT-DIR` must read `NDW_VAR_PROJECT_DIR`.
- **Breaking:** An empty `workspace_prefix` is no longer accepted: it falls back to `dyn-` with a warning instead of making every one-character workspace dynamic, so set a non-empty prefix.
- **Breaking:** The `.disabled` card class now marks only keys that do nothing or fail in the current mode; theme files that styled uncreated keys through `.disabled` should use `.uncreated`.
- **Breaking:** A `--config` file that does not exist is reported as a config error instead of silently meaning the defaults, and a daemon started with one skips `auto_delete_empty` cleanup until the file exists; create the file or drop the flag.
- Delete mode asks for a second press of the same key before closing a workspace that has windows; set `general.confirm_delete = false` to delete on one press.
- Deleting a workspace waits for its windows to close before removing its name and running `on_delete`; if a window is still open after 5 s, the workspace keeps its name and `delete KEY` exits non-zero.
- Hooks are launched through niri like `programs`: they get niri's environment and working directory, no longer stop partway when the process exits or the daemon restarts, and their output is discarded.
- `on_delete` hooks also run when the daemon removes an empty workspace (`auto_delete_empty`).
- Templates and their variables follow declaration order in the picker, the variable form and the automatic title instead of alphabetical order; auto-assigned template keys can change, so set `key` to keep one.
- The overlay footer says what a key press does in each mode, including that Delete closes the workspace's windows.
- Workspace titles on cards wrap onto two lines instead of being cut off at eight characters, and a name that still does not fit shows in full as a tooltip.
- The card of a workspace visible on another monitor gets an accent outline instead of the focused card's fill.
- `-c/--config` can also be given after the subcommand.

### Removed

- Prebuilt binaries are no longer attached to GitHub releases, since they only ran on Nix systems; use the flake or `cargo install`.

### Fixed

- Commands exit non-zero when they fail, and errors and config warnings are printed in the calling terminal even when the daemon handles the call; a relative `--config` path resolves against the caller's directory.
- The daemon's `auto_delete_empty` cleanup picks up config changes, including a Home Manager switch, and an edit that breaks the file keeps the last working settings instead of resetting them to the defaults.
- Windows of a new workspace's programs that open after you switch away are moved onto that workspace without taking focus, and `auto_delete_empty` no longer removes the workspace before they appear; without a daemon, a `switch` that starts programs now runs until each has a window, for up to 15 s.
- Workspace and template keys work on any XKB layout, including AZERTY's unshifted digits and non-Latin layouts that have a Latin layout alongside, and Shift+key selects too.
- `keybinds.close` binds ignore Caps Lock, match with Shift (`Ctrl+Shift+q`) and work while a non-Latin layout is active; an uppercase letter (`Ctrl+C`) means the lowercase key.
- With niri's `workspace-auto-back-and-forth`, closing the overlay or picking the workspace you are previewing no longer jumps to the previous workspace.
- Cancelling a hover preview returns to the workspace the overlay opened from, also when that workspace is unnamed or empty, and empty pinned keys preview like any other key.
- Move Window mode moves the window that was focused when the overlay opened, even if focus changes while it is open, and switching the overlay to another mode undoes a hover preview first.
- `move-window` fails when no window is focused instead of leaving an empty named workspace behind and reporting success.
- `on_create` hooks run when `move-window` creates the workspace.
- `on_delete` hooks run by `delete KEY` get the workspace's title in `NDW_WORKSPACE_NAME`, as they already did from the overlay.
- The Home Manager module evaluates without niri-flake; its keybinds are added only when niri-flake's Home Manager module is loaded and Home Manager's own niri module is off.
- The Home Manager daemon service starts only when niri has exported `NIRI_SOCKET` to the systemd user environment, as `niri --session` does, instead of retrying every 5 s in other desktops.
- Ordering a new workspace's columns no longer pulls you back to it after you switched away while its programs started.
- Programs started through a wrapper (`flatpak run`, `uwsm app --`, `env`, `sh -c`) are matched to their windows, and programs whose window matches nothing are named in a warning.
- Unnamed workspaces in the row above the keyboard respond to clicks, hovers and moves, and a pinned key whose workspace is missing shows an error instead of silently closing the overlay.
- `static` can pin a workspace whose name merely starts with the prefix (such as `dyn-alpha`), and pins match names case-insensitively, as niri does.
- Holding a key no longer carries over into the next view, where it could pick a template or submit an empty variable form; a held close key backs out one view at a time.
- The overlay fits the monitor height, so the static row and mode tabs are no longer cut off on ultrawide monitors.
- Moving focus to another monitor no longer closes the template picker or variable form and loses what was entered.
- Window title changes no longer rebuild the overlay, which cleared a shown error and could lose a click.
- When niri cannot be reached, the overlay shows the error as soon as it opens instead of only after a key press.
- The daemon no longer keeps every closed overlay in memory.
- The open overlay no longer misses updates when non-ASCII text in a window or workspace name arrives split across reads.
- A `command` variable is stopped after 10 s or when you leave the form, and a failed or slow command falls back to text input with the reason shown.
- Pressing Enter when a select variable's filter matches nothing no longer submits an empty value: `options` shows an error, `command` uses the typed text and `dir` uses the typed path if it is an existing absolute directory.
- Long options in a select variable are shortened with an ellipsis instead of widening the form past the screen, and clicking an option highlights it for Enter.
- `dir` variables list symlinks to directories.
- A template `key = 2` or a workspace `static = 1` written as a number is accepted instead of discarding the whole config.
- A `keybinds.close` list in which no entry parses falls back to the defaults so Escape still closes the overlay, and a close bind that takes a workspace key (`q`, `Mod+q`) produces a warning.
- Template variables used only in `title`, in an `on_create` hook or for the automatic title no longer warn as unreferenced, and a program with an unclosed quote warns when the config loads, not only when a workspace is created.
- The Home Manager module no longer triggers a `system` rename warning on every evaluation, and the flake package sets `meta.mainProgram` for `lib.getExe`.

## [0.12.0] - 2026-09-18

### Added

- `general.theme` config option picks the overlay colours: `gtk` (default) follows your GTK theme, `dark` and `light` are built-in palettes, and a path loads your own CSS file.
- Built-in colour schemes for `general.theme`: `catppuccin-mocha`, `catppuccin-latte`, `gruvbox-dark`, `gruvbox-light`, `nord`, `dracula`, `tokyo-night`, `rose-pine`, `everforest`, `kanagawa`, `solarized-dark`, `solarized-light`, `flexoki-dark` and `flexoki-light`.
- `programs.niri-dynamic-workspaces.themeCss` Home Manager option writes a theme file next to the config and selects it as `general.theme`.
- `NDW_APP_ID` environment variable sets the application id, so a second copy can run alongside an installed daemon instead of handing its commands to it.

### Changed

- **Breaking:** GTK 4.16 or newer is required; older versions show the overlay without colours.
- **Breaking:** Template variable values are no longer shell-quoted, so a value used inside `sh -c "..."` reaches the shell as-is; pass it as its own argument instead, e.g. `sh -c 'cd "$1" && make' _ {{path}}`.
- Updated for niri 26.04 (`niri-ipc` 26.4.0).

### Fixed

- The daemon no longer leaks about 10 MiB of GPU memory in niri each time the overlay closes while `inhibit_compositor_shortcuts` is enabled.
- Cards, backdrop, accent and urgent colours no longer vanish under GTK themes without libadwaita colours, such as GTK's own Default theme.
- Template `{{variable}}` placeholders work inside quoted arguments and when written with spaces (`{{ path }}`), a filled-in value is never substituted again, and a program command with unbalanced quotes fails before the workspace is created.
- `auto_delete_empty` no longer leaves an empty workspace behind until something else changes in niri.
- A workspace named like `dyn-alpha` is no longer taken for dynamic workspace `a` or auto-deleted; it now shows in the static workspace row.
- With `hover_preview`, closing the overlay returns to the original workspace after the template picker is cancelled or an action fails.
- Dropdown variables with many options stay responsive, show the best 50 matches and keep the selection in view; the template picker also scrolls to the selection.
- A template named `Empty` now runs its `on_create` hooks and sets `NDW_TEMPLATE`.

[Unreleased]: https://github.com/nickolaj-jepsen/niri-dynamic-workspaces/compare/v0.12.0...HEAD
[0.12.0]: https://github.com/nickolaj-jepsen/niri-dynamic-workspaces/compare/v0.11.0...v0.12.0
