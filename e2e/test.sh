#!/usr/bin/env bash
# Every test gets a fresh nested session; failures leave a screenshot and logs in e2e/out/.
set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
h=$here/harness.sh
out=${NDW_E2E_OUT:-$here/out}
failed=0

focused_ws() { "$h" state | jq -r '.[] | select(.is_focused).name // empty'; }
focused_id() { "$h" state | jq -r '.[] | select(.is_focused).id'; }
focused_id_is() { [[ $(focused_id) == "$1" ]]; }
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
window_on() { window_in "$(ws_id "$1")" "${2:-}"; }
# window_in <ws-id> [app-id]: window_on for a workspace without a name.
window_in() {
    windows | jq -e --arg ws "$1" --arg app "${2:-}" \
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

test_move_window_without_window_creates_nothing() {
    local err status
    "$h" app switch a || return 1
    err=$("$h" app move-window x 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *"no focused window"* ]] && no_ws dyn-x
}

test_overlay_move_without_window_stays_open() {
    "$h" overlay move-window && "$h" key x || return 1
    sleep 0.5
    "$h" open && no_ws dyn-x
}

test_overlay_card_click_switches() {
    "$h" overlay switch || return 1
    "$h" key c || return 1
    until_true has_ws dyn-c && "$h" closed
}

# The static row labels an unnamed workspace with its index, which niri does not resolve as a name.
test_overlay_static_card_moves_to_unnamed() {
    local unnamed
    spawn_window one && unnamed=$(focused_id) || return 1
    "$h" app switch a && spawn_window two || return 1
    # The row holds that workspace and the trailing empty one.
    "$h" overlay move-window && "$h" static 1 2 && "$h" closed || return 1
    until_true window_in "$unnamed" two
}

test_overlay_static_card_switches_to_unnamed() {
    local unnamed
    # Hover would focus the card before the click does.
    general hover_preview false || return 1
    spawn_window one && unnamed=$(focused_id) || return 1
    "$h" app switch a && spawn_window two || return 1
    "$h" overlay switch && "$h" static 1 2 && "$h" closed || return 1
    until_true focused_id_is "$unnamed"
}

test_overlay_missing_pin_errors() {
    local err status
    add_config <<'EOF' || return 1
[workspace.q]
static = "mail"
EOF
    err=$("$h" app switch q 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *"workspace 'mail' not found"* ]] || return 1
    "$h" overlay switch && "$h" key q || return 1
    sleep 0.5
    "$h" open
}

test_overlay_hover_escape_restores_unnamed() {
    local unnamed
    spawn_window one && unnamed=$(focused_id) || return 1
    "$h" app switch b && "$h" run niri msg action focus-workspace 1 || return 1
    until_true focused_id_is "$unnamed" || return 1
    "$h" overlay switch && "$h" hover b || return 1
    until_true focused_is dyn-b || return 1
    "$h" escape && "$h" closed || return 1
    until_true focused_id_is "$unnamed"
}

# The fixture's workspace-auto-back-and-forth turns re-focusing the active workspace into a jump.
test_overlay_escape_keeps_focus() {
    "$h" app switch a && "$h" app switch b || return 1
    "$h" overlay switch && "$h" escape && "$h" closed || return 1
    sleep 0.3
    focused_is dyn-b
}

test_overlay_hover_click_commits() {
    "$h" app switch b && "$h" app switch a || return 1
    # The pointer previews dyn-b on its way to the click.
    "$h" overlay switch && "$h" key b && "$h" closed || return 1
    sleep 0.3
    focused_is dyn-b
}

test_overlay_key_press_switches() {
    "$h" overlay switch && "$h" type c || return 1
    until_true has_ws dyn-c && "$h" closed || return 1
    # Now an existing workspace, with the same keys as the last call.
    "$h" app switch a && "$h" overlay switch && "$h" type c || return 1
    until_true focused_is dyn-c && "$h" closed
}

# press reads evdev codes through the fixture's "us,fr,ru" layout; switch-layout picks the group.
test_overlay_key_azerty_digit() {
    "$h" run niri msg action switch-layout 1 && "$h" overlay switch || return 1
    "$h" press 2 || return 1 # fr: &
    until_true has_ws dyn-1 && "$h" closed
}

test_overlay_key_azerty_shift_digit() {
    "$h" run niri msg action switch-layout 1 && "$h" overlay switch || return 1
    "$h" press 42+3 || return 1 # fr: Shift+é, which types 2
    until_true has_ws dyn-2 && "$h" closed
}

test_overlay_key_cyrillic() {
    "$h" run niri msg action switch-layout 2 && "$h" overlay switch || return 1
    "$h" press 30 || return 1 # ru: ф, on the key us types a with
    until_true has_ws dyn-a && "$h" closed
}

test_overlay_close_bind_ignores_caps_lock() {
    "$h" overlay switch || return 1
    "$h" press 58 && "$h" press 29+46 || return 1 # Caps Lock, then Ctrl+c
    "$h" closed && no_ws dyn-c
}

test_overlay_close_bind_on_cyrillic() {
    "$h" run niri msg action switch-layout 2 && "$h" overlay switch || return 1
    "$h" press 29+46 || return 1 # ru: Ctrl+с, on the key us types c with
    "$h" closed
}

test_overlay_close_bind_with_shift() {
    add_config <<'EOF' || return 1
[keybinds]
close = ["Escape", "Ctrl+Shift+q"]
EOF
    "$h" overlay switch || return 1
    "$h" press 29+42+16 || return 1 # Ctrl+Shift+q
    "$h" closed && no_ws dyn-q
}

