# Sourced by the render-*.sh scripts, which set $here, $harness and $tmp.
# shellcheck shell=bash

# The focused and urgent cards are named after their state, for people comparing themes.
declare -A names=([w]=web [e]=mail [a]=active [s]=frontend [d]=docs [c]=chat [n]=urgent)

# write_config <general.theme value>: the fixture config plus a theme and the named workspaces.
write_config() {
    local key
    {
        # hide_empty_static drops niri's trailing empty workspace from the row above the keyboard.
        sed "s|^\[general\]|[general]\ntheme = \"$1\"\nhide_empty_static = true|" "$here/fixtures/config.toml"
        for key in "${!names[@]}"; do
            printf '\n[workspace.%s]\nname = "%s"\n' "$key" "${names[$key]}"
        done
    } >"$tmp/config.toml"
}

# Create every named workspace, leaving a focused and n urgent.
populate() {
    local key window
    for key in w e s d c n; do
        "$harness" app switch "$key"
    done
    # Urgency belongs to windows, and focusing one clears it: open one on n, leave, then flag it.
    "$harness" run foot sh -c 'sleep 60' >/dev/null 2>&1 &
    until window=$("$harness" run niri msg -j windows | jq -er '.[0].id'); do
        sleep 0.1
    done
    "$harness" app switch a
    "$harness" run niri msg action set-window-urgent --id "$window"
}

# check_warnings <label>: an unknown theme or broken CSS still opens the overlay, just with the wrong colours.
check_warnings() {
    local warnings
    warnings=$("$harness" logs 50 | grep "config error:\|config warning:\|theme warning:\|Theme pars" | sort -u || true)
    if [[ -n $warnings ]]; then
        echo "$warnings" >&2
        echo "${0##*/}: '$1' did not load cleanly" >&2
        exit 1
    fi
}
