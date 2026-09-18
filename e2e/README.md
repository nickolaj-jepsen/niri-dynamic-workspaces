# End-to-end harness

Runs the overlay inside a nested, headless niri (cage with
`WLR_BACKENDS=headless`) so tests and agents can drive it without touching your
session. Clicks and Escape go in; screenshots and niri IPC state come out. No
GPU, TTY or seat required, so it also runs in CI.

## Quick start

```bash
nix develop --command cargo build
nix develop --command ./e2e/test.sh            # the suite
nix develop --command ./e2e/harness.sh start   # a session to poke at by hand
```

One session at a time lives in `$XDG_RUNTIME_DIR/ndw-e2e`; `stop` tears it down.
Screenshots go to `e2e/out/`.

## Verbs

| Command | What it does |
| --- | --- |
| `start` / `stop` | boot or tear down the nested session |
| `app <args...>` | run the overlay binary in it and wait for exit |
| `overlay [mode]` | open the overlay in the background, wait until it is mapped |
| `key <char>` | click the card for `a`-`z` / `0`-`9` |
| `mode <name>` | click `switch`, `delete` or `move` |
| `click <x> <y>` | click anywhere |
| `escape` | dismiss the overlay |
| `open` / `closed` | is the overlay mapped, or wait until it is not |
| `state` | workspaces as JSON |
| `shot [name]` | screenshot to `e2e/out/<name>.png` |
| `run <cmd...>` | run any command against the nested session |
| `logs [n]` | tail the overlay and compositor logs |

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

## Isolation

Each session gets its own `XDG_CONFIG_HOME` holding `e2e/fixtures/config.toml`,
its own D-Bus session bus, and `NDW_APP_ID=dev.nickolaj.niri-dynamic-workspaces.E2e`.
Without the private bus an installed daemon on the host bus would own the
application id and answer the invocation instead.

## Keyboard input does not work yet

niri mistranslates keycodes coming from the virtual-keyboard protocol, so a key
sent with `wtype` arrives as whatever sits at that evdev code in the
compositor's own layout: `wtype -k c` reaches the overlay as Escape. See
[niri#3394](https://github.com/niri-wm/niri/issues/3394); the fix is still open
in [niri#4548](https://github.com/niri-wm/niri/pull/4548). Until it lands the
tests click cards, which goes through the same `dispatch_action` path as a key
press. The `escape` verb is correct either way.

## Click coordinates

`key` and `mode` hold pixel positions for the default qwerty layout on the
1272x688 output cage hands us. If the overlay metrics change, open the overlay,
take a `shot`, read the new card centres off the image and update
`row_for_key` and `mode_position` in `harness.sh`.
