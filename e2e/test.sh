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
# stays_focused <ws> <secs>: the workspace keeps focus that long.
stays_focused() {
    local deadline=$((SECONDS + $2))
    while ((SECONDS < deadline)); do
        focused_is "$1" || return 1
        sleep 0.25
    done
}

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

# stop_window: SIGSTOP the only window's client, which then leaves close requests unanswered; prints its pid.
# Callers must SIGCONT it: a stopped client outlives the session.
stop_window() {
    local pid
    pid=$(windows | jq -r '.[0].pid')
    [[ $pid =~ ^[0-9]+$ ]] && kill -STOP "$pid" && echo "$pid"
}

test_delete_closes_windows_before_unnaming() {
    local pid ok
    "$h" app switch a && spawn_window && pid=$(stop_window) || return 1
    "$h" app delete a &
    sleep 1
    has_ws dyn-a
    ok=$?
    kill -CONT "$pid"
    # The caller returns once the name is gone.
    wait $! && ((ok == 0)) && windows | jq -e 'length == 0' >/dev/null && no_ws dyn-a
}

# delete_a_is_refused: delete a waits out the close timeout, fails, and dyn-a keeps its name and window.
delete_a_is_refused() {
    local err status
    err=$("$h" app delete a 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *"did not close"* ]] && has_ws dyn-a && window_on dyn-a
}

