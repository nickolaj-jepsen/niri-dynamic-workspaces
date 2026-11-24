#!/usr/bin/env bash
# One isolated headless Umbriel, with its own runtime directory and D-Bus.
set -euo pipefail
cd "$(dirname "$0")/.."
app=$(realpath "${NDW_BIN:-target/debug/niri-dynamic-workspaces}")
for tool in umbriel foot jq wtype dbus-run-session; do command -v "$tool" >/dev/null; done
[[ -x $app ]]

if [[ ${1:-} != --private-bus ]]; then
    run=$(mktemp -d /tmp/ndwu.XXXXXXXX)
    # No host service directories: don't activate portals or session services.
    cat > "$run/dbus.conf" <<'CONFIG'
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
  </policy>
</busconfig>
CONFIG
    exec dbus-run-session --config-file="$run/dbus.conf" -- "$0" --private-bus "$run"
fi

run=$2
export XDG_RUNTIME_DIR="$run" XDG_STATE_HOME="$run/state" XDG_CONFIG_HOME="$run/config"
export WAYLAND_DISPLAY=wayland-0 UMBRIEL_SOCKET="$run/umbriel-wayland-0.sock"
export NDW_UMBRIEL_INCLUDE="$run/generated/workspaces.toml" GSK_RENDERER=cairo
unset WAYLAND_SOCKET NIRI_SOCKET DISPLAY
mkdir "$run/generated"
: > "$NDW_UMBRIEL_INCLUDE"

cleanup() {
    local status=$?
    for pid in "${overlay:-}" "${keyboard:-}" "${server:-}"; do
        [[ -z $pid ]] || kill "$pid" 2>/dev/null || true
    done
    wait || true
    if ((status == 0)); then
        rm -rf "$run"
    else
        echo "Umbriel test logs: $run" >&2
        tail -30 "$run/compositor.log" >&2 || true
    fi
}
trap cleanup EXIT

cat > "$run/umbriel.toml" <<'CONFIG'
[general]
xwayland = false
show_cheatsheet = false
[animation]
enabled = false
[include.optional]
files = ["baseline.toml", "$NDW_UMBRIEL_INCLUDE"]
CONFIG
cat > "$run/baseline.toml" <<'CONFIG'
[[workspace]]
name = "BASE"
output = "HEADLESS-1"
CONFIG
cat > "$run/ndw.toml" <<CONFIG
[template.demo]
programs = ["foot --title ndw-umbriel-e2e sh -c 'sleep 120'"]
title = "Template title"
cwd = "$run/project dir"

[template.plain]
programs = ["true"]
CONFIG
mkdir -p "$run/project dir"

start() {
    env -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=$run/no-system-bus" \
        WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_HEADLESS_OUTPUTS=2 \
        umbriel -c "$run/umbriel.toml" > "$run/compositor.log" 2>&1 & server=$!
    until_true test -S "$UMBRIEL_SOCKET"
}

until_true() {
    for _ in {1..100}; do
        if "$@"; then return; fi
        sleep .05
    done
    printf 'Timed out: %s\n' "$*" >&2
    return 1
}

process_exited() { ! kill -0 "$1" 2>/dev/null; }
ndw() { "$app" -c "$run/ndw.toml" "$@"; }
workspaces() { umbriel workspaces --json | jq -e "$@"; }
windows() { umbriel windows --json | jq -e "$@"; }
layer_focused() {
    umbriel layers --json | jq -e 'any(.[]; .namespace == "niri-dynamic-workspaces" and .focused)' >/dev/null
}

start
baseline=$(workspaces -r '.[] | select(.name == "BASE").id')
ndw switch a --template demo
id=$(workspaces -r '.[] | select(.name == "dyn-a" and .focused).id')
# The directory survives the program and is reported for the focused workspace
# and by key; one created without it reports nothing.
test "$(ndw cwd)" = "$run/project dir"
test "$(ndw cwd a)" = "$run/project dir"
ndw switch e --template plain
if ndw cwd >/dev/null 2>&1; then echo "A workspace without cwd must fail" >&2; exit 1; fi
ndw switch a
until_true windows --arg id "$id" 'any(.[]; .title == "ndw-umbriel-e2e" and .workspace == $id)'
ndw --compositor umbriel rename a 'Renamed project'
workspaces --arg id "$id" 'any(.[]; .id == $id and .name == "dyn-a" and .focused)'
windows --arg id "$id" 'any(.[]; .title == "ndw-umbriel-e2e" and .workspace == $id)'
if ndw delete a; then echo "Deleting a populated Umbriel workspace must fail" >&2; exit 1; fi

