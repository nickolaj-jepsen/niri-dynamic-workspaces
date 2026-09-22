#!/usr/bin/env bash
# Every test gets a fresh nested session; failures leave a screenshot and logs in e2e/out/.
set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
h=$here/harness.sh
out=${NDW_E2E_OUT:-$here/out}
failed=0

focused_ws() { "$h" state | jq -r '.[] | select(.is_focused).name // empty'; }
ws_id() { "$h" state | jq -r --arg n "$1" '.[] | select(.name == $n) | .id'; }
focused_is() { [[ $(focused_ws) == "$1" ]]; }

# Actions run over IPC, so poll rather than assume the next call sees them.
until_true_for() {
    local deadline=$((SECONDS + $1))
    shift
    until "$@"; do
        ((SECONDS < deadline)) || return 1
        sleep 0.2
    done
}
until_true() { until_true_for 5 "$@"; }

has_ws() { [[ -n $(ws_id "$1") ]]; }
no_ws() { [[ -z $(ws_id "$1") ]]; }

windows() { "$h" run niri msg -j windows; }
# has_focused_window [app-id]: move-window acts on the focused window, so a mapped one is not enough.
has_focused_window() {
    windows | jq -e --arg app "${1:-}" 'any(.[]; .is_focused and ($app == "" or .app_id == $app))' >/dev/null
}
# window_on <ws-name> [app-id]: a window (with that app id) is on the workspace.
window_on() {
    windows | jq -e --arg ws "$(ws_id "$1")" --arg app "${2:-}" \
        'any(.[]; (.workspace_id | tostring) == $ws and ($app == "" or .app_id == $app))' >/dev/null
}
# windows_on <ws-name>: how many windows the workspace holds.
windows_on() {
    windows | jq --arg ws "$(ws_id "$1")" '[.[] | select((.workspace_id | tostring) == $ws)] | length'
}
# spawn_window [app-id]: open a foot on the focused workspace and wait until it has focus.
spawn_window() {
    "$h" run foot --app-id "${1:-foot}" sh -c 'sleep 60' >/dev/null 2>&1 &
    until_true has_focused_window "${1:-foot}"
}

# The app re-reads its config on every invocation; these rewrite it through the `config` verb.
config_toml() { echo "$("$h" run printenv XDG_CONFIG_HOME)/niri-dynamic-workspaces/config.toml"; }
# add_config: append stdin (TOML tables, usually a heredoc) to the session's config.
add_config() { cat "$(config_toml)" - | "$h" config; }
# general <key> <toml-value>: set a [general] key, replacing any earlier value.
general() {
    K=$1 V=$2 awk '
        /^\[/ { in_general = ($0 == "[general]") }
        in_general && $0 ~ "^" ENVIRON["K"] "[ \t]*=" { next }
        { print }
        $0 == "[general]" { print ENVIRON["K"] " = " ENVIRON["V"]; seen = 1 }
        END { if (!seen) print "[general]\n" ENVIRON["K"] " = " ENVIRON["V"] }
    ' "$(config_toml)" | "$h" config
}

test_switch_creates_workspace() {
    "$h" app switch a || return 1
    until_true has_ws dyn-a && focused_is dyn-a
}

test_delete_removes_workspace() {
    "$h" app switch a && "$h" app switch b || return 1
    until_true has_ws dyn-a || return 1
    "$h" app delete a || return 1
    until_true no_ws dyn-a
}

test_move_window_moves_it() {
    "$h" app switch a && spawn_window || return 1
    "$h" app move-window b || return 1
    until_true window_on dyn-b
}

test_overlay_card_click_switches() {
    "$h" overlay switch || return 1
    "$h" key c || return 1
    until_true has_ws dyn-c && "$h" closed
}

test_overlay_key_press_switches() {
    "$h" overlay switch && "$h" type c || return 1
    until_true has_ws dyn-c && "$h" closed || return 1
    # Now an existing workspace, with the same keys as the last call.
    "$h" app switch a && "$h" overlay switch && "$h" type c || return 1
    until_true focused_is dyn-c && "$h" closed
}

test_overlay_escape_closes() {
    "$h" overlay switch || return 1
    "$h" escape || return 1
    "$h" closed && no_ws dyn-c
}

test_overlay_delete_mode() {
    "$h" app switch d && "$h" app switch a || return 1
    until_true has_ws dyn-d || return 1
    "$h" overlay switch || return 1
    "$h" mode delete || return 1
    "$h" key d || return 1
    until_true no_ws dyn-d
}

test_daemon_serves_invocations() {
    "$h" daemon || return 1
    "$h" app switch a || return 1
    until_true has_ws dyn-a || return 1
    "$h" overlay switch && "$h" key c || return 1
    until_true has_ws dyn-c && "$h" closed || return 1
    # A repeat in the same mode closes the overlay, and the hold keeps the daemon up after it.
    "$h" overlay switch && "$h" app switch && "$h" closed && "$h" serving
}

test_daemon_deletes_empty_workspaces() {
    general auto_delete_empty true || return 1
    "$h" daemon || return 1
    "$h" app switch a && spawn_window || return 1
    "$h" app switch b && "$h" app switch c || return 1
    # Covers the debounce (0.5 s quiet, 2 s at most) and the 0.5 s confirming pass.
    until_true_for 15 no_ws dyn-b && has_ws dyn-a && has_ws dyn-c
}

tests=(
    switch_creates_workspace
    delete_removes_workspace
    move_window_moves_it
    overlay_card_click_switches
    overlay_key_press_switches
    overlay_escape_closes
    overlay_delete_mode
    daemon_serves_invocations
    daemon_deletes_empty_workspaces
)

[[ ${1:-} ]] && tests=("$@")

mkdir -p "$out"
for name in "${tests[@]}"; do
    if ! "$h" start >/dev/null; then
        echo "not ok - $name (session did not start)"
        "$h" logs 40 >"$out/fail-$name.log" 2>&1
        echo "    see $out/fail-$name.log"
        failed=1
        # A failed start can leave the bus and compositor running.
        "$h" stop
        continue
    fi
    if "test_$name" >/dev/null 2>&1; then
        echo "ok - $name"
    else
        echo "not ok - $name"
        "$h" shot "fail-$name" >/dev/null 2>&1
        "$h" logs 40 >"$out/fail-$name.log" 2>&1
        echo "    see $out/fail-$name.png and fail-$name.log"
        failed=1
    fi
    "$h" stop
done

exit $failed
