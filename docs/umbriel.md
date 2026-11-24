# Experimental Umbriel support

The same binary and config work on Umbriel. The app detects the compositor by
its IPC socket; pass `--compositor umbriel` or `--compositor niri` if both sockets
are reachable (for example in a nested session). Niri needs no migration.

For existing Umbriel workspaces, map keys to exact, case-sensitive names with
`[workspace.KEY]` and `static = "NAME"` in your niri-dynamic-workspaces config.
The overlay can also show and switch to the workspaces on the current output
without any mappings.

## Enable dynamic keys

Umbriel has no native IPC action to create a named workspace. This backend
uses an **app-owned, per-session file** for workspace rules. Add this to your
Umbriel config (or add the path to your existing `[include.optional]` list):

```toml
[include.optional]
files = ["$NDW_UMBRIEL_INCLUDE"]
```

Start Umbriel through a wrapper like this so the compositor and keybind commands
inherit the same path. This needs to wrap **Umbriel itself**, not individual
keybinds or the app's daemon:

```sh
#!/bin/sh
set -eu
session=$(mktemp -d "$XDG_RUNTIME_DIR/ndw-umbriel.XXXXXXXX")
trap 'rm -rf -- "$session"' EXIT
export NDW_UMBRIEL_INCLUDE="$session/workspaces.toml"
: > "$NDW_UMBRIEL_INCLUDE"
umbriel "$@"
```

Do not point the variable at your main config or a file you edit. The backend
replaces only the generated file, reloads Umbriel, and checks that the expected
workspace appeared before launching programs. A new compositor session gets a
fresh include; restarting the workspace app alone does not clear its workspaces.
The workflow requires Umbriel's dynamic inventory on the output where you create
workspaces (`workspaces = "dynamic"`, the default).

For example, in Umbriel's `[keybinds]`:

```toml
[keybinds]
"Mod+D" = "spawn niri-dynamic-workspaces"
"Mod+Shift+D" = "spawn niri-dynamic-workspaces delete"
"Mod+Ctrl+D" = "spawn niri-dynamic-workspaces move-window"
```

## Working directories

Templates work here too, `cwd` included: the path is kept in this app's include
next to the title, and `cwd` reads it back as on Niri. A session that starts
with a fresh include starts without directories, just as it starts without
workspace titles.

## Prototype limits

- One dynamic key identifies one workspace across all outputs. It is created on
  the current output and is switched to in place. Pulling it to another output
  with `--here` or the Alt variant is not supported yet.
- Rename and template titles appear in this app's overlay only. Umbriel shows the
  stable native name (e.g. `dyn-a`): changing the rule's name would create a new
  workspace rather than retitle the old one.
- Delete retires **empty app-owned workspaces only**. This backend never closes
  windows to delete a workspace, and does not apply `auto_delete_empty` yet.
- Programs use Umbriel's `spawn` action and its launch activation token. There
  is no Niri-style fallback that tracks and reorders their windows.
- Umbriel's move action targets the currently focused window. The overlay
  rejects the move if that window no longer matches its captured window.

The prototype was tested on Umbriel 0.1.0 (`a909081`).