# Move the original window to a freshly created workspace without following,
# then retire the vacated one.
ndw move-window b --no-follow
workspaces --arg id "$id" 'any(.[]; .id == $id and .focused) and any(.[]; .name == "dyn-b" and .window_count == 1)'
ndw delete a
workspaces --arg baseline "$baseline" 'all(.[]; .name != "dyn-a") and any(.[]; .id == $baseline)'
# A retired workspace leaves no directory behind for the next one on its key.
ndw switch a --template plain
if ndw cwd >/dev/null 2>&1; then echo "A recreated workspace must not inherit a cwd" >&2; exit 1; fi
ndw delete a

# One key across monitors. The existing workspace stays on its own output.
home_output=$(workspaces -r '.[] | select(.name == "dyn-b").output')
other_output=$(workspaces -r --arg home "$home_output" '[.[] | .output | select(. != $home)] | first')
umbriel msg workspace-switch 1 --output "$other_output"
ndw switch b
workspaces --arg home "$home_output" 'any(.[]; .name == "dyn-b" and .focused and .output == $home)'

# Keep keyboard capability advertised while GTK binds the seat.
wtype -k F24 -s 60000 & keyboard=$!
ndw switch > "$run/overlay.log" 2>&1 & overlay=$!
until_true layer_focused
# The only configured template would show the picker for a new key. Select the
# existing workspace instead, then open an overlay without templates to create.
wtype -s 100 b -s 100
until_true process_exited "$overlay"
wait "$overlay"
workspaces 'any(.[]; .name == "dyn-b" and .focused)'
"$app" -c /dev/null switch > "$run/overlay.log" 2>&1 & overlay=$!
until_true layer_focused
wtype -s 100 c -s 100
until_true process_exited "$overlay"
wait "$overlay"
workspaces 'any(.[]; .name == "dyn-c" and .focused)'
ndw delete c
ndw delete e

# Disconnect the output holding a populated managed workspace. The surviving
# window and workspace should still be reachable by key on the other output.
umbriel output-create NDW-TEST 1280x720 >/dev/null
umbriel msg workspace-switch 1 --output NDW-TEST
ndw switch d
virtual_id=$(workspaces -r '.[] | select(.name == "dyn-d" and .output == "NDW-TEST").id')
ndw switch b
ndw move-window d
windows --arg id "$virtual_id" 'any(.[]; .title == "ndw-umbriel-e2e" and .workspace == $id)'
umbriel output-destroy NDW-TEST
ndw switch d
workspaces 'any(.[]; .name == "dyn-d" and .focused and .output != "NDW-TEST" and .window_count == 1)'

# A missing include registration rolls back the file, with no template launch.
not_watched="$run/generated/not-watched.toml"
: > "$not_watched"
if NDW_UMBRIEL_INCLUDE="$not_watched" ndw switch z --template demo; then
    echo "Creation without a registered include must fail" >&2
    exit 1
fi
[[ ! -s $not_watched ]]
workspaces 'all(.[]; .name != "dyn-z")'
windows '[.[] | select(.title == "ndw-umbriel-e2e")] | length == 1'

# A fresh compositor gets a fresh include, even while the old file exists.
kill "$keyboard"; wait "$keyboard" || true; keyboard=
kill "$server"; wait "$server"; server=
export NDW_UMBRIEL_INCLUDE="$run/generated/new-session.toml"
: > "$NDW_UMBRIEL_INCLUDE"
start
workspaces 'all(.[]; .name != "dyn-b" and .name != "dyn-d") and any(.[]; .name == "BASE")'
echo "Umbriel prototype passed"