test_delete_keeps_workspace_with_open_window() {
    local pid ok
    "$h" app switch a && spawn_window && pid=$(stop_window) || return 1
    # Then through the daemon, which learns the outcome after its handler returned.
    delete_a_is_refused && "$h" daemon && delete_a_is_refused
    ok=$?
    kill -CONT "$pid"
    return "$ok"
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

# add_hooks: hooks that leave marks for has_mark; niri runs them with the session's XDG_CONFIG_HOME.
add_hooks() {
    add_config <<'EOF'
[hooks]
on_create = ['sleep 1; touch "$XDG_CONFIG_HOME/created-1-$NDW_WORKSPACE_KEY"', 'touch "$XDG_CONFIG_HOME/created-2-$NDW_WORKSPACE_KEY"']
on_delete = ['touch "$XDG_CONFIG_HOME/deleted-$NDW_WORKSPACE_NAME"']
EOF
}
# has_mark <name>: a hook left that mark.
has_mark() { "$h" run sh -c 'test -e "$XDG_CONFIG_HOME/$1"' _ "$1"; }

# Without a daemon the process exits right after the switch, before the second hook is due.
test_hooks_outlive_the_process() {
    add_hooks && "$h" app switch a || return 1
    until_true has_mark created-1-a && until_true has_mark created-2-a
}

test_move_window_runs_create_hooks() {
    add_hooks && "$h" app switch a && spawn_window || return 1
    "$h" app move-window b || return 1
    until_true window_on dyn-b && until_true has_mark created-2-b
}

test_delete_hook_gets_full_name() {
    add_hooks && "$h" app switch a || return 1
    "$h" run niri msg action set-workspace-name "dyn-a Notes" || return 1
    until_true has_ws "dyn-a Notes" || return 1
    "$h" app delete a || return 1
    until_true has_mark "deleted-dyn-a Notes"
}

# A reorder still waiting on a late window must not pull the user back after they leave.
test_reorder_leaves_focus_alone() {
    local ok
    # The direct foot is listed second, so it is out of its slot and the reorder cannot skip it.
    add_config <<'EOF' || return 1
[workspace.c]
programs = ["sh -c 'sleep 2; exec foot sleep 60'", "foot sleep 60"]
EOF
    "$h" app switch b || return 1
    # The reorder's hold keeps this instance up until it is done; the switch back is forwarded to it.
    "$h" app switch c &
    # The late foot maps on dyn-b, so the reorder waits out its 5 s budget and acts at about 5.6 s.
    until_true window_on dyn-c && "$h" app switch b && stays_focused dyn-b 8
    ok=$?
    wait
    ((ok == 0)) && focused_is dyn-b
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

test_overlay_hover_then_move_moves_origin_window() {
    "$h" app switch b && spawn_window on-b || return 1
    "$h" app switch a && spawn_window on-a || return 1
    "$h" overlay switch && "$h" hover b || return 1
    until_true focused_is dyn-b || return 1
    "$h" mode move && "$h" key c && "$h" closed || return 1
    until_true window_on dyn-c on-a && window_on dyn-b on-b
}

test_overlay_hover_then_delete_returns_to_origin() {
    "$h" app switch d || return 1
    "$h" app switch b && spawn_window on-b || return 1
    "$h" app switch a && "$h" overlay switch && "$h" hover b || return 1
    until_true focused_is dyn-b || return 1
    "$h" mode delete && "$h" key d || return 1
    until_true no_ws dyn-d && "$h" closed && focused_is dyn-a
}

test_overlay_move_follows_captured_window() {
    "$h" app switch b && spawn_window on-b || return 1
    "$h" app switch a && spawn_window on-a || return 1
    # Focus moves under the open overlay; the window it opened with still moves.
    "$h" overlay move-window && "$h" run niri msg action focus-workspace dyn-b || return 1
    until_true focused_is dyn-b || return 1
    "$h" key c && "$h" closed || return 1
    until_true window_on dyn-c on-a && window_on dyn-b on-b
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

# add_select_template <type> <source line>: template delta on key d whose one variable, branch, has that source.
add_select_template() {
    add_config <<EOF
[template.delta]
programs = ["true {{branch}}"]
key = "d"

[template.delta.variables.branch]
name = "Branch"
type = "$1"
$2
EOF
}

test_form_refuses_unmatched_option() {
    add_select_template options 'options = ["main", "develop"]' && "$h" overlay switch || return 1
    "$h" type 5 && "$h" type d || return 1 # delta's form
    "$h" type zzz && "$h" type -k Return || return 1
    sleep 0.3
    "$h" open && no_ws dyn-5 || return 1
    "$h" type -k BackSpace -k BackSpace -k BackSpace && "$h" type dev && "$h" type -k Return || return 1
    until_true has_ws "dyn-5 develop"
}

test_form_click_picks_option() {
    add_select_template options 'options = ["main", "develop"]' && "$h" overlay switch || return 1
    "$h" type 5 && "$h" type d || return 1 # delta's form
    sleep 0.5 # an early click can land before the form is laid out
    # Right of the text on develop's row, the second of two; read off a 'shot' if the form changes.
    "$h" click 700 389 && "$h" type -k Return || return 1
    until_true has_ws "dyn-5 develop"
}

test_form_takes_typed_command_value() {
    add_select_template command 'command = "echo main; echo develop"' && "$h" overlay switch || return 1
    "$h" type 5 && "$h" type d || return 1 # delta's form
    sleep 0.3 # the options load off the main thread
    "$h" type feature-x && "$h" type -k Return || return 1
    until_true has_ws "dyn-5 feature-x"
}

# A command whose options never arrive; slow.pid names its background sleep, which only a group kill reaches.
slow_command='command = '\''sleep 30 & echo $! >"$XDG_CONFIG_HOME/slow.pid"; wait'\'
slow_pid() {
    local pid
    pid=$(cat "$("$h" run printenv XDG_CONFIG_HOME)/slow.pid" 2>/dev/null) && [[ $pid ]] && echo "$pid"
}
# gone <pid>: the process has exited; a zombie counts, since its parent died with it.
gone() { [[ ! -e /proc/$1 || $(cut -d' ' -f3 "/proc/$1/stat" 2>/dev/null) == Z ]]; }

test_form_escape_stops_slow_command() {
    local pid
    add_select_template command "$slow_command" && "$h" overlay switch || return 1
    "$h" type 5 && "$h" type d || return 1 # delta's form
    until_true slow_pid >/dev/null && pid=$(slow_pid) || return 1
    "$h" type -k Escape || return 1 # back to the picker
    until_true gone "$pid" && "$h" open
}

test_form_offers_text_after_command_timeout() {
    local pid
    add_select_template command "$slow_command" && "$h" overlay switch || return 1
    "$h" type 5 && "$h" type d || return 1 # delta's form
    until_true slow_pid >/dev/null && pid=$(slow_pid) || return 1
    until_true_for 15 gone "$pid" || return 1
    "$h" type late && "$h" type -k Return || return 1
    until_true has_ws "dyn-5 late"
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

test_rename_sets_and_clears_title() {
    local err status
    "$h" app switch a && until_true has_ws dyn-a || return 1
    "$h" app rename a "My title" && until_true has_ws "dyn-a My title" || return 1
    # niri ignores a name that differs only in case, unless it goes through the bare name first.
    "$h" app rename a "my title" && until_true has_ws "dyn-a my title" || return 1
    "$h" app rename focused && until_true has_ws dyn-a || return 1
    err=$("$h" app rename z x 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *"workspace 'dyn-z' does not exist"* ]]
}

# marker_is <text>: a program wrote that text to $XDG_CONFIG_HOME/marker.
marker_is() { "$h" run sh -c '[ "$(cat "$XDG_CONFIG_HOME/marker" 2>/dev/null)" = "$1" ]' _ "$1"; }

test_cli_switch_template_with_vars() {
    local err status
    add_config <<'EOF' || return 1
[template.dev]
programs = ['''sh -c 'printf %s "$1" >"$XDG_CONFIG_HOME/marker"' sh {{project}}''']
on_create = ['touch "$XDG_CONFIG_HOME/created-$NDW_TEMPLATE-$NDW_VAR_PROJECT"']

[template.dev.variables.project]
name = "Project"
EOF
    "$h" app switch p --template dev --var 'project=my proj' || return 1
    until_true has_ws "dyn-p my proj" && until_true marker_is "my proj" || return 1
    until_true has_mark "created-dev-my proj" || return 1
    # Refused before anything is sent.
    err=$("$h" app switch q --template dev 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *"needs --var for: project"* ]] && no_ws dyn-q || return 1
    err=$("$h" app switch q --template nope 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *"unknown template 'nope' (known: dev)"* ]] && no_ws dyn-q
}

test_cli_switch_title() {
    local err status
    add_config <<'EOF' || return 1
[workspace.q]
static = "mail"
EOF
    # Through the daemon, which reads the flags off the forwarded command line.
    "$h" daemon && "$h" app switch t --title Notes || return 1
    until_true has_ws "dyn-t Notes" && "$h" app switch a && until_true focused_is dyn-a || return 1
    err=$("$h" app switch t --title Other 2>&1 >/dev/null)
    status=$?
    [[ $status == 0 && $err == *"only apply when it is created"* ]] || return 1
    until_true focused_is "dyn-t Notes" || return 1
    err=$("$h" app switch q --title x 2>&1 >/dev/null)
    status=$?
    [[ $status == 1 && $err == *"pinned to static workspace 'mail'"* ]]
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

# The first press on a workspace with windows only arms its delete; the same key again confirms.
test_overlay_delete_confirms_occupied() {
    "$h" app switch a && spawn_window || return 1
    "$h" overlay delete && "$h" key a || return 1
    sleep 0.5
    "$h" open && window_on dyn-a || return 1
    "$h" key a || return 1
    until_true no_ws dyn-a && "$h" closed
}

# Held past the repeat delay, the key arms the delete once; its repeats must not confirm it.
test_held_key_does_not_confirm_delete() {
    "$h" app switch a && spawn_window || return 1
    "$h" overlay delete && "$h" type -P a -s 1500 -p a || return 1
    "$h" open && window_on dyn-a || return 1
    "$h" type a || return 1
    until_true no_ws dyn-a && "$h" closed
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

# A misspelled key is ignored, and the daemon names it in the caller's terminal.
test_unknown_key_reaches_caller_via_daemon() {
    local err status
    general hover_previw false && "$h" daemon || return 1
    err=$("$h" app switch a 2>&1 >/dev/null)
    status=$?
    [[ $status == 0 && $err == *"unknown key 'general.hover_previw'"* ]] && until_true has_ws dyn-a
}

test_daemon_deletes_empty_workspaces() {
    general auto_delete_empty true && add_hooks || return 1
    "$h" daemon || return 1
    "$h" app switch a && spawn_window || return 1
    "$h" app switch b && "$h" app switch c || return 1
    # Covers the debounce (0.5 s quiet, 2 s at most) and the 0.5 s confirming pass.
    until_true_for 15 no_ws dyn-b && has_ws dyn-a && has_ws dyn-c || return 1
    until_true has_mark deleted-dyn-b
}

# The new workspace stays empty until its program maps, and must outlive the user leaving it.
test_daemon_keeps_workspace_for_slow_program() {
    local id
    general auto_delete_empty true && add_config <<'EOF' || return 1
[workspace.e]
programs = ["sh -c 'sleep 5; exec foot sleep 60'"]
EOF
    "$h" daemon || return 1
    "$h" app switch e && until_true has_ws dyn-e && id=$(ws_id dyn-e) || return 1
    "$h" app switch b || return 1
    # Past the debounce and the confirming pass, which removed it before.
    sleep 3
    [[ $(ws_id dyn-e) == "$id" ]] || return 1
    "$h" app switch e && until_true window_on dyn-e
}

# hover_away_from_empty_origin: an overlay opened on the empty dyn-a previews dyn-b; dyn-a stays meanwhile.
hover_away_from_empty_origin() {
    general auto_delete_empty true && "$h" daemon || return 1
    "$h" app switch b && spawn_window on-b || return 1
    "$h" app switch a && "$h" overlay switch && "$h" hover b || return 1
    until_true focused_is dyn-b || return 1
    # Past the debounce and the confirming pass.
    sleep 3
    has_ws dyn-a
}

test_overlay_hover_keeps_empty_origin() {
    hover_away_from_empty_origin || return 1
    "$h" escape && "$h" closed || return 1
    until_true focused_is dyn-a
}

# Committing the preview emits no event cleanup reacts to; the close itself must prompt a pass.
test_overlay_hover_commit_cleans_origin() {
    hover_away_from_empty_origin || return 1
    "$h" key b && "$h" closed || return 1
    until_true no_ws dyn-a
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
    delete_closes_windows_before_unnaming
    delete_keeps_workspace_with_open_window
    move_window_moves_it
    move_window_without_window_creates_nothing
    hooks_outlive_the_process
    move_window_runs_create_hooks
    delete_hook_gets_full_name
    reorder_leaves_focus_alone
    invalid_key_fails_in_caller
    missing_workspace_reports_to_caller
    rename_sets_and_clears_title
    cli_switch_template_with_vars
    cli_switch_title
    overlay_card_click_switches
    overlay_move_without_window_stays_open
    overlay_static_card_moves_to_unnamed
    overlay_static_card_switches_to_unnamed
    overlay_missing_pin_errors
    overlay_hover_escape_restores_unnamed
    overlay_escape_keeps_focus
    overlay_hover_click_commits
    overlay_hover_then_move_moves_origin_window
    overlay_hover_then_delete_returns_to_origin
    overlay_move_follows_captured_window
    overlay_key_press_switches
    overlay_key_azerty_digit
    overlay_key_azerty_shift_digit
    overlay_key_cyrillic
    overlay_close_bind_ignores_caps_lock
    overlay_close_bind_on_cyrillic
    overlay_close_bind_with_shift
    held_key_does_not_pick_template
    held_enter_does_not_submit_form
    form_refuses_unmatched_option
    form_click_picks_option
    form_takes_typed_command_value
    form_escape_stops_slow_command
    form_offers_text_after_command_timeout
    overlay_escape_closes
    overlay_delete_mode
    overlay_delete_confirms_occupied
    held_key_does_not_confirm_delete
    broken_config_still_opens
    check_reports_problems
    daemon_serves_invocations
    daemon_forwards_errors_to_caller
    unknown_key_reaches_caller_via_daemon
    daemon_deletes_empty_workspaces
    daemon_keeps_workspace_for_slow_program
    overlay_hover_keeps_empty_origin
    overlay_hover_commit_cleans_origin
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
