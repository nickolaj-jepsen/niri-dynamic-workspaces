# niri-dynamic-workspaces

![niri-dynamic-workspaces](docs/readme.png)

## Install

### Nix (flake)

```nix
# flake.nix inputs
niri-dynamic-workspaces.url = "github:nickolaj-jepsen/niri-dynamic-workspaces";
```

Build directly:

```bash
nix build github:nickolaj-jepsen/niri-dynamic-workspaces
nix run github:nickolaj-jepsen/niri-dynamic-workspaces
```

### Home Manager module

```nix
# Add to your Home Manager imports
imports = [ inputs.niri-dynamic-workspaces.homeModules.default ];

# Enable
programs.niri-dynamic-workspaces = {
  enable = true;
  keybind = "Mod+D";              # default — open switcher
  deleteKeybind = "Mod+Ctrl+D";  # default — open delete overlay
  moveWindowKeybind = "Mod+Shift+D"; # default — open move-window overlay
  daemon = true;                  # default — start daemon at login
  settings = {
    general.workspace_prefix = "dyn-";
    layout.max_columns = 4;
  };
};
```

This installs the package, adds a niri keybind, and writes the config file.

### Cargo

Requires GTK4, gtk4-layer-shell, and pkg-config development headers.

```bash
cargo install --git https://github.com/nickolaj-jepsen/niri-dynamic-workspaces
```

## Configuration

Config file: `~/.config/niri-dynamic-workspaces/config.toml`

All fields are optional with sensible defaults.

```toml
[general]
workspace_prefix = "dyn-"          # prefix for dynamic workspace names
default_programs = ["kitty"]       # programs launched when creating any new workspace
auto_delete_empty = true           # daemon: auto-delete empty unfocused workspaces

[layout]
max_columns = 4             # max columns in the card grid
min_columns = 2             # min columns in the card grid
max_windows_per_card = 4    # max windows shown per card
app_name_max_chars = 12     # truncate app names after N chars
window_title_max_chars = 18 # truncate window titles after N chars

[keybinds]
close = ["Escape", "Ctrl+c", "Ctrl+w", "Ctrl+q"]  # keys to dismiss the overlay

[workspace.a]                          # key: a-z, 0-9, or symbol like , . /
name = "Browser"                       # optional display name shown on the card
programs = ["firefox", "slack"]        # programs launched on create (replaces defaults)
# Configured workspaces that don't exist yet appear as muted cards with a dashed border.

[workspace.b]
programs = ["kitty --title myterm"]    # arguments supported via whitespace splitting

[workspace.1]                          # digit workspaces work too
name = "Comms"
programs = ["slack", "discord"]

# Symbol keys need TOML quoting:
# [workspace.","]
# name = "Quick"
```

### Usage

- **`niri-dynamic-workspaces`** or **`niri-dynamic-workspaces switch`** — opens the switcher overlay (press key to switch/create)
- **`niri-dynamic-workspaces delete`** — opens the delete overlay (press key to delete)
- **`niri-dynamic-workspaces move-window`** — opens the move-window overlay (press key to move the focused window)
- **`niri-dynamic-workspaces daemon`** — starts as a background daemon (no overlay shown)

All modes support toggle behavior: running the same command again closes the overlay.

#### Direct mode (no overlay)

Pass a workspace key to act immediately without opening the overlay:

```bash
niri-dynamic-workspaces switch a        # switch to / create dyn-a
niri-dynamic-workspaces delete a        # delete dyn-a
niri-dynamic-workspaces move-window a   # move focused window to dyn-a
```

### Daemon mode

By default the overlay process starts fresh each time a keybind is pressed, which includes GTK and CSS initialization. For faster overlay display, start a background daemon at login:

```
spawn-at-startup "niri-dynamic-workspaces" "daemon"
```

The daemon keeps GTK initialized and subsequent `switch`/`delete`/`move-window` invocations are forwarded to it over D-Bus, skipping startup overhead.

The Home Manager module enables daemon mode by default. To disable it:

```nix
programs.niri-dynamic-workspaces.daemon = false;
```

## Development

Enter the dev shell and build:

```bash
nix develop
cargo build
```

Lint and test:

```bash
cargo fmt -- --check   # check formatting
cargo clippy           # lint (clippy all + pedantic)
cargo test             # run unit tests
```
