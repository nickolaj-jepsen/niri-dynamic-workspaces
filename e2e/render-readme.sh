#!/usr/bin/env bash
# Render the README screenshot: the overlay over a busy workspace, as docs/readme.png.
# Run inside 'nix develop' after 'cargo build'.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(dirname "$here")
harness=$here/harness.sh
bin=${NDW_BIN:-$repo/target/debug/niri-dynamic-workspaces}
theme=fireproof

# Everything lands in a scratch dir first, so a failed run leaves docs/ untouched.
tmp=$(mktemp -d)
export NDW_E2E_OUT=$tmp/shots
trap '"$harness" stop; rm -rf "$tmp"' EXIT

# shellcheck source=e2e/render-lib.sh
. "$here/render-lib.sh"

window_count() { "$harness" run niri msg -j windows | jq length; }
action() { "$harness" run niri msg action "$@"; }

# spawn <cmd...>: open a window in a new column, right of the previous one, and wait for it.
spawn() {
    local before
    before=$(window_count)
    "$harness" run "$@" >/dev/null 2>&1 &
    until (($(window_count) > before)); do
        sleep 0.1
    done
}

# edit <file>: the keyfile backend and private dirs keep the editor off the host's settings and session.
edit() {
    spawn env GSETTINGS_BACKEND=keyfile XDG_DATA_HOME="$run/data" XDG_STATE_HOME="$run/state" \
        XDG_CACHE_HOME="$run/cache" gnome-text-editor --standalone --ignore-session "$1"
}

# term <shell command>: a terminal that has just run it; the command runs from the repo root.
term() {
    # Output printed before niri resizes the window reflows badly, so the command waits a moment.
    spawn foot -D "$repo" -o csd.preferred=none -o font=monospace:size=10 -o pad=12x12 \
        -o colors-dark.background=1c1b1a -o colors-dark.foreground=dad8ce \
        sh -c "sleep 1; printf '\$ %s\n' \"\$1\"; PATH=${bin%/*}:\$PATH; eval \"\$1\"; printf '\$ '; sleep 60" sh "$1"
}

write_config "$theme"
run=$(NDW_E2E_CONFIG=$tmp/config.toml NDW_E2E_SIZE=1920x1080 "$harness" start)

# libadwaita ignores GTK_THEME, so the editor goes dark through its own settings.
mkdir -p "$run/config/glib-2.0/settings" "$run/config/gtk-4.0"
cat >"$run/config/glib-2.0/settings/keyfile" <<'KEYFILE'
[org/gnome/TextEditor]
style-variant='dark'
restore-session=false
spellcheck=false
show-line-numbers=true
show-map=true
KEYFILE
# The theme's surfaces, so the apps behind the overlay sit in the same palette.
cat >"$run/config/gtk-4.0/gtk.css" <<'CSS'
@define-color window_bg_color #1c1b1a;
@define-color view_bg_color #1c1b1a;
@define-color headerbar_bg_color #282726;
@define-color window_fg_color #dad8ce;
@define-color accent_bg_color #cf6a4c;
CSS
# Without the settings portal GTK adds minimize and maximize; keep GNOME's close-only title bar.
cat >"$run/config/gtk-4.0/settings.ini" <<'INI'
[Settings]
gtk-decoration-layout=appmenu:close
INI

populate
# An editor beside a column of stacked terminals.
edit "$repo/style.css"
action set-column-width "66.667%"
term "niri-dynamic-workspaces --help"
action set-column-width "33.333%"
term "ls themes"
action consume-or-expel-window-left
# The view scrolled right while the columns were still wide; both fit from the first.
action focus-column-first
# Let the editor finish highlighting and the terminals settle into their size.
sleep 2

"$harness" overlay switch
"$harness" shot readme >/dev/null
check_warnings "$theme"
"$harness" stop
mv "$NDW_E2E_OUT/readme.png" "$repo/docs/readme.png"
echo "wrote docs/readme.png" >&2
