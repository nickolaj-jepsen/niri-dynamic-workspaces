#!/usr/bin/env bash
# Every test gets a fresh nested session; failures leave a screenshot and logs in e2e/out/.
set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
h=$here/harness.sh
out=${NDW_E2E_OUT:-$here/out}
failed=0

focused_ws() { "$h" state | jq -r '.[] | select(.is_focused).name // empty'; }
ws_id() { "$h" state | jq -r --arg n "$1" '.[] | select(.name == $n) | .id'; }

# Actions run over IPC, so poll rather than assume the next call sees them.
until_true() {
    local deadline=$((SECONDS + 5))
    until "$@"; do
        ((SECONDS < deadline)) || return 1
        sleep 0.2
    done
}

has_ws() { [[ -n $(ws_id "$1") ]]; }
no_ws() { [[ -z $(ws_id "$1") ]]; }

test_switch_creates_workspace() {
    "$h" app switch a || return 1
    until_true has_ws dyn-a && [[ $(focused_ws) == dyn-a ]]
}

test_delete_removes_workspace() {
    "$h" app switch a && "$h" app switch b || return 1
    until_true has_ws dyn-a || return 1
    "$h" app delete a || return 1
    until_true no_ws dyn-a
}

test_move_window_moves_it() {
    "$h" app switch a || return 1
    "$h" run foot sh -c 'sleep 60' &
    until_true test -n "$("$h" run niri msg -j windows | jq -r '.[0].id')" || return 1
    "$h" app move-window b || return 1
    until_true has_ws dyn-b || return 1
    local window_ws
    window_ws=$("$h" run niri msg -j windows | jq -r '.[0].workspace_id')
    [[ $window_ws == "$(ws_id dyn-b)" ]]
}

test_overlay_card_click_switches() {
    "$h" overlay switch || return 1
    "$h" key c || return 1
    until_true has_ws dyn-c && "$h" closed
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

tests=(
    switch_creates_workspace
    delete_removes_workspace
    move_window_moves_it
    overlay_card_click_switches
    overlay_escape_closes
    overlay_delete_mode
)

[[ ${1:-} ]] && tests=("$@")

for name in "${tests[@]}"; do
    if ! "$h" start >/dev/null; then
        echo "not ok - $name (session did not start)"
        failed=1
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
