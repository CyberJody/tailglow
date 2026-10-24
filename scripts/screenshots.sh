#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Regenerates screenshots/light/ and screenshots/dark/ with the fake mouse on
# a private X display (Xvnc :9 by default; never the desktop's display).
#
# Usage: scripts/screenshots.sh [GUI_BINARY] [OUT_ROOT]
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
gui="$(realpath "${1:-$here/../target/release/tailglow-gui}")"
root="$(realpath -m "${2:-$here/../screenshots}")"
disp="${TAILGLOW_TEST_DISPLAY:-:9}"
work="$(mktemp -d "${TMPDIR:-/tmp}/tailglow-shots.XXXXXX")"
xvnc_pid=""
trap '[ -n "$xvnc_pid" ] && kill "$xvnc_pid" 2>/dev/null; true' EXIT

if ! xdpyinfo -display "$disp" >/dev/null 2>&1; then
    Xvnc "$disp" -geometry 1040x760 -depth 24 -SecurityTypes None -localhost -rfbport 0 \
        >"$work/xvnc.log" 2>&1 &
    xvnc_pid=$!
    sleep 1.5
fi

shoot() { # theme fake outdir
    local theme="$1" fake="$2" dir="$3"
    mkdir -p "$dir"
    # Fresh state/config per run, so nothing from the desktop leaks in and
    # nothing is written to the user's config. Run inside the work dir with
    # a relative state path, so the About page shows "state.toml" rather
    # than a private temp path.
    mkdir -p "$work/$theme-$fake-run"
    (cd "$work/$theme-$fake-run" &&
        DISPLAY="$disp" WAYLAND_DISPLAY= TAILGLOW_FAKE="$fake" TAILGLOW_BLUEZ_BUS=off \
            TAILGLOW_STATE="state.toml" TAILGLOW_GUI_CONFIG="$work/gui.toml" \
            DBUS_SESSION_BUS_ADDRESS="unix:path=$work/no-bus" \
            timeout 60 "$gui" --theme "$theme" --screenshot "$dir" 2>&1) | sed "s/^/  [$theme $fake] /"
}

for theme in light dark; do
    out="$root/$theme"
    mkdir -p "$out"
    shoot "$theme" session2 "$out"
    shoot "$theme" none "$work/$theme-none"
    mv "$work/$theme-none/05-no-mouse.png" "$out/05-no-mouse.png"
    shoot "$theme" noperm "$work/$theme-noperm"
    mv "$work/$theme-noperm/05-no-mouse.png" "$out/06-no-permission.png"
done
ls -l "$root/light" "$root/dark"
