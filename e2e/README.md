# End-to-end harness

Runs the overlay inside a nested, headless niri (cage with
`WLR_BACKENDS=headless`) so tests and agents can drive it without touching your
session. Clicks and key presses go in; screenshots and niri IPC state come out.
No GPU, TTY or seat required, so it also runs in CI.

## Quick start

```bash
nix develop --command cargo build
nix develop --command ./e2e/test.sh            # the suite
nix develop --command ./e2e/harness.sh start   # a session to poke at by hand
```

One session at a time lives in `$XDG_RUNTIME_DIR/ndw-e2e`; `stop` tears it down.
Screenshots go to `e2e/out/`.

The suite drives `target/debug/niri-dynamic-workspaces` (`NDW_BIN` picks
another binary). Keep it a debug build: `daemon_frees_closed_overlays` counts
the `debug: overlay window freed` lines only debug builds log.

## Verbs

| Command | What it does |
| --- | --- |
| `start` / `stop` | boot or tear down the nested session |
| `app <args...>` | run the overlay binary in it and wait for exit |
| `daemon` | start the daemon in it, wait until it owns its D-Bus name |
| `serving` | does the daemon still own its D-Bus name |
| `config [file]` | replace its `config.toml` with a file, or stdin |
| `overlay [mode]` | open the overlay in the background, wait until it is mapped |
| `key <char>` | click the card for `a`-`z` / `0`-`9` |
| `hover <char>` | move the pointer onto that card, which previews it |
| `static <i> [n]` | click the `i`-th of `n` cards (default 1) in the static row |
| `mode <name>` | click `switch`, `delete` or `move`, and wait for the rebuilt grid |
| `click <x> <y>` | click anywhere |
| `type <args...>` | press keys with `wtype`, e.g. `type c` |
| `press <codes> [ms]` | press evdev codes through niri's layout (`46` = c, `29+46` = Ctrl+c), optionally held for `ms` |
| `escape` | dismiss the overlay |
| `open` / `closed` | is the overlay mapped, or wait until it is not |
| `state` | workspaces as JSON |
| `shot [name]` | screenshot to `e2e/out/<name>.png` |
| `run <cmd...>` | run any command against the nested session |
| `logs [n]` | tail the bus, compositor and overlay logs |

A session by hand:

```bash
./e2e/harness.sh start
./e2e/harness.sh overlay switch
./e2e/harness.sh shot grid          # look at e2e/out/grid.png
./e2e/harness.sh key c
./e2e/harness.sh state | jq
./e2e/harness.sh run niri msg -j windows
./e2e/harness.sh stop
```

## Theme gallery

`./e2e/render-themes.sh` renders every `themes/<name>.css` as
`theme = "<name>"` into `docs/themes/<name>.png` and regenerates
`docs/themes.md`. `gtk` is shown under both GTK variants and `gtk-*.css` are
support files. It fails if a theme logs a config or CSS warning. Run it after
changing `style.css`, `themes/` or card layout.

## README screenshot

`./e2e/render-readme.sh` renders `docs/readme.png`: the `fireproof` theme over
a 1920x1080 workspace holding a `gnome-text-editor` beside two stacked `foot`
terminals, all three coloured to match. The editor goes dark through its own
settings (libadwaita ignores `GTK_THEME`), written to a GSettings keyfile in
the session's config dir. Run it after changing the overlay's look.

## Isolation

Each session gets its own `XDG_CONFIG_HOME` holding `e2e/fixtures/config.toml`,
its own D-Bus session bus, and `NDW_APP_ID=dev.nickolaj.niri-dynamic-workspaces.E2e`.
Without the private bus an installed daemon on the host bus would own the
application id and answer the invocation instead. The bus runs from a config
written to the run dir, not the host's `/etc/dbus-1`, and `GDK_DEBUG=no-portals`
keeps GTK from waiting on a settings portal it activates there.

`fixtures/niri.kdl` turns on `workspace-auto-back-and-forth`, which makes
focusing the active workspace jump to the previous one. The suite then sees
the overlay re-focus a workspace, and a test must not re-focus the active
workspace itself expecting nothing to happen.

The nested niri loads EGL from the dev shell's mesa (`NDW_E2E_EGL_VENDOR`), so
it runs without `/run/opengl-driver`, as on CI. A session that fails to start
leaves its logs in `e2e/out/fail-<name>.log`.

`GTK_THEME` is pinned to `Default:dark` so screenshots never show the host
theme; set `NDW_E2E_GTK_THEME` before `start` to look at another one (empty
leaves it unset). `NDW_E2E_CONFIG` swaps in another `config.toml` at `start`,
and `config` replaces it in a running session. `test.sh` builds per-test
settings on that with `add_config` (appends TOML from stdin) and
`general <key> <value>`.
`NDW_E2E_SIZE=1920x1080` resizes cage's output with `wlr-randr`; `key`,
`hover`, `static` and `mode` only know the default size.

## Key presses

`type` passes its arguments to `wtype`, so it can also hold modifiers
(`-M ctrl -k c -m ctrl`) or keys (`-P c -s 1500 -p c`). niri applies `wtype`'s
keymap to the keys it sends, but resends that keymap only when it differs from
the last one. A new overlay then reads a repeat of the previous call's keycodes
with the compositor's layout, where the first keycode `wtype` hands out is
Escape, so `type` sends a throwaway F24 first, which the overlay ignores.
`escape` is correct either way.

`press` tests layout handling, which `type` cannot: `wtype`'s keymap has one
group and one level per key. It runs `wtype` on cage's display instead, where
the nested niri reads each key as a raw evdev code through its own layout
(`us,fr,ru` in `fixtures/niri.kdl`, us active). `wtype` numbers keysyms from
1 in order of first use, so `press` releases fillers for codes 1 up to the
highest one before pressing, which puts each pressed keysym on its code. It
waits 0.3 s before the first key: niri binds cage's keyboard only once the
session's first `wtype` has created it, and keys sent before that are lost.
Pick a group first with `run niri msg action switch-layout <index>`. Name the
key in a comment next to each `press`, since the codes are opaque.

## Click coordinates

`key` and `hover`, `static` and `mode` hold pixel positions for the default
qwerty layout on the 1272x688 output cage hands us. The static row is centred,
so `static` needs to know how many cards it holds. If the overlay metrics
change, open the overlay, take a `shot`, read the new card centres off the
image and update `row_for_key`, `static_position` and `mode_position` in
`harness.sh`.