# add_templates: beta on key 3 without variables, then gamma with a text variable.
add_templates() {
    add_config <<'EOF'
[template.beta]
programs = ["true"]
title = "BETA"
key = "3"

[template.gamma]
programs = ["true {{path}}"]
key = "g"

[template.gamma.variables.path]
type = "text"
EOF
}

test_held_key_does_not_pick_template() {
    add_templates && "$h" overlay switch || return 1
    # Opens the picker for 3, where 3 is beta's key; held past the repeat delay.
    "$h" type -P 3 -s 1500 -p 3 || return 1
    "$h" open && no_ws "dyn-3 BETA" || return 1
    "$h" type 1 || return 1 # Empty
    until_true has_ws dyn-3
}

test_held_enter_does_not_submit_form() {
    add_templates && "$h" overlay switch || return 1
    "$h" type 5 && "$h" type -k Down -k Down || return 1 # gamma
    # Opens gamma's form, held past the repeat delay.
    "$h" type -P Return -s 1500 -p Return || return 1
    "$h" open && no_ws dyn-5 || return 1
    # The release re-arms Enter.
    "$h" type -k Return || return 1
    until_true has_ws dyn-5
}

test_overlay_escape_closes() {
    "$h" overlay switch || return 1
    "$h" escape || return 1
    "$h" closed && no_ws dyn-c
}

test_invalid_key_fails_in_caller() {
    local err status
    err=$("$h" app switch Q 2>&1 >/dev/null)
    status=$?
    [[ $status == 2 && $err == *"invalid value 'Q'"* ]]
}

test_missing_workspace_reports_to_caller() {
    local err status
    err=$("$h" app delete z 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *dyn-z* ]]
}

test_broken_config_still_opens() {
    local log
    printf '[general\n' | "$h" config && "$h" overlay switch || return 1
    log=$("$h" logs 40)
    [[ $log == *"config error:"* ]] && "$h" escape && "$h" closed
}

test_check_reports_problems() {
    local err status bin=${NDW_BIN:-$here/../target/debug/niri-dynamic-workspaces}
    "$h" app check >/dev/null || return 1
    # No display, bus or HOME, as in a Nix build sandbox.
    env -i "$bin" check --config "$here/fixtures/config.toml" >/dev/null || return 1
    printf '[general\n' | "$h" config || return 1
    err=$("$h" app check 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *"config error:"* ]]
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

test_daemon_forwards_errors_to_caller() {
    local err status
    general layout '"workman"' && "$h" daemon || return 1
    # Both the error and the config warning come from the daemon.
    err=$("$h" app delete z 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *dyn-z* && $err == *"config warning:"* ]] || return 1
    err=$("$h" app switch a 2>&1 >/dev/null)
    status=$?
    [[ $status == 0 && $err == *"config warning:"* ]] || return 1
    err=$("$h" app switch Q 2>&1 >/dev/null)
    status=$?
    [[ $status == 2 && $err == *"invalid value 'Q'"* ]]
}

test_daemon_deletes_empty_workspaces() {
    general auto_delete_empty true || return 1
    "$h" daemon || return 1
    "$h" app switch a && spawn_window || return 1
    "$h" app switch b && "$h" app switch c || return 1
    # Covers the debounce (0.5 s quiet, 2 s at most) and the 0.5 s confirming pass.
    until_true_for 15 no_ws dyn-b && has_ws dyn-a && has_ws dyn-c
}

test_daemon_reloads_config_on_content_change() {
    local config
    config=$(config_toml)
    # Home Manager swaps in store files, which all have mtime 1.
    "$h" run touch -m -d @1 "$config" && "$h" daemon || return 1
    "$h" app switch a && "$h" app switch b || return 1
    until_true has_ws dyn-a || return 1
    general auto_delete_empty true && "$h" run touch -m -d @1 "$config" || return 1
    "$h" app switch c || return 1
    until_true_for 15 no_ws dyn-a && until_true no_ws dyn-b && has_ws dyn-c
}

# overlays_freed <n>: debug builds log a line each time an overlay window is disposed.
overlays_freed() { [[ $("$h" logs 1000 | grep -c "debug: overlay window freed") -eq $1 ]]; }

test_daemon_frees_closed_overlays() {
    "$h" daemon || return 1
    for _ in 1 2 3; do
        "$h" overlay switch && "$h" escape && "$h" closed || return 1
    done
    until_true overlays_freed 3
}

tests=(
    switch_creates_workspace
    delete_removes_workspace
    move_window_moves_it
    move_window_without_window_creates_nothing
    invalid_key_fails_in_caller
    missing_workspace_reports_to_caller
    overlay_card_click_switches
    overlay_move_without_window_stays_open
    overlay_static_card_moves_to_unnamed
    overlay_static_card_switches_to_unnamed
    overlay_missing_pin_errors
    overlay_hover_escape_restores_unnamed
    overlay_escape_keeps_focus
    overlay_hover_click_commits
    overlay_key_press_switches
    overlay_key_azerty_digit
    overlay_key_azerty_shift_digit
    overlay_key_cyrillic
    overlay_close_bind_ignores_caps_lock
    overlay_close_bind_on_cyrillic
    overlay_close_bind_with_shift
    held_key_does_not_pick_template
    held_enter_does_not_submit_form
    overlay_escape_closes
    overlay_delete_mode
    broken_config_still_opens
    check_reports_problems
    daemon_serves_invocations
    daemon_forwards_errors_to_caller
    daemon_deletes_empty_workspaces
    daemon_reloads_config_on_content_change
    daemon_frees_closed_overlays
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
