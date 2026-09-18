#!/usr/bin/env bash
# Drive the overlay inside a nested, headless niri: clicks in, screenshots and state out.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(dirname "$here")
run_dir=${NDW_E2E_DIR:-${XDG_RUNTIME_DIR:-/tmp}/ndw-e2e}
out_dir=${NDW_E2E_OUT:-$repo/e2e/out}
env_file=$run_dir/env
bin=${NDW_BIN:-$repo/target/debug/niri-dynamic-workspaces}

die() { echo "harness: $*" >&2; exit 1; }

wait_for() {
    local timeout=$1 deadline
    shift
    deadline=$((SECONDS + timeout))
    until "$@" >/dev/null 2>&1; do
        ((SECONDS < deadline)) || return 1
        sleep 0.1
    done
}

# niri colours its log even when redirected.
log_value() {
    sed -e 's/\x1b\[[0-9;]*m//g' "$run_dir/niri.log" | sed -n "s/.*$1//p" | tail -1
}

in_env() {
    [[ -f $env_file ]] || die "not started (run '$0 start')"
    # shellcheck disable=SC1090
    (. "$env_file"; exec "$@")
}

# Card centres for the default qwerty layout on cage's 1272x688 output; re-read them off a 'shot' if metrics change.
row_for_key() {
    case $1 in
    [0-9]) echo "202 170 1234567890" ;;
    [qwertyuiop]) echo "300 219 qwertyuiop" ;;
    [asdfghjkl]) echo "398 243 asdfghjkl" ;;
    [zxcvbnm]) echo "496 292 zxcvbnm" ;;
    *) return 1 ;;
    esac
}

key_position() {
    local spec y x0 keys rest
    spec=$(row_for_key "$1") || die "no such key: $1"
    read -r y x0 keys <<<"$spec"
    rest=${keys%%"$1"*}
    echo "$((x0 + 98 * ${#rest})) $y"
}

mode_position() {
    case $1 in
    switch) echo "546 618" ;;
    delete) echo "613 618" ;;
    move) echo "702 618" ;;
    *) die "no such mode: $1" ;;
    esac
}

# zwlr_virtual_pointer only moves relatively, so warp to the origin first.
move_to() {
    in_env wlrctl pointer move -5000 -5000
    in_env wlrctl pointer move "$1" "$2"
}

click_at() {
    move_to "$1" "$2"
    in_env wlrctl pointer click left
}

overlay_open() { [[ $(in_env niri msg -j layers | jq length) -gt 0 ]]; }
overlay_closed() { ! overlay_open; }

start() {
    stop
    mkdir -p "$run_dir/config/niri-dynamic-workspaces" "$out_dir"
    cp "$here/fixtures/config.toml" "$run_dir/config/niri-dynamic-workspaces/config.toml"
    [[ -x $bin ]] || die "no binary at $bin (cargo build, or set NDW_BIN)"
    for tool in cage niri grim wtype wlrctl jq dbus-daemon; do
        command -v "$tool" >/dev/null || die "missing $tool (run inside 'nix develop')"
    done

    # The overlay forwards invocations over D-Bus; a private bus keeps a host daemon from answering.
    local dbus_addr
    # Activated services (the settings portal) inherit this environment; the host config would leak its GTK theme.
    XDG_CONFIG_HOME="$run_dir/config" \
        dbus-daemon --session --nofork --print-address >"$run_dir/dbus.addr" 2>/dev/null &
    echo $! >"$run_dir/dbus.pid"
    wait_for 10 test -s "$run_dir/dbus.addr" || die "session bus did not start"
    dbus_addr=$(cat "$run_dir/dbus.addr")

    env -u WAYLAND_DISPLAY -u NIRI_SOCKET \
        XDG_CONFIG_HOME="$run_dir/config" \
        DBUS_SESSION_BUS_ADDRESS="$dbus_addr" \
        WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
        cage -- niri -c "$here/fixtures/niri.kdl" >"$run_dir/niri.log" 2>&1 &

    wait_for 30 grep -q "IPC listening on:" "$run_dir/niri.log" || {
        tail -5 "$run_dir/niri.log" >&2
        die "nested niri did not start"
    }

    cat >"$env_file" <<EOF
export XDG_CONFIG_HOME=$run_dir/config
export DBUS_SESSION_BUS_ADDRESS=$dbus_addr
export WAYLAND_DISPLAY=$(log_value "listening on Wayland socket: ")
export NIRI_SOCKET=$(log_value "IPC listening on: ")
export NDW_APP_ID=dev.nickolaj.niri-dynamic-workspaces.E2e
export GSK_RENDERER=cairo
export GTK_THEME=${NDW_E2E_GTK_THEME-Default:dark}
export LIBGL_ALWAYS_SOFTWARE=1
EOF
    wait_for 10 in_env niri msg version || die "nested niri IPC not responding"
    echo "$run_dir"
}

stop() {
    local pid
    # Matches cage and the niri it wraps; cage does not reap niri, and a leak holds its wayland socket.
    pkill -f "niri -c $here/fixtures/niri.kdl" 2>/dev/null || true
    pid=$(cat "$run_dir/dbus.pid" 2>/dev/null || true)
    [[ -n $pid ]] && kill "$pid" 2>/dev/null || true
    rm -rf "$run_dir"
}

usage() {
    cat <<'EOF'
usage: harness.sh <command>

  start            boot a nested headless niri with an isolated config
  stop             tear it down
  app <args...>    run the overlay binary in it, wait for exit
  overlay [mode]   open the overlay in the background, wait until it is mapped
  key <char>       click the card for a-z / 0-9
  mode <name>      click switch | delete | move
  click <x> <y>    click anywhere
  escape           dismiss the overlay
  open | closed    is the overlay mapped / wait until it is not
  state            workspaces as JSON
  shot [name]      screenshot to e2e/out/<name>.png
  run <cmd...>     run any command against the nested session
  logs [n]         tail the overlay and compositor logs
EOF
}

cmd=${1:-help}
shift || true
case $cmd in
start)   start ;;
stop)    stop ;;
run)     in_env "$@" ;;
app)     in_env "$bin" "$@" ;;
overlay) in_env "$bin" "${1:-switch}" >>"$run_dir/app.log" 2>&1 &
         wait_for 15 overlay_open || die "overlay did not open"
         # Mapped is not yet laid out; an early click can miss the widget it aims at.
         sleep 0.5 ;;
escape)  in_env wtype -k Escape ;;
click)   click_at "$1" "$2" ;;
key)     click_at $(key_position "$1") ;;
mode)    click_at $(mode_position "$1") ;;
shot)    in_env grim "$out_dir/${1:-shot}.png" && echo "$out_dir/${1:-shot}.png" ;;
state)   in_env niri msg -j workspaces ;;
open)    overlay_open ;;
closed)  wait_for "${1:-10}" overlay_closed || die "overlay stayed open" ;;
logs)    tail -n "${1:-20}" "$run_dir/app.log" "$run_dir/niri.log" ;;
*)       usage ;;
esac
