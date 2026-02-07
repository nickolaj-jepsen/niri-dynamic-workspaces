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
  keybind = "Mod+D";  # default
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
workspace_prefix = "dyn-"  # prefix for dynamic workspace names

[layout]
max_columns = 4             # max columns in the card grid
min_columns = 2             # min columns in the card grid
max_windows_per_card = 4    # max windows shown per card
app_name_max_chars = 12     # truncate app names after N chars
window_title_max_chars = 18 # truncate window titles after N chars

[keybinds]
close = ["Escape", "Ctrl+c", "Ctrl+w", "Ctrl+q"]  # keys to dismiss the overlay
delete_modifier = "Shift"   # modifier + a-z to delete a workspace
```

Valid modifiers: `Ctrl`/`Control`, `Shift`, `Alt`/`Mod1`, `Super`/`Mod4`.

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
